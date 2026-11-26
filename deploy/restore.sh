#!/bin/sh
# Restore an erpcraft installation's database from a backup, then check
# that the books agree (see docs/ops/backup.md).
#
#   deploy/restore.sh [--yes] [--project NAME] backups/erpcraft-20261125T020000Z.dump
#
# The database in use is not touched until the backup has been restored
# beside it and found sound:
#   1. the backup is restored into a new database next to the live one; a
#      file that is not a whole backup fails here, and nothing changed;
#   2. that database is compared with this image (erpcraft
#      --check-migrations): a backup from a newer erpcraft is refused, an
#      older one is migrated;
#   3. erpcraft verify runs on it: the general ledger balances, stock and
#      inventory accounts agree, open items and their accounts agree, tax
#      and the tax accounts agree;
#   4. only then the server is stopped, the live database renamed to
#      <name>_before_<time> and the restored one given its name, and the
#      server started again. The old database is kept: drop it once the
#      restored one is known to be right (the command is printed). If the
#      server does not come up, the two are swapped back.
# Everything since the backup is lost from the database in use, so it asks
# first unless --yes is given.
#
# Settings, from the environment: ENV_FILE and COMPOSE_FILE as for
# backup.sh. The compose project must be named (COMPOSE_PROJECT_NAME, in
# the environment or the env file), so a copy of deploy/ for a test can
# never act on production's volume. A project whose server is running is
# restored only when it is named again on the command line (--project).
set -eu

yes=no
named=
while [ $# -gt 1 ]; do
    case "$1" in
        --yes) yes=yes; shift ;;
        --project) named=${2:?--project needs a name}; shift 2 ;;
        *) break ;;
    esac
done
backup=${1:?usage: restore.sh [--yes] [--project NAME] BACKUP.dump}
if [ ! -r "$backup" ] || [ ! -s "$backup" ]; then
    echo "cannot read $backup, or it is empty" >&2
    exit 2
fi

here=$(cd "$(dirname "$0")" && pwd)
env_file=${ENV_FILE:-$here/erpcraft.env}
compose_file=${COMPOSE_FILE:-$here/compose.prod.yml}
setting() { sed -n "s/^$1=//p" "$env_file" | tail -n 1; }
project=${COMPOSE_PROJECT_NAME:-$(setting COMPOSE_PROJECT_NAME)}
if [ -z "$project" ]; then
    echo "name the compose project: set COMPOSE_PROJECT_NAME in $env_file" >&2
    exit 2
fi
export COMPOSE_PROJECT_NAME="$project"
compose() { docker compose -f "$compose_file" --env-file "$env_file" "$@"; }

if [ -n "$(compose ps -q --status running erpcraft 2>/dev/null)" ] && [ "$named" != "$project" ]; then
    echo "the server of project $project is running: name it to restore into it (--project $project)" >&2
    exit 2
fi

db_user=$(setting POSTGRES_USER)
db_user=${db_user:-erpcraft}
db_password=$(setting POSTGRES_PASSWORD)
db_name=$(setting POSTGRES_DB)
db_name=${db_name:-erpcraft}
stamp=$(date -u +%Y%m%dt%H%M%Sz)
fresh="${db_name}_restore_$stamp"
before="${db_name}_before_$stamp"

if [ "$yes" != yes ]; then
    printf 'This replaces database %s of project %s with %s; everything since is lost. Type yes: ' \
        "$db_name" "$project" "$backup"
    read -r answer
    [ "$answer" = yes ] || { echo "nothing restored"; exit 1; }
fi

compose up -d --wait postgres
psql() {
    compose exec -T postgres psql --username="$db_user" --dbname=postgres -v ON_ERROR_STOP=1 -q "$@"
}
drop_fresh() {
    psql -c "DROP DATABASE IF EXISTS \"$fresh\" WITH (FORCE)" > /dev/null 2>&1 || true
}
on_the_fresh() {
    compose run --rm --no-deps \
        -e "DATABASE_URL=postgres://$db_user:$db_password@postgres:5432/$fresh" erpcraft "$@"
}
unchanged() {
    drop_fresh
    echo "$1; the database in use is unchanged" >&2
    exit "$2"
}

# 1. Beside the live database.
psql -c "CREATE DATABASE \"$fresh\" OWNER \"$db_user\""
compose exec -T postgres pg_restore --username="$db_user" --dbname="$fresh" \
    --no-owner --exit-on-error < "$backup" || unchanged "$backup could not be restored" 3
echo "restored $backup into $fresh"

# 2. Where it stands against this image: 0 up to date, 10 migrations
# pending (an older backup), 20 migrated by a newer erpcraft.
set +e
on_the_fresh --check-migrations
status=$?
set -e
case "$status" in
    0) ;;
    10) on_the_fresh --migrate-only || unchanged "migrating the backup failed" 4 ;;
    20) unchanged "the backup is from a newer erpcraft: run that version's image" 20 ;;
    *) unchanged "the backup's migrations do not match this image's (above)" "$status" ;;
esac

# 3. The books of the backup.
on_the_fresh verify || unchanged "the restored books do not agree (above)" 5

# 4. The swap, with nobody connected.
compose stop erpcraft
rename() {
    psql -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
             WHERE datname IN ('$1', '$2') AND pid <> pg_backend_pid()" > /dev/null
    psql -c "ALTER DATABASE \"$1\" RENAME TO \"$3\"" -c "ALTER DATABASE \"$2\" RENAME TO \"$1\""
}
rename "$db_name" "$fresh" "$before"
if ! compose up -d --wait erpcraft; then
    echo "the server does not start on the restored database: putting the old one back" >&2
    compose stop erpcraft || true
    rename "$db_name" "$before" "$fresh"
    compose up -d erpcraft
    echo "the restored database is kept as $fresh" >&2
    exit 6
fi
echo "erpcraft is running on the restored database"
echo "the database it replaced is kept as $before; once the restore is known to be right:"
echo "  COMPOSE_PROJECT_NAME=$project docker compose -f $compose_file --env-file $env_file \\"
echo "    exec postgres psql -U $db_user -d postgres -c 'DROP DATABASE \"$before\"'"
