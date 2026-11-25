#!/bin/sh
# Restore an erpcraft installation's database from a backup, then check
# that the books agree (see docs/ops/backup.md).
#
#   deploy/restore.sh [--yes] backups/erpcraft-20261125T020000Z.dump
#
# What it does:
#   1. stops the erpcraft server (PostgreSQL keeps running);
#   2. drops the database and creates it empty: everything since the
#      backup is lost, so it asks first unless --yes is given;
#   3. restores the backup with pg_restore;
#   4. refuses to go on if the backup was migrated by a newer erpcraft
#      than the image (erpcraft --check-migrations);
#   5. migrates it if it is older, and runs erpcraft verify: the general
#      ledger balances, stock and inventory accounts agree, open items and
#      their accounts agree, tax and the tax accounts agree;
#   6. starts the server again.
#
# Settings, from the environment: ENV_FILE and COMPOSE_FILE as for
# backup.sh.
set -eu

yes=no
if [ "${1:-}" = "--yes" ]; then
    yes=yes
    shift
fi
backup=${1:?usage: restore.sh [--yes] BACKUP.dump}
[ -r "$backup" ] || { echo "cannot read $backup" >&2; exit 2; }

here=$(cd "$(dirname "$0")" && pwd)
env_file=${ENV_FILE:-$here/erpcraft.env}
compose_file=${COMPOSE_FILE:-$here/compose.prod.yml}
compose() { docker compose -f "$compose_file" --env-file "$env_file" "$@"; }

if [ "$yes" != yes ]; then
    printf 'This replaces the database with %s; everything since is lost. Type yes: ' "$backup"
    read -r answer
    [ "$answer" = yes ] || { echo "nothing restored"; exit 1; }
fi

compose stop erpcraft
# shellcheck disable=SC2016 # expanded inside the container
compose exec -T postgres sh -c '
    psql --username="$POSTGRES_USER" --dbname=postgres -v ON_ERROR_STOP=1 \
        -c "DROP DATABASE IF EXISTS \"$POSTGRES_DB\" WITH (FORCE)" \
        -c "CREATE DATABASE \"$POSTGRES_DB\" OWNER \"$POSTGRES_USER\""'
# shellcheck disable=SC2016
compose exec -T postgres sh -c \
    'pg_restore --username="$POSTGRES_USER" --dbname="$POSTGRES_DB" --no-owner --exit-on-error' \
    < "$backup"
echo "restored $backup"

# Where the restored database stands against this image: 0 up to date,
# 10 migrations pending (an older backup), 20 migrated by a newer erpcraft.
set +e
compose run --rm --no-deps erpcraft --check-migrations
status=$?
set -e
case "$status" in
    0) ;;
    10) compose run --rm --no-deps erpcraft --migrate-only ;;
    20) echo "the backup is from a newer erpcraft: run that version's image" >&2; exit 20 ;;
    *) exit "$status" ;;
esac

compose run --rm --no-deps erpcraft verify
compose start erpcraft
echo "erpcraft is running on the restored database"
