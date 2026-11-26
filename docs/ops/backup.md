# Backing up and restoring

Everything erpcraft keeps is in its PostgreSQL database: documents, postings, messages sent, logos, settings and users. The container itself holds nothing, so backing up the database is backing up erpcraft.

## Backing up

```sh
deploy/backup.sh                 # into ./backups
deploy/backup.sh /srv/backups    # or elsewhere
```

The script runs `pg_dump --format=custom` inside the postgres container, so the host needs no PostgreSQL tools. The result is one compressed file, `erpcraft-YYYYMMDDTHHMMSSZ.dump`. It is taken while erpcraft runs: pg_dump reads one consistent snapshot, and a posting is either in the backup whole or not at all. The script checks that the file can be read back before keeping it.

It then deletes old backups. By default it keeps every backup of the last 14 days, and the first backup of each month for 12 months:

```sh
KEEP_DAYS=30 KEEP_MONTHS=24 deploy/backup.sh /srv/backups
```

Run it every night from cron, and copy the directory off the machine (restic, rclone, rsync): a backup on the same disk does not survive the disk.

```cron
# m h dom mon dow  command
15 2 * * *  cd /opt/erpcraft && deploy/backup.sh /srv/backups >> /var/log/erpcraft-backup.log 2>&1
```

`ENV_FILE` and `COMPOSE_FILE` name another installation's files when there are several. To back up without Docker (a managed PostgreSQL), run `pg_dump --format=custom --no-owner "$DATABASE_URL" > erpcraft.dump` with PostgreSQL 17's tools.

## Restoring

```sh
deploy/restore.sh backups/erpcraft-20261125T021500Z.dump
```

Restoring replaces the database: everything recorded since the backup is lost, so the script asks first (`--yes` skips the question). The database in use is not touched until the backup is known to be sound:

1. the backup is restored with `pg_restore` into a new database beside the live one. Tables and their data come first, then indexes, constraints and the ledgers' triggers, so restoring does not trip the ledgers' rules. A file that is not a whole backup fails here, and nothing has changed;
2. the restored migrations are compared with the image's (`erpcraft --check-migrations`). A backup from an older erpcraft is migrated (`--migrate-only`). A backup from a newer one is refused: run the image it came from;
3. `erpcraft verify` runs on it, and the restore stops if the books do not agree;
4. only then is erpcraft stopped, the live database renamed `<name>_before_<time>` and the restored one given its name, and erpcraft started again. If it does not come up, the two are swapped back. The replaced database is kept: drop it once the restore is known to be right (the script prints the command).

On any failure before step 4 the restored copy is dropped and the database in use is as it was.

The compose project must be named (`COMPOSE_PROJECT_NAME` in the env file or the environment); `backup.sh` and `restore.sh` refuse otherwise, and so does `compose.prod.yml`. The project names the volume, so a copy of `deploy/` with a name of its own never acts on another installation's data. A project whose server is running is restored only when it is named again: `deploy/restore.sh --project erpcraft backups/...`.

Restore to a second installation now and then to prove the backups work: a copy of `deploy/` with its own `COMPOSE_PROJECT_NAME`, env file and port runs beside the first.

## Checking the books: `erpcraft verify`

```sh
docker compose -f deploy/compose.prod.yml --env-file deploy/erpcraft.env run --rm erpcraft verify
docker compose ... run --rm erpcraft verify --company OHIO --json
```

It reads the database in one read-only snapshot and changes nothing. It checks every company against itself:

| Check | What must hold |
|---|---|
| `trial_balance` | Debits equal credits in the general ledger. |
| `postings_balance` | Every posting balances on its own. |
| `stock_levels`, `lot_levels` | Every stock level, and every lot's level, is the sum of its entries. |
| `inventory` | The inventory accounts (the account rules for `inventory` and `inventory_in_transit`) carry exactly the value of the stock on hand. |
| `open_item_settlements` | Every open item is settled by exactly its settlements. |
| `open_items` | Every receivable and payable account carries exactly its open items, at what they are carried at (with revaluations standing). |
| `tax` | On every tax account, what the postings that recorded tax put there equals the tax recorded: the tax report. Payments to the tax authority record no tax and are left out. |

These are the checks the month-end scenario test makes, and they pass on its books. A failure names the account and both amounts. One that is not a restore problem: a manual journal entry straight onto an inventory, receivable or payable account. It balances, but leaves that ledger behind the general ledger. Reverse it, and use the document meant for that change (a stock adjustment, a payment, a credit note).

The command exits 0 when everything agrees and 1 when anything does not, so a script or monitor can run it.
