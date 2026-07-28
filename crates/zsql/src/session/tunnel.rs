//! The open-tunnel-then-connect flow a [`super::Session`] runs before
//! handing a URL to a driver, and the trait that lets tests substitute a
//! lightweight fake for a real SSH tunnel.

use std::net::SocketAddr;

use zsql_core::{Connection, ConnectionUrl, CoreError};

use crate::drivers;

/// Anything with the same drop-driven lifecycle contract as an open SSH
/// tunnel: dropping it must tear the tunnel down, and it exposes the local
/// loopback address a driver should dial instead of the real remote host.
/// Kept as a trait object (rather than naming [`zsql_ssh::SshTunnel`]
/// directly in [`super::Session`]) so tests can substitute a lightweight
/// fake without opening a real SSH session.
pub(crate) trait TunnelHandle: Send + Sync {
    /// The loopback address a driver should dial to reach this tunnel's
    /// remote endpoint.
    fn local_addr(&self) -> SocketAddr;
}

impl TunnelHandle for zsql_ssh::SshTunnel {
    fn local_addr(&self) -> SocketAddr {
        zsql_ssh::SshTunnel::local_addr(self)
    }
}

/// A successful connect's outcome: the live connection, and the tunnel it
/// was opened through, if any.
pub(crate) type TunneledConnectOutcome = (Box<dyn Connection>, Option<Box<dyn TunnelHandle>>);

/// Opens `ssh`'s tunnel (if given) before connecting to `url`, so a bad SSH
/// config surfaces as a connect failure before the driver is ever touched.
/// With no `ssh` config, this is exactly [`drivers::connect`]. `batch_size`
/// (typically [`Config::query`](crate::config::Config::query)'s
/// `batch_size`) is threaded onto the resulting connection.
///
/// Shared with `ui::connections`'s own Test and unsaved-Connect paths, which
/// need the identical tunnel-before-connect ordering outside of a
/// [`super::Session`].
#[tracing::instrument(name = "session_open_tunnel_before_connect", skip_all)]
pub(crate) async fn open_tunnel_and_connect(
    url: String,
    ssh: Option<zsql_ssh::SshConfig>,
    batch_size: usize,
) -> Result<TunneledConnectOutcome, CoreError> {
    let Some(ssh_cfg) = ssh else {
        let conn = drivers::connect(url, batch_size).await?;
        return Ok((conn, None));
    };

    let (remote_host, remote_port) = remote_target(&url)?;
    tracing::info!("opening ssh tunnel before connect");
    let tunnel = zsql_ssh::open_tunnel(ssh_cfg, remote_host, remote_port)
        .await
        .map_err(|err| CoreError::connection(err.to_string(), false))?;

    let (conn, tunnel) = connect_through_open_tunnel(url, Box::new(tunnel), batch_size).await?;
    Ok((conn, Some(tunnel)))
}

/// Connects to `url` through `tunnel`'s already-open local address, with the
/// resulting connection's row-batching set to `batch_size`. On failure,
/// `tunnel` is dropped as part of this same attempt (it is not returned in
/// the `Err` case), so a driver connect failure after a successfully opened
/// tunnel never leaves that tunnel outliving the failed attempt.
pub(super) async fn connect_through_open_tunnel(
    url: String,
    tunnel: Box<dyn TunnelHandle>,
    batch_size: usize,
) -> Result<(Box<dyn Connection>, Box<dyn TunnelHandle>), CoreError> {
    let tunnel_addr = tunnel.local_addr();
    let conn = drivers::connect_tunneled(url, tunnel_addr, batch_size).await?;
    Ok((conn, tunnel))
}

/// The real remote host and port an SSH tunnel for `url` should forward to:
/// `url`'s own host and (explicit or driver-default) port.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` cannot be parsed or has no host (a
/// sqlite URL never reaches this: SSH tunneling only applies to network
/// connections).
pub(super) fn remote_target(url: &str) -> Result<(String, u16), CoreError> {
    let parsed = ConnectionUrl::parse(url)?;
    let host = parsed.host().ok_or_else(|| {
        CoreError::Url("an SSH tunnel requires a network URL with a host".to_owned())
    })?;
    let port = match parsed.port() {
        Some(port) => port,
        None => drivers::detect_driver_default_port(url)?.ok_or_else(|| {
            CoreError::Url("an SSH tunnel requires an explicit port for this URL".to_owned())
        })?,
    };
    Ok((host, port))
}
