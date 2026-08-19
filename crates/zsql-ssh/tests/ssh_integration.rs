//! Live SSH tunnel integration tests. Gated behind the `ssh-integration-tests`
//! feature and skipped from the default gate entirely: they need a real sshd
//! reachable at `ZSQL_TEST_SSH_*` (see `scripts/ssh-dev.sh`), forwarding to a
//! real Postgres started with `scripts/pg-dev.sh`.
#![cfg(feature = "ssh-integration-tests")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use zsql_ssh::test_fixtures::{
    PG_PORT_DEFAULT, SSH_HOST_DEFAULT, SSH_PORT_DEFAULT, env_or, required_env,
};
use zsql_ssh::{HostKeyPolicy, SshAuth, SshConfig, open_tunnel};

mod support;
use support::ThrowawayAgent;

/// Postgres's `SSLRequest` startup packet: a 4-byte length prefix followed
/// by the fixed SSL-negotiation request code. Sending it and reading the
/// single-byte 'S'/'N' reply is the smallest real round trip a Postgres
/// server will answer, which is enough to prove the tunnel forwards bytes
/// end to end without needing full protocol/auth handling in this test.
const SSL_REQUEST: [u8; 8] = [0x00, 0x00, 0x00, 0x08, 0x04, 0xd2, 0x16, 0x2f];

/// The key fixture `scripts/ssh-dev.sh` provisions into the dev sshd's
/// `authorized_keys`, so the key-auth case needs no extra setup beyond it.
fn fixture_key_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/id_ed25519")
}

fn ssh_port() -> u16 {
    env_or("ZSQL_TEST_SSH_PORT", SSH_PORT_DEFAULT)
        .parse()
        .expect("ZSQL_TEST_SSH_PORT must be a valid port number")
}

fn remote_target() -> (String, u16) {
    let host = env_or("ZSQL_TEST_SSH_REMOTE_HOST", "host.docker.internal");
    let port = env_or("ZSQL_TEST_SSH_REMOTE_PORT", PG_PORT_DEFAULT)
        .parse()
        .expect("ZSQL_TEST_SSH_REMOTE_PORT must be a valid port number");
    (host, port)
}

/// Minimal single-threaded blocking executor so these tests do not need a
/// tokio runtime of their own: [`open_tunnel`]'s returned future is meant to
/// be drivable from any executor, and polling it manually here proves that.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Opens `cfg`'s tunnel and proves it forwards bytes by round-tripping a
/// Postgres `SSLRequest` through it.
fn assert_tunnel_forwards_to_postgres(cfg: SshConfig) {
    let (remote_host, remote_port) = remote_target();

    let tunnel = block_on(open_tunnel(cfg, remote_host, remote_port))
        .expect("tunnel should open against the dev sshd + postgres");

    let mut socket = TcpStream::connect(tunnel.local_addr())
        .expect("connecting to the tunnel's local loopback address should succeed");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set_read_timeout should succeed");

    socket
        .write_all(&SSL_REQUEST)
        .expect("writing the SSLRequest packet through the tunnel should succeed");

    let mut reply = [0u8; 1];
    socket
        .read_exact(&mut reply)
        .expect("postgres should reply to the SSLRequest through the tunnel");

    assert!(
        reply[0] == b'S' || reply[0] == b'N',
        "expected postgres's SSLRequest reply to be 'S' or 'N', got {:?}",
        reply[0]
    );
}

#[test]
fn tunnel_forwards_bytes_to_the_real_postgres_server_with_password_auth() {
    let ssh_host = env_or("ZSQL_TEST_SSH_HOST", SSH_HOST_DEFAULT);
    let ssh_user = required_env("ZSQL_TEST_SSH_USER");
    let ssh_password = required_env("ZSQL_TEST_SSH_PASSWORD");

    let mut cfg = SshConfig::new(ssh_host, ssh_user, SshAuth::Password(ssh_password));
    cfg.port = ssh_port();
    cfg.host_key = HostKeyPolicy::AcceptNew;

    assert_tunnel_forwards_to_postgres(cfg);
}

/// Dropping the tunnel must not just stop *new* connections from being
/// accepted -- it must also sever an already-open forwarded connection, so
/// a client mid-conversation with the real server sees its socket close
/// rather than being left to talk to a database through a tunnel that no
/// longer exists.
#[test]
fn dropping_the_tunnel_closes_an_already_open_forwarded_connection() {
    let ssh_host = env_or("ZSQL_TEST_SSH_HOST", SSH_HOST_DEFAULT);
    let ssh_user = required_env("ZSQL_TEST_SSH_USER");
    let ssh_password = required_env("ZSQL_TEST_SSH_PASSWORD");
    let (remote_host, remote_port) = remote_target();

    let mut cfg = SshConfig::new(ssh_host, ssh_user, SshAuth::Password(ssh_password));
    cfg.port = ssh_port();
    cfg.host_key = HostKeyPolicy::AcceptNew;

    let tunnel = block_on(open_tunnel(cfg, remote_host, remote_port))
        .expect("tunnel should open against the dev sshd + postgres");

    let mut socket = TcpStream::connect(tunnel.local_addr())
        .expect("connecting to the tunnel's local loopback address should succeed");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set_read_timeout should succeed");

    socket
        .write_all(&SSL_REQUEST)
        .expect("writing the SSLRequest packet through the tunnel should succeed");
    let mut reply = [0u8; 1];
    socket
        .read_exact(&mut reply)
        .expect("postgres should reply to the SSLRequest through the tunnel");

    drop(tunnel);

    // The already-open socket must observe the connection closing: either
    // a subsequent read returns immediately (EOF/error) instead of hanging,
    // or a write eventually fails. Polled with a bounded number of short
    // sleeps rather than asserted instantaneously, since the tunnel's
    // teardown runs asynchronously on its own background runtime.
    let mut observed_closed = false;
    for _ in 0..100 {
        let mut probe = [0u8; 1];
        let Ok(()) = socket.write_all(&SSL_REQUEST) else {
            observed_closed = true;
            break;
        };
        socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set_read_timeout should succeed");
        match socket.read(&mut probe) {
            Ok(0) => {
                observed_closed = true;
                break;
            }
            Ok(_) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::NotConnected
                ) =>
            {
                observed_closed = true;
                break;
            }
            Err(_timed_out) => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        observed_closed,
        "an already-open forwarded connection must close once its tunnel is dropped"
    );
}

#[test]
fn tunnel_forwards_bytes_to_the_real_postgres_server_with_key_auth() {
    let ssh_host = env_or("ZSQL_TEST_SSH_HOST", SSH_HOST_DEFAULT);
    let ssh_user = required_env("ZSQL_TEST_SSH_USER");

    let mut cfg = SshConfig::new(
        ssh_host,
        ssh_user,
        SshAuth::Key {
            path: fixture_key_path(),
            passphrase: None,
        },
    );
    cfg.port = ssh_port();
    cfg.host_key = HostKeyPolicy::AcceptNew;

    assert_tunnel_forwards_to_postgres(cfg);
}

/// Spawns its own throwaway `ssh-agent` loaded with the fixture key (torn
/// down on drop, including on panic), so agent auth gets the same
/// unattended coverage as password and key auth above rather than needing
/// an operator-provisioned agent.
#[test]
fn tunnel_forwards_bytes_to_the_real_postgres_server_with_agent_auth() {
    let _agent = ThrowawayAgent::spawn(&fixture_key_path());

    let ssh_host = env_or("ZSQL_TEST_SSH_HOST", SSH_HOST_DEFAULT);
    let ssh_user = required_env("ZSQL_TEST_SSH_USER");

    let mut cfg = SshConfig::new(ssh_host, ssh_user, SshAuth::Agent);
    cfg.port = ssh_port();
    cfg.host_key = HostKeyPolicy::AcceptNew;

    assert_tunnel_forwards_to_postgres(cfg);
}
