#!/usr/bin/env bash
# One trial run on a fresh copy of the month scenario.
#
#   tools/mcp-trial/run.sh LABEL [path-to-erpcraft-binary]
#
# Needs the template database erpcraft_mcptrial_base (see README.md). Starts
# the server on 127.0.0.1:8611 against a scratch copy, runs setup.ts and
# trial.ts, stops the server and drops the copy.
set -euo pipefail
label="${1:?label}"
bin="${2:-target/debug/erpcraft}"
here="$(cd "$(dirname "$0")" && pwd)"
admin="postgres://erpcraft:erpcraft@127.0.0.1:54320/postgres"
db=erpcraft_mcptrial_run
psql "$admin" -Xqc "DROP DATABASE IF EXISTS $db WITH (FORCE)"
psql "$admin" -Xqc "CREATE DATABASE $db TEMPLATE erpcraft_mcptrial_base"
DATABASE_URL="postgres://erpcraft:erpcraft@127.0.0.1:54320/$db" ERPCRAFT_BIND=127.0.0.1:8611 RUST_LOG=warn "$bin" &
server=$!
trap 'kill $server 2>/dev/null; wait $server 2>/dev/null; psql "$admin" -Xqc "DROP DATABASE IF EXISTS $db WITH (FORCE)"' EXIT
for _ in $(seq 1 60); do curl -sf 127.0.0.1:8611/healthz >/dev/null && break; sleep 1; done
cd "$here"
bun run setup.ts
bun run trial.ts --label "$label"
