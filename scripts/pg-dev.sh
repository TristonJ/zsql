#!/usr/bin/env bash
# Spin up a throwaway Postgres for local zsql development.
# Data is ephemeral (--rm); re-run `up` to reset. No just/make needed.
set -euo pipefail

NAME="${ZSQL_PG_NAME:-zsql-dev-pg}"
PORT="${ZSQL_PG_PORT:-5432}"
PASSWORD="${ZSQL_PG_PASSWORD:-zsql}"
DB="${ZSQL_PG_DB:-zsql}"
IMAGE="${ZSQL_PG_IMAGE:-postgres:17}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

case "${1:-up}" in
  up)
    docker run --rm -d \
      --name "$NAME" \
      -e POSTGRES_PASSWORD="$PASSWORD" \
      -e POSTGRES_DB="$DB" \
      -p "${PORT}:5432" \
      "$IMAGE" >/dev/null
    printf 'waiting for postgres'
    until docker exec "$NAME" pg_isready -U postgres >/dev/null 2>&1; do
      printf '.'; sleep 0.5
    done
    echo
    docker exec -i "$NAME" psql -v ON_ERROR_STOP=1 -U postgres -d "$DB" < "$HERE/../dev/seed.sql"
    echo "postgres up on localhost:${PORT}"
    echo "export DATABASE_URL=postgres://postgres:${PASSWORD}@localhost:${PORT}/${DB}"
    ;;
  down)
    docker stop "$NAME" >/dev/null && echo "stopped $NAME"
    ;;
  *)
    echo "usage: $0 [up|down]" >&2
    exit 1
    ;;
esac
