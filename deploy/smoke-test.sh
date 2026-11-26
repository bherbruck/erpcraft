#!/bin/sh
# The production image, tried end to end the way an installation runs it:
# build it, start it with compose.prod.yml on a fresh database, sign in,
# set a company up from a template, take a backup, restore it, check the
# books, and take everything down again. For CI, or before a release.
#
#   deploy/smoke-test.sh            # builds erpcraft:smoke first
#   ERPCRAFT_IMAGE=erpcraft:1.2 SKIP_BUILD=1 deploy/smoke-test.sh
#
# Needs docker with compose, curl and openssl. Uses its own compose project
# (erpcraft-smoke) and port (127.0.0.1:18600), and removes its volume.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
image=${ERPCRAFT_IMAGE:-erpcraft:smoke}
port=${SMOKE_PORT:-18600}
work=$(mktemp -d)
export COMPOSE_PROJECT_NAME=erpcraft-smoke
export ENV_FILE="$work/erpcraft.env"
export ERPCRAFT_ENV_FILE="$ENV_FILE"
compose() { docker compose -f "$here/compose.prod.yml" --env-file "$ENV_FILE" "$@"; }

cleanup() {
    compose down --volumes --remove-orphans > /dev/null 2>&1 || true
    rm -rf -- "$work"
}
trap cleanup EXIT INT TERM

step() { printf '\n== %s\n' "$*"; }
fail() { echo "SMOKE TEST FAILED: $*" >&2; compose logs --tail 80 erpcraft >&2 || true; exit 1; }

if [ "${SKIP_BUILD:-0}" != 1 ]; then
    step "building $image"
    docker build -t "$image" "$root"
fi

step "starting on a fresh database"
password=$(openssl rand -hex 16)
cat > "$ENV_FILE" <<EOF
POSTGRES_USER=erpcraft
POSTGRES_PASSWORD=$(openssl rand -hex 16)
POSTGRES_DB=erpcraft
ERPCRAFT_ADMIN_EMAIL=admin@smoke.example
ERPCRAFT_ADMIN_PASSWORD=$password
ERPCRAFT_IMAGE=$image
ERPCRAFT_PUBLISH=127.0.0.1:$port
RUST_LOG=info
EOF
grep -v '^COMPOSE_PROJECT_NAME=' "$ENV_FILE" > "$work/unnamed.env"
compose up -d

step "waiting for the health check"
container=$(compose ps -q erpcraft)
health=starting
for _ in $(seq 1 60); do
    health=$(docker inspect --format '{{.State.Health.Status}}' "$container")
    [ "$health" = healthy ] && break
    sleep 2
done
[ "$health" = healthy ] || fail "the container is $health"
base="http://127.0.0.1:$port"
curl -fsS "$base/healthz" > /dev/null || fail "/healthz"
curl -fsS "$base/" | grep -qi '<html' || fail "the admin UI is not served"
docker inspect --format '{{.Config.User}}' "$image" | grep -q nonroot || fail "the image runs as root"

step "signing in and setting a company up from a template"
token=$(curl -fsS -H 'content-type: application/json' \
    -d "{\"email\":\"admin@smoke.example\",\"password\":\"$password\"}" \
    "$base/api/auth/login" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')
[ -n "$token" ] || fail "no token from /api/auth/login"
curl -fsS -H "authorization: Bearer $token" -H 'content-type: application/json' \
    -d '{"template":"us_small_business","code":"SMOKE","name":"Smoke Test Inc","first_year":2026,"years":1}' \
    "$base/api/commands/company.setup" > "$work/setup.json" || fail "company.setup"
grep -q '"code":"SMOKE"' "$work/setup.json" || fail "company.setup said: $(cat "$work/setup.json")"

step "posting a journal entry"
company=$(sed -n 's/.*"company":{[^}]*"id":"\([^"]*\)".*/\1/p' "$work/setup.json")
bank=$(sed -n 's/.*"bank":{[^}]*"id":"\([^"]*\)".*/\1/p' "$work/setup.json")
revenue=$(sed -n 's/.*"revenue":{[^}]*"id":"\([^"]*\)".*/\1/p' "$work/setup.json")
[ -n "$company" ] && [ -n "$bank" ] && [ -n "$revenue" ] || fail "no company, bank or revenue in the setup"
api() { curl -fsS -H "authorization: Bearer $token" -H 'content-type: application/json' "$@"; }
entry=$(api -d "{\"company_id\":\"$company\",\"doc_date\":\"2026-03-15\",\"body\":{\"lines\":[
    {\"account_id\":\"$bank\",\"debit\":\"125.00\"},{\"account_id\":\"$revenue\",\"credit\":\"125.00\"}]}}" \
    "$base/api/documents/journal_entry" | sed -n 's/^{[^{]*"id":"\([^"]*\)".*/\1/p')
[ -n "$entry" ] || fail "no journal entry drafted"
api -d "{\"id\":\"$entry\"}" "$base/api/commands/journal_entry.post" | grep -q '"status":"posted"' \
    || fail "the journal entry did not post"

step "the migrations are current, and the books agree"
compose run --rm --no-deps erpcraft --check-migrations || fail "--check-migrations"
compose run --rm --no-deps erpcraft verify || fail "verify"

step "backing up, and restoring"
KEEP_DAYS=14 "$here/backup.sh" "$work/backups"
backup=$(ls -1 "$work"/backups/erpcraft-*.dump | head -1)
# What must never touch the data: a restore into a running project that
# is not named, and a file that is not a whole backup.
if "$here/restore.sh" --yes "$backup" 2> "$work/refused"; then
    fail "a restore into the running project without --project went ahead"
fi
grep -q -- "--project" "$work/refused" || fail "the refusal said: $(cat "$work/refused")"
head -c 4096 "$backup" > "$work/cut.dump"
if "$here/restore.sh" --yes --project erpcraft-smoke "$work/cut.dump"; then
    fail "a backup cut short was restored"
fi
echo "this is not a backup" > "$work/garbage.dump"
if "$here/restore.sh" --yes --project erpcraft-smoke "$work/garbage.dump"; then
    fail "a file that is no backup was restored"
fi
api "$base/api/documents/journal_entry/$entry" | grep -q '"status":"posted"' \
    || fail "a refused restore changed the database"
if KEEP_DAYS=14 COMPOSE_PROJECT_NAME= ENV_FILE="$work/unnamed.env" "$here/backup.sh" "$work/backups" 2> /dev/null; then
    fail "a backup ran without a project name"
fi
"$here/restore.sh" --yes --project erpcraft-smoke "$backup" || fail "restore"
compose exec -T postgres psql -U erpcraft -d postgres -Atc \
    "SELECT datname FROM pg_database WHERE datname LIKE 'erpcraft_before_%'" | grep -q . \
    || fail "the database the restore replaced is not kept"
for _ in $(seq 1 60); do
    curl -fsS "$base/healthz" > /dev/null 2>&1 && break
    sleep 2
done
curl -fsS -H "authorization: Bearer $token" "$base/api/entities/company?code=SMOKE" \
    | grep -q '"code":"SMOKE"' || fail "the company is not there after the restore"
api "$base/api/documents/journal_entry/$entry" | grep -q '"status":"posted"' \
    || fail "the journal entry is not there after the restore"

step "smoke test passed"
