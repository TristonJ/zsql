#!/usr/bin/env bash
# Spin up a throwaway MySQL or MariaDB for local zsql development, mirroring
# scripts/pg-dev.sh and scripts/mssql-dev.sh. Data is ephemeral (--rm);
# re-run `up` to reset. No just/make needed.
#
# One sqlx `MySql` driver (zsql-mysql) serves both engines -- MariaDB speaks
# the MySQL wire protocol, and sqlx has no separate MariaDB backend -- so
# this script brings up either one, on its own port so both can run at once:
#   ./scripts/mysql-dev.sh up            # MySQL 8 on port 3306
#   ./scripts/mysql-dev.sh up mysql      # (same, explicit)
#   ./scripts/mysql-dev.sh up mariadb    # MariaDB on port 3307
#   ./scripts/mysql-dev.sh down          # stop the MySQL container
#   ./scripts/mysql-dev.sh down mariadb  # stop the MariaDB container
set -euo pipefail

ACTION="${1:-up}"
ENGINE="${2:-${ZSQL_MYSQL_ENGINE:-mysql}}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PASSWORD="${ZSQL_MYSQL_PASSWORD:-zsql}"
DB="${ZSQL_MYSQL_DB:-zsql}"

case "$ENGINE" in
  mysql)
    NAME="${ZSQL_MYSQL_NAME:-zsql-dev-mysql}"
    PORT="${ZSQL_MYSQL_PORT:-3306}"
    IMAGE="${ZSQL_MYSQL_IMAGE:-mysql:8}"
    # The mysql:8 image ships its CLI client as `mysql`; the mariadb:11
    # image ships it as `mariadb` (no `mysql` symlink), so the two engines
    # need different container-side client binary names.
    CLIENT_BIN="mysql"
    ;;
  mariadb)
    NAME="${ZSQL_MARIADB_NAME:-zsql-dev-mariadb}"
    PORT="${ZSQL_MARIADB_PORT:-3307}"
    IMAGE="${ZSQL_MARIADB_IMAGE:-mariadb:11}"
    CLIENT_BIN="mariadb"
    ;;
  *)
    echo "usage: $0 [up|down] [mysql|mariadb]" >&2
    exit 1
    ;;
esac

case "$ACTION" in
  up)
    docker run --rm -d \
      --name "$NAME" \
      -e MYSQL_ROOT_PASSWORD="$PASSWORD" \
      -e MYSQL_DATABASE="$DB" \
      -p "${PORT}:3306" \
      "$IMAGE" >/dev/null
    printf 'waiting for %s' "$ENGINE"
    # Neither `mysqladmin ping` nor an authenticated query over the local
    # unix socket is a reliable readiness signal: the official images (both
    # engines) briefly run a "temporary server" during first-boot
    # initialization, and that temporary server listens only on the unix
    # socket, not TCP -- MySQL 8's temp server also rejects the root
    # password, but MariaDB's does not, so a socket-based authenticated
    # query still passes against MariaDB's temp server and races its
    # shutdown/restart into the real one. Probing over TCP sidesteps both:
    # it can only succeed once the real server is up and listening.
    until docker exec "$NAME" "$CLIENT_BIN" --protocol=tcp -h127.0.0.1 -P3306 -uroot -p"$PASSWORD" -e "SELECT 1" >/dev/null 2>&1; do
      printf '.'; sleep 1
    done
    echo
    docker exec -i "$NAME" "$CLIENT_BIN" --protocol=tcp -h127.0.0.1 -P3306 -uroot -p"$PASSWORD" "$DB" < "$HERE/../dev/mysql-seed.sql"
    echo "$ENGINE up on localhost:${PORT}"
    # zsql-mysql accepts both mysql:// and mariadb:// (one sqlx `MySql`
    # driver serves both); this prints mysql:// for either engine since
    # that is the scheme sqlx's own `MySqlConnectOptions` natively parses.
    echo "export ZSQL_TEST_MYSQL_URL=mysql://root:${PASSWORD}@localhost:${PORT}/${DB}"
    ;;
  down)
    docker stop "$NAME" >/dev/null && echo "stopped $NAME"
    ;;
  *)
    echo "usage: $0 [up|down] [mysql|mariadb]" >&2
    exit 1
    ;;
esac
