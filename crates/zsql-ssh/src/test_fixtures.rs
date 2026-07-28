//! One source of truth for the env var names and default values the
//! workspace's live SSH/database integration suites agree on with the dev
//! fixture scripts (`scripts/ssh-dev.sh`, `scripts/pg-dev.sh`,
//! `scripts/mysql-dev.sh`, `scripts/mssql-dev.sh`). Consumed both by this
//! crate's own `tests/ssh_integration.rs` and by `zsql`'s
//! `ssh_live_tests`, so a fixture default only ever needs to change here.

use std::env;

/// Reads `key` from the environment, falling back to `default` if unset.
#[must_use]
pub fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

/// Reads `key` from the environment, panicking with a directive message if
/// unset. Used for values with no safe default -- credentials, and the
/// dev-sshd user a fixture script assigns at `up` time -- so a missing
/// value fails loudly instead of silently targeting the wrong server.
///
/// # Panics
///
/// Panics if `key` is not set in the environment.
#[must_use]
pub fn required_env(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| panic!("{key} must be set to run ssh-integration-tests"))
}

/// `scripts/ssh-dev.sh`'s default `ZSQL_TEST_SSH_HOST`.
pub const SSH_HOST_DEFAULT: &str = "127.0.0.1";
/// `scripts/ssh-dev.sh`'s default `ZSQL_TEST_SSH_PORT` (mirrors its own
/// `ZSQL_SSH_PORT` default).
pub const SSH_PORT_DEFAULT: &str = "2222";

/// `scripts/pg-dev.sh`'s default `ZSQL_PG_PORT`.
pub const PG_PORT_DEFAULT: &str = "5432";
/// `scripts/pg-dev.sh`'s default `ZSQL_PG_PASSWORD`.
pub const PG_PASSWORD_DEFAULT: &str = "zsql";
/// `scripts/pg-dev.sh`'s default `ZSQL_PG_DB`.
pub const PG_DB_DEFAULT: &str = "zsql";

/// `scripts/mysql-dev.sh`'s default `ZSQL_MYSQL_PORT`.
pub const MYSQL_PORT_DEFAULT: &str = "3306";
/// `scripts/mysql-dev.sh`'s default `ZSQL_MYSQL_PASSWORD`.
pub const MYSQL_PASSWORD_DEFAULT: &str = "zsql";
/// `scripts/mysql-dev.sh`'s default `ZSQL_MYSQL_DB`.
pub const MYSQL_DB_DEFAULT: &str = "zsql";

/// `scripts/mssql-dev.sh`'s default `ZSQL_MSSQL_PORT`.
pub const MSSQL_PORT_DEFAULT: &str = "1433";
/// `scripts/mssql-dev.sh`'s default `ZSQL_MSSQL_PASSWORD`.
pub const MSSQL_PASSWORD_DEFAULT: &str = "zSql!DevPassw0rd";
/// `scripts/mssql-dev.sh`'s default `ZSQL_MSSQL_DB`.
pub const MSSQL_DB_DEFAULT: &str = "zsql";
