# erpcraft

An open-source ERP in Rust, built to be the base other ERPs are built on. MCP-native: every business command is also a tool an AI agent can call, with the same permissions as a person.

Early. What works today: drafting, posting and reversing journal entries against a general ledger, over HTTP and MCP. See [ARCHITECTURE.md](ARCHITECTURE.md).

## Run it

Needs Rust 1.85 or newer and Docker.

```sh
docker compose up -d          # Postgres on 127.0.0.1:54320
cp .env.example .env          # then change the admin password
set -a; . ./.env; set +a
cargo run -p erpcraft-server
```

The server applies its migrations, creates the first administrator from `ERPCRAFT_ADMIN_EMAIL` and `ERPCRAFT_ADMIN_PASSWORD` if no user exists, and listens on `127.0.0.1:8600`.

```sh
curl -s localhost:8600/api/auth/login \
  -d '{"email":"admin@example.com","password":"..."}'
# use the returned token as: Authorization: Bearer <token>
curl -s localhost:8600/api/meta -H "Authorization: Bearer $TOKEN"
```

- `GET /api/meta` describes every entity, document type and command; each command with its label, what it acts on, the document states it applies from, and the schemas of its input and output.
- `POST /api/commands/{name}` runs any command. Add `?preview=true` to see the outcome without saving, and an `Idempotency-Key` header to make a retry safe.
- `/api/entities/{entity}` and `/api/documents/{doc_type}` are REST shortcuts.
- A document comes with its postings and the ledger entries under each; the preview of a posting shows the entries it would write.
- An error is `{"error": {"code", "message"}}`, plus `field`, `collection` and `line` when it is about one place, and on a conflict `reason`: `stale`, `state`, `duplicate` or `in_use`.
- Lists take `q` (text search), `where` (a condition such as `record.debit > 100`), `sort`, `limit` and `offset`, and return `items` and `total`.
- `audit.history` gives the history of one record; `GET /api/meta` includes what the caller is allowed to do.
- `POST /mcp` is the MCP endpoint, with the same bearer token.

## Running it in production

A Docker image with the UI built in, a compose file with PostgreSQL, HTTPS through Caddy, backups, restores, a check that the books agree, and upgrades:

- [docs/ops/self-hosting.md](docs/ops/self-hosting.md): the image, settings, starting it, TLS by reverse proxy, the smoke test.
- [docs/ops/backup.md](docs/ops/backup.md): `deploy/backup.sh` with retention, `deploy/restore.sh`, and `erpcraft verify`.
- [docs/ops/UPGRADING.md](docs/ops/UPGRADING.md): `--check-migrations`, `--migrate-only`, and why a downgrade is refused.
- [CHANGELOG.md](CHANGELOG.md): what changed, by area.

## Admin UI

The UI in `ui/` builds to static files, and the server serves them from the directory named by `ERPCRAFT_UI_DIR`. There is no separate UI server.

```sh
(cd ui && bun install && bun run build)   # writes ui/dist
ERPCRAFT_UI_DIR=ui/dist cargo run -p erpcraft-server
```

Then open `http://127.0.0.1:8600/`. Without the variable, `/` is a JSON index of the API.

## Single sign-on

Users can sign in through any OpenID Connect provider (Microsoft Entra ID, Google, Okta, Keycloak, Authentik). Set `ERPCRAFT_PUBLIC_URL` to the address users reach the server at, then add a provider:

```sh
curl -s localhost:8600/api/commands/sso_provider.create -H "Authorization: Bearer $TOKEN" -d '{
  "name": "Google", "slug": "google", "issuer": "https://accounts.google.com",
  "client_id": "...", "client_secret": "...", "allowed_domains": ["example.com"]}'
```

The answer includes the `redirect_uri` to register with the provider (`<public URL>/api/auth/sso/google/callback`). `allowed_domains` is required; `["*"]` says any domain, but such a provider signs in only users bound to it with `sso_identity.set`. People who are users already can then sign in, and their first sign-in binds them to that provider's identity for good (change it with `sso_identity.set` or `.delete`); `"auto_create_users": true` makes a first sign-in create the user. Administrators sign in only through a provider created with `"trusted_for_admins": true`. Managing providers takes `sso_provider:manage` for all companies, which amounts to being able to sign in as any user who is not an administrator: grant it as you would administrator rights. Changing a provider's `issuer` or `client_id` discards its secret and switches it off until `sso_provider.set_secret` and `"active": true`. `auth.events` lists sign-ins, refused sign-ins and every change to providers and bindings. See "Single sign-on" in [ARCHITECTURE.md](ARCHITECTURE.md).

## Setting up a company

One command creates a company ready to work, from a country template:

```sh
curl -s localhost:8600/api/commands/setup.templates -H "Authorization: Bearer $TOKEN" -d '{}'
curl -s localhost:8600/api/commands/company.setup -H "Authorization: Bearer $TOKEN" -d '{
  "template": "de_skr03", "code": "MUSTER", "name": "Muster GmbH", "first_year": 2026}'
```

It creates the chart of accounts, fiscal years, a warehouse, purchasing, sales and tax settings, tax codes and account rules, all or nothing. Built in: `us_small_business` (USD), `ca_small_business` (CAD, GST and HST at the CRA's current rates) and `de_skr03` (EUR, DATEV SKR 03 numbering, VAT 19 % and 7 %). A country module adds its own with `erpcraft_setup::register`. See "Setting up a company" in [ARCHITECTURE.md](ARCHITECTURE.md).

## Bank statements

Import a statement into a draft, see what each line probably is, match it, and reconcile:

```sh
curl -s localhost:8600/api/commands/bank_statement.import -H "Authorization: Bearer $TOKEN" \
  -d '{"bank_account_id": "...", "format": "camt053", "content": "<Document>..."}'
```

Formats: camt.053, OFX, and CSV with a `bank_csv_mapping` saved per bank account. A camt.053 or OFX file names its account, which must be the bank account's IBAN or number recorded as a `bank_account_number`, and its currency. Amounts are read strictly: camt.053 writes a point and no thousands separator; OFX is meant to use a point too, and a single comma with 1 to 4 digits after it is read as a decimal comma only where there is no point (anything else is refused, not guessed). `bank_statement.proposals` proposes a recorded payment, open items to pay, or an account; `bank_statement.match` accepts one per line; `bank_statement.post` reconciles and locks the statement; `bank.reconciliation` explains the difference between the ledger and the bank to the cent. See "Bank statements and reconciliation" in [ARCHITECTURE.md](ARCHITECTURE.md).

## Printing

`document.render` turns any document into an HTML page to print; the browser prints it, or saves it as a PDF (the server makes none):

```sh
curl -s localhost:8600/api/commands/document.render -H "Authorization: Bearer $TOKEN" \
  -d '{"doc_type": "sales_invoice", "id": "..."}'
```

Purchase orders, sales orders, deliveries and invoices have built-in templates; every other document prints through a generic one. To change how a document looks, save a `print_template` (`POST /api/entities/print_template`) with MiniJinja HTML; `print_template.builtin` gives a built-in's source to start from. See "Printing" in [ARCHITECTURE.md](ARCHITECTURE.md) for what a template sees and what it may not contain.

## Test

```sh
docker compose up -d
cargo test --workspace
```

Each integration test creates and drops its own database.

## License

Apache License 2.0. See [LICENSE](LICENSE).
