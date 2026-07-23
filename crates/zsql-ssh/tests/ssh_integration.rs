//! Live SSH tunnel integration test. Gated behind the `ssh-integration-tests`
//! feature and skipped from the default gate entirely: it needs a real sshd
//! reachable at `ZSQL_TEST_SSH_*` (see `scripts/ssh-dev.sh`), forwarding to a
//! real Postgres started with `scripts/pg-dev.sh`.
#![cfg(feature = "ssh-integration-tests")]

use std::env;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use zsql_ssh::{HostKeyPolicy, SshAuth, SshConfig, open_tunnel};

/// Postgres's `SSLRequest` startup packet: a 4-byte length prefix followed
/// by the fixed SSL-negotiation request code. Sending it and reading the
/// single-byte 'S'/'N' reply is the smallest real round trip a Postgres
/// server will answer, which is enough to prove the tunnel forwards bytes
/// end to end without needing full protocol/auth handling in this test.
const SSL_REQUEST: [u8; 8] = [0x00, 0x00, 0x00, 0x08, 0x04, 0xd2, 0x16, 0x2f];

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn required_env(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| panic!("{key} must be set to run ssh-integration-tests"))
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    futures_lite_block_on(fut)
}

/// Minimal single-threaded blocking executor so this test does not need a
/// tokio runtime of its own: [`open_tunnel`]'s returned future is meant to
/// be drivable from any executor, and polling it manually here proves that.
fn futures_lite_block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::pin::pin;
    use std::task::{Context, Poll, Wake, Waker};

    struct NoopWaker;
    impl Wake for NoopWaker {
        fn wake(self: std::sync::Arc<Self>) {}
    }

    let waker = Waker::from(std::sync::Arc::new(NoopWaker));
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
fn tunnel_forwards_bytes_to_the_real_postgres_server() {
    let ssh_host = env_or("ZSQL_TEST_SSH_HOST", "127.0.0.1");
    let ssh_port: u16 = env_or("ZSQL_TEST_SSH_PORT", "2222")
        .parse()
        .expect("ZSQL_TEST_SSH_PORT must be a valid port number");
    let ssh_user = required_env("ZSQL_TEST_SSH_USER");
    let ssh_password = required_env("ZSQL_TEST_SSH_PASSWORD");
    let remote_host = env_or("ZSQL_TEST_SSH_REMOTE_HOST", "host.docker.internal");
    let remote_port: u16 = env_or("ZSQL_TEST_SSH_REMOTE_PORT", "5432")
        .parse()
        .expect("ZSQL_TEST_SSH_REMOTE_PORT must be a valid port number");

    let mut cfg = SshConfig::new(ssh_host, ssh_user, SshAuth::Password(ssh_password));
    cfg.port = ssh_port;
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

    assert!(
        reply[0] == b'S' || reply[0] == b'N',
        "expected postgres's SSLRequest reply to be 'S' or 'N', got {:?}",
        reply[0]
    );
}
