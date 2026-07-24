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
# Where the CA cert generated below is copied out to on the host, so a
# client (e.g. the ssh-integration-tests suite) can pass it as `sslrootcert`
# to verify a verify-full connection against this throwaway server.
HOST_CA_CERT="$HERE/../dev/tls/mssql-ca.crt"

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

# Generate a fresh CA and a server certificate it signs inside the running
# container, point `network.tlscert`/`network.tlskey` at them via
# `mssql-conf`, then restart the container to pick the setting up --
# `mssql-conf` itself reports these two settings need a server restart,
# unlike Postgres's SIGHUP-reloadable equivalents.
#
# A separate CA plus a leaf it signs is required, not one self-signed cert
# doing both jobs: a client trusting a certificate presented as a TLS
# server's own leaf refuses to also treat that same certificate as a CA.
enable_tls() {
  docker exec -u root "$NAME" bash -c '
    set -euo pipefail
    mkdir -p /var/opt/mssql/tls
    cd /var/opt/mssql/tls
    openssl genrsa -out ca.key 2048 >/dev/null 2>&1
    openssl req -x509 -new -nodes -key ca.key -sha256 -days 3 \
      -subj "/CN='"${NAME}"'-ca" \
      -addext "basicConstraints=critical,CA:true" \
      -addext "keyUsage=critical,keyCertSign,cRLSign" \
      -out ca.crt >/dev/null 2>&1
    openssl genrsa -out server.key 2048 >/dev/null 2>&1
    openssl req -new -key server.key -subj "/CN='"${NAME}"'" -out server.csr >/dev/null 2>&1
    printf "subjectAltName=DNS:host.docker.internal,DNS:localhost\nbasicConstraints=CA:false\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n" > server.ext
    openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
      -days 3 -out server.crt -extfile server.ext >/dev/null 2>&1
    chown -R mssql:mssql /var/opt/mssql/tls
    chmod 600 server.key ca.key
    rm -f server.csr server.ext ca.srl
  '
  docker exec -u root "$NAME" /opt/mssql/bin/mssql-conf set network.tlscert /var/opt/mssql/tls/server.crt >/dev/null
  docker exec -u root "$NAME" /opt/mssql/bin/mssql-conf set network.tlskey /var/opt/mssql/tls/server.key >/dev/null
  docker restart "$NAME" >/dev/null
  printf 'waiting for mssql to restart with tls'
  until sqlcmd_in_container -Q "SELECT 1" >/dev/null 2>&1; do
    printf '.'; sleep 1
  done
  echo
  mkdir -p "$(dirname "$HOST_CA_CERT")"
  docker cp "$NAME:/var/opt/mssql/tls/ca.crt" "$HOST_CA_CERT" >/dev/null
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
    enable_tls
    echo "mssql up on localhost:${PORT}"
    echo "export ZSQL_TEST_MSSQL_URL=mssql://sa:${PASSWORD}@localhost:${PORT}/${DB}?trustServerCertificate=true"
    echo "TLS enabled with a CA-signed cert; verify-full trusts it via:"
    echo "  export ZSQL_TEST_MSSQL_SSLROOTCERT=${HOST_CA_CERT}"
    ;;
  down)
    docker stop "$NAME" >/dev/null && echo "stopped $NAME"
    ;;
  *)
    echo "usage: $0 [up|down]" >&2
    exit 1
    ;;
esac
