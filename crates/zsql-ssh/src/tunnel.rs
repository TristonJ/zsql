//! The tunnel itself: a local loopback listener that forwards accepted
//! connections to a remote host:port over an SSH `direct-tcpip` channel.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::interval;

use crate::auth;
use crate::config::{HostKeyPolicy, SshAuth, SshConfig};
use crate::error::SshError;
use crate::handler::ClientHandler;
use crate::runtime;

/// Loopback address the local tunnel listener binds to, and the address
/// reported to the SSH server as the forwarding's originator. Named so it
/// is never re-typed as a bare literal at more than one call site.
const LOCAL_BIND_HOST: &str = "127.0.0.1";

/// Any port, requesting the OS assign an unused ephemeral one.
const EPHEMERAL_PORT: u16 = 0;

/// Bounds how long the SSH connect + authenticate phase may take before
/// `open_tunnel` gives up, so an unreachable or black-holed host cannot
/// hang the caller forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A live SSH tunnel: a local loopback listener that forwards each accepted
/// connection to a remote host:port over the tunnel's SSH session.
///
/// Dropping the tunnel signals the background runtime to stop accepting new
/// connections, close the listener, and disconnect the SSH session.
#[derive(Debug)]
pub struct SshTunnel {
    local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
}

impl SshTunnel {
    /// The loopback address a client should connect to in order to reach
    /// the tunnel's remote endpoint.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let span = tracing::info_span!("ssh_tunnel_shutdown", local_addr = %self.local_addr);
        let _enter = span.enter();
        if let Some(shutdown) = self.shutdown.take() {
            tracing::info!("signaling ssh tunnel shutdown");
            let _ = shutdown.send(());
        }
    }
}

/// Opens an SSH session to `cfg.host:cfg.port` and a local loopback listener
/// that forwards each accepted connection to `remote_host:remote_port`
/// through that session.
///
/// The session and listener run on `zsql-ssh`'s own shared background
/// runtime; this future only awaits a control message back from that
/// runtime, so it carries no tokio or russh types and can be driven from any
/// executor.
///
/// # Errors
///
/// Returns an error if `cfg.host_key` policy is not yet supported (Prompt),
/// the SSH host is unreachable, authentication fails, the session ends
/// abnormally, or the local loopback listener cannot be created.
#[tracing::instrument(name = "ssh_open_tunnel", skip(cfg), fields(host = %cfg.host, port = cfg.port))]
pub async fn open_tunnel(
    cfg: SshConfig,
    remote_host: String,
    remote_port: u16,
) -> Result<SshTunnel, SshError> {
    require_supported_host_key_policy(&cfg.host_key)?;

    let handle = runtime::handle();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    handle.spawn(run_tunnel(
        cfg,
        remote_host,
        remote_port,
        ready_tx,
        shutdown_rx,
    ));

    match ready_rx.await {
        Ok(Ok(local_addr)) => Ok(SshTunnel {
            local_addr,
            shutdown: Some(shutdown_tx),
        }),
        Ok(Err(err)) => Err(err),
        Err(_recv_error) => Err(SshError::RuntimeUnavailable),
    }
}

/// Only [`HostKeyPolicy::Prompt`] is unimplemented; the interactive
/// confirmation it needs is out of scope for this crate. Checked up front so
/// it fails immediately instead of after a wasted connect and handshake.
fn require_supported_host_key_policy(policy: &HostKeyPolicy) -> Result<(), SshError> {
    match policy {
        HostKeyPolicy::AcceptNew | HostKeyPolicy::KnownHosts(_) => Ok(()),
        HostKeyPolicy::Prompt => Err(SshError::UnsupportedHostKeyPolicy { policy: "prompt" }),
    }
}

pub(crate) type SessionHandle = russh::client::Handle<ClientHandler>;

/// Runs entirely on the shared background runtime: connects and
/// authenticates the SSH session, binds the local listener, reports
/// readiness (or a fatal error) through `ready_tx`, then serves the accept
/// loop until `shutdown_rx` fires.
async fn run_tunnel(
    cfg: SshConfig,
    remote_host: String,
    remote_port: u16,
    ready_tx: oneshot::Sender<Result<SocketAddr, SshError>>,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let established = match tokio::time::timeout(CONNECT_TIMEOUT, establish(&cfg)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(SshError::Connect {
            host: cfg.host.clone(),
            port: cfg.port,
            reason: "connection attempt timed out".to_owned(),
        }),
    };

    let (session, listener) = match established {
        Ok(pair) => pair,
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            return;
        }
    };

    let local_addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(err) => {
            let _ = ready_tx.send(Err(SshError::ListenerBind {
                reason: err.to_string(),
            }));
            return;
        }
    };

    if ready_tx.send(Ok(local_addr)).is_err() {
        // The caller already dropped its future; nothing left to serve.
        return;
    }

    accept_loop(
        session,
        listener,
        remote_host,
        remote_port,
        cfg.keepalive,
        shutdown_rx,
    )
    .await;
}

/// Connects and authenticates the SSH session, then binds the local
/// loopback listener. Kept as one step so a failure at any stage resolves
/// `open_tunnel` to an error instead of leaving a half-open listener with no
/// session.
#[tracing::instrument(name = "ssh_connect", skip(cfg), fields(host = %cfg.host, port = cfg.port))]
async fn establish(cfg: &SshConfig) -> Result<(SessionHandle, TcpListener), SshError> {
    if matches!(cfg.auth, SshAuth::Agent) {
        auth::ensure_agent_available().await?;
    }

    let config = Arc::new(russh::client::Config::default());
    let (handler, host_key_error) =
        ClientHandler::new(cfg.host_key.clone(), cfg.host.clone(), cfg.port);

    let mut session =
        match russh::client::connect(config, (cfg.host.as_str(), cfg.port), handler).await {
            Ok(session) => session,
            Err(err) => {
                return Err(
                    take_host_key_error(&host_key_error).unwrap_or(SshError::Connect {
                        host: cfg.host.clone(),
                        port: cfg.port,
                        reason: err.to_string(),
                    }),
                );
            }
        };

    auth::authenticate(&mut session, cfg.user.as_str(), &cfg.auth).await?;

    let listener = TcpListener::bind((LOCAL_BIND_HOST, EPHEMERAL_PORT))
        .await
        .map_err(|err| SshError::ListenerBind {
            reason: err.to_string(),
        })?;

    Ok((session, listener))
}

/// Recovers the specific host-key rejection reason the handler stashed
/// during the handshake, if any. `check_server_key` can only tell russh
/// "reject this" via a bare `false`, so the precise [`SshError`] travels out
/// through this cell instead of being lost to a generic connect failure.
fn take_host_key_error(cell: &Arc<Mutex<Option<SshError>>>) -> Option<SshError> {
    cell.lock().ok().and_then(|mut guard| guard.take())
}

/// Accepts inbound loopback connections and forwards each one to
/// `remote_host:remote_port` over a fresh `direct-tcpip` channel on
/// `session`, sending a keepalive on the interval given by `keepalive`,
/// until `shutdown_rx` fires.
async fn accept_loop(
    session: SessionHandle,
    listener: TcpListener,
    remote_host: String,
    remote_port: u16,
    keepalive: Duration,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut keepalive_ticks = keepalive_interval(keepalive).await;

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!("ssh tunnel accept loop stopping");
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((socket, peer)) => {
                        forward_connection(&session, socket, peer, &remote_host, remote_port).await;
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "ssh tunnel listener accept failed");
                    }
                }
            }
            () = tick_keepalive(&mut keepalive_ticks) => {
                if let Err(err) = session.send_keepalive(false).await {
                    tracing::warn!(error = %err, "ssh tunnel keepalive failed");
                }
            }
        }
    }

    drop(listener);
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "tunnel closed", "en")
        .await;
}

/// Builds the accept loop's keepalive ticker, or `None` if `keepalive` is
/// zero. `tokio::time::interval` panics on a zero period, and a caller
/// requesting a zero keepalive plainly means to disable it rather than
/// crash the tunnel.
async fn keepalive_interval(keepalive: Duration) -> Option<tokio::time::Interval> {
    if keepalive.is_zero() {
        return None;
    }
    let mut ticks = interval(keepalive);
    ticks.tick().await; // the first tick fires immediately; skip it
    Some(ticks)
}

/// Resolves on the next keepalive tick, or never if keepalive is disabled,
/// so `accept_loop`'s `select!` can treat both cases uniformly.
async fn tick_keepalive(ticks: &mut Option<tokio::time::Interval>) {
    match ticks {
        Some(ticks) => {
            ticks.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Opens a `direct-tcpip` channel for one accepted socket and spawns a
/// detached task that copies bytes both ways until either side closes.
async fn forward_connection(
    session: &SessionHandle,
    socket: TcpStream,
    peer: SocketAddr,
    remote_host: &str,
    remote_port: u16,
) {
    let channel = session
        .channel_open_direct_tcpip(
            remote_host.to_owned(),
            u32::from(remote_port),
            LOCAL_BIND_HOST,
            u32::from(peer.port()),
        )
        .await;

    let channel = match channel {
        Ok(channel) => channel,
        Err(err) => {
            tracing::warn!(error = %err, "failed to open ssh direct-tcpip channel");
            return;
        }
    };

    tokio::spawn(async move {
        let mut socket = socket;
        let mut stream = channel.into_stream();
        if let Err(err) = copy_bidirectional(&mut socket, &mut stream).await {
            tracing::debug!(error = %err, "ssh tunnel forwarded connection ended");
        }
    });
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{SshTunnel, keepalive_interval, open_tunnel, require_supported_host_key_policy};
    use crate::config::{HostKeyPolicy, SshAuth, SshConfig};
    use crate::error::SshError;
    use crate::runtime;

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn require_supported_host_key_policy_accepts_accept_new() {
        assert!(require_supported_host_key_policy(&HostKeyPolicy::AcceptNew).is_ok());
    }

    #[test]
    fn require_supported_host_key_policy_accepts_known_hosts() {
        let policy = HostKeyPolicy::KnownHosts("/home/alice/.ssh/known_hosts".into());
        assert!(require_supported_host_key_policy(&policy).is_ok());
    }

    #[test]
    fn require_supported_host_key_policy_rejects_prompt() {
        let err = require_supported_host_key_policy(&HostKeyPolicy::Prompt).unwrap_err();
        assert!(matches!(
            err,
            SshError::UnsupportedHostKeyPolicy { policy: "prompt" }
        ));
    }

    #[tokio::test]
    async fn keepalive_interval_returns_none_for_a_zero_keepalive_without_panicking() {
        assert!(keepalive_interval(Duration::ZERO).await.is_none());
    }

    #[tokio::test]
    async fn keepalive_interval_returns_a_ticker_for_a_nonzero_keepalive() {
        assert!(
            keepalive_interval(Duration::from_millis(10))
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn prompt_host_key_policy_is_rejected_before_touching_the_network() {
        let mut cfg = SshConfig::new("192.0.2.1", "alice", SshAuth::Password("hunter2".into()));
        cfg.host_key = HostKeyPolicy::Prompt;

        let result = open_tunnel(cfg, "db.internal".to_owned(), 5432).await;
        assert!(matches!(
            result,
            Err(SshError::UnsupportedHostKeyPolicy { policy: "prompt" })
        ));
    }

    #[tokio::test]
    async fn password_auth_against_a_closed_local_port_returns_a_connect_error() {
        // Bind then immediately drop a listener to obtain a port nothing is
        // listening on, so the connect attempt fails fast with a clear
        // refusal instead of hanging or needing real network access.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
        let closed_port = probe.local_addr().expect("probe local_addr").port();
        drop(probe);

        let mut cfg = SshConfig::new("127.0.0.1", "alice", SshAuth::Password("hunter2".into()));
        cfg.port = closed_port;

        let result = open_tunnel(cfg, "db.internal".to_owned(), 5432).await;
        match result {
            Err(SshError::Connect { host, port, .. }) => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, closed_port);
            }
            other => panic!("expected SshError::Connect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn key_auth_against_a_closed_local_port_returns_a_connect_error() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
        let closed_port = probe.local_addr().expect("probe local_addr").port();
        drop(probe);

        let mut cfg = SshConfig::new(
            "127.0.0.1",
            "alice",
            SshAuth::Key {
                path: fixture_path("id_ed25519"),
                passphrase: None,
            },
        );
        cfg.port = closed_port;

        let result = open_tunnel(cfg, "db.internal".to_owned(), 5432).await;
        assert!(matches!(result, Err(SshError::Connect { .. })));
    }

    #[tokio::test]
    async fn known_hosts_policy_attempts_a_real_connect_rather_than_failing_fast() {
        // A bad known_hosts path is only consulted once a server key is
        // received, so it must not short-circuit before the network attempt.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
        let closed_port = probe.local_addr().expect("probe local_addr").port();
        drop(probe);

        let mut cfg = SshConfig::new("127.0.0.1", "alice", SshAuth::Password("hunter2".into()));
        cfg.port = closed_port;
        cfg.host_key = HostKeyPolicy::KnownHosts("/nonexistent/known_hosts".into());

        let result = open_tunnel(cfg, "db.internal".to_owned(), 5432).await;
        assert!(matches!(result, Err(SshError::Connect { .. })));
    }

    /// Minimal single-threaded blocking executor with no tokio runtime of
    /// its own, so driving [`open_tunnel`]'s returned future here proves it
    /// needs no ambient tokio context -- the future only awaits a control
    /// channel back from the crate's own background runtime.
    fn block_on_without_a_tokio_runtime<F: std::future::Future>(fut: F) -> F::Output {
        use std::pin::pin;
        use std::task::{Context, Poll, Wake, Waker};

        struct NoopWaker;
        impl Wake for NoopWaker {
            fn wake(self: Arc<Self>) {}
        }

        let waker = Waker::from(Arc::new(NoopWaker));
        let mut cx = Context::from_waker(&waker);
        let mut fut = pin!(fut);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    #[test]
    fn open_tunnel_future_resolves_without_an_ambient_tokio_runtime() {
        // A plain #[test] (not #[tokio::test]) polled by hand: this proves
        // open_tunnel's returned future carries no tokio reactor dependency
        // of its own and is drivable from any executor.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
        let closed_port = probe.local_addr().expect("probe local_addr").port();
        drop(probe);

        let mut cfg = SshConfig::new("127.0.0.1", "alice", SshAuth::Password("hunter2".into()));
        cfg.port = closed_port;

        let result =
            block_on_without_a_tokio_runtime(open_tunnel(cfg, "db.internal".to_owned(), 5432));
        match result {
            Err(SshError::Connect { host, port, .. }) => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, closed_port);
            }
            other => panic!("expected SshError::Connect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dropping_ssh_tunnel_signals_shutdown_without_touching_the_network() {
        let handle = runtime::handle();
        let shutdown_seen = Arc::new(AtomicBool::new(false));
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let seen = shutdown_seen.clone();
        let task = handle.spawn(async move {
            let _ = shutdown_rx.await;
            seen.store(true, Ordering::SeqCst);
        });

        let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let tunnel = SshTunnel {
            local_addr,
            shutdown: Some(shutdown_tx),
        };
        drop(tunnel);

        task.await.expect("shutdown-observing task should complete");
        assert!(shutdown_seen.load(Ordering::SeqCst));
    }

    #[test]
    fn repeated_tunnel_style_create_and_drop_cycles_do_not_start_new_runtimes() {
        let _ = runtime::handle();
        let count_before = runtime::init_count();

        for _ in 0..5 {
            let handle = runtime::handle();
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let task = handle.spawn(async move {
                let _ = shutdown_rx.await;
            });
            let tunnel = SshTunnel {
                local_addr: "127.0.0.1:0".parse().unwrap(),
                shutdown: Some(shutdown_tx),
            };
            drop(tunnel);
            handle
                .block_on(task)
                .expect("task should observe shutdown and complete");
        }

        assert_eq!(
            runtime::init_count(),
            count_before,
            "repeated tunnel-style lifecycles must not spawn additional runtimes"
        );
    }
}
