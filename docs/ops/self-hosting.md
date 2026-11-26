# Running erpcraft yourself

One machine runs it: a small Linux server or VM with Docker, 2 GB of memory and a few GB of disk to begin with. erpcraft runs in one container, PostgreSQL 17 in another, and a reverse proxy in front gives it HTTPS. Backups are in [backup.md](backup.md), upgrades in [UPGRADING.md](UPGRADING.md).

## The image

```sh
docker build -t erpcraft:latest .
```

The `Dockerfile` builds in three stages: the admin UI with Bun, the server in release mode with Rust, and a runtime image of the two on `gcr.io/distroless/cc-debian12:nonroot`. The runtime image has no shell and no package manager. It runs as the unprivileged `nonroot` user, serves the UI from `/app/ui`, listens on port 8600, and checks its own health with `erpcraft healthcheck`, which asks `GET /healthz`. A first build takes several minutes. Later builds reuse the dependencies, which BuildKit caches.

The binary does what an operator needs beside serving:

| Command | What it does |
|---|---|
| `erpcraft` | Applies pending migrations, then serves. |
| `erpcraft --migrate-only` | Applies pending migrations and exits. |
| `erpcraft --check-migrations` | Reports where the database stands and changes nothing. Exits 0 when up to date, 10 when migrations are pending, 20 when a newer erpcraft migrated the database, 30 when it records a migration under another name than the binary's for the same module and number (renumbered or replaced on another branch: its script here never ran). |
| `erpcraft verify [--company CODE] [--json]` | Checks that the books agree. Exits 1 if they do not (see [backup.md](backup.md)). |
| `erpcraft healthcheck` | Asks the running server's `/healthz`. The container's health check uses it. |
| `erpcraft --version` | Prints the version. |

## Settings

```sh
cp deploy/erpcraft.env.example deploy/erpcraft.env
chmod 600 deploy/erpcraft.env
```

Fill in `POSTGRES_PASSWORD` (`openssl rand -hex 24`), `ERPCRAFT_ADMIN_PASSWORD` and `ERPCRAFT_PUBLIC_URL`. PostgreSQL takes the database password once, when its volume is created; to change it later, change it in the database too. The administrator is created only while there are no users. Once you have signed in and changed the password, remove both `ERPCRAFT_ADMIN_*` lines.

Other settings (single sign-on, MCP origins, outgoing mail) are described in [`.env.example`](../../.env.example) and the README. Mail servers are set per company in the application.

## Starting it

```sh
docker compose -f deploy/compose.prod.yml --env-file deploy/erpcraft.env up -d
docker compose -f deploy/compose.prod.yml --env-file deploy/erpcraft.env ps
```

`deploy/compose.prod.yml` runs PostgreSQL with its data on the volume `pgdata`, and erpcraft once the database is healthy. erpcraft gets its settings from the env file and its `DATABASE_URL` from the compose file. Its root filesystem is read-only, it cannot gain privileges, and it restarts unless stopped. Its port is published on `127.0.0.1:8600` only: the proxy is what the network sees. PostgreSQL publishes no port at all.

On the first start the server creates every table, then the administrator. Sign in at `https://erp.example.com/`, set up a company from a template (Settings, or `company.setup`), and change the administrator's password.

## HTTPS with a reverse proxy

erpcraft speaks plain HTTP and expects a proxy to handle TLS. [Caddy](https://caddyserver.com) gets and renews a Let's Encrypt certificate by itself:

```sh
sudo apt install caddy                       # or see caddyserver.com/docs/install
sudo cp deploy/Caddyfile.example /etc/caddy/Caddyfile
sudo nano /etc/caddy/Caddyfile               # your name instead of erp.example.com
sudo systemctl reload caddy
```

Point the name's DNS at the server and open ports 80 and 443. Port 8600 stays closed. `deploy/Caddyfile.example` also compresses responses, limits request bodies and sets the usual security headers. With nginx or Traefik, proxy to `127.0.0.1:8600` and keep the `Host` header. Whatever the proxy, set `ERPCRAFT_PUBLIC_URL` to the address users type: single sign-on builds its redirect address from it.

## Watching it

- `docker compose ... logs -f erpcraft` shows the server's log; raise `RUST_LOG` to `debug` when looking for a fault.
- `docker compose ... ps` shows whether the container is `healthy`.
- Back up every night and run `erpcraft verify` on a restore now and then: see [backup.md](backup.md).

## Trying the image

`deploy/smoke-test.sh` does what an installation does, in a project of its own:

1. Builds the image and starts it with `compose.prod.yml` on a fresh database.
2. Waits for it to be healthy and checks that the UI is served and that the image runs as `nonroot`.
3. Signs in and sets a company up from a template.
4. Checks the migrations and the books.
5. Backs up, restores, and checks the company is still there.
6. Removes everything it made.

Run it before a release, or in CI:

```sh
deploy/smoke-test.sh                                 # builds erpcraft:smoke
SKIP_BUILD=1 ERPCRAFT_IMAGE=erpcraft:latest deploy/smoke-test.sh
```
