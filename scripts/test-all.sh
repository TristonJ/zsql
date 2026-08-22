#!/usr/bin/env bash
# Run the whole workspace test suite, including the driver database tests
# that pg-dev.sh, mssql-dev.sh, and mysql-dev.sh exist to serve. Brings up
# Postgres, MSSQL, and both MySQL and MariaDB, exports the URLs the
# `driver-integration-tests` feature demands, runs cargo test, and stops
# whatever it started.
#
# zsql-mysql's own suite runs twice: once against MySQL as part of the main
# workspace run below, then again on its own against MariaDB
#
# Containers already running are reused and left running; only the ones this
# script starts are stopped on exit. Set ZSQL_KEEP_DB=1 to keep those too.
#
# Extra arguments are forwarded to cargo test:
#   ./scripts/test-all.sh -p zsql-mssql -- --nocapture
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Mirrors the defaults in pg-dev.sh, mssql-dev.sh, and mysql-dev.sh.
# Overriding a password or database name here requires overriding it for
# those scripts too, so the same variables drive both. Ports are not
# mirrored here: this script always reads the actual published port back
# from docker (see `published_port` below) rather than trusting a shell env
# var that may not match what a reused container was really started with.
PG_NAME="${ZSQL_PG_NAME:-zsql-dev-pg}"
PG_PASSWORD="${ZSQL_PG_PASSWORD:-zsql}"
PG_DB="${ZSQL_PG_DB:-zsql}"

MSSQL_NAME="${ZSQL_MSSQL_NAME:-zsql-dev-mssql}"
MSSQL_PASSWORD="${ZSQL_MSSQL_PASSWORD:-zSql!DevPassw0rd}"
MSSQL_DB="${ZSQL_MSSQL_DB:-zsql}"

MYSQL_NAME="${ZSQL_MYSQL_NAME:-zsql-dev-mysql}"
MARIADB_NAME="${ZSQL_MARIADB_NAME:-zsql-dev-mariadb}"
MYSQL_PASSWORD="${ZSQL_MYSQL_PASSWORD:-zsql}"
MYSQL_DB="${ZSQL_MYSQL_DB:-zsql}"

# Setting this writes a code coverage report to the given path
ZSQL_COVERAGE_FILE="${ZSQL_COVERAGE_FILE:-}"

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

# Reads the host port `name` actually publishes for its container-internal
# `internal_port` straight from docker, rather than trusting this shell's
# env vars: a reused container started earlier (by this script or by hand)
# under a different port would otherwise silently point the test URLs this
# script builds at a dead port.
published_port() {
  local name="$1" internal_port="$2" env_var="$3"
  local mapping
  mapping="$(docker port "$name" "$internal_port" 2>/dev/null | head -n1)"
  if [[ -z "$mapping" ]]; then
    echo "$name is not publishing port $internal_port; check it is running and $env_var is correct" >&2
    exit 1
  fi
  printf '%s\n' "${mapping##*:}"
}

# Generous enough for a loopback connect to a container port that is
# already up, tight enough that a dead port fails in seconds, not by
# hanging the test run.
TCP_PREFLIGHT_TIMEOUT_SECS=3

# Fails fast, naming the unreachable host:port and the env var controlling
# it, instead of letting cargo test hang against a dead port.
wait_for_port() {
  local host="$1" port="$2" env_var="$3"
  if ! timeout "$TCP_PREFLIGHT_TIMEOUT_SECS" bash -c ": >/dev/tcp/${host}/${port}" 2>/dev/null; then
    echo "cannot reach ${host}:${port} ($env_var); check the container is up and that env var names the right port" >&2
    exit 1
  fi
}

ensure_up "$PG_NAME" pg-dev.sh
ensure_up "$MSSQL_NAME" mssql-dev.sh
ensure_up "$MYSQL_NAME" mysql-dev.sh mysql
ensure_up "$MARIADB_NAME" mysql-dev.sh mariadb

PG_PORT="$(published_port "$PG_NAME" 5432 ZSQL_PG_PORT)"
MSSQL_PORT="$(published_port "$MSSQL_NAME" 1433 ZSQL_MSSQL_PORT)"
MYSQL_PORT="$(published_port "$MYSQL_NAME" 3306 ZSQL_MYSQL_PORT)"
MARIADB_PORT="$(published_port "$MARIADB_NAME" 3306 ZSQL_MARIADB_PORT)"

wait_for_port localhost "$PG_PORT" ZSQL_PG_PORT
wait_for_port localhost "$MSSQL_PORT" ZSQL_MSSQL_PORT
wait_for_port localhost "$MYSQL_PORT" ZSQL_MYSQL_PORT
wait_for_port localhost "$MARIADB_PORT" ZSQL_MARIADB_PORT

export ZSQL_TEST_POSTGRES_URL="postgres://postgres:${PG_PASSWORD}@localhost:${PG_PORT}/${PG_DB}"
export ZSQL_TEST_MSSQL_URL="mssql://sa:${MSSQL_PASSWORD}@localhost:${MSSQL_PORT}/${MSSQL_DB}?trustServerCertificate=true"
export ZSQL_TEST_MYSQL_URL="mysql://root:${MYSQL_PASSWORD}@localhost:${MYSQL_PORT}/${MYSQL_DB}"

echo "running workspace tests with database tests enabled (postgres, mssql, mysql)"
FEATURES="zsql-postgres/driver-integration-tests,zsql-mssql/driver-integration-tests,zsql-mysql/driver-integration-tests,zsql/driver-integration-tests"
if [[ -n "$ZSQL_COVERAGE_FILE" ]]; then
  cargo llvm-cov  --manifest-path "$HERE/../Cargo.toml"  --workspace --lcov --output-path "$ZSQL_COVERAGE_FILE" \
    --features $FEATURES \
    "$@"
else
  cargo test --manifest-path "$HERE/../Cargo.toml" --workspace \
    --features $FEATURES \
    "$@"
fi

echo "re-running zsql-mysql's own suite against MariaDB"
env ZSQL_TEST_MYSQL_URL="mysql://root:${MYSQL_PASSWORD}@localhost:${MARIADB_PORT}/${MYSQL_DB}" \
  cargo test --manifest-path "$HERE/../Cargo.toml" -p zsql-mysql \
  --features driver-integration-tests \
  "$@"
