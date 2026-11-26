# Upgrading erpcraft

An upgrade is a new image and a database migrated forward. Migrations belong to modules: each module numbers its own (`stock` 12, `sales` 15, ...), and the table `erpcraft_migration` records which have run. The server applies whatever is pending when it starts, one migration per transaction, under a lock, so two servers starting together do not both run one. What changed between versions is in [CHANGELOG.md](../../CHANGELOG.md).

## Before the first upgrade from 0.1.0: name the project

`compose.prod.yml` no longer falls back to the project name `erpcraft`. Add `COMPOSE_PROJECT_NAME=erpcraft` to `deploy/erpcraft.env` (the name the installation has had, so its volume `erpcraft_pgdata` is found again). A test copy of `deploy/` gets a name of its own. Without it compose, `backup.sh` and `restore.sh` refuse to run.

## The steps

```sh
cd /opt/erpcraft
git pull                                     # or download the release
docker build -t erpcraft:latest .            # or pull the release image

# 1. A backup to come back to.
deploy/backup.sh /srv/backups

# 2. What the new image will do to the database (changes nothing).
docker compose -f deploy/compose.prod.yml --env-file deploy/erpcraft.env \
    run --rm erpcraft --check-migrations

# 3. Migrate while the old server is stopped, then start the new one.
docker compose -f deploy/compose.prod.yml --env-file deploy/erpcraft.env stop erpcraft
docker compose -f deploy/compose.prod.yml --env-file deploy/erpcraft.env \
    run --rm erpcraft --migrate-only
docker compose -f deploy/compose.prod.yml --env-file deploy/erpcraft.env up -d

# 4. The books still agree.
docker compose -f deploy/compose.prod.yml --env-file deploy/erpcraft.env \
    run --rm erpcraft verify
```

Step 3 can also be just `up -d`, since the server migrates as it starts. Migrating first while nothing runs has two advantages: a failing migration shows its error in your terminal, not in a restart loop, and nobody uses the old server against a half-migrated schema. A migration that fails is rolled back whole, and the database stays at the version before it. Fix the cause, or restore the backup and stay on the old image.

`--check-migrations` exits 0 when there is nothing to do, 10 when migrations are pending (it lists them), 20 when the database is newer than the image, and 30 when it records a migration under another name than the image's for the same module and number; the server refuses to start on such a database.

## Going back

erpcraft does not migrate a database backwards. An older binary refuses to start against a database that a newer one migrated, and names the migrations it does not know:

```
erpcraft: the database has 2 migration(s) this binary does not know: stock 12 (units), sales 15 (units). It was migrated by a newer erpcraft (or one with other modules); run that version, or restore a backup taken before the upgrade.
```

`--migrate-only` and `verify` refuse the same way. To go back to an older version, restore the backup taken in step 1 and run the old image. Anything recorded after the upgrade is lost, so decide quickly.

## Versions

Upgrade from any version to any later one: the migrations in between run in order, oldest first. Read the changelog for anything marked as needing a step of your own, such as a new required setting or an account rule a new feature asks for. erpcraft is before 1.0; until then, a release may change the API, but never the data's meaning without a migration that carries the data across.

## Installations with their own modules

An installation that registers modules of its own (after the in-tree ones, in its own `main`) owns their migrations the same way. If such a module is removed, its migrations stay recorded. The binary then sees them as unknown and refuses to start, as it would after a downgrade. Delete the module's rows from `erpcraft_migration` (and its tables) only when you mean to drop its data.
