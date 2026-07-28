//! The MSSQL [`Driver`] and its live [`Connection`] implementation, built on
//! `tiberius` (the standard async TDS client for Rust; `sqlx` has no MSSQL
//! backend) over an `async-net` `TcpStream` so it runs on the same
//! smol/async-io reactor as the rest of this workspace, with no
//! `tokio::runtime::Runtime` ever constructed.
//!
//! `tiberius`'s public API exposes no attention/cancel token, unlike
//! Postgres's `pg_cancel_backend` side channel, so cancellation here is
//! cooperative-only: dropping (or calling `cancel()` on) a [`QueryHandle`]
//! stops the background query task from reading further rows and drops its
//! dedicated connection, but does not signal the server to abort a
//! statement already in flight.
//!
//! Unlike `zsql-postgres`'s pooled design, this driver keeps no persistent
//! connection at all: every operation (`stream_query`, `introspect`,
//! `ping`, `count_rows`) opens its own short-lived `tiberius::Client`
//! against the stored [`tiberius::Config`] and drops it when done. This
//! keeps a slow in-flight query and a liveness probe on physically separate
//! connections for free, without needing a hand-rolled connection pool on
//! top of a client `tiberius` does not itself pool.

use std::net::SocketAddr;
use std::time::Duration;

use async_io::Timer;
use async_net::TcpStream;
use async_trait::async_trait;
use futures::future::Either;
use futures::{StreamExt as _, pin_mut};
use tiberius::{Client, QueryItem};
use zsql_core::{
    BatchSink, ConnConfig, Connection, CoreError, Driver, QueryEvent, QueryHandle, RelationSchema,
    RowBatch, RowCount, SchemaTree,
};

use crate::error::{map_connect_error, map_io_connect_error, map_query_error};
use crate::url::MssqlUrl;
use crate::values::{column_metas, decode_row};

/// Rows are grouped into batches of at most this many rows before a
/// [`QueryEvent::Batch`] is pushed into the sink. Mirrors `zsql-postgres`'s
/// batch bound; bounded so a large result set streams to the UI
/// incrementally instead of arriving as one huge allocation.
const DEFAULT_QUERY_BATCH_SIZE: usize = 500;

/// How long to wait for a TCP connect plus TDS login handshake before giving
/// up. Mirrors zsql-postgres's pool acquire timeout. Without it, a firewalled
/// or black-hole host hangs the connect future forever -- and since every
/// operation opens its own fresh client, any of them could hang forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The MSSQL [`Driver`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MssqlDriver;

#[async_trait]
impl Driver for MssqlDriver {
    fn id(&self) -> &'static str {
        "mssql"
    }

    fn display_name(&self) -> &'static str {
        "Microsoft SQL Server"
    }

    fn default_port(&self) -> Option<u16> {
        Some(1433)
    }

    fn url_schemes(&self) -> &'static [&'static str] {
        &["mssql", "sqlserver"]
    }

    fn parse_url(&self, url: &str) -> Result<ConnConfig, CoreError> {
        crate::url::parse(url)?;
        ConnConfig::from_url(url)
    }

    #[tracing::instrument(
        name = "mssql_connect",
        skip_all,
        fields(driver = self.id(), tls_mode = tracing::field::Empty)
    )]
    async fn connect(&self, cfg: &ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
        // Never log `cfg.url`: it may embed a password. Only non-secret
        // fields (the driver id and TLS mode, never the URL) are attached to
        // this span.
        let url = crate::url::parse(&cfg.url)?;
        let config = build_tiberius_config(&url);
        let dial_addr = cfg.tunnel_local_addr;
        tracing::Span::current().record("tls_mode", tls_mode_for(&url).label());

        let mut client = open_client(&config, dial_addr).await?;
        liveness_check(&mut client).await?;
        drop(client);
        tracing::info!("mssql connection established");
        Ok(Box::new(MssqlConnection { config, dial_addr }))
    }
}

/// The TLS verification level `url` requests, for tracing only: `Off` when
/// TLS is disabled or the server certificate is trusted unconditionally
/// (`trustServerCertificate=true`), otherwise `VerifyFull` -- `tiberius` has
/// no separate "verify chain but not hostname" mode, so there is no
/// `VerifyCa` case for this driver.
fn tls_mode_for(url: &MssqlUrl) -> zsql_core::TlsVerify {
    if !url.encrypt || url.trust_server_certificate {
        zsql_core::TlsVerify::Off
    } else {
        zsql_core::TlsVerify::VerifyFull
    }
}

/// Build a `tiberius::Config` from this crate's own parsed URL fields.
fn build_tiberius_config(url: &MssqlUrl) -> tiberius::Config {
    let mut config = tiberius::Config::new();
    config.host(&url.host);
    config.port(url.port);
    if let Some(database) = &url.database {
        config.database(database);
    }
    if let Some(user) = &url.user {
        let password = url.password.as_deref().unwrap_or_default();
        config.authentication(tiberius::AuthMethod::sql_server(user, password));
    }
    config.encryption(if url.encrypt {
        tiberius::EncryptionLevel::Required
    } else {
        tiberius::EncryptionLevel::Off
    });
    if url.trust_server_certificate {
        config.trust_cert();
    } else if let Some(ca_cert) = &url.ca_cert {
        config.trust_cert_ca(ca_cert);
    }
    config
}

/// Open a fresh TCP connection and complete the TDS login handshake, giving
/// up after [`CONNECT_TIMEOUT`] so an unreachable host cannot hang forever.
///
/// When `dial_addr` is set, the TCP connection is made to that address (a
/// local tunnel's loopback endpoint) instead of `config.get_addr()`, while
/// `config` itself -- and in particular `config.host`, which drives TLS
/// hostname verification -- is untouched. This is what lets a tunneled
/// connection both route through the tunnel and verify the server's
/// certificate against its real hostname.
async fn open_client(
    config: &tiberius::Config,
    dial_addr: Option<SocketAddr>,
) -> Result<Client<TcpStream>, CoreError> {
    let connect = async {
        let tcp = match dial_addr {
            Some(addr) => TcpStream::connect(addr).await,
            None => TcpStream::connect(config.get_addr()).await,
        }
        .map_err(|err| map_io_connect_error(&err))?;
        tcp.set_nodelay(true)
            .map_err(|err| map_io_connect_error(&err))?;
        Client::connect(config.clone(), tcp)
            .await
            .map_err(map_connect_error)
    };
    pin_mut!(connect);
    match futures::future::select(connect, Timer::after(CONNECT_TIMEOUT)).await {
        Either::Left((result, _timer)) => result,
        Either::Right(_) => Err(CoreError::connection(
            format!(
                "connecting to the server timed out after {} seconds",
                CONNECT_TIMEOUT.as_secs()
            ),
            true,
        )),
    }
}

/// Run a trivial `SELECT 1` to confirm a freshly opened client is actually
/// usable, not just accepted. Returns the decoded value.
async fn liveness_check(client: &mut Client<TcpStream>) -> Result<i32, CoreError> {
    let stream = client
        .simple_query("SELECT 1 AS one")
        .await
        .map_err(map_connect_error)?;
    let row = stream
        .into_row()
        .await
        .map_err(map_connect_error)?
        .ok_or_else(|| CoreError::connection("liveness query returned no row".to_owned(), true))?;
    row.try_get::<i32, _>("one")
        .map_err(map_connect_error)?
        .ok_or_else(|| CoreError::connection("liveness query returned NULL".to_owned(), true))
}

/// A live MSSQL "connection": really just the config needed to open one, per
/// the module-level doc comment's no-persistent-connection design.
pub struct MssqlConnection {
    config: tiberius::Config,
    /// When set, every operation dials this address (a local tunnel's
    /// loopback endpoint) instead of `config.get_addr()`. See
    /// [`open_client`].
    dial_addr: Option<SocketAddr>,
}

#[async_trait]
impl Connection for MssqlConnection {
    fn stream_query(&self, sql: String, sink: BatchSink) -> QueryHandle {
        let (cancel_tx, cancel_rx) = flume::unbounded();
        let config = self.config.clone();
        let dial_addr = self.dial_addr;
        async_global_executor::spawn(run_query(config, dial_addr, sql, sink, cancel_rx)).detach();
        QueryHandle::new(cancel_tx)
    }

    #[tracing::instrument(name = "mssql_introspect", skip_all)]
    async fn introspect(&self) -> Result<SchemaTree, CoreError> {
        let mut client = open_client(&self.config, self.dial_addr).await?;
        crate::introspect::introspect(&mut client).await
    }

    #[tracing::instrument(name = "mssql_ping", skip_all)]
    async fn ping(&self) -> Result<(), CoreError> {
        let mut client = open_client(&self.config, self.dial_addr).await?;
        liveness_check(&mut client).await.map(|_| ())
    }

    #[tracing::instrument(name = "mssql_count_rows", skip(self))]
    async fn count_rows(&self, schema: &str, relation: &str) -> Result<RowCount, CoreError> {
        let mut client = open_client(&self.config, self.dial_addr).await?;
        let partition_row_count = fetch_partition_row_count(&mut client, schema, relation).await?;
        if let Some(row_count) = row_count_from_partition_stats(partition_row_count) {
            tracing::debug!(?row_count, "using partition-stats row-count estimate");
            return Ok(row_count);
        }
        tracing::debug!("no reliable partition-stats row found; falling back to an exact count");
        let exact = exact_row_count(&mut client, schema, relation).await?;
        Ok(RowCount::Exact(exact))
    }

    #[tracing::instrument(name = "mssql_describe_relation", skip(self))]
    async fn describe_relation(
        &self,
        schema: &str,
        relation: &str,
    ) -> Result<RelationSchema, CoreError> {
        let mut client = open_client(&self.config, self.dial_addr).await?;
        crate::describe::describe_relation(&mut client, schema, relation).await
    }

    /// The click-to-preview query for `relation` in `schema`, capped at
    /// `limit` rows, in this dialect's syntax.
    fn preview_query(&self, schema: &str, relation: &str, limit: u64) -> String {
        format!(
            "SELECT TOP ({limit}) * FROM {}.{}",
            crate::quoting::bracket_quote_ident(schema),
            crate::quoting::bracket_quote_ident(relation)
        )
    }

    /// This driver opens a short-lived client per operation (see the module
    /// doc comment) and holds nothing persistent, so there is nothing to
    /// release; a future move to a persistent connection strategy would
    /// implement teardown here.
    #[tracing::instrument(name = "mssql_close", skip_all)]
    async fn close(&self) {}
}

/// Look up `sys.dm_db_partition_stats` for `schema.relation`'s base table
/// (heap or clustered index, `index_id IN (0, 1)`), bind-parameterized
/// (never string-interpolated). Returns `None` if no matching row exists
/// (e.g. `relation` is a view, or does not exist).
async fn fetch_partition_row_count(
    client: &mut Client<TcpStream>,
    schema: &str,
    relation: &str,
) -> Result<Option<u64>, CoreError> {
    let sql = "SELECT SUM(ps.row_count) AS row_count \
               FROM sys.dm_db_partition_stats ps \
               JOIN sys.tables t ON t.object_id = ps.object_id \
               JOIN sys.schemas s ON s.schema_id = t.schema_id \
               WHERE s.name = @P1 AND t.name = @P2 AND ps.index_id IN (0, 1)";
    let stream = client
        .query(sql, &[&schema, &relation])
        .await
        .map_err(map_query_error)?;
    let row = stream.into_row().await.map_err(map_query_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let row_count: Option<i64> = row.try_get("row_count").map_err(map_query_error)?;
    Ok(row_count.map(|n| u64::try_from(n).unwrap_or(0)))
}

/// Decide whether a [`fetch_partition_row_count`] result is trustworthy
/// enough to report directly as a [`RowCount::Estimated`], with no network
/// access: `sys.dm_db_partition_stats` carries no "never analyzed" sentinel
/// the way Postgres's `pg_class.reltuples` does, so any present row count is
/// reported as-is, and only a missing row (the relation has no tracked
/// partition stats at all) signals the caller to fall back to an exact
/// count.
fn row_count_from_partition_stats(row_count: Option<u64>) -> Option<RowCount> {
    row_count.map(RowCount::Estimated)
}

/// Build `SELECT COUNT_BIG(*) FROM [schema].[relation]`, bracket-quoting both
/// identifiers so an adversarial schema/relation name cannot break out of
/// the identifier position.
fn exact_count_sql(schema: &str, relation: &str) -> String {
    // COUNT_BIG (not COUNT) returns bigint, so the count fits the neutral
    // u64 contract and cannot overflow on a relation with more than i32::MAX
    // rows the way COUNT's int result would.
    format!(
        "SELECT COUNT_BIG(*) FROM {}.{}",
        crate::quoting::bracket_quote_ident(schema),
        crate::quoting::bracket_quote_ident(relation)
    )
}

/// Run an exact `SELECT COUNT_BIG(*)` against `schema.relation`.
async fn exact_row_count(
    client: &mut Client<TcpStream>,
    schema: &str,
    relation: &str,
) -> Result<u64, CoreError> {
    let sql = exact_count_sql(schema, relation);
    let stream = client.simple_query(sql).await.map_err(map_query_error)?;
    let row = stream
        .into_row()
        .await
        .map_err(map_query_error)?
        .ok_or_else(|| CoreError::query("COUNT_BIG(*) returned no row".to_owned()))?;
    let count: i64 = row
        .try_get(0)
        .map_err(map_query_error)?
        .ok_or_else(|| CoreError::query("COUNT_BIG(*) returned NULL".to_owned()))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Read `@@ROWCOUNT`, the rows affected by the single most recent statement
/// on `client`'s session, right after a batch that produced no result set
/// (DDL, or DML without `OUTPUT`) completes successfully. `simple_query`'s
/// stream surfaces only column metadata and rows -- tiberius exposes no way
/// to read a `DONE` token's own row count -- so this follow-up query is the
/// only way to recover a count at all, and it can only ever see the last
/// statement in a multi-statement batch: a batch such as
/// `INSERT ...; DELETE ...` reports only the `DELETE`'s row count, not the
/// sum of both. Best-effort: any failure here degrades to `None` rather than
/// failing an already-successful query.
async fn fetch_last_rowcount(client: &mut Client<TcpStream>) -> Option<u64> {
    // ROWCOUNT_BIG (not @@ROWCOUNT) returns bigint, so a batch that affects
    // more than i32::MAX rows still reports its true count.
    let stream = client
        .simple_query("SELECT ROWCOUNT_BIG() AS affected")
        .await
        .ok()?;
    let row = stream.into_row().await.ok()??;
    let count: i64 = row.try_get("affected").ok()??;
    u64::try_from(count).ok()
}

/// Stream a query's results into `sink`. `sql` may hold several statements;
/// each result set that arrives emits its own [`QueryEvent::Columns`]
/// followed by that set's [`QueryEvent::Batch`]es, and the whole stream ends
/// with exactly one [`QueryEvent::Done`] -- or, on any failure, a single
/// `Err` in place of `Done`.
///
/// A statement that produces no result set at all (DDL, or DML without
/// `OUTPUT`) reports its row count via [`fetch_last_rowcount`] in `Done`. A
/// statement that does produce a result set (`SELECT`, or DML with
/// `OUTPUT`) leaves `Done.affected` as `None`, letting the caller derive a
/// count from the rows already streamed.
///
/// For a multi-statement batch in which no statement produces a result set,
/// `Done.affected` reflects only the last statement (see
/// [`fetch_last_rowcount`]), not a sum across the batch.
#[tracing::instrument(name = "mssql_stream_query", skip_all)]
async fn run_query(
    config: tiberius::Config,
    dial_addr: Option<SocketAddr>,
    sql: String,
    sink: BatchSink,
    cancel_rx: flume::Receiver<()>,
) {
    tracing::debug!(sql = %sql, "streaming query");

    let mut client = match open_client(&config, dial_addr).await {
        Ok(client) => client,
        Err(err) => {
            let _ = sink.send_async(Err(err)).await;
            return;
        }
    };

    let mut batch = RowBatch::new();
    let mut any_columns_sent = false;

    {
        let mut stream = match client.simple_query(sql).await {
            Ok(stream) => stream,
            Err(err) => {
                let _ = sink.send_async(Err(map_query_error(err))).await;
                return;
            }
        };

        loop {
            let step = futures::future::select(cancel_rx.recv_async(), stream.next());
            match step.await {
                futures::future::Either::Left(_) => {
                    // Cancelled: either an explicit `cancel()` call or every
                    // `QueryHandle` clone (hence every `cancel_tx`) was
                    // dropped. `client` (and its TCP connection) is dropped
                    // when this function returns.
                    tracing::debug!("query cancelled");
                    return;
                }
                futures::future::Either::Right((None, _)) => break,
                futures::future::Either::Right((Some(Ok(QueryItem::Metadata(meta))), _)) => {
                    if !batch.is_empty() {
                        let full = std::mem::take(&mut batch);
                        if sink.send_async(Ok(QueryEvent::Batch(full))).await.is_err() {
                            return;
                        }
                    }
                    let columns = column_metas(meta.columns());
                    if sink
                        .send_async(Ok(QueryEvent::Columns(columns)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    any_columns_sent = true;
                }
                futures::future::Either::Right((Some(Ok(QueryItem::Row(row))), _)) => {
                    batch.push(decode_row(&row));
                    if batch.len() >= DEFAULT_QUERY_BATCH_SIZE {
                        let full = std::mem::take(&mut batch);
                        if sink.send_async(Ok(QueryEvent::Batch(full))).await.is_err() {
                            return;
                        }
                    }
                }
                futures::future::Either::Right((Some(Err(err)), _)) => {
                    let _ = sink.send_async(Err(map_query_error(err))).await;
                    return;
                }
            }
        }
    }

    if !batch.is_empty() && sink.send_async(Ok(QueryEvent::Batch(batch))).await.is_err() {
        return;
    }

    let affected = if any_columns_sent {
        None
    } else {
        fetch_last_rowcount(&mut client).await
    };

    let _ = sink.send_async(Ok(QueryEvent::Done { affected })).await;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use zsql_core::{ConnConfig, Connection, Driver};

    use super::{MssqlDriver, row_count_from_partition_stats};

    const UNREACHABLE_URL: &str = "mssql://sa:pass@zsql-test-nonexistent-host.invalid/db";

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    #[test]
    fn tls_mode_for_is_off_when_the_certificate_is_trusted_unconditionally() {
        let url =
            crate::url::parse("mssql://sa:pw@db.internal/db?trustServerCertificate=true").unwrap();
        assert_eq!(super::tls_mode_for(&url), zsql_core::TlsVerify::Off);
    }

    #[test]
    fn tls_mode_for_is_off_when_encryption_is_disabled() {
        let url = crate::url::parse("mssql://sa:pw@db.internal/db?encrypt=false").unwrap();
        assert_eq!(super::tls_mode_for(&url), zsql_core::TlsVerify::Off);
    }

    #[test]
    fn tls_mode_for_is_verify_full_when_encrypted_and_the_certificate_is_not_trusted_unconditionally()
     {
        let url = crate::url::parse("mssql://sa:pw@db.internal/db").unwrap();
        assert_eq!(super::tls_mode_for(&url), zsql_core::TlsVerify::VerifyFull);
    }

    /// `tiberius::Config` panics if both `trust_cert` and `trust_cert_ca`
    /// are ever called on the same config; `trust_server_certificate=true`
    /// must win over a `sslrootcert` given alongside it, not trigger that
    /// panic.
    #[test]
    fn build_tiberius_config_prefers_trusting_the_certificate_unconditionally_over_a_ca_path() {
        let url = crate::url::parse(
            "mssql://sa:pw@db.internal/db?trustServerCertificate=true&sslrootcert=/ca.crt",
        )
        .unwrap();
        super::build_tiberius_config(&url);
    }

    #[test]
    fn build_tiberius_config_accepts_a_ca_path_with_no_trust_server_certificate() {
        let url = crate::url::parse("mssql://sa:pw@db.internal/db?sslrootcert=/ca.crt").unwrap();
        super::build_tiberius_config(&url);
    }

    #[test]
    fn driver_ids_are_stable() {
        let driver = MssqlDriver;
        assert_eq!(driver.id(), "mssql");
        assert_eq!(driver.display_name(), "Microsoft SQL Server");
    }

    #[test]
    fn parse_url_rejects_empty_string() {
        let driver = MssqlDriver;
        assert!(driver.parse_url("   ").is_err());
    }

    #[test]
    fn parse_url_rejects_a_malformed_url() {
        let driver = MssqlDriver;
        assert!(driver.parse_url("not a valid url").is_err());
    }

    #[test]
    fn parse_url_accepts_a_well_formed_url() {
        let driver = MssqlDriver;
        assert!(driver.parse_url("mssql://sa:pass@localhost/db").is_ok());
        assert!(driver.parse_url("sqlserver://localhost").is_ok());
    }

    #[test]
    fn connect_maps_unreachable_host_to_core_connection_error() {
        let driver = MssqlDriver;
        let cfg = ConnConfig::from_url(UNREACHABLE_URL).unwrap();
        let result = block_on(driver.connect(&cfg));
        match result {
            Err(zsql_core::CoreError::Connection { message, .. }) => {
                assert!(!message.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Connection, got {other:?}"),
            Ok(_) => panic!("connecting to an unreachable host must fail"),
        }
    }

    #[test]
    fn connect_maps_malformed_url_to_core_url_error() {
        let driver = MssqlDriver;
        let cfg = ConnConfig {
            url: "not a valid url".to_owned(),
            tunnel_local_addr: None,
            batch_size: zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        };
        let result = block_on(driver.connect(&cfg));
        assert!(matches!(result, Err(zsql_core::CoreError::Url(_))));
    }

    /// `open_client` must dial `dial_addr` (a stand-in for a tunnel's local
    /// loopback endpoint) rather than `config.get_addr()` (an unrelated,
    /// unreachable host), while leaving `config.host` -- what TLS hostname
    /// verification is checked against -- completely untouched. Proven by
    /// binding a real local listener, passing its address as `dial_addr`,
    /// and observing the connection actually arrive there.
    #[test]
    fn open_client_dials_the_override_address_leaving_config_host_untouched() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind local test listener");
        let dial_addr = listener.local_addr().expect("listener local_addr");

        let config = super::build_tiberius_config(
            &crate::url::parse(UNREACHABLE_URL).expect("url should parse"),
        );
        assert!(
            config.get_addr().contains("zsql-test-nonexistent-host"),
            "sanity check: config.host must still name the unreachable host"
        );

        let accept = std::thread::spawn(move || listener.accept());
        // `open_client` will fail the TDS handshake against a plain
        // listener -- only the dial target is under test here.
        let _ = block_on(super::open_client(&config, Some(dial_addr)));

        let (_, peer) = accept
            .join()
            .expect("accept thread must not panic")
            .expect("the dial must reach the local listener within this test");
        assert_ne!(
            peer.port(),
            0,
            "the local listener must have observed a real inbound connection"
        );
    }

    #[test]
    fn row_count_from_partition_stats_reports_a_present_row_count_as_estimated() {
        assert_eq!(
            row_count_from_partition_stats(Some(500)),
            Some(zsql_core::RowCount::Estimated(500))
        );
        assert_eq!(
            row_count_from_partition_stats(Some(0)),
            Some(zsql_core::RowCount::Estimated(0))
        );
    }

    #[test]
    fn row_count_from_partition_stats_signals_fallback_when_absent() {
        assert_eq!(row_count_from_partition_stats(None), None);
    }

    #[test]
    fn exact_count_sql_quotes_both_identifiers() {
        assert_eq!(
            super::exact_count_sql("dbo", "orders"),
            "SELECT COUNT_BIG(*) FROM [dbo].[orders]"
        );
    }

    #[test]
    fn exact_count_sql_is_safe_against_an_injection_shaped_relation_name() {
        let sql = super::exact_count_sql("dbo", "orders]; DROP TABLE users; --");
        assert_eq!(
            sql,
            "SELECT COUNT_BIG(*) FROM [dbo].[orders]]; DROP TABLE users; --]"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn exact_count_sql_is_safe_against_an_injection_shaped_schema_name() {
        let sql = super::exact_count_sql("dbo]; DROP TABLE users; --", "orders");
        assert_eq!(
            sql,
            "SELECT COUNT_BIG(*) FROM [dbo]]; DROP TABLE users; --].[orders]"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    /// Builds an [`super::MssqlConnection`] with no network I/O: `preview_query`
    /// is pure string-building, so this exercises it without opening a
    /// `tiberius` client.
    fn connection_for_test() -> super::MssqlConnection {
        let config = super::build_tiberius_config(
            &crate::url::parse(UNREACHABLE_URL).expect("url should parse"),
        );
        super::MssqlConnection {
            config,
            dial_addr: None,
        }
    }

    #[test]
    fn preview_query_uses_top_instead_of_limit() {
        let conn = connection_for_test();
        assert_eq!(
            conn.preview_query("schema", "relation", 200),
            "SELECT TOP (200) * FROM [schema].[relation]"
        );
    }

    #[test]
    fn preview_query_never_emits_a_limit_clause() {
        let conn = connection_for_test();
        let sql = conn.preview_query("dbo", "orders", 50);
        assert!(!sql.contains("LIMIT"));
    }

    #[test]
    fn preview_query_is_safe_against_an_injection_shaped_relation_name() {
        let conn = connection_for_test();
        let sql = conn.preview_query("dbo", "orders]; DROP TABLE users; --", 200);
        assert_eq!(
            sql,
            "SELECT TOP (200) * FROM [dbo].[orders]]; DROP TABLE users; --]"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
        assert_eq!(sql.matches("SELECT").count(), 1, "exactly one statement");
    }

    #[test]
    fn preview_query_is_safe_against_an_injection_shaped_schema_name() {
        let conn = connection_for_test();
        let sql = conn.preview_query("dbo]; DROP TABLE users; --", "orders", 200);
        assert_eq!(
            sql,
            "SELECT TOP (200) * FROM [dbo]]; DROP TABLE users; --].[orders]"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
        assert_eq!(sql.matches("SELECT").count(), 1, "exactly one statement");
    }

    #[test]
    fn preview_query_is_safe_against_a_close_bracket_in_the_relation_name() {
        let conn = connection_for_test();
        let sql = conn.preview_query("dbo", "weird]name", 10);
        assert_eq!(sql, "SELECT TOP (10) * FROM [dbo].[weird]]name]");
    }

    #[test]
    fn stream_query_pushes_single_error_when_the_host_is_unreachable() {
        let (tx, rx) = flume::unbounded();
        let config = super::build_tiberius_config(
            &crate::url::parse(UNREACHABLE_URL).expect("url should parse"),
        );
        let handle = super::MssqlConnection {
            config,
            dial_addr: None,
        };
        let _query_handle = handle.stream_query("SELECT 1".to_owned(), tx);

        let evt = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("stream_query must push exactly one event, not hang");
        match evt {
            Err(zsql_core::CoreError::Connection { message, .. }) => assert!(!message.is_empty()),
            other => panic!("expected a single CoreError::Connection, got {other:?}"),
        }
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "no further events should follow the error"
        );
    }
}

#[cfg(test)]
#[cfg(feature = "driver-integration-tests")]
mod database_tests {
    use std::time::Duration;

    use zsql_core::{ConnConfig, Driver, RelationSchema};

    use super::MssqlDriver;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    /// Reads `ZSQL_TEST_MSSQL_URL` for database-tests. Deliberately its own
    /// env var, distinct from the Postgres suite's `ZSQL_TEST_POSTGRES_URL`,
    /// so this crate's database tests never collide with a concurrently
    /// running Postgres driver's own database tests.
    fn live_database_url() -> String {
        std::env::var("ZSQL_TEST_MSSQL_URL")
            .expect("ZSQL_TEST_MSSQL_URL must be set to run database tests")
    }

    /// Connects to `ZSQL_TEST_MSSQL_URL` via [`live_database_url`]
    fn live_connection() -> Box<dyn zsql_core::Connection> {
        let url = live_database_url();
        let driver = MssqlDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        block_on(driver.connect(&cfg)).expect("connect should succeed")
    }

    /// Receive one event with a generous timeout so a broken implementation
    /// fails the test instead of hanging it.
    fn recv(
        rx: &flume::Receiver<Result<zsql_core::QueryEvent, zsql_core::CoreError>>,
    ) -> Result<zsql_core::QueryEvent, zsql_core::CoreError> {
        rx.recv_timeout(Duration::from_secs(20))
            .expect("expected an event within the timeout")
    }

    #[test]
    fn connect_succeeds_against_a_live_database_when_configured() {
        live_connection();
    }

    #[test]
    fn ping_succeeds_against_a_live_database_when_configured() {
        let conn = live_connection();
        block_on(conn.ping()).expect("ping should succeed against a reachable database");
    }

    #[test]
    fn ping_completes_while_a_slow_query_is_streaming_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        // A fast marker SELECT ahead of the delay gives an observable
        // signal (its `Columns` event) that the batch has actually been
        // dispatched and is executing server-side, so the ping below
        // genuinely races a query that is mid-flight rather than one not
        // yet sent. `WAITFOR DELAY` alone streams nothing until it
        // completes, so it offers no such signal by itself.
        let handle = conn.stream_query(
            "SELECT 1 AS marker; WAITFOR DELAY '00:00:03'".to_owned(),
            tx,
        );
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected the marker SELECT's Columns first, got {other:?}"),
        }

        let ping_started = std::time::Instant::now();
        block_on(conn.ping()).expect("ping must succeed independently of the slow query");
        assert!(
            ping_started.elapsed() < Duration::from_secs(2),
            "ping took {:?}, which suggests it was blocked behind the slow query",
            ping_started.elapsed()
        );

        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                Ok(_) => {}
                Err(err) => panic!("slow query must not fail alongside a probe: {err:?}"),
            }
        }
        drop(handle);
    }

    #[test]
    fn stream_query_maps_a_representative_type_spread_when_configured() {
        let conn = live_connection();

        let sql = "SELECT \
            CAST(1 AS bit) AS b, \
            CAST(2 AS tinyint) AS ti, \
            CAST(3 AS smallint) AS si, \
            CAST(4 AS int) AS i, \
            CAST(5 AS bigint) AS bi, \
            CAST(1.5 AS real) AS r, \
            CAST(2.5 AS float) AS f, \
            CAST(123.456 AS decimal(9,3)) AS dec, \
            CAST('hi' AS varchar(10)) AS vc, \
            CAST(N'hi' AS nvarchar(10)) AS nvc, \
            CAST(NULL AS nvarchar(10)) AS nothing, \
            CAST('11111111-1111-1111-1111-111111111111' AS uniqueidentifier) AS u, \
            CAST(0x0102 AS varbinary(10)) AS bin, \
            CAST('2024-01-15' AS date) AS d, \
            CAST('13:45:30' AS time) AS tm, \
            CAST('2024-01-15 13:45:30' AS datetime2) AS dt2, \
            CAST('2024-01-15 13:45:30 +00:00' AS datetimeoffset) AS dto, \
            CAST(19.99 AS money) AS mny"
            .to_owned();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);

        let columns = match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => columns,
            other => panic!("expected Columns first, got {other:?}"),
        };
        assert_eq!(columns.len(), 18, "one column per selected expression");

        let mut rows = Vec::new();
        let affected = loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { affected }) => break affected,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        };
        assert_eq!(affected, None);

        assert_eq!(rows.len(), 1, "the query returns exactly one row");
        let cells = &rows[0].0;
        assert_eq!(cells[0], zsql_core::Value::Bool(true));
        assert_eq!(cells[1], zsql_core::Value::Int(2));
        assert_eq!(cells[2], zsql_core::Value::Int(3));
        assert_eq!(cells[3], zsql_core::Value::Int(4));
        assert_eq!(cells[4], zsql_core::Value::Int(5));
        assert_eq!(cells[5], zsql_core::Value::Float(1.5));
        assert_eq!(cells[6], zsql_core::Value::Float(2.5));
        assert_eq!(cells[7], zsql_core::Value::Numeric("123.456".to_owned()));
        assert_eq!(cells[8], zsql_core::Value::Text("hi".to_owned()));
        assert_eq!(cells[9], zsql_core::Value::Text("hi".to_owned()));
        assert_eq!(cells[10], zsql_core::Value::Null);
        assert_eq!(
            cells[11],
            zsql_core::Value::Uuid("11111111-1111-1111-1111-111111111111".to_owned())
        );
        assert_eq!(cells[12], zsql_core::Value::Bytes(vec![0x01, 0x02]));
        assert_eq!(
            cells[13],
            zsql_core::Value::Timestamp("2024-01-15".to_owned())
        );
        assert_eq!(
            cells[14],
            zsql_core::Value::Timestamp("13:45:30".to_owned())
        );
        assert_eq!(
            cells[15],
            zsql_core::Value::Timestamp("2024-01-15T13:45:30".to_owned())
        );
        assert_eq!(
            cells[16],
            zsql_core::Value::Timestamp("2024-01-15T13:45:30+00:00".to_owned())
        );
        // `money` (the last column) is deliberately left unmapped by
        // `values.rs`; exercised on its own below.
        assert!(matches!(cells[17], zsql_core::Value::Unknown(_)));
    }

    #[test]
    fn stream_query_maps_an_unmapped_money_type_to_unknown_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT CAST(19.99 AS money) AS mny".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(rows.len(), 1);
        assert!(
            matches!(rows[0].0[0], zsql_core::Value::Unknown(_)),
            "money must decode to Value::Unknown, got {:?}",
            rows[0].0[0]
        );
    }

    #[test]
    fn stream_query_keeps_statements_as_separate_result_sets_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1 AS n; SELECT 2 AS n".to_owned(), tx);

        let mut columns_events = 0usize;
        let mut rows_per_set: Vec<Vec<zsql_core::Value>> = Vec::new();
        let affected = loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Columns(columns)) => {
                    assert_eq!(columns.len(), 1);
                    assert_eq!(columns[0].name, "n");
                    columns_events += 1;
                    rows_per_set.push(Vec::new());
                }
                Ok(zsql_core::QueryEvent::Batch(batch)) => {
                    let current = rows_per_set
                        .last_mut()
                        .expect("a Columns event must precede any Batch");
                    current.extend(batch.rows.into_iter().map(|row| row.0[0].clone()));
                }
                Ok(zsql_core::QueryEvent::Done { affected }) => break affected,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        };

        assert_eq!(affected, None);
        assert_eq!(columns_events, 2);
        assert_eq!(
            rows_per_set,
            vec![
                vec![zsql_core::Value::Int(1)],
                vec![zsql_core::Value::Int(2)]
            ]
        );
    }

    #[test]
    fn stream_query_batches_large_result_sets_when_configured() {
        let conn = live_connection();

        let row_count = super::DEFAULT_QUERY_BATCH_SIZE * 2 + 7;
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            format!(
                "SELECT TOP ({row_count}) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS g \
                 FROM sys.all_objects a1 CROSS JOIN sys.all_objects a2"
            ),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }

        let mut total_rows = 0usize;
        let mut batch_count = 0usize;
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => {
                    assert!(batch.len() <= super::DEFAULT_QUERY_BATCH_SIZE);
                    assert!(!batch.is_empty(), "a sent Batch must never be empty");
                    total_rows += batch.len();
                    batch_count += 1;
                }
                Ok(zsql_core::QueryEvent::Done { affected }) => {
                    assert_eq!(affected, None);
                    break;
                }
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(total_rows, row_count);
        assert!(
            batch_count >= 3,
            "expected at least 3 batches, got {batch_count}"
        );
    }

    #[test]
    fn stream_query_reports_affected_rows_for_dml_when_configured() {
        let conn = live_connection();

        run_ddl(
            &*conn,
            "IF OBJECT_ID('dbo.zsql_test_dml_rowcount', 'U') IS NOT NULL \
             DROP TABLE dbo.zsql_test_dml_rowcount",
        );
        run_ddl(&*conn, "CREATE TABLE dbo.zsql_test_dml_rowcount (n int)");
        run_ddl(
            &*conn,
            "INSERT INTO dbo.zsql_test_dml_rowcount (n) VALUES (1), (2), (3)",
        );

        let (tx, rx) = flume::unbounded();
        let _handle =
            conn.stream_query("UPDATE dbo.zsql_test_dml_rowcount SET n = n".to_owned(), tx);
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Done { affected }) => assert_eq!(affected, Some(3)),
            other => panic!("expected a lone Done, got {other:?}"),
        }

        run_ddl(&*conn, "DROP TABLE dbo.zsql_test_dml_rowcount");
    }

    #[test]
    fn stream_query_emits_columns_for_a_zero_row_result_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1 AS one WHERE 1 = 0".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Done { affected }) => assert_eq!(affected, None),
            other => panic!("expected Done with no Batch in between, got {other:?}"),
        }
    }

    #[test]
    fn preview_query_executes_against_a_live_seeded_table_when_configured() {
        let conn = live_connection();

        // The `TOP` preview must be valid T-SQL end to end, not merely the
        // right string: a `LIMIT`-shaped query would come back as an Err here.
        let sql = conn.preview_query("dbo", "users", 5);
        assert!(
            sql.contains("TOP (5)"),
            "expected a TOP-limited preview: {sql}"
        );

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert!(!columns.is_empty()),
            other => panic!("a syntax error would arrive as Err; expected Columns, got {other:?}"),
        }
        let mut rows = 0usize;
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows += batch.len(),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert!(
            rows >= 1,
            "the seeded users table should return at least one row"
        );
        assert!(rows <= 5, "TOP (5) must cap the result at five rows");
    }

    #[test]
    fn dropping_the_query_handle_stops_further_rows_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query(
            "SELECT TOP (100000000) a1.name AS g \
             FROM sys.all_objects a1 CROSS JOIN sys.all_objects a2 \
             CROSS JOIN sys.all_objects a3"
                .to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        drop(handle);

        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(zsql_core::QueryEvent::Batch(_))) => {}
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) => {
                    panic!("a cancelled query must not reach Done")
                }
                Ok(Err(err)) => panic!("unexpected error after cancellation: {err:?}"),
                Ok(Ok(zsql_core::QueryEvent::Columns(_))) => {
                    panic!("Columns must only be sent once")
                }
                Err(flume::RecvTimeoutError::Disconnected) => break,
                Err(flume::RecvTimeoutError::Timeout) => {
                    panic!("cancellation did not stop the background task promptly")
                }
            }
        }
    }

    #[test]
    fn calling_cancel_stops_further_rows_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query(
            "SELECT TOP (100000000) a1.name AS g \
             FROM sys.all_objects a1 CROSS JOIN sys.all_objects a2 \
             CROSS JOIN sys.all_objects a3"
                .to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        handle.cancel();

        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(zsql_core::QueryEvent::Batch(_))) => {}
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) => {
                    panic!("a cancelled query must not reach Done")
                }
                Ok(Err(err)) => panic!("unexpected error after cancellation: {err:?}"),
                Ok(Ok(zsql_core::QueryEvent::Columns(_))) => {
                    panic!("Columns must only be sent once")
                }
                Err(flume::RecvTimeoutError::Disconnected) => break,
                Err(flume::RecvTimeoutError::Timeout) => {
                    panic!("cancellation did not stop the background task promptly")
                }
            }
        }
        drop(handle);
    }

    #[test]
    fn introspect_builds_schema_tree_matching_the_seeded_database_when_configured() {
        let conn = live_connection();

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        assert_eq!(tree.catalogs.len(), 1);
        let catalog = &tree.catalogs[0];

        assert!(
            catalog
                .schemas
                .iter()
                .all(|s| s.name != "sys" && s.name != "INFORMATION_SCHEMA" && s.name != "guest"),
            "system schemas must be excluded, got schemas: {:?}",
            catalog.schemas.iter().map(|s| &s.name).collect::<Vec<_>>()
        );

        let dbo = catalog
            .schemas
            .iter()
            .find(|s| s.name == "dbo")
            .expect("the seeded database has a dbo schema");

        let users = dbo
            .tables
            .iter()
            .find(|r| r.name == "users")
            .expect("the seeded users table is present");
        assert_eq!(users.kind, zsql_core::RelationKind::Table);

        let recent_orders = dbo
            .tables
            .iter()
            .find(|r| r.name == "recent_orders")
            .expect("the seeded recent_orders view is present");
        assert_eq!(recent_orders.kind, zsql_core::RelationKind::View);

        let email = users
            .columns
            .iter()
            .find(|c| c.name == "email")
            .expect("users.email column is present");
        assert!(!email.nullable, "users.email is declared NOT NULL");

        let display_name = users
            .columns
            .iter()
            .find(|c| c.name == "display_name")
            .expect("users.display_name column is present");
        assert!(display_name.nullable);
    }

    #[test]
    fn introspect_orders_schemas_relations_and_columns_deterministically_when_configured() {
        let conn = live_connection();

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let catalog = &tree.catalogs[0];

        let schema_names: Vec<&str> = catalog.schemas.iter().map(|s| s.name.as_str()).collect();
        let mut sorted_schema_names = schema_names.clone();
        sorted_schema_names.sort_unstable();
        assert_eq!(schema_names, sorted_schema_names);

        let dbo = catalog
            .schemas
            .iter()
            .find(|s| s.name == "dbo")
            .expect("the seeded database has a dbo schema");
        let relation_names: Vec<&str> = dbo.tables.iter().map(|r| r.name.as_str()).collect();
        let mut sorted_relation_names = relation_names.clone();
        sorted_relation_names.sort_unstable();
        assert_eq!(relation_names, sorted_relation_names);

        let users = dbo
            .tables
            .iter()
            .find(|r| r.name == "users")
            .expect("the seeded users table is present");
        let column_names: Vec<&str> = users.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            column_names,
            vec!["id", "email", "display_name", "is_active"],
            "columns must be in ordinal position, not alphabetical"
        );
    }

    #[test]
    fn introspect_includes_a_second_schema_including_an_empty_one_when_configured() {
        let conn = live_connection();

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let catalog = &tree.catalogs[0];

        let analytics = catalog
            .schemas
            .iter()
            .find(|s| s.name == "analytics")
            .expect("the seeded analytics schema is present");
        let page_views = analytics
            .tables
            .iter()
            .find(|r| r.name == "page_views")
            .expect("the seeded analytics.page_views table is present");
        assert_eq!(page_views.kind, zsql_core::RelationKind::Table);

        let empty_ns = catalog
            .schemas
            .iter()
            .find(|s| s.name == "empty_ns")
            .expect("the seeded empty_ns schema is present even though it holds nothing");
        assert!(empty_ns.tables.is_empty());
    }

    #[test]
    fn count_rows_returns_an_estimated_count_when_configured() {
        let conn = live_connection();

        let table = "zsql_test_count_rows_estimated";
        run_ddl(
            &*conn,
            &format!("IF OBJECT_ID('dbo.{table}', 'U') IS NOT NULL DROP TABLE dbo.{table}"),
        );
        run_ddl(&*conn, &format!("CREATE TABLE dbo.{table} (n int)"));
        run_ddl(
            &*conn,
            &format!(
                "INSERT INTO dbo.{table} (n) \
                 SELECT TOP (500) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) \
                 FROM sys.all_objects a1 CROSS JOIN sys.all_objects a2"
            ),
        );

        let row_count = block_on(conn.count_rows("dbo", table)).expect("count_rows must run");
        tracing::info!(
            ?row_count,
            "count_rows_returns_an_estimated_count_when_configured executed against the live database"
        );
        match row_count {
            zsql_core::RowCount::Estimated(n) => assert_eq!(n, 500),
            zsql_core::RowCount::Exact(n) => panic!("expected Estimated, got Exact({n})"),
        }

        run_ddl(&*conn, &format!("DROP TABLE dbo.{table}"));
    }

    #[test]
    fn count_rows_falls_back_to_exact_for_a_view_when_configured() {
        let conn = live_connection();

        // A view has no `sys.dm_db_partition_stats` row of its own, so
        // `count_rows` must fall back to an exact `COUNT(*)`.
        let row_count =
            block_on(conn.count_rows("dbo", "recent_orders")).expect("count_rows must run");
        assert!(
            matches!(row_count, zsql_core::RowCount::Exact(_)),
            "expected Exact for a view, got {row_count:?}"
        );
    }

    /// Seed a parent/child table pair (unique-suffixed so parallel test runs
    /// cannot collide) exercising a primary key, a foreign key, a
    /// single-column unique constraint, a check constraint, and a secondary
    /// non-unique index, and describe the child table.
    fn describe_seeded_child(conn: &dyn zsql_core::Connection, suffix: &str) -> RelationSchema {
        let parent = format!("zsql_test_describe_parent_{suffix}");
        let child = format!("zsql_test_describe_child_{suffix}");

        run_ddl(
            conn,
            &format!("IF OBJECT_ID('dbo.{child}', 'U') IS NOT NULL DROP TABLE dbo.{child}"),
        );
        run_ddl(
            conn,
            &format!("IF OBJECT_ID('dbo.{parent}', 'U') IS NOT NULL DROP TABLE dbo.{parent}"),
        );
        run_ddl(
            conn,
            &format!(
                "CREATE TABLE dbo.{parent} ( \
                     id   int IDENTITY NOT NULL, \
                     code nvarchar(50) NOT NULL, \
                     CONSTRAINT pk_{parent} PRIMARY KEY (id), \
                     CONSTRAINT uq_{parent}_code UNIQUE (code) \
                 )"
            ),
        );
        run_ddl(
            conn,
            &format!(
                "CREATE TABLE dbo.{child} ( \
                     id        int IDENTITY NOT NULL, \
                     parent_id int NOT NULL, \
                     qty       int NOT NULL DEFAULT 1, \
                     status    nvarchar(20) NOT NULL DEFAULT 'open', \
                     CONSTRAINT pk_{child} PRIMARY KEY (id), \
                     CONSTRAINT fk_{child}_parent FOREIGN KEY (parent_id) \
                         REFERENCES dbo.{parent} (id), \
                     CONSTRAINT ck_{child}_qty CHECK (qty > 0) \
                 )"
            ),
        );
        run_ddl(
            conn,
            &format!("CREATE INDEX ix_{child}_status ON dbo.{child} (status)"),
        );

        let schema = block_on(conn.describe_relation("dbo", &child))
            .expect("describe_relation should succeed for a seeded table");

        run_ddl(conn, &format!("DROP TABLE dbo.{child}"));
        run_ddl(conn, &format!("DROP TABLE dbo.{parent}"));
        schema
    }

    #[test]
    fn describe_relation_reports_column_key_and_default_detail_when_configured() {
        let conn = live_connection();
        let schema = describe_seeded_child(&*conn, "cols");

        let id = schema
            .columns
            .iter()
            .find(|c| c.name == "id")
            .expect("id column present");
        assert!(id.is_primary_key);
        assert!(!id.nullable);
        assert!(id.foreign_key.is_none());

        let parent_id = schema
            .columns
            .iter()
            .find(|c| c.name == "parent_id")
            .expect("parent_id column present");
        assert!(!parent_id.is_primary_key);
        assert!(!parent_id.nullable);
        let fk = parent_id
            .foreign_key
            .as_ref()
            .expect("parent_id carries a foreign key");
        assert_eq!(fk.schema, "dbo");
        assert_eq!(fk.table, "zsql_test_describe_parent_cols");
        assert_eq!(fk.columns, vec!["id".to_owned()]);

        let qty = schema
            .columns
            .iter()
            .find(|c| c.name == "qty")
            .expect("qty column present");
        assert_eq!(qty.default.as_deref(), Some("((1))"));

        let status = schema
            .columns
            .iter()
            .find(|c| c.name == "status")
            .expect("status column present");
        assert_eq!(status.default.as_deref(), Some("('open')"));
    }

    #[test]
    fn describe_relation_reports_the_primary_and_secondary_indexes_when_configured() {
        let conn = live_connection();
        let schema = describe_seeded_child(&*conn, "idx");

        let pk_index = schema
            .indexes
            .iter()
            .find(|i| i.name == "pk_zsql_test_describe_child_idx")
            .expect("the primary key's backing index is listed");
        assert!(pk_index.unique);
        assert_eq!(pk_index.definition, "(id)");

        let status_index = schema
            .indexes
            .iter()
            .find(|i| i.name == "ix_zsql_test_describe_child_idx_status")
            .expect("the secondary index is listed");
        assert!(!status_index.unique);
        assert_eq!(status_index.definition, "(status)");
    }

    #[test]
    fn describe_relation_reports_the_primary_foreign_and_check_constraints_when_configured() {
        let conn = live_connection();
        let schema = describe_seeded_child(&*conn, "con");

        let pk_constraint = schema
            .constraints
            .iter()
            .find(|c| c.name == "pk_zsql_test_describe_child_con")
            .expect("primary key constraint present");
        assert_eq!(pk_constraint.kind, zsql_core::ConstraintKind::PrimaryKey);
        assert_eq!(pk_constraint.definition, "PRIMARY KEY (id)");

        let fk_constraint = schema
            .constraints
            .iter()
            .find(|c| c.name == "fk_zsql_test_describe_child_con_parent")
            .expect("foreign key constraint present");
        assert_eq!(fk_constraint.kind, zsql_core::ConstraintKind::ForeignKey);
        assert_eq!(
            fk_constraint.definition,
            "FOREIGN KEY (parent_id) REFERENCES dbo.zsql_test_describe_parent_con(id)"
        );

        let check_constraint = schema
            .constraints
            .iter()
            .find(|c| c.name == "ck_zsql_test_describe_child_con_qty")
            .expect("check constraint present");
        assert_eq!(check_constraint.kind, zsql_core::ConstraintKind::Check);
        assert!(
            check_constraint.definition.contains("qty"),
            "check definition should mention qty, got {}",
            check_constraint.definition
        );
    }

    #[test]
    fn describe_relation_reports_a_single_column_unique_constraint_when_configured() {
        let conn = live_connection();

        run_ddl(
            &*conn,
            "IF OBJECT_ID('dbo.zsql_test_describe_unique', 'U') IS NOT NULL \
             DROP TABLE dbo.zsql_test_describe_unique",
        );
        run_ddl(
            &*conn,
            "CREATE TABLE dbo.zsql_test_describe_unique ( \
                 id   int IDENTITY NOT NULL, \
                 code nvarchar(50) NOT NULL, \
                 CONSTRAINT pk_zsql_test_describe_unique PRIMARY KEY (id), \
                 CONSTRAINT uq_zsql_test_describe_unique_code UNIQUE (code) \
             )",
        );

        let schema = block_on(conn.describe_relation("dbo", "zsql_test_describe_unique"))
            .expect("describe_relation should succeed for a seeded table");

        let code = schema
            .columns
            .iter()
            .find(|c| c.name == "code")
            .expect("code column present");
        assert!(code.is_unique);
        assert!(!code.is_primary_key);

        let unique_constraint = schema
            .constraints
            .iter()
            .find(|c| c.name == "uq_zsql_test_describe_unique_code")
            .expect("unique constraint present");
        assert_eq!(unique_constraint.kind, zsql_core::ConstraintKind::Unique);
        assert_eq!(unique_constraint.definition, "UNIQUE (code)");

        run_ddl(&*conn, "DROP TABLE dbo.zsql_test_describe_unique");
    }

    #[test]
    fn describe_relation_returns_err_for_a_nonexistent_relation_when_configured() {
        let conn = live_connection();

        let result = block_on(conn.describe_relation("dbo", "zsql_test_describe_missing"));
        assert!(
            matches!(result, Err(zsql_core::CoreError::Introspection { .. })),
            "expected a CoreError::Introspection, got {result:?}"
        );
    }

    /// Run `sql` (typically DDL/DML setup) to completion against `conn`,
    /// panicking on any error and discarding whatever events it produces.
    fn run_ddl(conn: &dyn zsql_core::Connection, sql: &str) {
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql.to_owned(), tx);
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) => break,
                Ok(Ok(_)) => {}
                Ok(Err(err)) => panic!("ddl setup failed: {err:?}"),
                Err(err) => panic!("ddl setup did not complete: {err:?}"),
            }
        }
    }
}
