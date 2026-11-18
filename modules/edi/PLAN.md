# erpcraft-edi: build plan

The design is in ARCHITECTURE.md, "Electronic data interchange (EDI)". This file is the order to build it in. Each phase ends green (`clippy -D warnings`, its tests, the workspace builds) and is usable by itself: after phase 1 a company can already take a partner's orders without a translator, carrying the files by hand.

Run everything as `nice -n 10 cargo ... -j 4 -- --test-threads 4`.

## What the spike left

| File | Holds |
|---|---|
| `src/segment.rs` | `Delimiters`, `Segment`, `Element`, the one `Tokenizer`, `write_segment`, the limits and `Budget`, `Charset` |
| `src/x12.rs` | `read` (ISA/GS/ST envelopes, errors kept by 997 and TA1 code), `Envelope`, `write` |
| `src/x12/po850.rs` | `PurchaseOrder` and `read`, which returns the order and a `SetVerdict` |
| `src/x12/ack.rs` | `FunctionalAck::of`, `.body`, `.read` (997) |
| `src/edifact.rs` | `read` (UNA/UNB/UNH envelopes, errors by CONTRL code), `number` |
| `src/gs1.rs` | `check_digit`, `is_valid`, `sscc` |
| `tests/spike.rs`, `tests/fixtures/` | 13 tests on a made-up 850 and ORDERS |

The crate has no database, kernel or network dependency yet, and is not registered in `apps/server`. Keep the format layer that way: everything under `src/x12`, `src/edifact`, `src/segment.rs` and `src/gs1.rs` stays free of `sqlx` and the kernel, so it can be fuzzed and tested without a database.

Rules that hold in every phase:

- **Typed in the middle.** A reader turns segments into a typed message (`PurchaseOrder`); a writer turns a typed message into segments. Nothing maps segments straight to database rows, and nothing maps a document straight to segments. The typed message is where a partner's guide is applied and what a proposal shows.
- **Every writer is checked by its reader.** A message to send is written, read back with the reader an incoming one would meet, and compared with what was meant. A test per message does the same on a fixture.
- **A reader returns what it could not place.** `unhandled` and `notes` exist on every typed message.
- **Fixtures are made up.** No real company, retailer or trademark; GTINs begin with 04 (restricted circulation), GLNs are sample numbers.
- **X12's licence is a hard constraint.** (See docs/research/2026-11-edi, C.10.) The code is a generic tokenizer and envelope reader plus typed readers for the segments it acts on, described in our own words. No dictionary of the standard, no table of code-list descriptions, no text of an implementation guide, in code, comments, tests, built-in guides or documentation. A partner's segments and codes are that partner's guide, which the company enters as data. Fixtures are written by us, never copied from a guide or a reference site. A reviewer refuses a change that breaks this.
- **No new engine.** A guide's conditions and values are `erpcraft_rules::Expression`. No script language, no XSLT, no mapping DSL.

## What other crates must grow

The module uses only the public module API. These are the additions it needs, each small, each in the phase named. Agree them with whoever owns the crate before building on them.

| Phase | Where | What | Why |
|---|---|---|---|
| 1 | `modules/sales` | `customer_reference` (text) on the sales order, copied to its deliveries and invoices and into their summaries; searchable | Every company needs the customer's order number on an order, EDI or not. Until it exists, phase 1 writes it into the memo. |
| 1 | `modules/sales` | `requested_date` (date) on the sales order header | Ship windows. Without it the proposal shows the dates and the draft's memo carries them. |
| 2 | `erpcraft-kernel` | A way for a subscriber to run a command as a named user (`Kernel::run_as(user_id, command, input)`), with that user's permissions and all policies | Automation acts as the partner's `run_as_user_id`. Check first whether the outbox worker already has it. |
| 5 | `erpcraft-kernel`, `apps/server` | `Module::jobs()`: named periodic work with an interval, run by the server's background task, one server at a time (a lock row, as the outbox uses) | Polling a mailbox, retrying a send, noticing a late acknowledgement. Until then `edi.poll` is a command an external timer or an agent calls. |
| 5 | new crate `erpcraft-net`, or `erpcraft-core` | The public-address allow-list of `modules/webhooks/src/address.rs`, moved where two modules can use it | SFTP hosts and AS2 URLs must not reach the internal network. |
| 6 | `apps/server` | Mount `erpcraft_edi::as2::router(kernel)` beside the API and auth routers | AS2 is an HTTP endpoint that is not a command: its body is a MIME message and its caller has no token. |

## Phase 1: the log, partners, and an 850 into a sales order proposal

The company can receive a partner's 850 by upload (or an agent's call), see what it says, turn it into a draft sales order, and hand back a 997.

**Format layer**

- `src/x12/ta1.rs`: write a TA1 for an interchange with `InterchangeError`s or `ISA14 = 1`; read one.
- `src/x12/ack.rs`: the 999 (005010) as a second form of the same `FunctionalAck` (`IK3`, `IK4`, `IK5`, `AK9`; `CTX` read and ignored). `FunctionalAck::body` takes which form to write.
- `src/explain.rs`: `explain(&Segment, context) -> String`, one sentence per segment for the tags the readers know (`PO1*1*48*EA*12.5` becomes "Line 1: 48 EA at 12.5"), the bare tag otherwise.
- Fuzzing: `fuzz/` with `cargo-fuzz` targets `x12_read` and `edifact_read` (not run in CI; the README says how). The spike's mutation test stays as the cheap guard.

**Migration `0001_edi.sql`** (module `edi`)

- `edi_partner`: `id`, `company_id`, `code`, `name`, `customer_id` (references `customer`), `syntax` (`x12`, `edifact`), `usage` (`test`, `production`), `our_qualifier`, `our_id`, `their_qualifier`, `their_id`, `our_group_id`, `their_group_id` (X12's GS02 and GS03; the interchange ids when empty), `version` (`004010`, `005010`, `D96A`), `ack_form` (`997`, `999`, `contrl`, `none`), `ack_hours` (default 24), `element_separator`, `component_separator`, `repetition_separator`, `segment_terminator`, `line_breaks` (bool), `run_as_user_id` (references the user directory), `active`, `created_at`. Unique `(company_id, code)`. Unique `(their_qualifier, their_id, our_qualifier, our_id, usage)` where active: an interchange finds one partner.
- `edi_interchange`: `id`, `company_id` (null until routed), `partner_id` (null when unroutable), `direction` (`in`, `out`), `syntax`, `control`, `usage`, `sha256`, `size`, `payload bytea`, `transport` (`manual` now), `transport_ref`, `state` (`stored`, `unreadable`, `unroutable`, `read`; out: `ready`, `sent`, `failed`), `error`, `received_at` or `created_at`, `sent_at`. Unique `(partner_id, direction, sha256)`. A trigger refuses any change to `payload`, `sha256`, `direction` and `control`, and any delete.
- `edi_message`: `id`, `interchange_id`, `company_id`, `partner_id`, `direction`, `message` (`850`, `997`, `ORDERS`), `group_control`, `control`, `position` (which set of the interchange), `state`, `business_key` (the order number), `purpose`, `doc_type`, `document_id`, `answers_id` (the message this one answers), `superseded_by`, `findings jsonb`, `ack_status` (`A`, `E`, `R`), `ack_message_id`, `ack_due_at`, `created_at`. Unique `(partner_id, direction, group_control, control, interchange_id)`.
  - States in: `received`, `rejected` (syntax), `test`, `proposed`, `drafted`, `dismissed`.
  - States out: `invalid`, `ready`, `sent`, `acknowledged`, `acknowledged_with_errors`, `rejected`, `failed`.
- `edi_control`: `partner_id`, `kind` (`interchange`, `group`, `set`), `next_value`; primary key `(partner_id, kind)`.
- `edi_item_xref`: `id`, `partner_id` (null: every partner, for a GTIN), `item_id`, `kind` (`gtin`, `buyer_item`, `seller_item`), `value`, `partner_uom`, `units_per` (how many of the item's own unit one `partner_uom` is; default 1). Unique `(partner_id, kind, value)` with null as a value of its own (`NULLS NOT DISTINCT`). A check: a `gtin` is 8, 12, 13 or 14 digits; the command checks its check digit.
- `edi_uom_xref`: `partner_id` (nullable), `partner_uom`, `uom`. Unique `(partner_id, partner_uom)`.
- `edi_location_xref`: `partner_id`, `qualifier`, `code`, `address_id` (a `party_address` of the partner's customer, plain column, checked by the command). Unique `(partner_id, qualifier, code)`.
- `edi_document`: `doc_type`, `document_id` (primary key together), `partner_id`, `message_id`, `order_number`, `release`, `order_date`, `header jsonb` (the references and dates to echo, as sent). `edi_document_line`: `doc_type`, `document_id`, `line_id` (the document's line), `partner_line_id`, `product_ids jsonb` (as sent, in order), `partner_uom`, `partner_qty`, `partner_price`.

**Rust**

- `src/lib.rs`: `EdiModule` (`name` `edi`; `depends_on` `sales`, `stock`, `party`). Register in `apps/server/src/lib.rs` and the root `Cargo.toml`.
- `src/entities.rs`: `edi_partner`, `edi_item_xref`, `edi_uom_xref`, `edi_location_xref` as generic entities (`company_field` where they have one); `edi_interchange` and `edi_message` read-only, as `einvoice_import` is.
- `src/store.rs`: store an interchange (first, in a transaction of its own, before anything reads it), find a repeat by hash.
- `src/inbound.rs`: read on `spawn_blocking` behind `catch_unwind` (a panic is a refusal); route by the ISA; per group build the `FunctionalAck`; write the 997 as an `out` interchange with numbers from `src/control.rs`; set each message's state.
- `src/control.rs`: `next(conn, partner, kind)`, the row locked `FOR UPDATE`, taken as the last step before the bytes are fixed.
- `src/proposal.rs`: `Proposal { order: PurchaseOrder, customer, ship_to, lines: [{item, uom, qty, price, how_matched}], findings: [{severity, code, line, message}], would_draft }`. Items by `edi_item_xref`, trying the kinds in the order `gtin`, `seller_item`, `buyer_item` (phase 4 lets a guide choose); quantities and prices converted by `units_per`; the ship-to by `edi_location_xref`. Everything read with the caller's rights, as `einvoice.read` does.
- `src/draft.rs`: `document::create` of a `sales_order`, then `edi_document` and its lines.
- `src/policy.rs`: the points `edi.auto_draft` and `edi_order.duplicate`.

**Commands**

| Command | Does |
|---|---|
| `edi.receive` (`content` or `content_base64`, optional `company_id`) | Stores, reads, routes, acknowledges. Returns the interchange, its messages and their states. Sending the same bytes again returns the first result. |
| `edi.proposal` (`message_id`) | Read-only: the order as typed, what each part resolves to, the findings. |
| `edi.draft` (`message_id`, `items: {partner_line_id: item_id}`, `shipping_address_id`, `remember`, `accept_notes`) | Drafts the sales order; `creates`. Refused while a blocking finding stands. `remember` saves the choices as cross-references. |
| `edi.dismiss` (`message_id`, `reason`) | Ends a message nobody will draft. |
| `edi.explain` (`message_id` or `content`) | Each segment with what it says. |
| `edi_interchange.content` (`id`) | The bytes, base64 and as text: what to hand to the partner while there is no transport (the 997), and the evidence later. |
| `edi_interchange.mark_sent` (`id`) | A person says they delivered it. |

**Tests** (`apps/server/tests/edi.rs`, on the spike's fixture)

- receive → one interchange `read`, one message `proposed`, one 997 `ready` whose bytes read back as accepting set `0001` of group `417`;
- the same bytes again: the same ids, no second 997, no second control number;
- the same control number with other bytes: stored, `rejected`, nothing drafted;
- an unknown sender: `unroutable`, no 997, nothing else written;
- `ISA15 = T` to a production partner: acknowledged, state `test`, `edi.draft` refused;
- a broken set: 997 with `R`, AK3/AK4 naming the place, message `rejected`;
- proposal with an unknown item: a blocking finding on that line; `edi.draft` with `items` and `remember` drafts and a second order then resolves by itself;
- a case-pack line (`units_per` 6): 120 CA at 31.20 drafts 720 each at 5.20, and `edi_document_line` keeps 120 CA at 31.20;
- a price that does not divide (31.25 / 6): a blocking finding, never a rounded price;
- the draft's total equals the order's lines to the cent;
- a second 850 with the same order number: refused by `edi_order.duplicate`, allowed by a rule;
- a reader without `customer:read` gets a proposal without the customer;
- `payload` cannot be updated or deleted with SQL;
- two `edi.receive` at once for the same partner: two distinct 997 control numbers.

**Done when** the tests pass and, by hand over MCP: upload the fixture, read the proposal, draft, approve the order, download the 997.

## Phase 2: answering: 855, 856 and 810

**Format layer**: `src/x12/poack855.rs`, `src/x12/asn856.rs`, `src/x12/invoice810.rs`, each a typed message with `read` and `write`.

- 855: `BAK` (purpose, acknowledgement type `AD` with detail no change, `AC` with detail and change, `RD` rejected with detail), per line `PO1` echoed and `ACK` (`IA` accepted, `IQ` quantity changed, `IP` price changed, `IR` rejected, `IB` backordered, with quantity and date), `CTT`.
- 856: `BSN` (shipment id, date, time, structure code), the `HL` tree as a typed tree (`Shipment { orders: [Order { packs | items }] }`), `TD1`, `TD5`, `TD3`, `REF` (BOL, PRO), `DTM` shipped and estimated delivery, `N1` ship from and ship to, `PRF`, `MAN*GM*<SSCC>`, `LIN`, `SN1`, `PO4`, `CTT`. The writer numbers `HL01` and fills `HL02` from the tree; the reader rebuilds the tree and refuses a parent that does not exist or comes later. Both orders of pack and item are written: shipment, order, pack, item (pick and pack) and shipment, order, item, pack (standard carton), with tare (pallet) optional above pack.
- 810: `BIG` (invoice date and number, order date and number), `REF`, `N1`, `ITD`, `DTM`, `IT1` with product ids echoed, `PID`, `TDS` (total in cents, no decimal point), `TXI` or `SAC` for tax and charges, `CAD`, `CTT`.

**Migration `0002_shipment.sql`**

- `edi_settings`: `company_id` (primary key), `gs1_company_prefix`, `sscc_extension` (default 0), `next_sscc_serial`.
- `edi_shipment`: `delivery_id` (primary key, a `sales_delivery`), `partner_id`, `carrier_scac`, `carrier_name`, `service`, `bill_of_lading`, `tracking`, `shipped_at`, `estimated_delivery`, `gross_weight`, `weight_uom`, `state` (`packing`, `packed`).
- `edi_pack`: `id`, `delivery_id`, `parent_id` (a pallet), `kind` (`pallet`, `carton`), `sscc` (unique over the whole table, for good), `weight`. A trigger checks the check digit and refuses a change of `sscc`.
- `edi_pack_line`: `pack_id`, `delivery_line_id`, `qty`. A deferred trigger: when the shipment is `packed`, the quantities per delivery line add up to exactly what the delivery line delivered, and no carton is empty.

**Rust**: `src/outbound.rs` (generate, validate, envelope, store), `src/subscriber.rs` (wants `sales_order.approve`, `sales_delivery.post`, `sales_invoice.post`, `sales_credit_note.post`; acts only for a document with an `edi_document`, or one that follows such an order), `src/shipment.rs`, `src/label.rs`.

**Commands**

| Command | Does |
|---|---|
| `edi.generate` (`doc_type`, `id`, `message`) | Builds, checks and stores the message. With `preview`, shows segments and explanations and stores nothing. |
| `edi_shipment.pack` (`delivery_id`, carrier fields, `packs: [{kind, parent, weight, lines: [{delivery_line_id, qty}]}]`) | Records the packing; assigns each new pack an SSCC from `edi_settings`. |
| `edi_shipment.auto_pack` (`delivery_id`) | One carton per `units_per` of each line, for goods that ship in their own cases. |
| `edi_shipment.ship` (`delivery_id`) | Checks the packing is complete, posts the delivery through `sales_delivery.post`, generates the 856: one transaction, so the notice exists the moment the goods are posted as gone. |
| `edi.label` (`pack_id`) | The data of the GS1-128 label and its ZPL text. |
| `edi.pending` | Report: proposals waiting, messages `invalid`, sent and not acknowledged past `ack_hours`, deliveries posted with no 856 sent. |
| `edi_message.resend` (`id`) | The same bytes and control numbers again. |

Inbound 997s are now matched: `edi.receive` of a group `FA` sets `ack_status` on the messages it names.

**Policies**: `edi.send`, `edi.auto_approve`, `edi_invoice.before_asn`.

**Tests**

- each writer's output reads back equal, and an edited byte in a count or total is caught by validation;
- 855 for an order approved as received: every `ACK` is `IA`; with a quantity cut on a draft before approval: `IQ` with the new quantity, `BAK02` `AC`;
- 856 quantities equal the posted delivery in the partner's unit (720 each is 120 CA); `edi_shipment.ship` with one carton short is refused and the delivery stays a draft;
- SSCCs: valid check digit, never issued twice under concurrency (two `pack` at once), refused by the database when repeated;
- 810 `TDS01` equals the invoice's gross total in cents; an invoice with tax writes it; a tax group writes one `TXI` per component;
- outbound echoes: the `PO1`/`IT1`/`LIN` ids and `PO101` line ids are those of the 850 even after the cross-reference was changed;
- a 997 `R` from the partner sets the message `rejected` with the place in words; `edi.generate` again supersedes it with a new control number;
- `edi_message.resend` sends identical bytes;
- a delivery posted without packing shows in `edi.pending`;
- a test partner's messages carry `ISA15 = T`.

**Done when** the whole order-to-cash exchange runs on fixtures: 850 in, 997 out, 855 out, 856 out, 810 out, three 997s in, nothing pending.

## Phase 3: EDIFACT

Same typed messages, second syntax. `src/edifact/orders.rs` reads ORDERS D.96A into `PurchaseOrder` (generalised where needed: rename nothing, add what EDIFACT has and X12 lacks behind `Option`); `ordrsp.rs`, `desadv.rs`, `invoic.rs` write from the typed messages of phase 2; `contrl.rs` builds and reads CONTRL (`UCI`, `UCM` with action 7 acknowledged, 4 rejected, and the codes the envelope reader already keeps).

- `edifact::write`: UNA always written, UNB to UNZ, `UNT` counts, release characters, the decimal mark the partner's profile names.
- Qualifiers: `NAD` `BY`, `SU`, `DP`, `IV`; `LIN` with `EN` (EANCOM in D.96A: a GTIN as `EN`), `PIA` `SA` (supplier's number) and `IN` (buyer's); `QTY` 21 ordered, 12 despatched, 47 invoiced; `PRI` `AAA` net and `AAB` gross; `DTM` 137, 2, 10, 11, 35; `MOA` 77, 79, 125, 124, 86; `TAX` 7 `VAT`; DESADV `CPS`, `PAC`, `PCI` `33E`, `GIN` `BJ` for the SSCC.
- Routing by UNB sender and receiver with their qualifiers (14 is a GLN). `ack_form` `contrl` or `none`: many EANCOM partners want no CONTRL.
- Tests: the phase 1 and 2 tests again on EDIFACT fixtures; a decimal comma; released characters in names; ISO 8859-1 input; UNOC output refuses a character outside it.

**Done when** the same order-to-cash exchange runs on ORDERS, ORDRSP, DESADV and INVOIC, and the INVOIC's totals agree with the invoice as phase 2's 810 does. Note in the README that a German INVOIC is not an e-invoice under § 14 UStG after the transition unless agreed (the einvoice README has the dates).

## Phase 4: a partner's guide as data

Until here every partner is read and written the standard's way. Now what differs per partner is a record.

**Migration `0003_guide.sql`**: `edi_guide` (`id`, `code`, `name`, `syntax`, `version`, `base_guide_id`, `notes`), `edi_guide_message` (`guide_id`, `message`, `direction`, `spec jsonb`, unique on the three), `edi_partner.guide_id`.

**`spec`** has exactly three parts, each closed:

1. `options`: choices the standard leaves open, by name. Inbound: `item_match` (the order of kinds and the qualifiers that carry each), `ship_to` (role and id qualifier), `dates` (which qualifier is the requested ship date, the cancel date), `references` (which are kept to echo). Outbound: `hierarchy` (`SOPI`, `SOTPI`, `SOIP`, `SOI`), `pack_qualifier`, `ack_detail` (line level or header only), `free_text` (what replaces a separator in a description; identifiers are never altered), `uom` (the code written for a unit).
2. `constraints`: per segment and element, `required`, `not_used`, `min`, `max`, `codes`. Checked on what is written (a failure makes the message `invalid`) and on what is read (a failure is a finding on the proposal: the 997 answers the standard, not the guide).
3. `extra`: segments added at a named place of an outbound message, each element a constant or an `Expression` over a closed set of facts (`partner.*`, `order.*` as received, `document.*` summary fields, `line.*`), with an optional condition. This is what covers "`REF*IA` with our vendor number at the header".

`src/guide.rs`: load with its base (one level), check on save (the message exists, tags and positions exist in the typed writer's layout, expressions parse and read only declared facts), apply. `guides/x12_004010.json`, `x12_005010.json`, `edifact_d96a.json`: the built-in bases, loaded at start-up like setup templates.

**Commands**: `edi_guide.check` (`guide_id`, `content`: how a sample file reads under a guide, with every constraint it breaks), `edi_guide.derive` (`content`: proposes options from samples, such as which qualifiers the partner uses), `edi_guide.export` and `.import` (a guide as one JSON file, so guides can be shared between companies).

**MCP prompt** `edi_onboard_partner`: read the partner's guide and samples, create the partner, guide and cross-references, run `edi_guide.check` on the samples, generate test documents.

**Tests**: a guide that requires `REF*DP` makes an 850 without it a finding; a guide with `hierarchy` `SOTPI` writes pallets; an `extra` segment appears where named and a failing expression makes the message `invalid`, never a segment with an empty value; a guide that names a tag the message has not is refused on save; export then import gives the same guide.

**Done when** two made-up partners with different guides exchange the same order differently, with no code between them.

## Phase 5: SFTP

SFTP comes before AS2 on purpose. Walmart, Target and Amazon require AS2 software on Drummond's certification list, which ours is not, so those partners are reached over SFTP: their own where they offer it, a VAN's mailbox, or a certified gateway that forwards the bytes unchanged. Phase 5 must therefore be complete enough to run a large retailer on: acceptance below includes a mailbox with other parties' files in it.

**Crate**: see README.md, "Crates". Host keys pinned; no agent, no known_hosts file.

**Migration `0004_transport.sql`**: `edi_transport` (`id`, `company_id`, `kind` `manual`/`sftp`/`as2`, `host`, `port`, `username`, `host_key_sha256`, `inbound_dir`, `outbound_dir`, `inbound_pattern`, `outbound_name`, `after_read` (`delete`, `rename`, `leave`), `poll_minutes`, `active`), `edi_transport_secret` (`transport_id`, `kind` `password`/`private_key`, `secret`; not an entity), `edi_partner.transport_id`, `edi_interchange.attempts`, `.next_attempt_at`, `.last_error`.

**Rust**: `src/transport.rs` (the trait: `send(interchange)`, `fetch() -> files`), `src/transport/sftp.rs`, `src/jobs.rs`.

- Fetch: list, take at most 100 files of at most 4 MiB by their stated size, read each under the size limit, store it (committed) and only then delete or rename it; a file seen again is a repeat by hash. Names from the server are text to record, never paths.
- Send: write under a temporary name, rename to the final one; success is the rename. Failure: retry after 1 minute, doubling to an hour, 24 hours in all, then `failed` and the event `edi.send_failed`.
- Commands: `edi_transport.create`, `.update`, `.set_secret`, `.delete` (sensitive, permission `edi_transport:manage` for all companies), `edi_transport.test` (connects, lists, changes nothing), `edi.poll`, `edi.send_pending`.

**Tests** against an in-process SFTP server (the same crate's server side) on localhost with `ERPCRAFT_EDI_ALLOW_PRIVATE=1`: round trip; a host key that changed is refused; a private address is refused without the variable; a file that grows past the limit while read is refused; a crash between store and delete delivers once; a mailbox holding files addressed to other receivers leaves them untouched and stores them as `unroutable` only if the pattern matches; retry and give-up; `edi_transport.test` leaks no secret into the audit log.

**Done when** two erpcraft servers trade the order-to-cash exchange through one SFTP server, unattended.

## Phase 6: AS2

**Who this is for.** OEMs and smaller partners, who ask that AS2 interoperates and not that it is certified. It is not the way to Walmart, Target or Amazon (phase 5 is). Certifying it with Drummond is a separate, later decision for the project's owner, with a fee and a test event twice a year; nothing in this phase assumes it, and `edi_transport` records `certified` as false so that `edi_transport.create` can warn when a partner's guide demands certification.

**Crates**: see README.md. Before starting, evaluate `asx-rs` (docs/research/2026-11-edi, A): if it has matured, does not force OpenSSL and passes the tampering tests below, use it and drop `as2/cms.rs`. This phase is the riskiest; build it as `src/as2/` behind its own feature until its interoperability test passes.

**Migration `0005_as2.sql`**: on `edi_transport`: `as2_url`, `our_as2_id`, `their_as2_id`, `their_certificate` (PEM), `sign_algorithm`, `encrypt_algorithm`, `compress`, `mdn` (`sync`, `async`), `allow_legacy` (SHA-1 and 3DES, off); our certificate and key in `edi_transport_secret`; `edi_as2_message` (`message_id` unique per partner, `interchange_id`, `mic`, `mdn_state`, `mdn bytea`).

**Rust**: `as2/mime.rs` (bounded MIME reader and writer), `as2/cms.rs` (sign, verify, encrypt, decrypt; the one place that touches the crypto crates), `as2/send.rs`, `as2/receive.rs` (`POST /edi/as2`), `as2/mdn.rs`.

- Receive: at most 8 MiB read; `AS2-To` and `AS2-From` name one transport; decrypt, verify against that partner's certificate, inflate under the limit; the interchange's sender must be a partner of that transport; store; answer with a signed MDN carrying the MIC. A repeat of a `Message-ID` answers the same MDN and stores nothing.
- Send: sign, compress if asked, encrypt, POST with a time limit and no redirects to a public https address; a signed MDN whose MIC matches is the receipt; an asynchronous MDN is matched by `Original-Message-ID`.
- `as2.certificate` (`transport_id`): our public certificate to give the partner; `as2.rotate` to add a second certificate before the first expires; `edi.pending` lists certificates expiring within 30 days.

**Tests**: two servers in one test trade signed and encrypted messages both ways, sync and async MDN; every tampering case (body, signature, MIC, wrong certificate, `AS2-From` of another partner, an interchange of another partner inside a valid AS2 message) is refused; legacy algorithms refused unless allowed; a compressed bomb is refused; a repeat is not stored twice. **Interoperability**: run by hand against OpenAS2 and one Python AS2 library (both GPL: run as separate programs, never linked or copied from), both directions, and record the versions and the date in the README, as the e-invoice README records its validator run.

**Done when** the interoperability run passes and its record is in the README.

## Phase 7: 846, 860, 820

- **846 inventory advice, out**: from `stock.availability` for the items a partner has cross-references for, on a schedule per partner (`edi_partner.inventory_minutes`) or `edi.generate`. `BIA`, `LIN`, `QTY` (`33` available), in the partner's unit, rounded down to whole partner units. EDIFACT: INVRPT.
- **860 change, in** (and an 850 with purpose `01` or `05`): `src/x12/change860.rs`; a proposal of changes against the order `edi_document` names: per line add, delete, quantity, price, date (`POC`). A draft order is changed through `document::update`; an approved one is a finding for a person, and what follows is theirs to decide (an 865 answers it). EDIFACT: ORDCHG.
- **820 remittance advice, in**: `src/x12/remit820.rs`; `BPR`, `TRN`, `RMR` per invoice with paid amount and discount, `ADX` adjustments. A proposal like a bank line's: the open items it names by invoice number, drafted through `payment.draft`; an adjustment (a chargeback deducted from the payment) is a finding with its reason code, never booked by itself. EDIFACT: REMADV.
- Policies: `edi_order.change` (facts: what changed, the order's state), `edi_remittance.deduction` (facts: amount, reason code; default refuse, so a deduction always reaches a person).

**Tests**: 846 quantities equal `stock.availability` converted; an 860 on a draft changes it and on an approved order changes nothing; an 820 that pays three invoices and deducts one chargeback drafts a payment for the three and reports the deduction.

**Done when** the three run on fixtures in both syntaxes.

## After the plan

Buying by EDI (850 out to vendors; 855, 856 and 810 in) reuses every reader and writer above, which already exist both ways because each writer is checked by its reader. It needs proposals against purchase orders, as `einvoice.import` has for invoices. Not planned in detail.
