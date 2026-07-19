#!/usr/bin/env bash
# Spin up a throwaway MSSQL for local zsql development, mirroring
# scripts/pg-dev.sh. Data is ephemeral (--rm); re-run `up` to reset. No
# just/make needed.
set -euo pipefail

NAME="${ZSQL_MSSQL_NAME:-zsql-dev-mssql}"
PORT="${ZSQL_MSSQL_PORT:-1433}"
PASSWORD="${ZSQL_MSSQL_PASSWORD:-zSql!DevPassw0rd}"
DB="${ZSQL_MSSQL_DB:-zsql}"
IMAGE="${ZSQL_MSSQL_IMAGE:-mcr.microsoft.com/mssql/server:2022-latest}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The 2022 image ships `sqlcmd` under one of these two paths depending on
# image revision; the newer `mssql-tools18` build defaults to encrypted
# connections and needs `-C` to trust the server's own (self-signed, for
# local dev) certificate.
sqlcmd_in_container() {
  if docker exec "$NAME" test -x /opt/mssql-tools18/bin/sqlcmd; then
    docker exec -i "$NAME" /opt/mssql-tools18/bin/sqlcmd -C -S localhost -U sa -P "$PASSWORD" "$@"
  else
    docker exec -i "$NAME" /opt/mssql-tools/bin/sqlcmd -S localhost -U sa -P "$PASSWORD" "$@"
  fi
}

case "${1:-up}" in
  up)
    docker run --rm -d \
      --name "$NAME" \
      -e ACCEPT_EULA=Y \
      -e MSSQL_SA_PASSWORD="$PASSWORD" \
      -p "${PORT}:1433" \
      "$IMAGE" >/dev/null
    printf 'waiting for mssql'
    until sqlcmd_in_container -Q "SELECT 1" >/dev/null 2>&1; do
      printf '.'; sleep 1
    done
    echo
    sqlcmd_in_container -Q "IF DB_ID('${DB}') IS NULL CREATE DATABASE [${DB}]"
    sqlcmd_in_container -d "$DB" -i /dev/stdin < "$HERE/../dev/mssql-seed.sql"
    echo "mssql up on localhost:${PORT}"
    echo "export ZSQL_TEST_MSSQL_URL=mssql://sa:${PASSWORD}@localhost:${PORT}/${DB}?trustServerCertificate=true"
    ;;
  down)
    docker stop "$NAME" >/dev/null && echo "stopped $NAME"
    ;;
  *)
    echo "usage: $0 [up|down]" >&2
    exit 1
    ;;
esac
