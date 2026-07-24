#!/usr/bin/env bash
# Spin up a throwaway sshd for local zsql SSH-tunnel development, mirroring
# scripts/pg-dev.sh. Forwards to the dev Postgres via the host, so run
# scripts/pg-dev.sh up first. Ephemeral (--rm); re-run `up` to reset. No
# just/make needed.
set -euo pipefail

NAME="${ZSQL_SSH_NAME:-zsql-dev-ssh}"
PORT="${ZSQL_SSH_PORT:-2222}"
USER_NAME="${ZSQL_SSH_USER:-zsql}"
PASSWORD="${ZSQL_SSH_PASSWORD:-zsql}"
IMAGE="${ZSQL_SSH_IMAGE:-lscr.io/linuxserver/openssh-server:latest}"
PG_PORT="${ZSQL_PG_PORT:-5432}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE_PUBKEY="${ZSQL_SSH_FIXTURE_PUBKEY:-${SCRIPT_DIR}/../crates/zsql-ssh/tests/fixtures/id_ed25519.pub}"

case "${1:-up}" in
  up)
    if [ ! -f "$FIXTURE_PUBKEY" ]; then
      echo "fixture public key not found: $FIXTURE_PUBKEY" >&2
      echo "(crates/zsql-ssh/tests/fixtures/id_ed25519.pub should be committed)" >&2
      exit 1
    fi
    docker run --rm -d \
      --name "$NAME" \
      --add-host=host.docker.internal:host-gateway \
      -e PUID=1000 \
      -e PGID=1000 \
      -e PASSWORD_ACCESS=true \
      -e USER_NAME="$USER_NAME" \
      -e USER_PASSWORD="$PASSWORD" \
      -e PUBLIC_KEY="$(cat "$FIXTURE_PUBKEY")" \
      -p "${PORT}:2222" \
      "$IMAGE" >/dev/null
    # The published port opens (via docker-proxy) before sshd inside the
    # container has generated its config and started, so readiness is polled
    # on the container's own state rather than the forwarded port.
    printf 'waiting for sshd'
    ready=""
    for _ in $(seq 1 120); do
      if docker exec "$NAME" sh -c 'test -f /config/sshd/sshd_config && pgrep -f "sshd.*-D" >/dev/null 2>&1'; then
        ready=1
        break
      fi
      printf '.'
      sleep 0.5
    done
    echo
    if [ -z "$ready" ]; then
      echo "sshd did not become ready; see: docker logs $NAME" >&2
      exit 1
    fi
    # The linuxserver/openssh-server image ships AllowTcpForwarding no, which
    # rejects the direct-tcpip channels the tunnel relies on. Enable it in the
    # config sshd actually runs with, then reload sshd to pick it up.
    docker exec "$NAME" sh -c '
      cfg=/config/sshd/sshd_config
      if grep -q "^AllowTcpForwarding" "$cfg"; then
        sed -i "s/^AllowTcpForwarding.*/AllowTcpForwarding yes/" "$cfg"
      else
        echo "AllowTcpForwarding yes" >> "$cfg"
      fi
      kill -HUP "$(pgrep -f "sshd.*-D" | head -1)"
    '
    echo "sshd up on localhost:${PORT}"
    echo "  ssh user:     ${USER_NAME}"
    echo "  ssh password: ${PASSWORD}"
    echo "  ssh key:      ${FIXTURE_PUBKEY%.pub} (authorized via ${FIXTURE_PUBKEY})"
    echo "forwards from inside the container reach the dev postgres at host.docker.internal:${PG_PORT}"
    echo "(run scripts/pg-dev.sh up first so that target is reachable)"
    echo
    echo "export ZSQL_TEST_SSH_HOST=127.0.0.1"
    echo "export ZSQL_TEST_SSH_PORT=${PORT}"
    echo "export ZSQL_TEST_SSH_USER=${USER_NAME}"
    echo "export ZSQL_TEST_SSH_PASSWORD=${PASSWORD}"
    echo "export ZSQL_TEST_SSH_REMOTE_HOST=host.docker.internal"
    echo "export ZSQL_TEST_SSH_REMOTE_PORT=${PG_PORT}"
    echo
    echo "cargo test -p zsql-ssh --features ssh-integration-tests --test ssh_integration"
    ;;
  down)
    docker stop "$NAME" >/dev/null && echo "stopped $NAME"
    ;;
  *)
    echo "usage: $0 [up|down]" >&2
    exit 1
    ;;
esac
