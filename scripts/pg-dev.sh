#!/usr/bin/env bash
# Spin up a throwaway Postgres for local zsql development.
# Data is ephemeral (--rm); re-run `up` to reset. No just/make needed.
set -euo pipefail

NAME="${ZSQL_PG_NAME:-zsql-dev-pg}"
PORT="${ZSQL_PG_PORT:-5432}"
PASSWORD="${ZSQL_PG_PASSWORD:-zsql}"
DB="${ZSQL_PG_DB:-zsql}"
IMAGE="${ZSQL_PG_IMAGE:-postgres:18}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Where the self-signed cert generated below is copied out to on the host,
# so a client (e.g. the ssh-integration-tests suite) can pass it as
# `sslrootcert` to verify a verify-ca/verify-full connection against this
# throwaway server.
HOST_CA_CERT="$HERE/../dev/tls/pg-ca.crt"

# Generate a fresh CA and a server certificate it signs, directly inside the
# running container (as the `postgres` user, so ownership/permissions are
# already correct for Postgres's own key-file strictness check), then reload
# the server to start using it. `ssl`/`ssl_cert_file`/`ssl_key_file` all take
# effect on a config reload, no restart needed.
#
# A separate CA plus a leaf it signs is required, not one self-signed cert
# doing both jobs: rustls-based clients (e.g. sqlx-postgres) refuse to trust
# a certificate presented as a TLS server's own leaf when that same
# certificate is also marked as a CA.
enable_tls() {
  local pgdata
  pgdata="$(docker exec "$NAME" printenv PGDATA)"
  docker exec -u postgres "$NAME" bash -c "
    set -euo pipefail
    cd '$pgdata'
    openssl genrsa -out ca.key 2048 >/dev/null 2>&1
    openssl req -x509 -new -nodes -key ca.key -sha256 -days 3 \
      -subj '/CN=${NAME}-ca' \
      -addext 'basicConstraints=critical,CA:true' \
      -addext 'keyUsage=critical,keyCertSign,cRLSign' \
      -out ca.crt >/dev/null 2>&1
    openssl genrsa -out server.key 2048 >/dev/null 2>&1
    openssl req -new -key server.key -subj '/CN=${NAME}' -out server.csr >/dev/null 2>&1
    printf 'subjectAltName=DNS:host.docker.internal,DNS:localhost\nbasicConstraints=CA:false\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n' > server.ext
    openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
      -days 3 -out server.crt -extfile server.ext >/dev/null 2>&1
    chmod 600 server.key ca.key
    rm -f server.csr server.ext ca.srl
  "
  docker exec "$NAME" psql -U postgres -v ON_ERROR_STOP=1 \
    -c "ALTER SYSTEM SET ssl = on;" \
    -c "ALTER SYSTEM SET ssl_cert_file = 'server.crt';" \
    -c "ALTER SYSTEM SET ssl_key_file = 'server.key';" \
    -c "SELECT pg_reload_conf();" >/dev/null
  mkdir -p "$(dirname "$HOST_CA_CERT")"
  docker cp "$NAME:$pgdata/ca.crt" "$HOST_CA_CERT" >/dev/null
}

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
    enable_tls
    echo "postgres up on localhost:${PORT}"
    echo "export DATABASE_URL=postgres://postgres:${PASSWORD}@localhost:${PORT}/${DB}"
    echo "TLS enabled with a self-signed cert; verify-ca/verify-full trust it via:"
    echo "  export ZSQL_TEST_PG_SSLROOTCERT=${HOST_CA_CERT}"
    ;;
  down)
    docker stop "$NAME" >/dev/null && echo "stopped $NAME"
    ;;
  *)
    echo "usage: $0 [up|down]" >&2
    exit 1
    ;;
esac
