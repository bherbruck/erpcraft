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

- `GET /api/meta` describes every entity, document type and command.
- `POST /api/commands/{name}` runs any command. Add `?preview=true` to see the outcome without saving, and an `Idempotency-Key` header to make a retry safe.
- `/api/entities/{entity}` and `/api/documents/{doc_type}` are REST shortcuts.
- `POST /mcp` is the MCP endpoint, with the same bearer token.

## Test

```sh
docker compose up -d
cargo test --workspace
```

Each integration test creates and drops its own database.
