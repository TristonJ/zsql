//! Live SSH tunnel integration tests: a dockerized sshd forwarding to real
//! Postgres/MySQL/MSSQL databases. Gated behind the `ssh-integration-tests`
//! feature and skipped from the default `cargo test --all` run entirely --
//! see `scripts/ssh-dev.sh` (sshd) plus `scripts/pg-dev.sh`,
//! `scripts/mysql-dev.sh`, and `scripts/mssql-dev.sh` (the databases it
//! forwards to) for how to bring the fixtures up.
#![cfg(all(test, feature = "ssh-integration-tests"))]

use std::time::Duration;

use gpui::{AppContext as _, TestAppContext};
use zsql_ssh::test_fixtures::{
    MSSQL_DB_DEFAULT, MSSQL_PASSWORD_DEFAULT, MSSQL_PORT_DEFAULT, MYSQL_DB_DEFAULT,
    MYSQL_PASSWORD_DEFAULT, MYSQL_PORT_DEFAULT, PG_DB_DEFAULT, PG_PASSWORD_DEFAULT,
    PG_PORT_DEFAULT, SSH_HOST_DEFAULT, SSH_PORT_DEFAULT, env_or, required_env,
};
use zsql_ssh::{HostKeyPolicy, SshAuth, SshConfig};

use crate::session::{LivenessState, Session, SessionState};

/// The SSH config every test tunnels through: the dev sshd provisioned by
/// `scripts/ssh-dev.sh`, authenticating with a password (key/agent auth are
/// already covered without a database behind them by `zsql-ssh`'s own
/// `ssh-integration-tests`; this suite's job is proving the tunnel carries a
/// real driver connection end to end, not re-covering auth methods).
fn ssh_config() -> SshConfig {
    let host = env_or("ZSQL_TEST_SSH_HOST", SSH_HOST_DEFAULT);
    let user = required_env("ZSQL_TEST_SSH_USER");
    let password = required_env("ZSQL_TEST_SSH_PASSWORD");
    let port: u16 = env_or("ZSQL_TEST_SSH_PORT", SSH_PORT_DEFAULT)
        .parse()
        .expect("ZSQL_TEST_SSH_PORT must be a valid port number");

    let mut cfg = SshConfig::new(host, user, SshAuth::Password(password));
    cfg.port = port;
    cfg.host_key = HostKeyPolicy::AcceptNew;
    cfg
}

/// A network database's URL for a tunneled connect: the host the *sshd
/// container* reaches the database at (`host.docker.internal`, published by
/// `scripts/ssh-dev.sh`'s `--add-host`), never `localhost` -- from inside
/// that container, `localhost` would mean the container itself.
fn postgres_url() -> String {
    let password = env_or("ZSQL_PG_PASSWORD", PG_PASSWORD_DEFAULT);
    let db = env_or("ZSQL_PG_DB", PG_DB_DEFAULT);
    let port = env_or("ZSQL_PG_PORT", PG_PORT_DEFAULT);
    format!("postgres://postgres:{password}@host.docker.internal:{port}/{db}")
}

/// A verify-full-requesting Postgres URL, trusting the self-signed
/// certificate `scripts/pg-dev.sh` generates via the CA file path it prints
/// as `ZSQL_TEST_PG_SSLROOTCERT`.
fn postgres_verify_full_url() -> String {
    let ca_path = required_env("ZSQL_TEST_PG_SSLROOTCERT");
    format!(
        "{}?sslmode=verify-full&sslrootcert={ca_path}",
        postgres_url()
    )
}

fn mysql_url() -> String {
    let password = env_or("ZSQL_MYSQL_PASSWORD", MYSQL_PASSWORD_DEFAULT);
    let db = env_or("ZSQL_MYSQL_DB", MYSQL_DB_DEFAULT);
    let port = env_or("ZSQL_MYSQL_PORT", MYSQL_PORT_DEFAULT);
    format!("mysql://root:{password}@host.docker.internal:{port}/{db}")
}

fn mssql_url() -> String {
    let password = env_or("ZSQL_MSSQL_PASSWORD", MSSQL_PASSWORD_DEFAULT);
    let db = env_or("ZSQL_MSSQL_DB", MSSQL_DB_DEFAULT);
    let port = env_or("ZSQL_MSSQL_PORT", MSSQL_PORT_DEFAULT);
    format!("mssql://sa:{password}@host.docker.internal:{port}/{db}?trustServerCertificate=true")
}

/// A verify-full-requesting MSSQL URL, trusting the CA `scripts/mssql-dev.sh`
/// generates via the CA file path it prints as `ZSQL_TEST_MSSQL_SSLROOTCERT`.
fn mssql_verify_full_url() -> String {
    let password = env_or("ZSQL_MSSQL_PASSWORD", MSSQL_PASSWORD_DEFAULT);
    let db = env_or("ZSQL_MSSQL_DB", MSSQL_DB_DEFAULT);
    let port = env_or("ZSQL_MSSQL_PORT", MSSQL_PORT_DEFAULT);
    let ca_path = required_env("ZSQL_TEST_MSSQL_SSLROOTCERT");
    format!("mssql://sa:{password}@host.docker.internal:{port}/{db}?sslrootcert={ca_path}")
}

async fn connect_and_run_select_one(cx: &mut TestAppContext, url: String) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io_with_kicker(cx.executor());

    let session = cx.new(|_cx| Session::new(&crate::config::Config::default()));
    session
        .update(cx, |session, cx| {
            session.connect_to_with_ssh(url, Some(ssh_config()), cx)
        })
        .await;

    session.read_with(cx, |session, _app| {
        assert!(
            matches!(session.state(), SessionState::Connected),
            "expected the tunneled connect to succeed, got {:?}",
            session.state()
        );
        assert!(
            session.has_tunnel_for_test(),
            "a successful tunneled connect must leave the session holding its tunnel"
        );
    });

    session
        .update(cx, |session, cx| session.run_query("SELECT 1", cx))
        .await;

    session.read_with(cx, |session, _app| {
        assert!(
            matches!(session.state(), SessionState::Results(_)),
            "expected the tunneled query to complete, got {:?}",
            session.state()
        );
    });
}

#[gpui::test]
async fn connect_and_query_postgres_through_the_tunnel(cx: &mut TestAppContext) {
    connect_and_run_select_one(cx, postgres_url()).await;
}

/// A tunneled Postgres connect that requests `sslmode=verify-full` must
/// still succeed: the tunnel translation caps it to certificate-chain
/// verification (no hostname check happens once the dial target is the
/// tunnel's loopback address), so this proves that cap actually connects
/// rather than merely being asserted against a string in a unit test.
#[gpui::test]
async fn connect_and_query_postgres_verify_full_through_the_tunnel(cx: &mut TestAppContext) {
    connect_and_run_select_one(cx, postgres_verify_full_url()).await;
}

#[gpui::test]
async fn connect_and_query_mysql_through_the_tunnel(cx: &mut TestAppContext) {
    connect_and_run_select_one(cx, mysql_url()).await;
}

#[gpui::test]
async fn connect_and_query_mssql_through_the_tunnel(cx: &mut TestAppContext) {
    connect_and_run_select_one(cx, mssql_url()).await;
}

/// A tunneled MSSQL connect requesting full verification (the real hostname
/// checked against a CA-signed certificate, not `trustServerCertificate`)
/// must succeed: the dial target is the tunnel's loopback address while
/// `tiberius::Config::host` stays the real remote hostname the certificate
/// was issued for.
#[gpui::test]
async fn connect_and_query_mssql_verify_full_through_the_tunnel(cx: &mut TestAppContext) {
    connect_and_run_select_one(cx, mssql_verify_full_url()).await;
}

/// With a tunnel active, the liveness probe traverses it rather than the
/// real remote host directly: killing only the tunnel (not the database)
/// must make the next probe report `Unreachable`, because the probe's
/// target -- the tunnel's local loopback listener -- is genuinely gone,
/// even though the database behind it is perfectly healthy.
#[gpui::test]
async fn killing_the_tunnel_but_not_the_database_makes_the_next_probe_unreachable(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io_with_kicker(cx.executor());

    let session = cx.new(|_cx| Session::new(&crate::config::Config::default()));
    session
        .update(cx, |session, cx| {
            session.connect_to_with_ssh(postgres_url(), Some(ssh_config()), cx)
        })
        .await;
    session.read_with(cx, |session, _app| {
        assert!(matches!(session.state(), SessionState::Connected));
    });

    // `connect_to_with_ssh` already started the probe loop on success; no
    // need to (re)start it here.
    let interval = session.read_with(cx, |session, _app| session.probe_interval_for_test());
    let became_healthy = trigger_probe_and_wait(cx, &session, interval, |liveness| {
        matches!(liveness, LivenessState::Healthy)
    });
    assert!(
        became_healthy,
        "a probe through a live tunnel must report healthy before it is killed"
    );

    let tunnel_addr = session
        .read_with(cx, |session, _app| session.tunnel_local_addr_for_test())
        .expect("session must be holding a tunnel before it is killed");
    session.update(cx, |session, _cx| {
        session.kill_tunnel_for_test();
    });

    // The tunnel's local listener must be gone -- proof that killing it
    // stopped serving *new* connections, distinct from the liveness probe
    // below proving an existing connection through it stops working too.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        std::net::TcpStream::connect_timeout(&tunnel_addr, Duration::from_secs(2)).is_err(),
        "the tunnel's local listener must refuse new connections once it is killed"
    );

    let became_unreachable = trigger_probe_and_wait(cx, &session, interval, |liveness| {
        matches!(liveness, LivenessState::Unreachable(_))
    });
    let final_liveness = session.read_with(cx, |session, _app| session.liveness().clone());
    assert!(
        became_unreachable,
        "a probe after the tunnel (but not the database) died must report Unreachable, got {final_liveness:?}"
    );
}

/// Advances the deterministic test clock by two full probe `interval`s (a
/// comfortable margin past exactly one, so the probe loop's next timer --
/// re-armed for `interval` past whenever it last fired, which may already
/// be some real time in the past relative to this call -- is guaranteed to
/// have elapsed rather than landing exactly on a boundary and being missed),
/// then polls `session`'s liveness purely with real sleeps, *without*
/// advancing the virtual clock any further. The probe's own real socket IO
/// runs on a background OS thread outside the `TestAppContext`'s dispatcher,
/// so it needs real wall-clock time to complete; advancing the virtual clock
/// again here would race the probe's own timeout timer forward and could
/// falsely time it out before the real round trip (through a real SSH
/// tunnel to a real database) ever finishes.
fn trigger_probe_and_wait(
    cx: &mut TestAppContext,
    session: &gpui::Entity<Session>,
    interval: Duration,
    matches_target: impl Fn(&LivenessState) -> bool,
) -> bool {
    cx.executor().advance_clock(interval * 2);
    cx.run_until_parked();

    for _ in 0..100 {
        let matched = session.read_with(cx, |session, _app| matches_target(session.liveness()));
        if matched {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
        cx.run_until_parked();
    }
    session.read_with(cx, |session, _app| matches_target(session.liveness()))
}
