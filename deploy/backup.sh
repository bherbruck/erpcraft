#!/bin/sh
# Back up an erpcraft installation's database with pg_dump, and keep a
# number of backups (see docs/ops/backup.md).
#
#   deploy/backup.sh [directory]
#
# The backup is pg_dump's custom format (compressed, restorable with
# pg_restore), written as erpcraft-YYYYMMDDTHHMMSSZ.dump. It is taken
# inside the postgres container, so the host needs no PostgreSQL tools, and
# while erpcraft runs: pg_dump reads one consistent snapshot.
#
# Settings, from the environment:
#   ENV_FILE      the installation's env file (default deploy/erpcraft.env)
#   COMPOSE_FILE  its compose file (default deploy/compose.prod.yml)
#   KEEP_DAYS     keep every backup this many days (default 14)
#   KEEP_MONTHS   and the first backup of each month this many months
#                 (default 12); anything older is deleted
set -eu

here=$(cd "$(dirname "$0")" && pwd)
dir=${1:-$here/../backups}
env_file=${ENV_FILE:-$here/erpcraft.env}
compose_file=${COMPOSE_FILE:-$here/compose.prod.yml}
keep_days=${KEEP_DAYS:-14}
keep_months=${KEEP_MONTHS:-12}

compose() { docker compose -f "$compose_file" --env-file "$env_file" "$@"; }

mkdir -p "$dir"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
file="$dir/erpcraft-$stamp.dump"

# shellcheck disable=SC2016 # expanded inside the container
compose exec -T postgres sh -c \
    'pg_dump --username="$POSTGRES_USER" --dbname="$POSTGRES_DB" --format=custom --no-owner' \
    > "$file.partial"
# A backup that cannot be read is no backup.
compose exec -T postgres pg_restore --list < "$file.partial" > /dev/null
mv "$file.partial" "$file"
echo "backed up to $file ($(wc -c < "$file") bytes)"

# Retention: every backup of the last KEEP_DAYS days, and the first of each
# month for KEEP_MONTHS months.
today=$(date -u +%s)
seen_months=" "
for backup in $(ls -1 "$dir"/erpcraft-*.dump 2>/dev/null | sort); do
    name=$(basename "$backup" .dump)
    day=$(echo "$name" | sed -n 's/^erpcraft-\([0-9]\{8\}\)T.*/\1/p')
    [ -n "$day" ] || continue
    age=$(( (today - $(date -u -d "$day" +%s)) / 86400 ))
    month=$(echo "$day" | cut -c1-6)
    first_of_month=no
    case "$seen_months" in
        *" $month "*) ;;
        *) first_of_month=yes; seen_months="$seen_months$month " ;;
    esac
    if [ "$age" -le "$keep_days" ]; then
        continue
    fi
    if [ "$first_of_month" = yes ] && [ "$age" -le $((keep_months * 31)) ]; then
        continue
    fi
    echo "removing $backup ($age days old)"
    rm -f -- "$backup"
done
