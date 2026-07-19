#!/usr/bin/env bash
# Run the whole workspace test suite, including the driver database tests
# that pg-dev.sh and mssql-dev.sh exist to serve. Brings both databases up,
# exports the URLs the `driver-integration-tests` feature demands, runs
# cargo test, and stops whatever it started.
#
# Containers already running are reused and left running; only the ones this
# script starts are stopped on exit. Set ZSQL_KEEP_DB=1 to keep those too.
#
# Extra arguments are forwarded to cargo test:
#   ./scripts/test-all.sh -p zsql-mssql -- --nocapture
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Mirrors the defaults in pg-dev.sh and mssql-dev.sh. Overriding a port,
# password, or database name here requires overriding it for those scripts
# too, so the same variables drive both.
PG_NAME="${ZSQL_PG_NAME:-zsql-dev-pg}"
PG_PORT="${ZSQL_PG_PORT:-5432}"
PG_PASSWORD="${ZSQL_PG_PASSWORD:-zsql}"
PG_DB="${ZSQL_PG_DB:-zsql}"

MSSQL_NAME="${ZSQL_MSSQL_NAME:-zsql-dev-mssql}"
MSSQL_PORT="${ZSQL_MSSQL_PORT:-1433}"
MSSQL_PASSWORD="${ZSQL_MSSQL_PASSWORD:-zSql!DevPassw0rd}"
MSSQL_DB="${ZSQL_MSSQL_DB:-zsql}"

STARTED=()

cleanup() {
  local status=$?
  if [[ "${ZSQL_KEEP_DB:-0}" != "1" ]]; then
    for script in "${STARTED[@]:-}"; do
      [[ -n "$script" ]] && "$HERE/$script" down || true
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
  if running "$name"; then
    echo "reusing running $name"
  else
    "$HERE/$script" up
    STARTED+=("$script")
  fi
}

ensure_up "$PG_NAME" pg-dev.sh
ensure_up "$MSSQL_NAME" mssql-dev.sh

export ZSQL_TEST_POSTGRES_URL="postgres://postgres:${PG_PASSWORD}@localhost:${PG_PORT}/${PG_DB}"
export ZSQL_TEST_MSSQL_URL="mssql://sa:${MSSQL_PASSWORD}@localhost:${MSSQL_PORT}/${MSSQL_DB}?trustServerCertificate=true"

echo "running workspace tests with database tests enabled"
cargo test --manifest-path "$HERE/../Cargo.toml" --workspace \
  --features zsql-postgres/driver-integration-tests,zsql-mssql/driver-integration-tests \
  "$@"
