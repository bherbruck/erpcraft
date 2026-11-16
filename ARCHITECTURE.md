# erpcraft architecture

erpcraft is an open-source ERP written in Rust, meant to be the layer other ERPs are built on. It ships a reference ERP in the same repository, and it is MCP-native: an AI agent can do what a user can do, through the same commands and with the same permissions.

Sibling project: SolidCraft (parametric CAD, same maker, same MCP-native approach). Domain: `erpcraft.io`.

Status: the first slice works end to end. A journal entry can be drafted, posted and reversed over HTTP and over MCP. Sections below say what is **built** and what is **planned**.

## Decisions

| Topic | Decision |
|---|---|
| Language | Rust |
| Database | PostgreSQL only. Its transactions, row locks and triggers carry the invariants. |
| Scope | Kernel plus a full reference ERP, in one repository |
| Extension model | Rust crates implementing the `Module` trait, registered at compile time |
| Interfaces | HTTP and MCP, both generated from one registry |
| Tenancy | One database per tenant; many companies inside it |
| License | `MIT OR Apache-2.0` in `Cargo.toml`, as SolidCraft. License files are not added yet; confirm before the first public push. |

## Crates

```
crates/
  erpcraft-core     errors, identifiers, money rounding
  erpcraft-db       Postgres pool, module-scoped migrations
  erpcraft-rules    rule expressions: check, evaluate, compile to SQL
  erpcraft-kernel   registry, commands, documents, postings, lists,
                    user directory, authorization, audit, outbox
  erpcraft-auth     identity: passwords, tokens, sign-in routes
  erpcraft-gl       general ledger: accounts, periods, entries
  erpcraft-api      HTTP
  erpcraft-mcp      MCP server
modules/
  accounting        journal entries
apps/
  server            the `erpcraft` binary
```

Planned: `erpcraft-stock` (stock ledger, its own crate because valuation and costing are a subsystem of their own), single sign-on inside `erpcraft-auth` (OIDC, SAML, SCIM), then the modules inventory, purchasing, sales, manufacturing, and the admin UI in `ui/`.

Dependencies point one way: `core` ← `db`, `rules` ← `kernel` ← `gl` ← modules. The kernel knows nothing about accounting, and nothing about passwords or tokens.

## The model

### Commands

Every business operation, read or write, is a registered **command**: a name such as `journal_entry.post`, a typed input with a JSON Schema, a permission, and a handler. HTTP, MCP and the future UI all call the same executor, so no interface can go around a business rule.

The executor gives every command:

- one database transaction;
- an authorization check before the handler runs;
- an idempotency key (optional): the same key and input return the first result, the same key with different input is refused;
- a preview mode: run, return the outcome, roll back. A preview reserves nothing; the real run validates again;
- an audit record of who ran what with which input, and which records it touched. A command that fails or is refused is recorded too, with the reason;
- an outbox event, written in the same transaction, for delivery after commit.

Queries are commands marked read-only. A module publishes its reads (`gl.trial_balance`) the same way as its writes, so other modules and reporting tools do not need to know its tables.

Entity metadata describes the data. Commands define what the system may do.

### Lists

Every list of entity records or documents takes the same parameters and returns one page as `items` plus the size of the whole list as `total`:

- `q`: text to find in the fields the entity declares as its search fields (a code, a name);
- `where`: a condition in the rule language, for example `record.debit > 100 && record.account_type == 'asset'`;
- `sort`: field names, `-` for descending;
- `limit` and `offset`.

All of it runs in the database, in the same query as the caller's row permissions. So a total never counts a record the caller may not see, and a field withheld from the caller cannot be searched, sorted by, or named in a condition.

### Audit and history

The audit log is written only by the command executor and cannot be changed, even with direct database access. Each entry names the records the command touched, so every record has a history (`audit.history`): who ran which command on it and when, failed attempts included. Whoever may read a record may read its history; the inputs of the commands are shown only with the permission `audit:read`, because an input can contain fields the reader is not allowed to see.

### Outbox

A module can register **subscribers** that react to committed changes: an integration, a notification, a projection. A worker in the server hands each event to every subscriber that wants it.

- Delivery is at least once. A subscriber that succeeded is not called again when another one fails and the event is retried, except if the server stops at the wrong moment; handlers must tolerate a repeat.
- A failed delivery is retried after 10 seconds, then with a doubling delay up to an hour, without limit. `outbox.list` shows failing events and their last error.
- There is no ordering guarantee: a failing event does not hold back the ones behind it.
- Several servers can run the worker at once; each event is taken by one of them.

Delivered events are deleted after 30 days and idempotency keys after 24 hours. The audit log and the ledgers are never cleaned.

### Documents and postings

A **document** is a business record with a workflow that its module defines: states, which states lock the content, and the allowed transitions. A journal entry goes draft → posted → reversed. A purchase order will have more states and stay open across several receipts.

A **posting** is the immutable record that a document changed a ledger. A document can post many times. A posting has no status and is never edited; it is undone by a second posting that reverses it, and it can be reversed at most once.

Ledger entries hang off a posting. Each entry records the document line that caused it and the command that created its posting. Three dates are kept apart: when the event happened (`doc_date`), its accounting date (`posting_date`), and when the system recorded it (`created_at`).

### Ledgers own their rules

The kernel coordinates transactions, permissions, audit and reversal. Each ledger enforces its own invariants. The general ledger's are database triggers, so they hold for every module and for raw SQL:

- a posting's entries balance in the company's accounting currency, checked at commit;
- the posting date falls in an open fiscal period;
- the account belongs to the company, is active, and is not a group account;
- amounts fit the currency's decimals, and the accounting amount equals the transaction amount times the captured rate;
- entries and postings cannot be updated, deleted or truncated.

### Company and currency

Every document, posting and entry belongs to exactly one company. Each company has one accounting currency. An entry stores the transaction currency and amount, the exchange rate captured at posting, and the resulting accounting amount. Amounts are exact decimals, sent as strings in JSON, rounded half away from zero to the currency's minor units; Rust and Postgres use the same rule.

### Concurrency guarantees

| Situation | Guarantee | How |
|---|---|---|
| Two requests post the same draft | One posts; the other gets the same result | Row lock on the document |
| An idempotency key is reused with other input | Refused | Stored input hash |
| A period closes while a journal posts | They wait for each other; a closed period never receives an entry | Share lock taken by each entry, exclusive lock taken by the close |
| Two users edit the same draft | The stale edit is refused | Version number on the document |
| Two users edit the same plain record | The stale edit is refused, if the caller sends the `_version` it read | The row's `xmin`, which Postgres changes on every update |
| Two servers deliver the outbox | Each event is taken by one | Row lock, skipping locked rows |
| A number series under load | No gaps, no duplicates | Counter row locked until commit |
| A change triggers an integration | The event exists if and only if the change committed | Outbox row in the same transaction |

Each row has a test in `apps/server/tests/`.

### Identity and authorization

The two are separate. The kernel (`authz/`) holds the directory of users and decides what an actor may do. `erpcraft-auth` answers only "who is this?": it is an ordinary module that supplies the kernel's `Authenticator`, so an installation can replace it with its own.

Built:

- Users sign in with a password and receive an opaque token, stored hashed.
- A token can carry **scopes** that cap it below its user's rights. This is how an agent gets less than the person it works for.
- A **role** is a list of permission rules. A role is granted to a user for one company or for all companies. A user's rights are the sum of their grants.
- A rule allows or denies one `resource:action`, optionally only for records matching a condition, optionally only for some fields:

  ```json
  "account:read"
  { "allow": "stock_entry:read", "where": "record.warehouse in user.warehouses" }
  { "allow": "item:update", "fields": ["description", "bin"] }
  { "deny":  "item:read", "fields": ["cost"] }
  { "deny":  "journal_entry:post", "where": "record.created_by == user.id" }
  ```

- **Deny wins.** A record is denied if any deny rule applies, otherwise allowed if any allow rule applies, otherwise denied.
- **Row rules** become part of the database query, so lists, counts and paging only ever include what the caller may see. Records out of reach read as not found. Every write is checked too: a create against the new record, an update against the record before and after.
- **Field rules** remove withheld fields from every record that leaves and refuse writes to them. A withheld field cannot be used as a filter.
- **User attributes** are free-form facts on the user (`{"warehouses": ["W1"]}`) that rules read as `user.<name>`, so one role serves every warehouse.
- **Nothing undecidable helps the caller.** A rule that cannot be evaluated never allows and always denies. An action checked without a record, such as a total over a ledger, treats conditional rules the same way.
- Rules are checked when a role is saved: the syntax, the fields they name, and that they can be compiled to SQL.
- `/api/meta` describes, under `access`, what the caller could do: per entity whether they can read, create and update, and which fields are limited (`all`, `some`, `none`); per document type the actions open to them; and the commands they could run. An interface uses it to decide what to show. It is a courtesy: every command still checks for itself.

Limits of what is built:

- An administrator is not subject to rules.
- Rules on a document see its header (company, status, dates, creator), not its lines.
- Field rules apply to entities, not to documents.
- Rules are parsed on every request; nothing is cached yet.

Planned:

- Postgres row-level security as a second wall, which requires the server to connect as a non-owner role.
- Single sign-on.

### Rules

A rule condition is one expression over `record` and `user`. The syntax is CEL. `erpcraft-rules` is the only crate that knows the engine; the kernel sees a `Rule` that can be checked, evaluated against a record, and compiled to a SQL condition with bound parameters. Only the part of CEL that maps to SQL is accepted: comparisons, `in`, `&&`, `||`, `!`, literals, record fields and user attributes. The same crate serves the `where` parameter of lists, and is meant to serve validations and workflow conditions later.

## Contracts for later

Recorded now so that later work does not break them.

**The module boundary.** Modules in this repository use only the public module API, as an outside module would. If the reference ERP needs something the API lacks, the API grows. Compiled modules are trusted server code: the module API is an architectural boundary, not a security sandbox.

**Module upgrades.** A module's name owns its migrations and must never change. Today the registry checks only that a module's dependencies are registered before it. Still to define: version compatibility between a module and the kernel API, and disabling a module while its historical documents stay readable.

**Configuration is not code.** New behavior is a Rust crate. A custom field, an approval threshold, a numbering series or a print template must not need a rebuild: these will live in the database and extend the registry at startup.

**Cross-module flows are the real test.** After this slice, the next proof is one flow that crosses modules: purchase order → partial receipt → vendor invoice → return.

**Agents at scale.** A full ERP will have hundreds of commands. `meta.describe` already takes a module name so an agent can load one module at a time, and the MCP tool list already leaves out commands the caller can never run. A fuller discovery tool comes when the count requires it.

**Rules and scripts are separate decisions.** Rules use CEL, behind `erpcraft-rules`, so the engine can be replaced; stored rules would then need converting. The script engine is not chosen.

- A *rule* is one expression: a row permission, a list filter, a validation, an approval threshold, a computed value. Rules that filter lists must compile to a SQL `WHERE` clause, so the engine must expose its syntax tree and must not allow loops.
- A *script* is custom logic that runs without a Rust rebuild. It must be sandboxed.
- Whatever is chosen goes behind its own crate, so the kernel never depends on an engine directly.

Four engines were compared hands-on in November 2026 (CEL, Rhai, Cedar, Monty). The results and the test code are in [docs/research/2026-11-rule-engines](docs/research/2026-11-rule-engines/README.md).

## Known gaps in what is built

- Outbox subscribers run inside the server. Delivery to the outside (webhooks, a message broker) is not built; it would be a subscriber.
- A refused stale edit does not say what the other person changed.
- A caller that sends no `_version` when updating or deleting a plain record is not checked for staleness.
- A plain record can be deleted only while nothing refers to it. There is no way yet to retire a record that is in use, such as an account that should take no new entries, except where its entity has a field for it.
- Documents cannot yet refer to the documents they came from. This comes with the purchasing flow, which is the first to need it.
- Exchange rates are supplied by the caller. There are no rate tables, no revaluation, and a rounding difference needs an explicit line.
- Intercompany postings are not modeled.
- The MCP endpoint supports tools over stateless HTTP only.

## Milestones

1. Done: the headless ledger slice. Kernel, general ledger, journal entries, HTTP and MCP.
   Also done: identity in its own crate, row and field permission rules, list search and filters, record history, outbox delivery, per-user access in the description.
2. Admin UI generated from the registry.
3. Stock ledger and the purchasing flow across modules.
4. Single sign-on. (Row and field permission rules are done.)
5. Remaining reference modules: sales, manufacturing.
