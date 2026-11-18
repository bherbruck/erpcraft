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
| Target | Small and medium companies, the range of SAP Business One or a minimal Dynamics 365: a manufacturer in Ohio or in Germany. United States, Canada and the European Union first. |
| Localization | Country-specific rules (tax, reporting, formats) are modules, meant to be opened to the community. Nothing country-specific goes into the kernel or the ledgers. |
| Structure | High cohesion, low coupling. A module owns its tables, documents and rules, and reaches others only through their public API. |
| Extension model | Rust crates implementing the `Module` trait, registered at compile time |
| Interfaces | HTTP and MCP, both generated from one registry |
| Tenancy | One database per tenant; many companies inside it |
| License | `MIT OR Apache-2.0` in `Cargo.toml`, as SolidCraft. License files are not added yet; confirm before the first public push. |

## Crates

```
crates/
  erpcraft-core     errors, identifiers, money rounding
  erpcraft-db       Postgres pool, module-scoped migrations
  erpcraft-rules    rule expressions: check, evaluate, compile to SQL;
                    wider expressions for policies, evaluated in memory
  erpcraft-kernel   registry, commands, documents, postings, lists,
                    user directory, authorization, audit, outbox
  erpcraft-auth     identity: passwords, tokens, sign-in routes
  erpcraft-gl       general ledger: accounts, periods, entries
  erpcraft-stock    stock ledger: items, warehouses, quantity and value on hand
  erpcraft-openitems  what is owed to vendors and by customers, item by item
  erpcraft-tax      tax codes, the accounts they post to, tax recorded per posting
  erpcraft-party    addresses, tax identifiers, electronic addresses and contacts of the company, customers and vendors
  erpcraft-api      HTTP
  erpcraft-mcp      MCP server
modules/
  accounting        journal entries
  inventory         stock adjustments, transfers between warehouses
  purchasing        purchase orders, receipts, vendor invoices
  sales             sales orders, deliveries, customer invoices
  payments          paying vendors, being paid by customers
  webhooks          sends committed changes to HTTP endpoints outside
  manufacturing     bills of materials, production orders
  printing          documents as printable HTML, from templates kept in the database
  setup             a new company in one command, from a country template
  banking           bank statements: import, matching, reconciliation
  assets            fixed assets: register, depreciation runs, disposals
  einvoice          electronic invoices after EN 16931: XRechnung (UBL, CII), ZUGFeRD / Factur-X XML
apps/
  server            the `erpcraft` binary
```

Planned: SAML and SCIM inside `erpcraft-auth` (single sign-on with OpenID Connect is built), then tax and localization modules per country, and the admin UI in `ui/`.

Dependencies point one way: `core` ← `db`, `rules` ← `kernel` ← `gl`, `stock` ← modules. The kernel knows nothing about accounting, and nothing about passwords or tokens. The two ledgers know nothing about each other: a module that moves stock posts its value to the general ledger itself, in the same posting.

## The model

### Commands

Every business operation, read or write, is a registered **command**: a name such as `journal_entry.post`, a typed input with a JSON Schema, a permission, and a handler. HTTP, MCP and the future UI all call the same executor, so no interface can go around a business rule.

The executor gives every command:

- one database transaction;
- an authorization check before the handler runs;
- an idempotency key (optional): the same key and input return the first result, the same key with different input is refused;
- a preview mode: run, return the outcome, roll back. A preview reserves nothing; the real run validates again;
- an audit record of who ran what with which input, and which records it touched. A command that fails or is refused is recorded too, with the reason. For a command marked sensitive (it handles secrets) neither the input nor anything made from it is recorded: a failure keeps only the error code and a fixed sentence for that code ("the input was refused"), because any error message may quote what was sent, such as a secret pasted into the wrong field. The caller still gets the full message;
- an outbox event, written in the same transaction, for delivery after commit.

Queries are commands marked read-only. A module publishes its reads (`gl.trial_balance`) the same way as its writes, so other modules and reporting tools do not need to know its tables.

Entity metadata describes the data. Commands define what the system may do.

A command describes itself, so that an interface need not derive anything from its name or try it to see whether it applies. Besides its input schema, `meta.describe` gives for each command: a `label` for a button; the `resource` it is about and whether that is an entity or a document type (`resource_kind`); whether it acts on one record named by the input `id` (`target`); for a command on a document, or on an entity that declares a `state_field` (the fiscal period's `status`), the states it applies from (`from_states`), checked against the document type's states or the field's values when the server starts; whether it reverses, cancels or deletes (`destructive`); whether it is a `report` to show as a page, with a title; and an `output_schema`, the JSON Schema of what it returns. Every property of an input, and every property anywhere in a report's output, has a `title` (from the field name unless declared), so a form and a table can label it. Decimals are strings in both schemas. A module states these with the builder methods of `CommandSpec`; all but `from_states`, `destructive` and `report` have a default taken from the name and the input.

### Errors

Every interface sends the same error object: `code` (`validation`, `not_found`, `conflict`, `forbidden`, `unauthorized`, `internal`) and `message`, a complete sentence. Where it is known, the object also says where the error applies, so that an interface can mark the place instead of reading the sentence:

- `field`: the field the error is about;
- `collection` and `line`: the collection of a document body and the line in it, counted from 1; `field` is then a field of that line;
- `reason`, on a `conflict` only: `stale` (the record changed after it was read: read it again), `state` (the record or document is in a state that does not allow this), `duplicate` (a unique value is taken), `in_use` (something still refers to the record). A conflict that is none of these has no reason.

Input that cannot be read is reported the same way, in words: which field, on which line, what was expected and what kind of value came instead. The value itself is never repeated, for any command: the caller knows what they sent, and the sentence goes on into the audit log. Over HTTP the object is the `error` of the response body; over MCP it is the `error` of the tool result's structured content, and the result's text is the same JSON. Modules raise such errors with `Error::field`, `Error::line`, `Error::stale`, `Error::state`, `Error::duplicate` and `Error::in_use`, and read a document body with `erpcraft_kernel::input::parse`.

### The registry describes, the interface renders

Whatever an interface would otherwise guess is declared in the registry and comes with `meta.describe`. A module states each with one builder call, and most have a default.

- **How a state is shown.** Each state of a document type has a `tone` (`neutral`, `info`, `success`, `warning`, `danger`): the meaning of a colour, not a colour. Unless a module says otherwise with `StateMeta::tone`, an editable state is neutral, a locked one success, `open` and `released` info, `cancelled` neutral (it never happened) and `reversed` danger (it happened and was taken back).
- **Copies of linked fields.** A field can declare that it copies a field of the record a link beside it refers to (`FieldMeta::shown_from`, such as a summary's `vendor_name` from `vendor_id`). A reader who may not read that record, or that field of it, gets it blank in every document read and list, and may not filter or sort a list by it unless they read every such record and field.
- **What a ledger's entries hold.** Each ledger declares the schema of the rows it shows under a posting (`Ledger::entry_schema`, with a title on every property), published in `meta.describe` as `ledgers: [{name, label, entry_schema}]` and put into the output schema of every command that returns a document, under `postings[].entries.<ledger>`.
- **Fields written together.** An entity can name fields that are checked against each other (`written_together`: a policy rule's `point` and `condition`, an account rule's `scope_type` and `scope_id`); an update that sends some of a group and not the rest is refused, naming the missing field.
- **What a field refers to.** Besides a link to an entity, a field can refer to a document of one type, optionally only in given states (`FieldType::document_in`); to a line of the document another field holds (`FieldType::line_of`); to a record of the entity another field names (`FieldType::entity_named_by`); to a row that a query command lists, such as an open item (`FieldType::Lookup`, with the inputs that narrow the list, and `by_keys`, the input that lists given rows whatever their state, so that a value already chosen can be shown: `open_item.list` takes `ids`, and a posted payment shows the items it settled through it); or to a posting. The kernel knows nothing about open items: the lookup names the command. A link to an entity can also name fixed `filters` that say which records to offer, in the form `entity.list` takes them: a payment's `bank_account_id` offers accounts with `is_bank`, which setup templates set for bank and cash accounts and an upgrade set for the accounts posted payments used. The registry refuses a reference that leads nowhere, and a test refuses a document body field named `*_id` that is a bare id.
- **What a decimal is.** `.money()`, `.unit_price()`, `.quantity()`, `.rate(n)` or `.percent(n)` give a decimal field its role and its number of decimal places, and say where its currency (the company's, or a field beside it) or its unit (a field of a linked record, such as the item's) is. For an entity the decimal places are read from the table.
- **What a field starts as and offers.** The default of an entity's field is read from the table; a choice has a label for every value (`FieldType::options` makes them from the values, `FieldType::labelled` states them); `.help(...)` adds one line of guidance.
- **How a record is shown and retired.** An entity names the fields that tell a person which record it is (`display`; its search fields unless declared), its plural, the boolean field that says a record is no longer used (`active_field`; a field named `active` unless declared), and the record it belongs to and is edited inside, if any (`child_of`). `entity.list` leaves retired records out when asked with `active_only`, which is what a lookup wants. Document types have a plural too, and modules a label.
- **What every document has.** The header fields (`number`, `status`, `doc_date`, `memo`, who created it and when) are described once, under `document_header`, like the fields of a body.

### Lists

Every list of entity records or documents takes the same parameters and returns one page as `items` plus the size of the whole list as `total`:

- `q`: text to find in the fields the entity declares as its search fields (a code, a name);
- `where`: a condition in the rule language, for example `record.debit > 100 && record.account_type == 'asset'`;
- `sort`: field names, `-` for descending;
- `limit` and `offset`.

All of it runs in the database, in the same query as the caller's row permissions. So a total never counts a record the caller may not see, and a field withheld from the caller cannot be searched, sorted by, or named in a condition.

### The MCP endpoint

`POST /mcp` speaks the Model Context Protocol, revision 2026-07-28, in which nothing is kept between requests: every request says which revision it speaks, and `server/discover` says what the server offers. Clients of the four revisions before it, which open with `initialize`, are answered too, each with what its revision knows. The bearer token is the one of the HTTP API.

- **Tools.** Every command is a tool, described from what the command declares: a title, its input schema, an output schema that its structured result conforms to, and the hints of the specification (read-only for a query, destructive for a command that reverses, cancels or deletes, idempotent for one that moves a record out of a state). A tool that writes also takes `preview` and `idempotency_key`. A tool the caller can never run is not listed. The list is paged.
- **Finding tools.** Two tools come first in the list so that an agent need not read the rest: `erpcraft_modules` says what each module holds, and `erpcraft_tools` returns the tools of one module or for one resource.
- **Resources** are reference material that is the same for every caller: the sections of this document on commands, errors, documents and policies (`erpcraft://guide/...`, served from this file, so there is one text), the policy points with their facts (`erpcraft://policies`), and what each module holds (`erpcraft://modules/{module}`). Records and documents are not resources: what a caller may read of them is decided per call, by the tools.
- **Prompts** are pieces of real work: entering a vendor invoice against an order, closing a period, explaining a refusal. Each names the tools it uses from the registry, and is not offered to a caller who could not run them.
- **Web pages.** A request with an `Origin` header is let in only from the server's own pages and from this machine, or exactly from the origins in `ERPCRAFT_MCP_ALLOWED_ORIGINS`. The first is judged by the `Host` header, which does not stop DNS rebinding; what does is that the endpoint takes a bearer token and no cookie. An installation reachable from browsers should set the list.

### Audit and history

The audit log is written only by the command executor and cannot be changed, even with direct database access. Each entry names the records the command touched, so every record has a history (`audit.history`): who ran which command on it and when, failed attempts included. Whoever may read a record may read its history; the inputs of the commands are shown only with the permission `audit:read`, because an input can contain fields the reader is not allowed to see.

### Outbox

A module can register **subscribers** that react to committed changes: an integration, a notification, a projection. A worker in the server hands each event to every subscriber that wants it.

- Delivery is at least once. A subscriber that succeeded is not called again when another one fails and the event is retried, except if the server stops at the wrong moment; handlers must tolerate a repeat.
- A failed delivery is retried after 10 seconds, then with a doubling delay up to an hour, without limit. `outbox.list` shows failing events and their last error.
- There is no ordering guarantee: a failing event does not hold back the ones behind it.
- Several servers can run the worker at once; each event is taken by one of them.

Delivered events are deleted after 30 days and idempotency keys after 24 hours. The audit log and the ledgers are never cleaned.

**Webhooks** carry events to the outside. The `webhooks` module is one such subscriber: for every event it sends a POST to each active webhook that subscribes to the event's topic (a command name, or a prefix such as `purchase_order.*`; no topics means all). The body is `{id, topic, created_at, payload}`, exactly what the outbox holds, so the event of a command that handles secrets says only which command ran and who ran it. Each request carries `X-Erpcraft-Event`, `X-Erpcraft-Delivery` (the event id, the same on every retry, so a receiver can drop repeats) and `X-Erpcraft-Signature`: `sha256=` and the hex HMAC-SHA256 of the body under the webhook's secret.

- Only a 2xx answer within 10 seconds counts. Anything else fails the event, which the outbox retries as above; redirects are not followed. The module notes per event and webhook what arrived, so a retry goes only to the webhooks that have not had the event. `webhook.deliveries` and the webhook record show the last failure.
- A webhook can be limited to one company. The company of an event is taken from its payload (`output.company_id`, else `input.company_id`); an event that names none goes only to webhooks without that limit.
- A webhook must point to a public address over https. Public is an allow-list: the global unicast space of each family (IPv4 1.0.0.0 to 223.255.255.255, IPv6 `2000::/3`) less the blocks the special-purpose registries mark as not globally reachable. An IPv6 address that carries an IPv4 address (IPv4-mapped, IPv4-compatible, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`) is judged by that address; Teredo is refused whole. This is checked when the webhook is saved and again when a request is sent, by the name lookup the HTTP client itself uses. The refusal is one sentence whether the address is private, the name leads to a private address or the name does not exist, and names no address, so that whoever may save a webhook cannot map the names of the internal network; the detail goes to the server's log. `ERPCRAFT_WEBHOOKS_ALLOW_PRIVATE=1` lifts this for development, and with it http to localhost works.
- Webhooks are managed with commands of their own (`webhook.create`, `webhook.update`, `webhook.set_secret`, `webhook.delete`), marked sensitive, not with the generic entity commands, so that neither the secret nor the URL reaches the audit log or the outbox. The secret is returned once, when it is created or generated. It is stored as given, because it must sign: whoever can read the database can read it.

### Documents and postings

A **document** is a business record with a workflow that its module defines: states, which states lock the content, and the allowed transitions. A journal entry goes draft → posted → reversed. A purchase order will have more states and stay open across several receipts.

The body of a document is accepted on write exactly as it is returned on read. Fields the system sets (a line's `line_no`, `qty_received` on an order line, the value of a receipt line) are declared read-only in the type's description and ignored on input, never trusted. A line sent back with its `id` keeps that id when the draft is saved, whatever its position, so that what refers to the line stays valid; a line without an id, or with one the document does not have, is new.

A **posting** is the immutable record that a document changed a ledger. A document can post many times. A posting has no status and is never edited; it is undone by a second posting that reverses it, and it can be reversed at most once.

A document is read with its postings, and each posting with the entries it wrote, by ledger: `postings[*].entries.gl`, `.stock`, `.open_items`, `.tax`. Each ledger supplies its own (`Ledger::entries`) and applies its own permissions: reading a document does not give the right to read a ledger, so a ledger's entries are left out for a reader who may not read them, and fields withheld on its entries are withheld here. Because a preview runs the command and reads the document before rolling back, the preview of a posting shows the entries it would write. A ledger names the tables it writes (`Ledger::entry_tables`); one statement asks which of them hold rows under each posting, and a ledger is asked for its entries only where it wrote some. The tables are what was written, so no module has to remember to mark anything; a ledger that names no tables is asked for every posting.

A command that returns a document returns all of this by default. A caller that only needs the outcome, such as an importer, asks for the header and summary alone: `?view=header` over HTTP, which leaves out `body`, `postings` and `links`, and the argument `view: "header"` over MCP, offered on every tool that returns a document, where `body` is null and `postings` and `links` are empty so that the result still fits the tool's output schema. The queries for the rest are not run. With an idempotency key, the view is part of what a retry must repeat.

The kernel loads a document's body once per change: to work out the summary (`DocumentHandler::summary_of` gets the body just loaded, and reads lines from it), and the view at the end of the command shows that same body when the document has not changed since, as known by its version. A handler that implements only `summary` still works; it reads what it needs itself.

Ledger entries hang off a posting. Each entry records the document line that caused it and the command that created its posting. Three dates are kept apart: when the event happened (`doc_date`), its accounting date (`posting_date`), and when the system recorded it (`created_at`).

A document type can declare a **summary**: the few figures that say what a document is without opening it, such as the vendor and the total of an order, or net, tax and gross of an invoice. The type's handler works them out (`DocumentHandler::summary`) and the kernel stores them with the header whenever a body is saved or the document changes state, so that lists return them, sort by them (`summary.total_net`) and filter by them without reading a body, and the document itself shows the same figures. Amounts are worked out in Rust as the posting works them out, line by line with the same rounding, so that the total shown on a draft is the total that will be posted; a figure that is not known yet, such as the cost of a delivery before the stock ledger has valued it, is null. A document saved before its type had a summary gets one from the server's background work after it starts, a batch at a time, or from `document.refresh_summaries` (granted for all companies; it takes a `doc_type` and a `limit`, at most 1000, and says how many it refreshed and how many remain). A batch skips documents another transaction holds rather than waiting for them.

A document and each of its postings carry the name of the user who created them (`created_by_name`). Whoever may read a document may see those names, as with a record's history; there is no separate right to read users.

`search.records` finds records and documents of every kind in one call, a few per entity and document type. It runs the lists' own queries, so it finds exactly what the lists would show the caller: rows out of reach are not found, and a withheld field is neither searched nor shown. Its cost is bounded: at least two characters, at most ten matches per kind, fifty in all.

### Documents that follow from each other

A document can be linked to the document it follows from: a receipt to its purchase order, an invoice to that order. The link is kept by the kernel and shown on both documents, to readers who may read the other one.

Sales orders work the same way, with deliveries and invoices. A purchase order shows the pattern for a document that stays open while others are posted against it. The order itself posts nothing. Each receipt and each invoice is its own document with its own posting, and locks the order before it does anything. That lock is what stops two receipts from together receiving more than the company accepts (by default, more than was ordered; see Policies). How far each order line has got is kept beside the order, because it changes while the order's content is locked. An order closes by itself when everything on it is received and invoiced, can be closed short by a person, and reopens if a reversal leaves it with goods received and not invoiced.

The next document can be drafted from the one it follows: `purchase_order.receive` and `purchase_order.invoice`, `sales_order.deliver` and `sales_order.invoice` create a draft with a line for everything still to come, linked to its order; `purchase_order.return` and `sales_order.return` a return of everything received or delivered and not yet returned; `vendor_invoice.credit` and `sales_invoice.credit` a credit note for what was returned and not yet credited on the invoice's lines; and `payment.draft` one that settles open items in full.

### Returns and credit notes

Goods that go back are a business event of their own, dated when it happens, not a reversal: a reversal undoes a mistake. A **purchase return** sends goods received on an order back to the vendor, a **vendor credit note** reduces what is owed for a posted invoice; a **sales return** brings goods delivered on an order back, a **sales credit note** reduces what the customer owes. Each is a document with its own posting, linked to the order or the invoice, and reversible while nothing depends on it.

- A purchase return takes the goods out of stock at the moving average, as every issue does, and takes out of goods received not invoiced (GRNI) what they were received for: the average received value of the order line. The two differ only where the item's cost moved since the receipt; the difference goes to purchase price variance, as an invoice's difference from its receipt does. Stating the issue's value was not an option, because the stock ledger values every issue itself.
- A vendor credit note puts back into GRNI what the credited goods were invoiced at (the average cleared value of the line), debits payables by what is credited, sends the difference to purchase price variance, and takes the tax back. So GRNI holds goods received and not invoiced, less goods returned and not credited, and clears when both sides are level. Whichever document makes them level (a return, a credit, an invoice or a replacement receipt) takes the value that is left, so nothing stays behind to the cent; the database checks this per order line.
- A sales return brings the goods back at the cost they left for: the average cost of what was delivered on the order line and has not come back, the last of it taking what is left. Cost of goods sold is taken back by exactly that. Goods that come back onto a level below zero are handled like any receipt: the variance goes to `stock_variance`.
- A sales credit note takes back revenue and the tax charged, and credits receivables.
- Only goods received (or delivered) on the order and not yet returned can go back: an invariant, because more would leave the order owing goods that never arrived and GRNI or cost of sales wrong. A credit note line credits a line of its invoice, never more than the invoice charged on it. The part of a line covered by goods returned and not yet credited counts on the order; any other part is a credit of price only (an allowance, a discount), which goes to purchase price variance or takes back revenue alone, and is a judgement: `vendor_credit_note.without_return` and `sales_credit_note.without_return`, refused by default.
- An order line keeps what was received (delivered) and invoiced in all, and what was returned and credited apart; what the order shows as received and invoiced is net. A return can be made against an open or a closed order. A closed order stays closed after a return unless the company wants it open again for replacements (`purchase_order.reopen_on_return`, `sales_order.reopen_on_return`); replacements received against it are counted like any receipt. A return or credit note on an order that is open does not change its state; one that leaves goods returned and not credited keeps the order from closing.
- A credit note takes tax back at the code, direction and rate the invoice recorded for that line, whatever the code says now, as negative base and amount dated in the credit note's period, so a period already reported does not change. The tax ledger refuses to credit more of an entry than it charged, in base or in tax, and the last of a line's base takes the last of its tax. The policy `tax_code.use` is not asked: a credit must be able to undo what an invoice charged at a code since retired.
- A credit note opens an item below zero (see open items) for the vendor or customer. A payment settles it with that party's invoices, netting them; `payment.apply_credit` applies it to one invoice without money moving.
- An invoice that a posted credit note credits cannot be reversed, nor a return whose goods were credited; reverse the credit note first. A credit note that a payment settled or applied cannot be reversed; reverse the payment first. They post nothing; the draft is checked, changed and posted like one entered by hand. A command that creates a document says so in its description (`creates`), so an interface goes to the new document.

### Ledgers own their rules

The kernel coordinates transactions, permissions, audit and reversal. Each ledger enforces its own invariants. The general ledger's are database triggers, so they hold for every module and for raw SQL:

- a posting's entries balance in the company's accounting currency, checked at commit;
- the posting date falls in an open fiscal period;
- a posting cannot be dated in a closed fiscal period, whichever ledgers it writes to. This one is a trigger on the kernel's `posting` table that the general ledger module puts there, because fiscal periods are the general ledger's and a closed period is closed for every ledger. A date in no period at all is let through (such a posting cannot have general ledger entries);
- the dates of a fiscal period cannot change, and the period cannot be deleted, while it is closed or once anything is posted inside it;
- the account belongs to the company, is active, and is not a group account;
- amounts fit the currency's decimals, and the accounting amount equals the transaction amount times the captured rate;
- entries and postings cannot be updated, deleted or truncated.

The general ledger also says which account a company posts to for a purpose. An **account rule** names the account for a purpose such as `inventory`, either as the company's default or for one record of another module: an item, a warehouse. A module that posts a value asks `account_for` with what the posting is about, most specific first, and gets the account of the first rule that applies. So every module that moves stock (receipts, deliveries, adjustments, transfers, production) lands on the same account without knowing the others, and the inventory accounts together are worth what the stock ledger says is on hand. The ledger does not know what an item or a warehouse is; it only matches the scope it is given.

The stock ledger's, also triggers:

- stock on hand never goes below zero, per item and warehouse, unless the company's policy allows it (see Policies, `stock.negative`). The ledger then lets an issue through only if its entry names an active policy rule of that point that allows, for that company, and carries the facts the rule was asked with; the ledger checks those facts against the level it has locked (on hand, issuing, short by, item and warehouse) and copies the rule's condition onto the entry. The rule, the facts and the condition stay on the entry, so an auditor can ask the condition again. Goods that are not on hand leave at the last cost known for the item in that warehouse, an estimate, and the level carries that estimate as a negative value. When goods arrive on a level below zero they first cover what is missing; the difference between their real cost and the estimate is a variance, which is not stock value, and the rest comes into stock at its real cost;
- valuation is moving average: a receipt comes in at the cost the caller states, an issue goes out at the average cost of what is on hand. The ledger works out the value of an issue; the caller cannot state it;
- the last of the stock takes the last of the value with it, so rounding never leaves value on an empty shelf;
- a reversal takes back exactly the quantity, value and variance of the original entry, and is refused if the stock has since moved on. Taking back a receipt that arrived on a level below zero may leave the level as far below zero as it stood before that receipt, not further;
- quantity and value of a level always point the same way: never a positive quantity with a negative value or the reverse, and an empty shelf is worth nothing;
- the level on hand changes only through entries.

Open items are the third ledger: the detail behind the payables and receivables accounts. An invoice opens an item under its posting; a payment settles items under its own. Its rules, also triggers:

- an item is never settled for more than its amount;
- an item is a debt, above zero, or a credit, below zero: a credit note, or money paid on account. A settlement runs the way of its item, so what is settled of a credit is below zero too;
- reversing the posting that opened an item calls the item off, which is refused once any of it was paid;
- reversing a payment takes back exactly its settlements;
- items and settlements are never edited.

Because purchasing and sales only open items and payments only settle them, the three modules do not know each other. A payment can settle an item whatever module opened it.

A payment settles the debts and credits of one party together: an allocation to a credit is below zero, and the payment comes to the debts less the credits. A payment that comes to nothing needs no bank account and moves no money; `payment.apply_credit` posts one to apply a credit to a debt. A payment can also carry money beyond its allocations (`on_account`, with the party and the payables or receivables account when nothing is allocated): it opens a credit of the party, applied later like a credit note. A payment that comes to less than zero is money coming back, a refund.

Tax is the fourth: a record of the tax on each posting, which a tax return is built from. A tax code is a rate; a company names the account for tax it collects and the account for tax vendors charge it (an asset where it can be reclaimed, as with VAT; an expense where it cannot, as with US sales tax). An invoice asks what the tax on a line is, posts it to the general ledger, and records it. Tax is worked out and rounded per line. A reversal is recorded in the period it is posted in, so a period already reported does not change. Which code applies where, rates over time and the form of a return are left to country modules.

**Tax rules.** A sales or vendor invoice line that names no tax code starts with what the company's tax rules (`tax_rule`, per company) propose, when the draft is saved: the rule for the invoice's customer or vendor (an exempt buyer, a buyer under the reverse charge), else the rule for the line's item, else the charge's own code, else the company's default (the rule without a scope). A code the line gives is kept, and a line can be changed to any code before posting. The rules are matched like account rules: the tax module does not know what a customer or an item is.

**Tax the buyer accounts for.** A code marked `self_assessed` is tax the buyer owes rather than the seller: the EU reverse charge, an intra-Community acquisition, US use tax. On a purchase the vendor charges none and is owed the net amount. The company owes the tax and, where it may, reclaims it: `erpcraft_tax::quote` gives two taxes per component, `paid` on the paid account (an asset where it is reclaimed, an expense for use tax that is not) and `collected` on the collected account, of the same amount. Both are posted (`Tax::debit_credit`) and recorded as entries marked `self_assessed`, so the tax report shows the tax due and the tax reclaimed and still equals the tax accounts. `Tax::total`, what is owed with an invoice, leaves them out, and so does `posted_value`. A credit note takes both back from the accounts they were posted to, and an early-payment discount takes neither back. On a sale the seller charges none: the entry records the base with no tax, and the e-invoice writes the line as category AE (reverse charge) at 0 % with the reason VATEX-EU-AE, unless the code's e-invoice category says otherwise.

A line can carry more than one tax. A **tax group** is a tax code made of component codes (`tax_code_component`, in order): GST plus British Columbia PST, or a US state plus a county. Each component is a plain code with its own rate and, per company, its own collected and paid accounts, so each authority's tax is owed on its own account. A code without components is a group of one, exactly as before. Tax is worked out and rounded per component per line; the line's tax is their sum, and a draft's summary adds them the same way, so what it shows is what is posted. A component marked `compound` is charged on the net amount plus the taxes of the components before it (as Quebec QST was until 2012, and some US and Canadian taxes still are); otherwise on the net amount alone. Each component is one tax entry, which names the group (`group_code_id`), so `tax.report` shows each authority's code on its own row and still equals the tax accounts. A credit note takes back each component's entry, a compounding one on what is credited plus the taxes taken back before it, and the last of an entry's base takes the last of its tax. Groups are one level deep, and a group has no accounts of its own; its rate is the sum of its parts, for people to read, and is not charged. The policy `tax_code.use` is asked for the group and for each component, so an inactive component refuses the line by default.

Whether a tax paid can be claimed back is said by its paid account, not by a flag: an asset where it is reclaimed (VAT, GST, Quebec QST as an input tax refund), an expense where it is not (British Columbia, Saskatchewan and Manitoba PST, US sales tax).

Decisions in the stock ledger that are choices, not necessities, and can be revisited:

- Moving average, not FIFO or standard cost. It needs no cost layers and suits interchangeable goods.
- Stock below zero is an estimate until the goods arrive. An issue of 10 with 4 on hand worth 12.00 and a last cost of 3.00 leaves at 12.00 + 6 × 3.00 = 30.00, and the level stands at -6, worth -18.00. A receipt of 10 at 3.50 then covers the 6 with goods that cost 21.00: 3.00 of variance. The other 4 come in at 14.00, so the level is 4 worth 14.00, and the receipt's 35.00 is 32.00 of stock value plus 3.00 of variance. The module that brought the goods in posts the variance to the account for the purpose `stock_variance`, which is asked for only when there is one. The last cost is the unit cost of the most recent receipt into that warehouse that has not been taken back, in the order the ledger recorded them: taking back that receipt puts back the cost of the receipt before it. Where nothing was ever received, the missing goods leave at nothing and their whole cost becomes variance when they arrive.
- `stock.on_hand` lists a level below zero with its negative quantity and value, so its total equals the inventory accounts at every moment.
- Entries are valued when they are recorded. A back-dated entry does not revalue what was recorded after it.
- Items are shared by all companies; warehouses belong to one.
- The stock ledger does not check fiscal periods itself. A posting dated in a closed period is refused for every ledger by the general ledger's guard on postings; a stock-only posting dated where the company has no period at all is allowed.

### Company and currency

Every document, posting and entry belongs to exactly one company. Each company has one accounting currency. An entry stores the transaction currency and amount, the exchange rate captured at posting, and the resulting accounting amount. Amounts are exact decimals, sent as strings in JSON, rounded half away from zero to the currency's minor units; Rust and Postgres use the same rule.

**Buying and selling in other currencies.** A purchase or sales order has a `currency`, the accounting currency when none is given, fixed once the order is approved (its content is locked then). Its prices are in that currency, and so are the invoices and credit notes that follow it. A document in a foreign currency posts at its own rate: the rate it states (`fx_rate`), or else the rate on record for its date; once posted, `fx_rate` shows the rate it posted at. A stated rate is checked against the rate on record by the policy `fx.rate_differs` (by default any rate is accepted).

- **Stock is valued in the accounting currency.** A receipt converts the order price at the receipt's rate: that is what the goods come into stock for and what goods received not invoiced holds. Deliveries and returns move stock at its value and carry no rate.
- **An invoice posts at the invoice's rate.** Payables (receivables) and, for a sale, revenue are posted in the invoice's currency at that rate; tax is worked out in that currency, rounded to its decimals, line by line, and the tax account and the tax record hold its value in the accounting currency, so a return still equals the tax accounts. For a purchase, goods received not invoiced is cleared by what the goods were received for, and the difference from what they are invoiced at is purchase price variance, whether it comes from the price or from the rate: the goods cost what the invoice says in the accounting currency, and the variance is the correction of their cost, as it is for a price difference in the accounting currency. Separating the two would need the receipt's value in the order's currency kept per order line and gives no different total. Cost of goods sold stays what the stock ledger says.
- **A credit note posts at the rate of the invoice it credits**, so it takes back exactly what the invoice put in the books, tax included. An exchange difference arises only when money moves (or a credit is applied).
- **Open items keep both currencies.** An item holds what is owed in the document's currency (`currency`, `txn_amount`, `txn_settled`) beside its value in the accounting currency at the document's rate (`amount`, `settled`), which is what the control account holds. A payment settles items in their own currency; the ledger works out what each settlement takes off the item's value: its share at the item's own rate, and all that is left when it settles the last of the item, so a settled item leaves nothing in the control account (the database checks this). The payment relieves the control account by exactly those values and moves the bank account at the payment's rate; the difference is a **realized exchange gain or loss**, posted to the account the company names for `fx_gain` or `fx_loss` (or for both, `fx_difference`). Applying a credit to a debt in a foreign currency realizes the difference between their two rates the same way.
- **One currency per payment.** A payment settles items of one currency, and netting credits and debts of a party needs them in the same currency; mixed currencies are refused with a message that says to pay each currency apart. Converting between two foreign currencies in one payment is not built.
- **Bank accounts can hold a currency** (`account.currency`); a payment through such an account is in its currency, and one through an account without a currency is in the currency of what it pays. The ledger does not refuse an entry in another currency on an account that holds one; payments do.
- **Revaluation at a period end, reversed the next day.** An `fx_revaluation` document values, as of its date, every payable and receivable still open in a foreign currency at the rate on record for that day, and posts each item's difference from what it is carried at to its control account against unrealized exchange gain or loss (`fx_unrealized_gain`, `fx_unrealized_loss`, else `fx_gain` and `fx_loss`, else `fx_difference`). The open items ledger records the difference per item (`open_item_revaluation`), so an item's value on a day is its amount, less what was settled, plus the revaluations standing that day, and aging as of the period end equals the control accounts. The revaluation is reversed the day after its date: at once when that day's period is open, else with `fx_revaluation.reverse`. This is the usual revalue-and-reverse approach: once reversed the item is carried at its invoice's rate again and a payment realizes its whole difference against that, so the unrealized difference of the period end and the realized one of the payment never count the same movement twice. A payment dated after a revaluation that still stands is refused until it is reversed.
- Summaries show the document's currency. An invoice or credit note also shows `total_net_accounting`, `total_tax_accounting` and `total_gross_accounting`, what it posted in the accounting currency (null for a draft in another currency).

### Lines: discounts, charges, services and invoices without an order

A line of an order, invoice or credit note may carry a discount: a percent, an amount off the whole line, or both. Its net amount is its quantity times its price, rounded, less the percent discount (rounded) and the amount, each line on its own (`erpcraft_tax::line_net`), so that a document's total is the sum of its lines and the tax is worked out on each line's net amount. An invoice line takes its order line's discounts unless it gives its own, the amount in proportion to the quantity; a credit note line takes its invoice line's the same way. An invoice line records the net amount it posted, and credit notes are capped by it. Goods are received at the order line's net price.

A line is stock (an item and its warehouse), a **charge** (`charge`, shared by companies: freight, insurance, handling, a service, with the tax code its lines start with), or an **account** with a description. A line on an account may not be on an account another ledger keeps in step: a bank or cash account, payables, receivables, goods received not invoiced, a tax account or an inventory account (`erpcraft_gl::line_account_refusal`). On a sale a charge's revenue goes to the account rule `charge_revenue` for the charge, else the company's, else the sales revenue account; on a purchase to `charge_expense`. An order's line that is not stock counts as received or delivered in full: invoicing it completes it, and receipts, deliveries and returns refuse it. An invoice can carry charge and account lines of its own besides its order's lines.

An invoice without an order (rent, utilities, a service, a machine to capitalise) names its vendor or customer and currency and has only lines of its own. It opens an open item with payment terms like any invoice and can be credited line by line. A direct vendor invoice asks the policy `vendor_invoice.without_order` when it is posted (facts: total, currency, vendor code; allowed by default), since no approved order stands behind it. A machine bought this way is on its asset class's `asset_cost` account, and `asset.capitalise` takes the invoice line.

**Landed cost.** A charge marked `landed_cost` on a vendor invoice (freight on the goods, or a carrier's own invoice) is shared out over the goods of receipts: those the line names (`receipt_ids`), else every receipt of the invoice's order, by their value, their quantity or their weight (`allocate_by`; weight is the quantity times the item's `weight`, refused while an item has none), rounded per receipt line, the last taking what is left. Of what each item in each warehouse gets, the share of the goods still on hand raises their value through a **cost adjustment** in the stock ledger: an entry with no quantity, refused on a level with nothing on hand or one it would take below zero, reversed exactly with its posting. The inventory account is debited by the same amount, so it still equals stock value. The share of goods already gone (sold, used) is a cost of those goods: it goes to the account rule `landed_cost_variance` (by item, else the company's), or else to purchase price variance, where purchasing already puts every other difference in what goods cost; purchasing cannot tell whether the goods went to a customer or into production, so it does not post to a cost of goods sold account of its own. Under average costing the goods still on hand are taken to be the received ones as far as the level still holds that many.

### Payment terms, discounts and aging

Payment terms (`payment_terms`, shared by companies) say when an invoice is due: a number of days after its date, or after the end of its month, and optionally an early-payment discount (a percent, within a number of days counted the same way). A vendor or customer names its usual terms; an order takes them from its party and an invoice from its order, each able to name others. When an invoice posts, its open item gets its due date and, if the terms give one, the last day and the amount of its discount in the item's currency. A credit note is due on its own date, and money on account has no due date.

A payment allocation can keep back a `discount`, up to what the item offers, when the money pays the rest of the item; the item is then settled in full. `payment.draft` fills the discount in when the payment is dated by the discount date. After that date the policy `payment.late_discount` refuses it unless the company allows it. The discount is posted at the payment's rate: of each tax the invoice recorded, the same share is taken back to the tax account the invoice posted to and recorded in the tax ledger as a credit of the invoice's entry, in the payment's period, and the rest goes to the account for `discount_received` (payables) or `discount_given` (receivables). Where a cash discount does not change the tax (US sales tax), the company refuses the policy `payment.discount_tax` and the whole discount goes to the discount account.

**Discount steps.** Terms may offer more than one early-payment discount: 2 % within 10 days, 1 % within 20, net 30. The terms' own discount is the first step and `payment_terms_step` rows add the others. An item keeps every step it offers in `open_item_discount`, and the step in force on the payment's date applies.

**Instalments.** Terms may make an invoice due in parts. Each `payment_terms_instalment` row is one part before the last: a percent due a number of days after the date, counted as the net days are. The last part is what is left, due at the net days. The parts are fewer days than the net days and leave something for the last; a trigger refuses anything else, including net days moved before a part. When an invoice posts, its open item keeps its parts in `open_item_instalment`, in the item's currency: each part's due date and amount, rounded, with the last taking what rounding leaves. A deferred trigger checks that they add up to the item. The item's own due date is its last part's. A payment allocation may name the part it pays (`instalment`, 1 for the first), and the ledger refuses more than is open of it. An allocation that names none, a credit applied to the item, or a reversal calling it off pays the earliest part still open first. What is open of each part is worked out from the settlements, not stored (`open_item_instalments(item, as_of)`): first what was paid for it by name, then the unnamed payments in order of the parts. `open_item.list` shows each item's parts with what is open of them, and `open_item.aging` ages an item by its parts. What is open of the item in the accounting currency is shared over its open parts by what is open of each in the item's currency, each at its own due date, with the last part taking what rounding leaves. An early-payment discount is about the whole item: it is taken with the payment that settles all the rest of it.

`open_item.aging` puts what is open of payables or receivables as of a day into buckets by days past due (current, 1-30, 31-60, 61-90, over 90 unless other bounds are given), per party, in the accounting currency at the value each item is carried at, with what falls due in the next 7 and 30 days, and each control account's balance beside what its items add up to. As of a past day it counts only postings dated by then.

### Concurrency guarantees

| Situation | Guarantee | How |
|---|---|---|
| Two requests post the same draft | One posts; the other gets the same result | Row lock on the document |
| An idempotency key is reused with other input | Refused | Stored input hash |
| A period closes while a journal posts | They wait for each other; a closed period never receives an entry | Share lock taken by each entry, exclusive lock taken by the close |
| Two users edit the same draft | The stale edit is refused | Version number on the document |
| Two users edit the same plain record | The stale edit is refused, if the caller sends the `_version` it read | The row's `xmin`, which Postgres changes on every update |
| Two people take the last stock at once | One is refused; stock never goes negative unless the company's policy allows it, and then each sees what the other left | Row lock on the level of that item and warehouse, taken in a fixed order |
| Two payments settle the same invoice at once | Together never more than is owed | Row lock on the open item, taken in a fixed order |
| Two servers deliver the outbox | Each event is taken by one | Row lock, skipping locked rows |
| A number series under load | No gaps, no duplicates. Numbers follow the order in which commands finish, closely but not strictly | Counter row locked from the end of the command until commit: a document takes its number in its last step, the move to a locked state |
| A change triggers an integration | The event exists if and only if the change committed | Outbox row in the same transaction |

Each row has a test in `apps/server/tests/`.

### Identity and authorization

The two are separate. The kernel (`authz/`) holds the directory of users and decides what an actor may do. `erpcraft-auth` answers only "who is this?": it is an ordinary module that supplies the kernel's `Authenticator`, so an installation can replace it with its own.

Built:

- Users sign in with a password and receive an opaque token, stored hashed.
- Users sign in through an identity provider with **OpenID Connect** (Microsoft Entra ID, Google, Okta, Keycloak, Authentik; several at once) and receive the same kind of token. See [Single sign-on](#single-sign-on) below.
- **Users are administered** with `user.list` (search by name or email, filter by active, `ids` to show given users; each with the roles they hold and for which company), `user.get` (also the attributes), `user.update` (name; administrator rights, which only an administrator grants or takes away), `user.deactivate` and `user.activate`, `user.assign_role` (granting a role held already changes nothing) and `user.revoke_role`. Reading takes `user:read` for all companies, which also shows attributes: they are facts rules read, not secrets. No command returns a credential. The last active administrator can neither stop being one nor be switched off, and only an administrator switches an administrator off. A deactivated user's tokens stop working on the next request, since every request loads the user and checks `active`. Users are also the read-only entity `user`, so a field can link to one (a document's `created_by`) and generic reads find them. How a user signs in is the identity module's business: the UI reads `sso_identity.list` for a user's binding. Every change is recorded on the user, so `audit.history` with resource `user` shows it.
- **Nobody hands on more than they hold.** An administrator grants anything. Anyone else grants or revokes a role (`user.assign_role`, `user.revoke_role`) only for someone else, and only a role whose every allowing rule they hold themselves at least as far: for the company it is granted for or for all companies, for at least its fields, and with no condition or the very same one (a condition that reads `user.<attribute>` never counts as the same, since another user's attributes reach other records). A deny anywhere in their own roles that touches what the role allows means they do not hold all of it, and a token limited by scopes hands nothing on. Writing, changing or deleting a `role` record follows the same rule, for all companies, for the role as it was and as it would be. Attributes widen what rules reach, so `user.set_attributes` is handing on rights too: someone who is not an administrator sets them only for another user who is not an administrator and all of whose roles they could have granted.
- **The directory is read for all companies.** An entity can declare `read_needs_global`: then reading it, generic lists and reads included, takes the grant for all companies, not one for any company. `user`, `role` and `sso_provider` declare it. Other shared records (items, vendors, customers, tax codes) stay readable under any company's grant, because every company uses them.
- **An administrator acts as one only on a credential that could have signed one in.** Each token records how its user signed in (`password`, or `sso:` and the provider), and a token made with `auth.create_token` carries the origin of the one in use on. When a token authenticates, an administrator whose token came through a provider not trusted for administrators (or one renamed or deleted since) is refused, not quietly given fewer rights, so that they know to sign in again; the same is checked when a login code is exchanged. This covers a user made an administrator after signing in. `auth.create_token` makes tokens of at most 365 days (90 by default).
- A command input that refers to a record says so in its JSON Schema with `x-field-type`, in the form of a field type: `{"kind": "link", "entity": "user", "key": "id"}` on the `user_id` of `user.assign_role`, `user.revoke_role`, `user.set_attributes` and the `sso_identity` commands.
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
- **Nothing undecidable helps the caller.** A rule that cannot be evaluated never allows and always denies. A null is undecidable like a missing value: a user attribute that is null is one the user does not have, and a field that is null can be asked `== null` or `!= null` and compared with nothing else. A record and a list filtered in the database are decided the same way. An action checked without a record, such as a total over a ledger, treats conditional rules the same way.
- Rules are checked when a role is saved: the syntax, the fields they name, and that they can be compiled to SQL.
- `/api/meta` describes, under `access`, what the caller could do: per entity whether they can read, create and update, and which fields are limited (`all`, `some`, `none`); per document type the actions open to them; and the commands they could run. An interface uses it to decide what to show. It is a courtesy: every command still checks for itself.

Limits of what is built:

- An administrator is not subject to rules.
- Rules on a document see its header (company, status, dates, creator), not its lines.
- Field rules apply to entities, not to documents.

Planned:

- Postgres row-level security as a second wall, which requires the server to connect as a non-owner role.
- SAML, and provisioning of users by the identity provider (SCIM).

#### Single sign-on

A provider is a record of `sso_provider`: a name for the sign-in page, a slug for the URLs, the issuer, the client id, the scopes, the email domains that may sign in, whether a first sign-in creates the user and with which role, whether the provider must state that the email address is verified, and whether administrators may sign in through it (`trusted_for_admins`). Providers are managed with the commands `sso_provider.create`, `.update`, `.set_secret` and `.delete`, and read with `sso_provider.list` (permission `sso_provider:read`). The client secret is in a table of its own that is not an entity: no read returns it, and the commands that take it are sensitive, so it reaches neither the audit log, the outbox nor the idempotency table.

**Whoever may configure providers can sign in as any user who is not an administrator.** A provider decides who its users are: point one at a server you run, name a domain, and that server's word is enough to sign in, by email address, every user of the domain who is not bound to another identity yet. So there is one permission for every change to providers and bindings, `sso_provider:manage`, held for all companies, and it must be held as closely as administrator rights. Besides it:

- A provider that grants a role to the users it creates (`default_role_id`) grants that role to whoever it lets in, so every change to who such a provider lets in (issuer, client id, domains, creating users, the role or its company, requiring a verified email, trust for administrators, switching it on) also takes `user:assign_role`, as does setting the role.
- Only an administrator can set `trusted_for_admins`, or change, give a secret to or delete a provider that has it, or change the binding of an administrator.
- The client secret belongs to one client at one issuer. Changing `issuer` or `client_id` discards the secret, the identities users are bound to at the provider and the sign-ins on their way, and makes the provider inactive. A provider cannot be active without a secret (enforced by the database too), so it signs nobody in until `sso_provider.set_secret` gives it the new secret and `sso_provider.update` switches it on. Without that, holding `update` was enough to send the real secret to a server of one's choosing.
- `allowed_domains` is required and cannot be empty: any domain is said with `["*"]`, never meant by omission. A provider saved before this has none, and signs nobody in until it is given some.
- Every change to a provider or a binding is kept in `auth_event` (kind `sso_provider` or `sso_identity`): who made it, which provider, and what changed. Never the secret.

**Users are bound to an identity.** The first sign-in of a user through a provider binds them (`sso_identity`) to that provider and the subject (`sub`) it states. From then on they are accepted only from that provider under that subject, whatever email address it states; another provider, or another subject with their address, is refused (`not_bound`). The first sign-in of an existing user who is not bound is allowed only from a provider that names the user's domain in `allowed_domains`; `["*"]` binds nobody by address and signs in only users bound to it with `sso_identity.set`. A user created at first sign-in is bound at once. An administrator signs in only through a provider marked `trusted_for_admins`, bound or not (`admin_not_allowed`). An administrator changes a binding explicitly with `sso_identity.set` (user, provider, subject; `auth.events` shows the subject of every sign-in, refused ones too) or `sso_identity.delete`, after which the next allowed sign-in binds the user again; `sso_identity.list` shows the bindings.

The flow is the authorization code flow with PKCE:

1. `GET /api/auth/sso` lists the active providers for the sign-in page. It needs no token.
2. `GET /api/auth/sso/{slug}/start?return_to=/path` notes a login attempt (state, nonce and PKCE verifier, good for ten minutes), gives the browser a cookie, and sends it to the provider. `return_to` must be a path on this server.
3. The provider sends the browser to `GET /api/auth/sso/{slug}/callback`. The state is checked and used up, the code is exchanged at the provider's token endpoint, and the ID token is checked: its signature against the provider's published keys, which are read again when a token names a key that is not known; the issuer; the audience; that it has not expired and was issued just now; and the nonce. Then the email address must be verified and of an allowed domain; the user is the one bound to the identity, or else the one with the address if the provider may bind them, or a new one if the provider creates users; and must be active.
4. The browser is sent to `return_to` with `sso_code`, a login code good once and for a minute. The page posts it to `POST /api/auth/sso/exchange` and receives what a password sign-in returns. The token is never in a URL. A refused sign-in goes to `/sign-in?sso_error=<word>` instead.

What keeps it safe:

- The redirect URI is built from `ERPCRAFT_PUBLIC_URL`, never from the headers of a request.
- The server calls only the issuer an administrator configured and the endpoints that issuer's discovery document names, all https, without following redirects, with a time limit and a size limit. `ERPCRAFT_SSO_ALLOW_INSECURE=1` allows plain http for an issuer on the same machine, for development and tests.
- Only algorithms with a public key are accepted for the ID token. The claims are checked by the JWT library and again by this crate on typed values.
- The state, the login code and the cookie are stored as hashes, and compared in constant time. The cookie ties a sign-in to the browser that started it, so that a link to somebody else's finished sign-in signs nobody in. Its name is `__Host-erpcraft_sso_` and an id of the sign-in, with `Secure`, `Path=/` and no `Domain`, so a sibling subdomain cannot plant it; only exactly that name is accepted for that sign-in. Over plain http, which the public URL may use only on the same machine, the prefix cannot be used and the name is `erpcraft_sso_` and the id. The PKCE verifier and the client secret must be sent to the provider, so they are stored as they are.
- A user's first sign-in matches them by email address, so an address the provider does not vouch for is refused. Microsoft Entra ID sends no `email_verified`: either add its optional claim `xms_edov`, which is accepted in its place, or set `require_verified_email` to false on that provider, which is right only for a single-tenant application, where the organization controls the addresses.
- A provider that creates users must name the email domains that may sign in (`["*"]` is refused there).

A user created at sign-in has no password, and `user.create` takes none for a user who is to sign in only through a provider, so an installation can do without passwords for everyone but its first administrator. Each token records how its user signed in (`password`, or `sso:` and the provider's slug). Sign-ins and refused sign-ins, by password or through a provider, are kept in `auth_event` and listed by `auth.events`, since the audit log holds only commands.

### Rules

A rule condition is one expression over `record` and `user`. The syntax is CEL. `erpcraft-rules` is the only crate that knows the engine; the kernel sees a `Rule` that can be checked, evaluated against a record, and compiled to a SQL condition with bound parameters. Only the part of CEL that maps to SQL is accepted: comparisons, `in`, `&&`, `||`, `!`, literals, record fields and user attributes. The same crate serves the `where` parameter of lists, and is meant to serve validations and workflow conditions later.

A rule or an expression is text that any signed-in user can send, and the engine walks it by recursion, so its size is limited before anything recursive sees it: at most 4,000 characters, parsed on a thread with a stack of its own (64 MB of address space), and refused if the parsed tree is more than 64 levels deep. Without that, 11 nested parentheses were enough to overflow the stack of a request in an unoptimized build, which aborts the server. The measurements are in `crates/erpcraft-rules/src/parse.rs`.

What parsing a source gave is remembered inside `erpcraft-rules`, by its exact text, so a rule is parsed once and not on every request: stored rules and policies (`parse_stored`) in a cache of 1,024 entries, conditions sent with requests (`parse`) in one of 256, and refusals in a third of 256, each also bounded by the bytes of source it holds and each dropping what was used longest ago. They are separate so that no stream of different `where` conditions or broken ones can push out the handful of stored rules every request needs. `ERPCRAFT_RULE_CACHE=off` turns it off.

A condition that is only ever evaluated in memory, such as a policy rule, is an `Expression` of the same crate and in the same syntax. It never becomes SQL, so it can say more: arithmetic (`+ - * / %`), comparisons between any two values, `condition ? a : b` and `has(user.x)`, besides `&&`, `||`, `!`, `in` and literals. Nothing of this widens what a rule that must filter accepts.

### Bank statements and reconciliation

A `bank_statement` (module `banking`) is a document per bank account (an `is_bank` account of the company) and period, in the account's currency: an opening balance, lines (booking and value date, a signed amount, counterparty name and account, reference, the bank's own transaction id) and a closing balance. Two things are invariants, refused when the statement is saved: opening plus lines is the closing balance; and a statement starts no earlier than the account's last statement ends and opens at its closing balance. A bank transaction id is unique per bank account.

`bank_statement.import` (`bank_account_id`, `format`, `content`) reads a file into a draft. Formats: **CSV**, read with a `bank_csv_mapping` saved per bank account (delimiter, header or not, date format, decimal comma, which column holds the booking and value date, a signed amount or a debit and a credit, counterparty, reference and transaction id; a row without a transaction id gets one made of what it says); **ISO 20022 camt.053** (any version from .001.02, namespaces ignored, one statement per file, read with `quick-xml`; a DTD or an unknown entity is refused, so nothing outside the file is read); **OFX** 1 (SGML) and 2 (XML), one statement per file, the opening balance worked out from the closing one, which the file must state (a file cut off before its balance, its last transaction or `</OFX>` is refused). The account a camt.053 or OFX file names must be the bank account's, as its `bank_account_number` records it (a file that names none is imported with a note saying so), and every amount must be in the statement's currency. Amounts are capped at what `numeric(20, 4)` holds; camt.053 amounts are unsigned with a point, `CdtDbtInd` exactly `CRDT` or `DBIT`; lines must be booked within the statement's period. Files are read on a blocking thread, in time linear in their size, with at most 500,000 elements nested at most 64 deep. Files are at most 5 MiB and 10,000 transactions; a malformed one is refused with the line (and for XML the element) it is about. A file whose transactions were all imported before returns that statement; one that repeats only some is refused.

`bank_statement.proposals` says what each line not matched yet probably is, best first: a posting that moved the bank account by exactly the line's amount within ten days, not reversed and not matched to another line; open items it pays, by a document number in the reference (grouped by party) or by amount, scored higher when the counterparty is the party (only for a caller who may read open items, and party names only where the caller may read the party); or an account, where an earlier line of the same counterparty or reference was posted to one, or the text reads like a fee or interest. `bank_statement.match` records the choice per line: `posting`; `open_items`, which runs `payment.draft` for them through the bank account, dated the booking date (the caller needs `payment:create`); `account`; or `clear`. A posting of another amount than the line is accepted only with an account for the difference and when the policy point `bank_statement.amount_difference` allows it (refused by default; facts `line_amount`, `posting_amount`, `difference`, `difference_abs`).

`bank_statement.post` reconciles and locks: every line must be matched; a payment drafted for a line must be posted, and what a line's posting moved through the bank account must be the line less its difference; lines and differences matched to an account are posted then, one posting per line, by the statement itself.

`bank.reconciliation` (`bank_account_id`, `as_of`) sets the ledger against the statements: the ledger balance of the account (in its currency), the closing balance of the last statement ending by then, the postings on no statement yet (outstanding), the statement lines or parts of lines the books do not hold yet (unmatched), and the opening balance of the first statement (money that was there before). Outstanding less unmatched less that opening balance is the difference, to the cent; `unexplained` reports anything else and is zero while the statements follow each other.

The module reads general ledger entries, postings, documents and open items with SQL, as a report does; it writes the ledger only through `erpcraft_gl::post` and payments only through `payment.draft`.

### Fixed assets

The module `assets` keeps the fixed asset register: what the company owns and uses for years, what it cost, and its depreciation month by month. The general ledger holds the money; the register explains it, and `asset.register` shows whether the two agree.

- **Asset classes** (`asset_class`) are per company, because their accounts are: a method (`straight_line`, `declining_balance` with a yearly `declining_rate` in percent, or `none` for land) and a useful life in months. Their accounts are account rules with scope `asset_class` (or the company's default for the purpose): `asset_cost`, `accumulated_depreciation`, `depreciation_expense`, `asset_disposal_gain`, `asset_disposal_loss`. Where depreciation is written off the asset account directly, as SKR 03 does, `accumulated_depreciation` is the cost account itself.
- **An asset** (`asset`) is created planned, with its class's method and life unless it gives its own, and a salvage value. Its status (`planned`, `in_service`, `fully_depreciated`, `disposed`), cost and dates are written only by commands. Once it has been depreciated, its method, life, rate, salvage value and dates no longer change (a trigger); a capitalised asset is never deleted, and its class does not change.
- **Capitalising** (`asset.capitalise`) names a posted document line, such as a journal entry line, whose ledger entry debited the class's `asset_cost` account: the entry's amount is the cost, its posting date the acquisition date. Nothing is posted, as the ledger holds the cost already; an entry is capitalised once (a unique key). The policy point `asset.below_minimum` is asked (facts `cost`, `minimum`, `class_code`): by default an asset below the company's `asset_settings.minimum_cost` is refused, 0 meaning no minimum. `asset.put_in_service` starts the depreciation of an asset capitalised without an in-service date.
- **Taking over** (`asset.take_over`) records an asset from an older system with its cost and its depreciation up to the end of a month (`depreciated_through`), from an opening date (by default the day after). It posts nothing: the opening balances come in as a journal entry of their own, and until they do the register shows the difference. Depreciation continues over the life that is left.
- **The convention** (`asset_settings.convention`, per company): `full_month` (the default) depreciates the month an asset goes into service in full, `next_month` starts the month after. Once the company has depreciated anything it no longer changes (a trigger).

**Depreciation.** Every month of an asset's life has its amount, worked out the same way by a run, a disposal and the schedule, in the accounting currency's cents. Straight line spreads cost less salvage less depreciation taken over evenly over the months left, rounded per month; the last month takes what rounding left. Declining balance takes the yearly rate of the book value at the start of each calendar year, or of the asset's first year pro rata by months, as one block per year: a month is a twelfth of the year's amount and the year's last month takes the remainder. Each year it switches to straight line over the months left once that is more, as German and US practice allow. Depreciation never goes below the salvage value.

**A depreciation run** (`depreciation_run`, `DEP-`) is dated the last day of a month. `depreciation_run.post` depreciates every asset in service by then for the months since its last depreciation, so a missed month, or an asset capitalised late, is caught up, and posts one entry per asset (expense debit, accumulated depreciation credit), with the run's line for the asset as the source line. A draft shows what it would post. Runs go forward, enforced in the database: a month of an asset is depreciated once (a unique index over the months not reversed), and a run for a period already run, or before one, is refused (`depreciation_period` and its trigger). A run with nothing to depreciate posts nothing. `depreciation_run.reverse` reverses the company's latest run exactly, on its own date, so the register and the ledger agree at every date; not while an asset it depreciated is disposed of.

**A disposal** (`asset_disposal`, `ADS-`) sells or scraps an asset on its date: `asset_disposal.post` first depreciates the months that ended by then and were not yet, then removes the cost and the accumulated depreciation, books the proceeds to the account named (a bank, or a clearing account that a sales invoice to the buyer settles), and posts the difference to the book value as a gain or a loss. A disposal dated before a month already depreciated is refused. `asset_disposal.reverse` undoes it exactly, and the asset is depreciated again from where it was.

**Reports.** `asset.register` (report "Fixed asset register"): per asset in the books on the day, its cost, accumulated depreciation, book value and the depreciation since 1 January; beside it every account the classes' rules name for cost and accumulated depreciation, with what the register puts on it, its ledger balance and the difference, and `ties` when every difference is zero. `asset.schedule` (report "Depreciation schedule"): one asset month by month, posted (with the document) and planned.

`company.setup` creates the template's asset classes and their account rules (`asset_classes` in a template; `asset_disposal_gain` and `asset_disposal_loss` among its roles). `us_small_business` has furniture and equipment, computers, vehicles, machinery and land, with one accumulated depreciation account; `de_skr03` has Büroeinrichtung, EDV-Hardware, EDV-Software, Pkw, Lkw, Maschinen and Unbebaute Grundstücke on their SKR 03 accounts, written off directly, with the lives of the AfA-Tabelle (the template's sources say where each comes from).

### Electronic invoices (EN 16931)

The module `einvoice` writes and reads structured electronic invoices after the European standard EN 16931, the format German law requires between domestic businesses: received since 2025, issued from 2027 or 2028 by turnover (§ 27 Abs. 38 UStG). Its README names the sources it was built from, and lists exactly which rules it checks and which it does not.

- **Export.** `einvoice.export` (`doc_type` `sales_invoice` or `sales_credit_note`, `id`, `syntax` `ubl` or `cii`, `profile` `xrechnung` or `en16931`) writes a posted document as XRechnung 3.0 (or EN 16931 alone), in UBL 2.1 or CII.
  - The seller's and the buyer's particulars, addresses, tax identifiers, contacts and electronic addresses, and the buyer reference or Leitweg-ID, are party data (see [Party data](#party-data)), read with `erpcraft_party::load`. The module keeps only what is e-invoicing's own: the VAT category of a tax code and its exemption reason (`einvoice_tax_category`, S by default for a rate above zero). The IBAN, BIC and holder of the account paid into are banking's `bank_account_number` (read with `erpcraft_banking::account_number`), the one place bank account numbers are kept.
  - Anything missing is refused at once, every item named with its business term and the record and field to set.
  - Line discounts are line allowances (reason code 95). A charge line such as freight is a line of its own.
  - The VAT breakdown carries the tax recorded per line, so the invoice says what was posted. The standard's rounding rule (BR-CO-17) allows that, within one currency unit.
  - A tax group (several taxes on a line) is refused: EN 16931 has one VAT rate per line.
- **Checks.** The XML is read back with the reader incoming invoices use, and checked: mandatory terms, line arithmetic, the VAT breakdown and the document totals (BR-CO-10 to 17), decimals, and XRechnung's German rules (BR-DE-*), with the tolerances of the official Schematron.
  - The official validator (KoSIT, Java and Schematron) is not run here. The README says how to run it on the output.
  - The reader and the checks pass every business case of the KoSIT XRechnung test suite (66 files, UBL and CII), with `EINVOICE_SAMPLES` pointing at them (`modules/einvoice/tests/samples.rs`).
- **Import.** `einvoice.read` reads an incoming invoice (XML, or the XML embedded in a ZUGFeRD / Factur-X PDF) and reports the rules it breaks, the vendor, the order and each line's order line and tax code. It creates nothing. `einvoice.import` drafts the vendor invoice.
  - The vendor is the one whose `party_tax_id` (scheme `eu_vat`) is the seller's VAT identifier, or the one confirmed with `vendor_id`; names only propose.
  - When the buyer's order reference is an open purchase order of that vendor, the invoice is drafted against it, line by line by line reference or item code. Other lines, and a direct invoice, go to `account_id`.
  - What the draft will come to is worked out as posting works it out and set against the invoice's VAT, total and amount due to the cent. A difference, an amount already paid, or a buyer the invoice does not identify is drafted only with `accept_differences`, and noted in the memo. A line charged VAT without a tax code stops the import until one is chosen (`line_tax_codes`).
  - What `einvoice.read` proposes (vendors, party records, the order) is read with the caller's rights.
  - Numbers beyond `numeric(20, 6)` are refused when read, and the arithmetic is checked, so no input can make it panic.
  - Refused: an invoice that breaks a checked rule, one addressed to another VAT identifier, a credit note, document level allowances and charges, and an invoice number of the vendor imported before (`einvoice_import`, with the file's SHA-256).
- **Hostile input.**
  - XML is read with `quick-xml`. A DTD, any entity but XML's own, more than 5 MiB, 200,000 elements or 64 levels is refused.
  - A PDF is parsed with `lopdf` on a thread of its own, waited for on the blocking pool with a time limit, and a panic is a refusal. Each stream lopdf inflates is limited to 4 MiB and what a load keeps to 200,000 objects and 64 MiB. It is never rendered and is refused if it holds JavaScript. The embedded file is inflated under the XML limit by the module itself.

### Party data

What the company, a customer or a vendor is known by outside the books is kept by the crate `erpcraft-party` (module `party`), one schema for all three.

**The decision.** The records themselves stay with the modules that own them: the company with the kernel, customers with sales, vendors with purchasing. Their addresses, tax identifiers and contacts could have been added to each of the three tables by its owner. They are kept in one shared module instead, keyed by `party_type` (`company`, `customer`, `vendor`) and `party_id`, the way account rules are keyed by scope. The reasons:
- the three need the same data in the same shape;
- printing, e-invoices and tax reports read it the same way for whichever party a document names;
- one model and one reader keep it cohesive;
- none of the owning modules has to know about it.

A trigger checks that the party exists, and a row never moves to another party.

**Rights follow the owner.** The tables are not generic entities, so no permission of the party module's own can get around the owner's.
- `party.get` (`party_type`, `party_id`) reads all a party has on record, and `party.set` replaces the parts it is given (`profile`, `addresses`, `tax_ids`, `electronic_addresses`, `contacts`).
- Both first read the owning company, customer or vendor with the caller's rights, its row rules and the company row filter included; a record the caller cannot read is not found.
- `party.set` then requires the owner's own right to change that record: `customer:update` for all companies for a customer or vendor, which all companies share, and `company:update` for that company for the company. The change is in the owner's history.

**No orphans.** A trigger on `company`, `customer` and `vendor` deletes a party's rows with its record, so they never dangle. They never block a deletion either: they hold no foreign key to their owner. A record that something else refers to, such as a customer with orders, cannot be deleted, as before, and its rows stay with it. The account the company is paid into is set to nothing if that account is deleted.

| Record | What | One or many |
|---|---|---|
| `party_profile` | legal name, trading name, commercial register entry, further legal information; for a customer the `invoice_reference` its invoices quote (a Leitweg-ID for a German public buyer); for the company the bank account it is paid into (`payment_account_id`, a bank account of its own) | one per party |
| `party_address` | `billing` or `shipping`, attention, street and second line, city, post code, region, country | many; at most one default of each kind |
| `party_tax_id` | `scheme` `eu_vat`, `tax_number` (such as the German Steuernummer), `us_ein`, `ca_bn` or `other`, the value, the issuing country | one per scheme |
| `party_electronic_address` | an EAS scheme (EM for email, 0204 Leitweg-ID, 9930 German VAT number, 0088 GLN, a Peppol participant identifier) and the address | many; at most one default |
| `party_contact` | name, role, telephone, email | many; at most one default |

**Reading.**
- `erpcraft_party::load` gives a party's particulars, its default billing address, its tax identifiers by scheme, and its default electronic address and contact.
- `load_readable` gives the same only to a caller who may read the party's record, and nothing to anyone else.
- `document.parties` (`doc_type`, `id`) gives the seller and the buyer of a document: the company and the customer of a sales document, the vendor and the company of a purchase document. A party's data shows only to someone who may read its record; the company is always named. Printing gives the result to every template as `parties`.
- Bank account numbers are not party data: they are banking's `bank_account_number`, the one place they are kept.

**Documents keep the address they were posted with.** A posted invoice prints, and is sent as an e-invoice, with the address it was posted with, even after the customer moves.
- The table `document_party` (party migration 2) holds, per document and `role` (`seller`, `buyer`, `ship_to`), the `party_type` and `party_id` and a copy of what the document shows: the name and particulars, the invoice reference, the account paid into, the address lines and country, the tax identifiers, the electronic address and the contact, and the id of the address it was copied from.
- `erpcraft_party::snapshot` writes one role; `snapshot_sale` (company sells, customer buys at its billing address, goods go to its shipping address) and `snapshot_purchase` (vendor sells, company buys) write a document's. The posting command calls them before it moves the document to a locked state, and the document child guard keeps the copy unchanged from then on: approving a sales or purchase order, posting a sales invoice or delivery, a vendor invoice or a receipt. A credit note keeps the parties of its invoice and a return those of its order (`copy_parties`); when that document kept none, the party's defaults now.
- A draft or an order may name one of its party's addresses: `billing_address_id` and `shipping_address_id` on a sales order and invoice, `shipping_address_id` on a delivery, `billing_address_id` (the vendor's) on a purchase order and vendor invoice. Saving refuses an address that is not that party's. An invoice or delivery that names none takes its order's; without one, the default of the kind is copied, and `ship_to` is kept only when the customer has a shipping address. The ids are plain columns, not foreign keys, so a party may still remove an address that a posted document copied; a draft whose address was removed is refused at posting, on the field that names it. The forms choose from `party.get`, which takes `address_kind` to list one kind.
- `party.set` keeps an address's id when it is given back with it (as document lines keep theirs); one left out is removed.
- `document.parties` and the e-invoice export read the snapshot when the document has one, and the party's current data otherwise; `document.parties` adds `ship_to` when the document kept one, and is otherwise as before. What a document kept is never replaced, but what it did not have when it was posted (no address yet, no VAT identifier, no contact) is filled in from the party's data now, so that a document posted before the data was entered can still be printed and sent. The rights are the same: a party shows only to someone who may read its record.
- Documents posted before this existed keep nothing, and show the party's current data.

E-invoicing kept this data in tables of its own (`einvoice_profile`, `einvoice_party`) until the party module existed; its migration 3 moved the rows across and dropped those tables. It added nothing where the party module had a record already.

### Setting up a company

`company.setup` turns a template into a company that can buy, sell, post tax and close its year: the company, its chart of accounts with groups and parents, its fiscal years, a warehouse, the purchasing, sales and tax settings, its tax codes, and the default account rules for `inventory` and `stock_variance`. Input: `template`, `code`, `name`, optionally `base_currency`, `first_year`, `start_month` (else the template's), `years` (1 to 5), and `bank_account_code` / `bank_account_name` to name the bank account. It writes through `entity::create`, so every validation and permission of each record applies and the audit log lists every record under the one command; it is one transaction, so a refusal at any point (a company code in use, a shared tax code that exists with another rate) leaves nothing. It needs `company:setup` for all companies and the right to create each kind of record. The output names, per role, the account created: `bank`, `retained_earnings` and the rest, which a payment or a year-end close then uses. `setup.templates` lists the templates, optionally for one `country`.

A `SetupTemplate` (`modules/setup`) is data: code, name, description, country, currency, start month, the accounts (`code`, `name`, `type`, `group`, `parent`), the account of each role (`bank`, `inventory`, `stock_variance`, `grni`, `payable`, `receivable`, `revenue`, `cogs`, `price_variance`, `retained_earnings`), tax codes with the accounts for tax collected and paid, a warehouse, and the sources it was taken from. `SetupTemplate::check` refuses one whose roles or tax accounts are not postable accounts of its chart. Built in:

- `us_small_business`: a US chart in groups (1000 assets to 6000 operating expenses), USD, calendar year, and one flat `US-SALES-TAX` code at 0 %: US sales tax depends on the buyer's jurisdiction, which this ERP does not look up. An example group shows the shape of a combined rate: `US-EXAMPLE-STATE-COUNTY` of `US-EXAMPLE-STATE` (4 %, owed on 2100) and `US-EXAMPLE-COUNTY` (1.5 %, owed on 2110), labelled as examples, not real rates.
- `ca_small_business`: a Canadian chart in groups, CAD, calendar year, GST/HST payable on 2100 and input tax credits on 1350, and the codes `CA-GST` (5 %), `CA-HST-ON` (13 %), `CA-HST-NB`, `CA-HST-NL`, `CA-HST-PE` (15 %), `CA-HST-NS` (14 % since April 1, 2025), `CA-ZERO` and `CA-EXEMPT` (0 %), at the rates the Canada Revenue Agency publishes (cited in the template's `sources`). Where the provincial tax is not harmonized, a line carries a group: `CA-GST-PST-BC` (GST 5 % and PST 7 %), `CA-GST-PST-SK` (GST and PST 6 %), `CA-GST-RST-MB` (GST and RST 7 %) and `CA-GST-QST` (GST and QST 9.975 %, on the price without the GST), with each provincial tax owed on its own account (2110 to 2140). PST and RST paid are expensed on 6130; QST paid is an input tax refund on 1360. The provincial rates are cited from each province's finance ministry and Revenu Québec in the template's `sources`.
- `de_skr03`: the accounts this ERP posts to, and common cost and revenue accounts, numbered as in the DATEV standard chart SKR 03 (verified against the DATEV SKR 03 chart for 2023; the template's `sources` say where), EUR, calendar year, and `DE-USt19` and `DE-USt7` with Umsatzsteuer 1776 / 1771 and abziehbare Vorsteuer 1576 / 1571. SKR 03 has no account for goods received not invoiced, so 1700 Sonstige Verbindlichkeiten is used. Cost of goods sold and price differences both go to 3200 Wareneingang. In DATEV, 1571/1576, 1771/1776, 3300/3400 and 8300/8400 are automatic VAT accounts; here tax is posted explicitly, so revenue goes net to 8400.

**A country module adds a template** without touching `modules/setup`: it carries a JSON file and calls `erpcraft_setup::register(SetupTemplate::from_json(include_str!("fr_pcg.json"))?)` when it is constructed, before the server starts. A code can be registered once. Tax codes are shared by all companies, so a template's codes should carry the country (`DE-USt19`); one that exists with the same rate is reused.

Limits: one revenue account per company, so a German company's 7 % sales also land on 8400 unless the line is posted otherwise; no opening balances (post a journal entry); a company's chart can be extended afterwards but not re-applied from a newer template.

### Printing

A document is printed as an HTML page that the browser prints, to paper or to a PDF. The server makes no PDF.

`document.render` (`doc_type`, `id`, optional `template_id`) returns `{html, template_id, title}`. The template is the one asked for (it must be active, for the document's type, and for the document's company or for all), else the active default of the document's company, else the active default for all companies, else the one built into `modules/printing`: purchase order, sales order, sales delivery and sales invoice have their own; every other type prints through a generic template that lays out its header fields, collections and summary from the type's description. `print_template.builtin` returns a built-in's source to start from, and `print_template.preview` (`doc_type`, `id`, `body`) renders a template that is not saved, as an editor shows it: the source gets exactly the checks of saving one and the same limits, nothing is stored, and it takes the right to create or change templates.

A print template is a record of `print_template`: `doc_type`, `name`, `company_id` (empty for every company), `body`, `is_default`, `active`. It is written through the generic entity API. Templates are shared records like policy rules: writing one needs `print_template:create` or `:update` for all companies. The database allows one default per document type and company, and one for all companies. When a template is saved, its document type must exist, and its body must compile and contain nothing active (below).

The template language is MiniJinja 3.1. It was chosen because it is maintained, has no dependencies worth naming, is built for templates written by people other than the developers, and in version 3 has fuel that charges for instructions, bytes produced and items visited, checked before an allocation is made, so it bounds both time and memory. A template is given:

- `doc`: the document exactly as `document.get` returns it to the caller (header, `summary`, `created_by_name`, `body`, `postings`, `links`). It is read through `document.get` itself, so the caller must be allowed to read the document.
- `meta`: the type's `label`, `fields`, `collections` and `summary`.
- `company`: `code`, `name`, `currency` and the currency's `decimals`, of the document's company.
- `refs`: for each id the document refers to (links to entities and documents, and lines of other documents, such as the order line of an invoice line), `{display, record}` as far as the caller may read it, read through the kernel's entity and document reads so that row and field rules apply. What the caller may not read is not there, and prints as nothing.
- `title`.

Money and quantities are exact decimal strings. Filters format them without floats: `money` (the currency's places, or `money(n)`), `price` (a unit price: at least the currency's places, at most six), `decimal` (trailing zeros dropped, or `decimal(n)`), `mul` (an exact product, such as quantity times price), `date` (`15 Mar 2026` by default: day, month in letters, year, which reads the same in every country where `03/04/2026` does not; there is no locale on a company yet; or `date("%Y-%m-%d")`), `ref` (what tells a person which record an id is, such as `WIDGET Widget`; `ref("short")` gives only its first part, the code, for a narrow column), and `show(field)`, which formats a value as its field's description says (`show(field, "short")` for a reference by its code). The generic template prints the first reference of each line in full and the others by their code.

Every rendered page goes through a real HTML sanitizer, `ammonia` 4.2 on the `html5ever` parser, with an allow-list for printed documents: text, headings, tables, lists, `div`/`span`/`section`, `img` and `a`; the attributes `class`, `style` (only layout and type properties, none that can load a URL), `title`, `lang` and `dir`, cell spans, and `href`/`src` that are `http`, `https`, `mailto` or a `#fragment`. Scripts, event handlers, frames, objects, forms, `base`, `meta`, `link`, `style` elements, `svg` and `math` are removed, with the content of `script`, `style`, `title` and `textarea`. A template writes the page's body; the server wraps it in a fixed head with the title and the one built-in stylesheet, so a template styles with the stylesheet's classes and `style` attributes. When a template is saved, a scanner still refuses what will not work, with a message for whoever wrote it; safety rests on the sanitizer, not on that scanner.

What is guaranteed:

- A template reaches no data beyond what the caller may read: `document.get`, the entity and document reads under the caller's rights, and the code, name and currency of the document's company. It cannot read files (MiniJinja is built without its loader, and without `include`, `import`, `extends` and macros), reach the network, or call anything in the server but the formatting filters.
- Whatever it prints is escaped for HTML. The `safe` filter is removed and `{% autoescape %}` is refused, so a memo or a vendor name with markup is text.
- A render is bounded: fuel of 2,000,000 per render, recursion depth 50, and the page at most 4 MiB. A runaway template ends with an error, not a busy server.
- No active content: whatever a template or the data in it makes, the sanitizer keeps only the allow-list above, as a browser's own parser would read the page (raw-text elements, bogus comments, character references without a semicolon and the rest).
- A template asked for by id that cannot print this document (another company's, another type's, inactive, or none) gets one answer, which says nothing about it. References are resolved once per distinct record per render, at most 2,000 per render.

What it relies on: the interface shows the page in an iframe with `sandbox` and without `allow-scripts` (and without `allow-same-origin`), as a second wall in case the check above misses a construction a browser would run; and the people who may write templates are trusted not to make a page that misleads, since a template decides what the page says.

### Policies

Two kinds of refusal look alike and are not.

- An **invariant** is true for every company: a posting balances, an entry is never edited, a posting is reversed at most once, stock does not go below zero. Breaking one would make the books wrong. Invariants are compiled in, and where they concern a ledger they are the ledger's database triggers.
- A **policy** is a business judgement that a careful company could make either way: whether to accept a delivery of a few percent more than was ordered, or a vendor invoice above the order price. A policy is configured by the company, with expressions, and needs no rebuild.

The rule of thumb for a module author: if a careful company could reasonably want it otherwise, it is a policy point, with today's behaviour as its default. If breaking it would make the books wrong, it is an invariant and belongs in the ledger's triggers.

**A policy point** is declared in code by the module that makes the judgement (`Module::policies`): a name, a description for people and agents, the facts the module supplies with their types, the decision when nothing is configured, and the words for a refusal.

```rust
PolicyPoint::new("purchase_receipt.over_receipt", "Asked for every line of a purchase receipt ...")
    .number("ordered", "Quantity ordered on the order line")
    .number("over_by", "By how much the line would be received beyond the quantity ordered ...")
    .number("over_percent", "over_by as a percentage of the quantity ordered")
    .refuse_when("facts.over_by > 0")
    .refusal("more would be received than was ordered")
```

The registry refuses a point declared twice, a fact declared twice, and a default that does not parse or reads a fact the point does not declare. Points are part of `meta.describe` (`policies`), so an interface or an agent can find what can be configured and which facts each condition may read.

**A policy rule** is a record of the entity `policy_rule`, managed like any other record:

```json
{ "company_id": "…", "point": "purchase_receipt.over_receipt", "effect": "allow",
  "condition": "facts.over_percent <= 5", "priority": 10 }
{ "company_id": null, "point": "purchase_receipt.over_receipt", "effect": "refuse", "condition": null,
  "message": "at most 5% more than ordered is accepted, not {over_percent}%", "priority": 20 }
```

`effect` is `allow` or `refuse`. `condition` is an expression over `facts` and `user` (the same `user` as in permission rules: the attributes, plus `id`, `email` and `name`); null means always. Without a `company_id` the rule applies to every company. `message` is what the user is told when the rule refuses, and `{fact}` in it shows a fact. A rule is checked when it is saved: the point exists, the condition parses, and it reads only facts that point declares. Because the condition is checked against the point, `point` and `condition` are sent together, on a change as well.

**Deciding.** The module asks at the moment of the judgement, inside its command: `ctx.decide(point, company_id, facts)`, or `ctx.policy(point, company_id)` once and `evaluate(facts)` for each line of a document. The active rules of the point that apply to the company are looked at in order of `priority`, lowest first; at the same priority a company's own rule comes before one for all companies. The first rule whose condition holds decides. If none does, the point's default decides. A refusal is a validation error with the rule's message, or else the module's own words.

- **Nothing undecidable loosens anything.** A rule whose condition cannot be worked out (a user attribute that is missing or null, a text compared with a number, a division by zero) does not match if it would allow, and does match if it would refuse. It is logged. This is the stance the permission rules take.
- **An administrator is subject to policies.** Permission rules say who may do what, and an administrator may do everything. A policy says how the company works, and that holds whoever asks.
- **Numbers in conditions are floating-point numbers**, including quantities and prices that are exact decimals everywhere else. That suits a threshold or a percentage. A condition decides whether something may happen; it never produces an amount.
- A module supplies exactly the facts its point declares. Anything else is an internal error, not a refusal: it is a mistake in the module.

The points so far:

| Point | Default | Facts |
|---|---|---|
| `purchase_receipt.over_receipt` | refuse when `facts.over_by > 0` | `ordered`, `already_received`, `receiving`, `over_by`, `over_percent` |
| `sales_delivery.over_delivery` | refuse when `facts.over_by > 0` | `ordered`, `already_delivered`, `delivering`, `over_by`, `over_percent` |
| `vendor_invoice.price_differs` | allow | `order_price`, `invoice_price`, `difference`, `difference_percent` |
| `sales_invoice.price_differs` | allow | `order_price`, `invoice_price`, `difference`, `difference_percent` |
| `stock.negative` | refuse | `on_hand`, `issuing`, `short_by`, `item_code`, `warehouse_code`, `context` |
| `vendor.inactive` | refuse when `facts.doc_type == 'purchase_order' && !facts.active` | `doc_type`, `vendor_code`, `active` |
| `customer.inactive` | refuse when `facts.doc_type == 'sales_order' && !facts.active` | `doc_type`, `customer_code`, `active` |
| `purchase_order.closed` | refuse when `facts.doc_type == 'purchase_receipt' \|\| facts.to_invoice <= 0` | `doc_type`, `complete`, `ordered`, `received`, `invoiced`, `to_invoice` |
| `sales_order.closed` | refuse when `facts.doc_type == 'sales_delivery' \|\| facts.to_invoice <= 0` | `doc_type`, `complete`, `ordered`, `delivered`, `invoiced`, `to_invoice` |
| `posting.date` | allow | `days_from_today`, `doc_type`, `posting_date`, `reversal` |
| `production_order.inactive_bom` | refuse when `!facts.active` | `bom_code`, `active` |
| `tax_code.use` | refuse when `!facts.active` | `code`, `rate`, `active`, `direction` |
| `fx.rate_differs` | allow | `currency`, `doc_type`, `stated`, `on_record`, `difference_percent` |
| `vendor_credit_note.without_return` | refuse when `facts.without_return > 0` | `crediting`, `returned`, `without_return`, `invoice_price`, `credit_price` |
| `sales_credit_note.without_return` | refuse when `facts.without_return > 0` | `crediting`, `returned`, `without_return`, `invoice_price`, `credit_price` |
| `purchase_order.reopen_on_return` | refuse (stays closed) | `ordered`, `received`, `returning` |
| `sales_order.reopen_on_return` | refuse (stays closed) | `ordered`, `delivered`, `returning` |
| `purchase_order.approve` | allow | `total_net`, `currency`, `lines` |
| `payment.post` | allow | `amount`, `currency`, `direction` |

Each default is what the module did before the point existed, so a company that configures nothing sees no change, in the same words. The points of returns and credit notes came with those documents; their defaults are the strict choice: no credit without a return, and a closed order stays closed.

- `vendor.inactive` and `customer.inactive` are asked when an order is approved (`doc_type` is the order's type) and when a receipt, delivery or invoice against it is posted, active party or not. By default only approving an order for an inactive party is refused; what was ordered before can still arrive, go out and be invoiced.
- `purchase_order.closed` and `sales_order.closed` are asked when a receipt, delivery or invoice is drafted or posted against a closed order; `complete` tells an order that closed by itself from one closed short by hand. A draft or cancelled order is never referred to: its lines can still change, or it never happened. Allowing it keeps the books and the order's progress right: the goods are counted on the order's lines like any others, valued at the order price, and owed for in goods received not invoiced. The order stays closed: posting against an order never reopens it (only a reversal that leaves it with goods not yet invoiced does, as before). By default an invoice is let through for what a closed order holds received and not invoiced, which before these points it never held, so that goods a company let in can always be invoiced.
- `posting.date` is declared by the kernel and asked by `document::create_posting` and `document::reverse_posting`, so every posting of every module is asked, reversals with `reversal` true. `days_from_today` is the posting date minus today in UTC. It does not replace the closed-period guard, which is an invariant.
- `production_order.inactive_bom` is asked when a production order is released; `tax_code.use` for every invoice line with a tax code when the invoice is posted (`erpcraft_tax::quote`). A draft shows tax with `erpcraft_tax::estimate`, which asks nothing.

Cancelling an order after goods were received or delivered against it stays refused, as an invariant: cancelled means the order never happened, yet the goods would remain in stock and in goods received not invoiced (or out of stock and in cost of sales) with no order left to invoice them against. Closing the order is the way to stop it; with `purchase_order.closed` a company decides what may still follow a closed order.

`stock.negative` is declared by the stock ledger and asked by `erpcraft_stock::post` for every issue that would take a level below zero. `context` says what is taking the stock out (`sales_delivery`, `production`, `adjustment`, `transfer`), so a company can let deliveries run ahead of receipts and still hold production to what is on hand. It is the one point whose decision a ledger checks again: the entry carries the rule that allowed it and the facts it was asked with, and the trigger refuses an entry that names no rule, one that is not such a rule, or facts that are not those of the movement. The trigger cannot evaluate the condition; it copies it onto the entry instead.

Where a company allows it, an order line can be received (or delivered) beyond its quantity. The goods come into stock at the order price like the rest, and what was received can be invoiced. An order line is **complete** when at least the quantity ordered has been received and everything received has been invoiced; so an over-received order stays open until the extra goods are invoiced too, and then closes by itself. The database no longer refuses a line received beyond its order: that was a judgement. It still refuses invoicing more than was received, and leaving value in goods received not invoiced: those keep the books right.

### Approvals

A policy rule has a third effect, `approve`: where it decides, the command does not run yet. It waits until enough other people have approved, and then runs as the person who asked, with exactly the input they gave. Because it is an outcome of the policy, every policy point has it, and no module does anything for it.

```json
{ "company_id": "…", "point": "purchase_order.approve", "effect": "approve",
  "condition": "facts.total_net > 10000", "approver_permission": "purchase_order:approve_large",
  "approvals_needed": 1, "approval_days": 14,
  "message": "an order of {total_net} {currency} needs a controller's approval" }
```

Who may approve is `approver_role_id` (holders of that role for the request's company or for all companies), `approver_permission` (whoever holds that permission for the company; any name, it need not belong to a command), or both, either sufficing; a rule with effect `approve` must name one, and only such a rule may. `approvals_needed` distinct people must approve (1 to 10, default 1); `approval_days` is how long the request waits (default 14). An administrator may approve any request; nobody, administrators included, may approve their own. An undecidable `approve` rule matches, like a refusal: nothing undecidable loosens anything.

**The pending response.** A command that needs approval answers HTTP 202 Accepted (over MCP: the tool result, `isError` false, with the same object as `structuredContent`):

```json
{ "status": "pending_approval", "preview": false,
  "approval_request": { "id": "…", "command": "purchase_order.approve", "point": "purchase_order.approve",
    "message": "an order of 20000.0000 USD needs a controller's approval",
    "facts": { "total_net": "20000.0000", "currency": "USD", "lines": "1" },
    "approvers": { "role_id": null, "permission": "purchase_order:approve_large", "approvals_needed": 1 },
    "expires_at": "2026-03-29T…Z" } }
```

`facts` are the facts as a person reads them, all text. `status` is the one key to look at: no command's own result has `status: "pending_approval"`. Asking again (the same person, command and input, or the same idempotency key) while the request is pending answers with the same request. A preview of such a command answers 200, `{"preview": true, "result": <the object above with "preview": true and "id": null>}`, and creates nothing.

**Recording.** The command's transaction is rolled back and the request is kept in `approval_request`: company, command, input, who asked and when, the idempotency key, the point, the rule, the facts it decided on, the rule's approvers, and `expires_at`. The audit log has the asking as a command with outcome `error` and code `pending_approval`, and the outbox has `approval.requested`.

**Deciding.** `approval.list` (scope `to_approve`, the default: pending requests the caller may approve; `mine`: the caller's own; `all`: with `approval:read` for all companies), `approval.get`, `approval.approve` (`id`, optional `reason`) and `approval.reject` (`id`, `reason` required). A person decides a request once (`approval_decision`); someone who may not approve it is refused, and one who cannot see it is told it does not exist. Each decision is an outbox event, `approval.approved` or `approval.rejected`. A rejected or expired request is final; the command can simply be asked again, which makes a new request.

**Running.** The approval that makes up the number needed runs the command, inside `approval.approve`'s transaction, behind a savepoint:

- as the requester, with the recorded input, after checking their permission for it again;
- with approval granted for that point and those facts: when the command asks the point again, a rule that would ask approval allows instead. If the facts are no longer the same (the order was edited, an amount changed) the approval does not apply, the run is refused and the request is marked `stale`;
- once: the request moves to `executed` with the command's result stored on it, or `failed` (with the error) when the command refused. The decision stands either way. A second approval of a request that is no longer pending is refused (409);
- audited as the requester's command, with the approval request among its subjects, next to the approver's `approval.approve`, so a record's history shows both; its outbox event (topic: the command) carries `approval_request_id` and `approved_by`.

Statuses: `pending`, `executed`, `failed`, `stale`, `rejected`, and `expired`, which is a pending request past `expires_at` (shown so; the row stays `pending`).

**What cannot be approved.** A point declared `.not_approvable()` refuses a rule with effect `approve` when it is saved. `stock.negative` is one: its ledger checks the deciding rule again in a trigger, and an approval would not be a rule the trigger knows. Invariants are not policy points at all, so nothing a ledger forbids can be approved into the books.

The points that ask for approval in practice: `purchase_order.approve` (asked when an order is approved, after the vendor check; default allow), `payment.post` (asked when a payment is posted, `amount` being what it pays out or takes in, allocations plus on account; default allow), and any refusing point whose refusal a company would rather turn into a question, such as `vendor_credit_note.without_return`.

## Contracts for later

Recorded now so that later work does not break them.

**The module boundary.** Modules in this repository use only the public module API, as an outside module would. If the reference ERP needs something the API lacks, the API grows. Compiled modules are trusted server code: the module API is an architectural boundary, not a security sandbox.

**Module upgrades.** A module's name owns its migrations and must never change. Today the registry checks only that a module's dependencies are registered before it. Still to define: version compatibility between a module and the kernel API, and disabling a module while its historical documents stay readable.

**Configuration is not code.** New behavior is a Rust crate. A custom field, an approval threshold, a numbering series or a print template must not need a rebuild: these will live in the database and extend the registry at startup. Print templates and policy rules already do; see [Printing](#printing).

**Cross-module flows are the real test.** The purchasing flow is the first: purchase order → partial receipts → vendor invoices, across three documents, two ledgers and three modules, with each step reversible. It needed one addition to the kernel, document links. Still missing from it is the return of goods after they were invoiced.

**Agents at scale.** A full ERP will have hundreds of commands. `meta.describe` already takes a module name so an agent can load one module at a time, and the MCP tool list leaves out commands the caller can never run. Two discovery tools (`erpcraft_modules`, `erpcraft_tools`) let an agent load the tools of one module or for one resource instead of the whole list. What a module is for is still derived from what it holds; a module has no description of its own to show.

**Rules and scripts are separate decisions.** Rules use CEL, behind `erpcraft-rules`, so the engine can be replaced; stored rules would then need converting. The script engine is not chosen.

- A *rule* is one expression: a row permission, a list filter, a validation, an approval threshold, a computed value. Rules that filter lists must compile to a SQL `WHERE` clause, so the engine must expose its syntax tree and must not allow loops.
- A *script* is custom logic that runs without a Rust rebuild. It must be sandboxed.
- Whatever is chosen goes behind its own crate, so the kernel never depends on an engine directly.

Four engines were compared hands-on in November 2026 (CEL, Rhai, Cedar, Monty). The results and the test code are in [docs/research/2026-11-rule-engines](docs/research/2026-11-rule-engines/README.md).

## Known gaps in what is built

- Manufacturing: the upgrade from separate materials and finished goods accounts gave each product of an existing bill of materials its own account rule. A bill created later posts its product to the company's default inventory account unless an account rule is added for that item; nothing remembers that the company kept finished goods apart.
- An outbox pass keeps one database connection for its whole length while each handler may use others, so a slow subscriber with a small pool can make requests wait.
- The guard on stock levels stops plain SQL from changing them, but not another database trigger.
- Webhooks: a webhook that keeps failing is retried without limit, at most hourly per event, until it is fixed, deactivated or deleted; nothing switches it off by itself. Events that pass while a webhook is inactive are not sent later, and there is no way to send an event again by hand. Secrets are not encrypted in the database. Delivery to a message broker is not built; it would be another subscriber. The refusal of an address does not say why, but how long it takes still can: a name that does not exist may answer at another speed than one that leads inward. The list of blocks that are not globally reachable is compiled in and follows the registries only with a new release; a Teredo address is refused even where what it carries is public.
- A refused stale edit does not say what the other person changed.
- A caller that sends no `_version` when updating or deleting a plain record is not checked for staleness.
- A plain record can be deleted only while nothing refers to it. There is no way yet to retire a record that is in use, such as an account that should take no new entries, except where its entity has a field for it.
- Payments: a payment settles items of one party; money on account is a credit of that party, applied to its invoices by hand (`payment.apply_credit`), nothing applies it by itself. A small difference left of an item is written off with the payment (`write_off` on an allocation, to the account for `write_off`), as far as the policy `payment.write_off` allows (nothing unless configured); it is written off gross, without taking back the tax on it. An early-payment discount is taken only with the payment that settles the rest of the item, up to what the item's terms offer; a credit note posted after a discount was taken credits the invoice line's tax as far as the discount left any, but its own amount is not reduced by the discount. Instalments: a part's discount is not built, so there is no discount per instalment. A credit note reduces the earliest parts first, like an unnamed payment, not the last. `payment.draft` proposes the whole item, not the part falling due. The e-invoice export states only the last due date and no terms per part (BT-20 text). An invoice's parts follow its terms; they cannot be set by hand on one invoice. Aging counts a payment on account, which has no due date, as due on its date.
- Tax: tax rules propose a line's code from its party, item or charge or the company's default; nothing looks at the parties' countries or VAT identifiers, and a line cannot say "no tax" where a rule proposes one (it takes a 0 % code instead). A rule for a record that was deleted stays and matches nothing. A rate change means a new code. Tax-inclusive prices, tax on freight, exemption certificates and jurisdiction lookup are not there. Tax the buyer accounts for (reverse charge, use tax) is a property of the code: nothing chooses it from the parties' countries or VAT identifiers, and the tax report shows it under its code, in both directions, without boxes of its own (a country module maps codes to a return's boxes). An early-payment discount on an invoice with such tax does not reduce the tax the company assessed. Orders carry no tax, only invoices do.
- Sales: a credit note always refers to an invoice and takes back its lines' tax; a credit not tied to an invoice (goodwill, a rebate over many invoices) is not built.
- Returns and credit notes: the facts of a credit without return are about one line, not the whole credit note or the party. A return of goods that are no longer on hand goes through the stock ledger like any issue, so the company's `stock.negative` policy decides (context `purchase_return`). Nothing reports goods returned and not credited, though GRNI shows their value. Over-receipt is judged against what was received and kept, so replacements for returned goods are not over-receipts.
- Purchasing and sales share about 250 lines of near-identical order-progress code. It should move into a shared crate once a third order-like document shows what is really common.
- Purchasing: A credit note for a landed-cost charge credits price variance (or `landed_cost_variance`) and does not take the cost back out of the stock it raised. A landed-cost invoice cannot be reversed once the goods it raised have all left the warehouse. A receipt can be reversed only before its invoice (afterwards goods go back with a purchase return); goods are invoiced only after they were received: an invoice before receipt needs an account for goods invoiced not received and a way to clear it against the receipt, which is an accounting feature, not a policy switch. Accounts come from one settings record per company.
- Exchange rates: a posting uses the rate it states, or else the latest rate on record for that date. Rates are entered by hand; the opposite direction and cross rates are not inferred. A rounding difference in a journal entry needs an explicit line.
- Foreign currency: open items are revalued at a period end (`fx_revaluation`, reversed the next day), but foreign bank balances and other monetary balances are not. The reversal is dated the day after the revaluation, which is the first day of the next period only when the revaluation is dated at a period end. A payment in one foreign currency for items in another, or a bank charge taken out of a foreign payment, is not built. Purchase price variance includes the exchange difference between receipt and invoice. Non-recoverable tax and freight are not added to the cost of goods. The stated rate on a receipt applies to all its lines; a receipt in parts at different rates averages the value in GRNI per order line.
- Intercompany postings are not modeled.
- Year-end close: a fiscal year is one fiscal period, so a year kept as twelve monthly periods cannot be closed in one step. The whole result goes to one equity account; there is no split across several accounts and no closing by dimension. An income or expense account that has a balance and was made inactive must be reactivated first. Reports recognize closing entries by the command name on their posting (`year_end_close.`), and `gl.trial_balance` and `gl.account_ledger` show them like any other entry. Closing a later year does not check that the earlier ones are closed.
- Manufacturing: one step, one warehouse, no routing, labor or overhead; the product is worth exactly the components consumed. An order whose component quantities do not come out to 6 decimals is refused at release. A completed order cannot be reversed once any of the product has been used.
- Reports that total a ledger (trial balance, balance sheet, income statement, stock on hand, tax report) are refused to a user whose right to read that ledger depends on a row condition, because a total cannot honour it.
- Single sign-on: nothing limits how often sign-in can be tried, by password or through a provider, and the record of sign-ins (`auth_event`) is never trimmed; both belong in front of a server on the open internet. Login attempts and login codes that nobody came back for are removed when the next sign-in starts, not by the clock. A user who has never signed in through a provider is matched to its identity by email address at the first sign-in, so whoever holds the address at a provider that names its domain at that moment gets the account; bindings made before this change do not exist, so every user who signed in before is bound by their next sign-in. Whoever holds `sso_provider:manage` can sign in as any user who is not an administrator (see above); there is no lesser permission for, say, renaming a provider. Signing out here does not sign out at the provider, and a user removed at the provider keeps the tokens already issued until they expire or the user is deactivated here. Password sign-in cannot be switched off for an installation, and a password once set cannot be removed. Client secrets are not encrypted in the database. A provider whose issuer differs per tenant (Microsoft's `common` and `organizations` endpoints) is not supported: use the tenant's own issuer. The failure page is fixed at `/sign-in`. Not built: SAML, SCIM, sign-in with a client assertion or a certificate in place of a client secret, and a command that tests a provider's settings before somebody tries to sign in.
- Canada: the provincial sales taxes are set up at their general rates only; reduced rates, exemptions and the rules for what is taxable in each province are the person's choice of code per line. PST that is not recoverable is expensed on 6130 when it is on goods for stock as well, rather than added to the goods' cost. Goods bought for resale are normally bought PST-free under the business's registration, which the template leaves to the person entering the invoice.
- Tax groups: the US example group is a shape, not a jurisdiction; nothing looks up the rates of a place. A component's rate change is a new code, and a group keeps its components as they are; changing a group's components changes what later invoices charge, not what was recorded. Groups are one level deep.
- Party data: a document keeps one billing and one shipping address; there is no delivery address per line, and a purchase document keeps no ship-to of the company's own (the receiving warehouse is not an address). A delivery keeps the customer's default billing address, not the one its order named. The e-invoice export writes no delivery address (BG-13) yet, though the invoice keeps one. Sales and purchasing call the party crate, so the party module must be installed with them. There is no generic list or search over party rows (party data is read per party). Setup templates create no party data.
- Electronic invoices: no XSD validation and only the rules the README lists (no code lists, no rules for the categories AE, K, G, L and M beyond rate and reason); the official validator is a separate step. Writing a ZUGFeRD / Factur-X PDF (PDF/A-3 with XMP metadata) is not built, nor sending or receiving (Peppol, e-mail, public buyers' portals). Incoming credit notes, document level allowances and charges, attachments, invoicing periods and gross prices are not imported. A tax group cannot be written. Requests are limited by the server's request size (2 MB) before the module's own limits.
- Fixed assets: a vendor invoice cannot capitalise an asset yet, because purchasing posts what it buys to goods received not invoiced and inventory, never to an account of the line's choosing; until it can, a journal entry moves the cost to the asset account and is capitalised. An asset is capitalised from one ledger line, whole: neither several lines nor part of one. Amounts are in the accounting currency only. Declining balance works in calendar years, not the company's fiscal year; "this year's depreciation" in the register is from 1 January too. No impairment, revaluation, partial disposal, transfer between classes or companies, separate tax and book depreciation, or units-of-production method. A disposal books proceeds to an account; it does not draft the sales invoice, and posts no tax. Only the latest depreciation run can be reversed.
- Banking: one statement per file; no MT940, BAI2 or CODA; no automatic matching (every match is accepted by a person or an agent through `bank_statement.match`); a line pays open items through one payment drafted for them, never part of a payment or several payments; a posted statement cannot be reversed; statements are entered in order and only the latest can be edited; a file in a currency other than the account's is refused, not converted; no direct bank connection.
- Printing: no PDF on the server, no images or logos other than what a template links to, no letterhead or address of the company or the party (neither is stored yet), and the built-in templates are in English. An image may still be loaded from another site (`img` with an https source is allowed), and a template's own stylesheet is dropped: it styles with the built-in classes and `style` attributes. A template cannot be tried on a document before it is saved. Template records are shared: there is no right to manage only one company's templates.
- Stock: no units of measure beyond one per item, no lots or serial numbers, no reserved stock. A receipt cannot be reversed once part of it has been issued; goods still on hand can be returned instead, and a revaluation document does not exist yet.
- Policies: a rule can allow, refuse or ask for approval. It cannot warn or set a value (a tolerance, a default). A decision is not recorded on its own: a refusal is in the audit log as a failed command, with its message; that a rule allowed something is not noted anywhere. A company's active rules are read from the database once per command, whatever points the command asks, and their conditions are parsed through the rule cache; nothing is kept between commands. A condition is checked for the facts it names when it is saved, but not for their types, so `facts.ordered == 'x'` is saved and then never decidable. Policy rules are shared records: whoever may write them may write them for every company; there is no right to manage only one company's policies. A rule has no validity dates. Approvals: approvers are not told by the system other than through the outbox (`approval.requested`); there is no delegation of approval, no escalation when a request waits, and a request does not say which other commands are waiting on the same record. The facts are scalars about one line; a condition cannot see the whole document, the vendor or the item. A party's inactivity, a closed order, a posting date, a retired bill or tax code and the price of an invoice are policy points; other refusals that are judgements may remain compiled in where nobody has asked for them yet (for example the line limits of a document, or a payment allocated in full). `posting.date` measures from today in UTC, not in the company's time zone, which is not recorded. The facts of `purchase_order.closed` and `sales_order.closed` add up quantities across lines, whatever their items. A document's form offers only open orders to refer to, even where the company allows a closed one.
- Stock below zero: the ledger checks that the rule an entry names is an active allowing rule of `stock.negative` for the company and that the facts on the entry are true, but not that the condition held for them; that is decided in `erpcraft_stock::post`. Raw SQL can still go below zero by naming such a rule with true facts. What it cannot do is hide it: the entry keeps the facts and the condition, so the condition can be asked again. The fact `context` is the caller's word, and user attributes a condition reads are not recorded; entries from before the facts were kept have none. A rule that an entry names can no longer be deleted, only made inactive. The estimate is the last receipt cost in the same warehouse; the cost of the item elsewhere is not looked at. A reversal that would leave quantity and value pointing different ways is refused (for example taking back an issue after the level went further below zero at another cost), as is taking back a receipt once its goods have moved on. The cost of sales of goods delivered before they arrived stays at the estimate; the difference shows as stock variance, not as cost of goods sold. Nothing reports levels below zero or how long they have been so, and there is no limit by value. A reversal is not asked the policy.
- Over-receipt and over-delivery: goods beyond the order are valued at the order price, and the order's own quantity is not changed. Nothing reports lines received beyond their order. Sales has no policy on the invoice price yet.
- The MCP endpoint answers each request with one JSON object: there are no server-sent streams, no `subscriptions/listen` and no change notifications, so a client learns of new tools when the `ttlMs` of its list runs out. Elicitation, sampling, logging, completion and tasks are not offered. It was tested by speaking the protocol directly, not with an official client.
- Posting cost per line: a journal entry of 100 lines posts in about 60 ms, 0.3 ms per line. It was 1.3 ms before ledger entries were written in one statement, and 0.5 ms while the ledger's rules were checked by a trigger per row; they are now checked once per statement. Of what remains, about 0.1 ms is the four foreign keys of an entry. The lines of purchasing, sales, inventory and manufacturing documents, the values a posting sets on them, order progress and stock entries are written one statement per document, not per line. Stock entries go in one `INSERT ... SELECT ... ORDER BY line_no`, so the ledger's trigger still values them one row at a time in the order given; the levels they find are read once under the lock and followed line by line, so a policy on stock below zero is asked with what each line will find, and the trigger checks it. What is left per line: the account rule asked for each line's inventory account.

## Milestones

1. Done: the headless ledger slice. Kernel, general ledger, journal entries, HTTP and MCP.
   Also done: identity in its own crate, row and field permission rules, list search and filters, record history, outbox delivery, per-user access in the description.
2. Admin UI generated from the registry.
3. Done: the stock ledger, and the purchasing and sales flows across modules. Also done: open items, payments and tax. Next in this line: returns, country modules.
4. SAML and SCIM. (Single sign-on with OpenID Connect, and row and field permission rules, are done.)
5. Done: manufacturing (bills of materials, production orders), and the financial reports (balance sheet, income statement, account ledger).
