#!/usr/bin/env bash
# Run the whole workspace test suite, including the driver database tests
# that pg-dev.sh, mssql-dev.sh, and mysql-dev.sh exist to serve. Brings up
# Postgres, MSSQL, and both MySQL and MariaDB, exports the URLs the
# `driver-integration-tests` feature demands, runs cargo test, and stops
# whatever it started.
#
# zsql-mysql's own suite runs twice: once against MySQL as part of the main
# workspace run below, then again on its own against MariaDB -- one sqlx
# `MySql` driver serves both engines, and this is what proves it.
#
# Containers already running are reused and left running; only the ones this
# script starts are stopped on exit. Set ZSQL_KEEP_DB=1 to keep those too.
#
# Extra arguments are forwarded to cargo test:
#   ./scripts/test-all.sh -p zsql-mssql -- --nocapture
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Mirrors the defaults in pg-dev.sh, mssql-dev.sh, and mysql-dev.sh.
# Overriding a port, password, or database name here requires overriding it
# for those scripts too, so the same variables drive both.
PG_NAME="${ZSQL_PG_NAME:-zsql-dev-pg}"
PG_PORT="${ZSQL_PG_PORT:-5432}"
PG_PASSWORD="${ZSQL_PG_PASSWORD:-zsql}"
PG_DB="${ZSQL_PG_DB:-zsql}"

MSSQL_NAME="${ZSQL_MSSQL_NAME:-zsql-dev-mssql}"
MSSQL_PORT="${ZSQL_MSSQL_PORT:-1433}"
MSSQL_PASSWORD="${ZSQL_MSSQL_PASSWORD:-zSql!DevPassw0rd}"
MSSQL_DB="${ZSQL_MSSQL_DB:-zsql}"

MYSQL_NAME="${ZSQL_MYSQL_NAME:-zsql-dev-mysql}"
MYSQL_PORT="${ZSQL_MYSQL_PORT:-3306}"
MARIADB_NAME="${ZSQL_MARIADB_NAME:-zsql-dev-mariadb}"
MARIADB_PORT="${ZSQL_MARIADB_PORT:-3307}"
MYSQL_PASSWORD="${ZSQL_MYSQL_PASSWORD:-zsql}"
MYSQL_DB="${ZSQL_MYSQL_DB:-zsql}"

# Entries of "script|extra-args-for-down", one per container this script
# itself started (and so must stop on exit).
STARTED=()

cleanup() {
  local status=$?
  if [[ "${ZSQL_KEEP_DB:-0}" != "1" ]]; then
    for entry in "${STARTED[@]:-}"; do
      [[ -n "$entry" ]] || continue
      local script="${entry%%|*}" args="${entry#*|}"
      # shellcheck disable=SC2086 # $args is a small, script-controlled word list, not user input
      "$HERE/$script" down $args || true
    done
  elif [[ ${#STARTED[@]} -gt 0 ]]; then
    echo "ZSQL_KEEP_DB=1, leaving databases running"
  fi
  exit "$status"
}
trap cleanup EXIT

running() {
  [[ -n "$(docker ps --quiet --filter "name=^${1}$")" ]]
}

ensure_up() {
  local name="$1" script="$2"
  shift 2
  if running "$name"; then
    echo "reusing running $name"
  else
    "$HERE/$script" up "$@"
    STARTED+=("$script|$*")
  fi
}

ensure_up "$PG_NAME" pg-dev.sh
ensure_up "$MSSQL_NAME" mssql-dev.sh
ensure_up "$MYSQL_NAME" mysql-dev.sh mysql
ensure_up "$MARIADB_NAME" mysql-dev.sh mariadb

export ZSQL_TEST_POSTGRES_URL="postgres://postgres:${PG_PASSWORD}@localhost:${PG_PORT}/${PG_DB}"
export ZSQL_TEST_MSSQL_URL="mssql://sa:${MSSQL_PASSWORD}@localhost:${MSSQL_PORT}/${MSSQL_DB}?trustServerCertificate=true"
export ZSQL_TEST_MYSQL_URL="mysql://root:${MYSQL_PASSWORD}@localhost:${MYSQL_PORT}/${MYSQL_DB}"

echo "running workspace tests with database tests enabled (postgres, mssql, mysql)"
cargo test --manifest-path "$HERE/../Cargo.toml" --workspace \
  --features zsql-postgres/driver-integration-tests,zsql-mssql/driver-integration-tests,zsql-mysql/driver-integration-tests \
  "$@"

echo "re-running zsql-mysql's own suite against MariaDB"
env ZSQL_TEST_MYSQL_URL="mysql://root:${MYSQL_PASSWORD}@localhost:${MARIADB_PORT}/${MYSQL_DB}" \
  cargo test --manifest-path "$HERE/../Cargo.toml" -p zsql-mysql \
  --features driver-integration-tests \
  "$@"
