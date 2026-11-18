#!/usr/bin/env bash
# Load "a month in the life of Ohio Widgets" into a scratch database, for a demo.
#
#   tools/scenario/month.sh [database-name]     (default: erpcraft_scenario_month)
#
# Runs the scenario test (apps/server/tests/scenario_month.rs) with
# SCENARIO_KEEP_DB=1, so the test keeps its database, then renames it. Point
# a server at it with DATABASE_URL=.../<database-name>. Never touches the
# database in .env; an existing database of the given name is refused.
set -euo pipefail
name="${1:-erpcraft_scenario_month}"
[[ "$name" =~ ^[a-z_][a-z0-9_]*$ ]] || { echo "not a database name: $name" >&2; exit 2; }
url="${DATABASE_URL:-postgres://erpcraft:erpcraft@127.0.0.1:54320/erpcraft}"
admin="${url%/*}/postgres"
if psql "$admin" -XAtc "SELECT 1 FROM pg_database WHERE datname = '$name'" | grep -q 1; then
  echo "database $name exists; drop it or choose another name" >&2
  exit 1
fi
out=$(SCENARIO_KEEP_DB=1 nice -n 10 cargo test -j 4 -p erpcraft-server --test scenario_month \
  a_month_in_the_life_of_ohio_widgets -- --exact --nocapture 2>&1 | tee /dev/stderr)
kept=$(grep -o 'SCENARIO_DATABASE=[a-z0-9_]*' <<<"$out" | tail -1 | cut -d= -f2)
[[ -n "$kept" ]] || { echo "the scenario did not keep a database" >&2; exit 1; }
psql "$admin" -Xqc "ALTER DATABASE \"$kept\" RENAME TO \"$name\""
echo "Ohio Widgets' March 2026 is in database $name (sign in as admin@example.com / correct-horse-battery, the test harness's administrator)."
