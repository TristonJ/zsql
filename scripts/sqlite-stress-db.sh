#!/usr/bin/env bash
# Generate a stress-test SQLite database file for manual/perf testing of the
# sqlite driver, the results grid, and the schema sidebar. No Docker, no
# network, no just/make: this just runs the crate's own generator example
# and prints what it produced.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${ZSQL_STRESS_DB_PATH:-$HERE/../dev/stress.sqlite3}"

cargo run --quiet -p zsql-sqlite --example gen_stress_db -- "$OUT"
