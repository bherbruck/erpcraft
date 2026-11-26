# erpcraft-edi

Electronic data interchange: ANSI X12 and UN/EDIFACT read and written by the ERP itself, so that a company trades orders, ship notices and invoices with its partners without a value-added network or a translation service in between.

What is built today (PLAN.md phases 1 to 4):

- the format layer: one tokenizer for both syntaxes, which takes the separators from the file (the ISA of X12, the UNA of EDIFACT); X12 interchanges, functional groups and transaction sets, read and written; the 850 purchase order as a typed structure; the 997 and the 999 functional acknowledgements, built for what was received and read back; the TA1 for an interchange; a sentence per segment (`explain`); EDIFACT interchanges and messages, read (an ORDERS in the tests); GS1 check digits and the SSCC of a carton;
- the module (`edi`): trading partners and their numbers for items, units and places as entities; the log of every interchange with its bytes, stored before it is read and never changed; `edi.receive` (store, read, route, acknowledge), `edi.proposal`, `edi.draft` (a sales order through the normal document code), `edi.dismiss`, `edi.explain`, `edi_interchange.content` and `.mark_sent`; the policy points `edi.auto_draft` and `edi_order.duplicate`;
- EDIFACT messages (phase 3, the format layer): ORDERS read into the `PurchaseOrder` the 850 is read into (and written, for test files), ORDRSP, DESADV and INVOIC written from the typed messages of the 855, 856 and 810 and read back to check them, and CONTRL built for an interchange and read; `edifact::write` writes a UNA always, counts the UNT and UNZ, releases separators in data, writes numbers in the partner's decimal mark, and checks text against the character set the UNB names (UNOA, UNOB, UNOC as ISO 8859-1, UNOY as UTF-8). The codes the messages act on are mapped to the typed messages' own in `src/edifact/common.rs`, in this module's words;
- EDIFACT partners (phase 3, the module): a partner of syntax `edifact` and version `D96A`, with its `decimal_mark` and `charset` (migration 5), is routed by the UNB's sender and recipient with their qualifiers; its ORDERS is proposed and drafted as an 850 is; approving, shipping and invoicing write the ORDRSP, DESADV and INVOIC where the 855, 856 and 810 would be; a CONTRL goes back for every interchange that is not only CONTRLs when `ack_form` is `contrl`, and the partner's CONTRLs mark what they answer acknowledged or rejected. The INVOIC also names the currency, the buyer and the seller as the order did, and its tax as VAT;
- partners' guides as data (phase 4): `edi_guide` and `edi_guide_message` (migration 6) with a spec per message and direction in three closed parts. `options`: received, `item_match`, `ship_to`, `dates`, `references`; sent, `hierarchy` (`SOTPI`, `SOPI`, `SOIP`, `SOI`), `pack_qualifier`, `ack_detail`, `free_text`, `uom`. `constraints`: `required`, `not_used`, `min`, `max` and `codes` per segment, element and component; what is sent and breaks one is `invalid`, what is received and breaks one is a note on the proposal (the acknowledgement answers the standard, not the guide). `extra`: segments added after or before one the writer writes, each element a constant or an expression over `partner.*`, `order.*`, `document.*` and, inside a line, `line.*`, with an optional `condition`; one that cannot be worked out makes the message `invalid`. A spec is checked when it is saved. A guide builds on one other guide of the company, and every guide on the module's base for its syntax and version (`guides/*.json`, in the code rather than in the database, so an upgrade changes them for everyone). Options speak the typed message's codes (a ship-to is `ST` in both syntaxes); constraints the file's own. `edi_guide.check`, `.derive`, `.export` and `.import`; the MCP prompt `edi_onboard_partner`. The structure codes written for the hierarchies (BSN05) are not checked against the standard;
- phase 7, so far: `edi.inventory_advice` makes a partner's 846 (INVRPT for an EDIFACT partner) from `stock.availability` for every item the partner has a number for, from the partner's warehouse if it names one, in the partner's unit and rounded down to whole units of it. BIA02 is written `SI` and INVRPT's quantity is `QTY` 145 (both not checked against the standard; a guide's constraint can refuse them). A partner may now have a vendor as well as, or instead of, a customer (migration 8), for remittance advice;
- answering (phase 2): the 855, 856 and 810 as typed messages, read and written; `edi.generate`, `edi_shipment.pack`, `.auto_pack` and `.ship`, `edi.label` (GS1-128 data and ZPL), `edi.pending`, `edi_message.resend`; a subscriber that drafts, approves and answers as the partner's run-as user where the policies `edi.auto_draft`, `edi.auto_approve`, `edi.send` and `edi_invoice.before_asn` allow; the partner's 997s and 999s matched to what they answer.

ARCHITECTURE.md, "Electronic data interchange (EDI)", says what the module becomes. [PLAN.md](PLAN.md) is the order it is built in.

## How EDI is used, and what was checked

This section says what the design rests on. Each statement is marked:

- **read**: read in the source named, on 18 November 2026;
- **computed**: checked by a test in this crate;
- **not checked**: written from working knowledge of the standards and the trade, and not read in a primary source by the author of this file. Check it before relying on it; the X12 standard itself is behind a licence and was not consulted.

A wider survey, with more sources and its own marks, is in [docs/research/2026-11-edi](../../docs/research/2026-11-edi/README.md). Where it verified something this file marks "not checked", it is right and this file is only more cautious; three things it settled are taken over below.

### The files

- **X12 envelope** (not checked against the standard; the spike reads and writes it and the tests hold):
  - An interchange is `ISA` … `IEA`, holding functional groups `GS` … `GE`, holding transaction sets `ST` … `SE`.
  - The ISA is fixed width, 106 characters. The element separator is its 4th character, the component separator (ISA16) its 105th, the segment terminator its 106th. From interchange version 00402, ISA11 is the repetition separator; before, it is the letter `U`.
  - ISA13 is a control number of nine digits, repeated in IEA02. GS06 is repeated in GE02, ST02 in SE02. IEA01, GE01 and SE01 count groups, sets and segments (the SE count includes ST and SE).
  - ISA14 `1` asks for a TA1, the acknowledgement of the interchange. ISA15 is `P` production or `T` test.
  - There is no escape: data cannot contain a separator.
- **The 997** (not checked against the standard): `AK1` names the group answered, `AK2` a set, `AK3` a segment in error (tag, position counted from the ST, loop, code), `AK4` an element (position, reference number, code, a copy of the bad data), `AK5` the set's status and up to five codes, `AK9` the group's status and counts. Statuses: `A` accepted, `E` accepted with errors, `R` rejected, `P` partially accepted (group only). The codes this crate raises are named in `src/x12/ack.rs`. The 999 of version 005010 says the same with `IK3`, `IK4`, `IK5` and a `CTX` for context.
- **What a small supplier exchanges** (not checked):
  - 850 purchase order in; 997 back for it;
  - 855 order acknowledgement out: accepted, changed or rejected per line;
  - 856 ship notice out, before the truck arrives. Its body is a tree of `HL` segments: shipment, order, optionally tare (pallet), pack (carton) and item. In "pick and pack" the item is under the pack; in "standard carton" the pack is under the item. BSN05 says which: `0001` pick and pack, `0002` standard carton (the research, C.6). Each carton carries `MAN*GM*` and its SSCC;
  - 810 invoice out;
  - 846 inventory advice out, 860 order change in, 820 remittance advice in.
- **Versions**: 004010 and 005010 are what retail and manufacturing partners ask for (not checked). The envelopes differ in ISA11 and ISA12; the transaction sets read here do not differ in what is read.
- **EDIFACT** (not checked against ISO 9735; the spike reads it and the tests hold):
  - `UNA` is optional and states six characters in this order: component separator, element separator, decimal mark, release character, a fifth that is reserved (a space) up to syntax version 3 and the repetition separator from version 4, and the segment terminator. Without it: `:` `+` `.` `?` and `'`.
  - An interchange is `UNB` … `UNZ` holding messages `UNH` … `UNT`. UNT counts the message's segments including UNH and UNT and repeats the UNH reference; UNZ counts messages and repeats the UNB control reference.
  - `CONTRL` acknowledges syntax, as the 997 does. In European retail (EANCOM, GS1's subset of EDIFACT) many partners do not want one.
  - D.96A is the directory most EANCOM guides are written against. The UNECE publishes the directories without charge.
  - **A German INVOIC is not an e-invoice by itself.** The einvoice README gives the transition of § 27 Abs. 38 UStG (read there): EDI invoices under Recommendation 94/820/EC may be issued until 31 December 2027, and from 2028 an invoice between domestic businesses must be an e-invoice. After that an INVOIC counts as one only where the two businesses have agreed on it and it lets the information EN 16931 requires be extracted correctly and completely (§ 14 Abs. 1 UStG; not checked here). This module's INVOIC does not claim that: it carries what the order-to-cash exchange needs, not every statement § 14 Abs. 4 asks of an invoice (the VAT identifiers, for one). For a domestic German buyer after the transition, send the e-invoice (`einvoice.export`) as well, or agree the INVOIC's content with the buyer in writing.
- **GS1** (computed): the check digit of a GTIN, GLN or SSCC weights the digits 3, 1, 3, 1 … from the right and brings the sum to a multiple of ten. An SSCC is 18 digits: an extension digit, the GS1 company prefix, a serial reference and the check digit, printed after the application identifier `(00)` on a GS1-128 label.

### Acknowledgements and retries

- A 997 acknowledges syntax, not business. Partners expect it within a time their guide states, commonly a day (not checked).
- An acknowledgement is never acknowledged.
- A repeat is recognised by control numbers on the receiving side, so a resend must be the same bytes with the same numbers.
- AS2 (read, RFC 4130): "The Message-ID on a POST operation can be reused if and only if all of the content (including the original Date) is identical" (section 5.5); retry intervals and counts are "implementation dependent" and "selected as part of the trading partner agreement".

### Transports

- **AS2** (read, [RFC 4130](https://www.rfc-editor.org/rfc/rfc4130.txt)): an S/MIME message, signed, encrypted or both, sent by HTTP POST with the headers `AS2-From`, `AS2-To`, `AS2-Version` and `Message-ID`. The sender asks for a receipt with `Disposition-Notification-To`, and for a signed one with `Disposition-Notification-Options`; the receipt (MDN) carries a digest of what was received, which is the proof of delivery. Without `Receipt-Delivery-Option` the MDN comes back in the HTTP response (synchronous); with it, to the URL it names (asynchronous). The RFC of 2005 recommends SHA-1 for sending and that receivers support MD5 and SHA-1; partners today ask for SHA-256 and AES (not checked: no public partner guide was read), which is why the older algorithms are off unless a transport allows them. Compression is [RFC 5402](https://www.rfc-editor.org/rfc/rfc5402.txt) (read: before or after signing, both allowed).
- **SFTP**: a directory to read and one to write on the partner's or a VAN's server. No standard says how; partners' guides do (not checked).
- **VANs** carry interchanges between mailboxes and bill by document or by kilocharacter, with set-up and mapping fees per partner (not checked: no price list was read; treat every figure you hear as that provider's).
- **Certification**: Walmart, Target and Amazon require AS2 software on Drummond's certification list (the research, C.1; Drummond's own claim). This module's AS2 is not on it, so those partners are reached over SFTP or a certified gateway; see PLAN.md, phases 5 and 6.

### Chargebacks

Buyers deduct from what they pay for a ship notice that is late, missing or wrong, for a label that does not scan, and for deliveries outside the window. The amounts are in each buyer's vendor manual, per shipment or as a percentage of the invoice, and differ by buyer and by year. **No buyer's own document was found; what secondary sources say is in the research, C.7, and no figure is relied on here.** What the design takes from it does not depend on the figure: a ship notice must be made from the posted delivery and its packing, not typed, and must be on its way before the truck.

### Partner guides

A partner publishes an implementation guide: which segments and elements it sends and wants, which qualifiers carry what, which codes are allowed. Two guides for the same transaction set differ in exactly these, and writing the translation for one is what a mapping fee pays for. They are the partner's documents, usually handed to its suppliers, sometimes public. This repository contains none.

### The X12 licence

(Read, [x12.org/products/licensing-program](https://x12.org/products/licensing-program).) X12 states that it "manages the exclusive copyright to all standards, publications, and products", and that an organisation that "is or intends to use X12 IP in an solution to license or otherwise provide to your customer, a third party" is "considered an X12 Commercial Use Licensing Partner"; one that uses it "in an internal solution to send or receive X12 transactions" an internal use one.

What that means for an open-source ERP is a legal question that this work did not answer and that should be put to a lawyer, or to X12, before a release. Until then it is a hard constraint on every change to this module:

- no text and no table of the standard is in the repository;
- segments and elements are described in our own words, as far as the code acts on them;
- the only codes named are those the code raises or reads;
- a partner's guide is data the company enters from what its partner gave it.

EDIFACT has no such question: the directories are published by the UNECE for use without charge (not checked beyond their being downloadable).

## Crates

Versions and licences are as crates.io showed them on 18 November 2026.

**For the files: none.** The format layer has no dependency beyond `chrono`, `rust_decimal` and `serde`, which the workspace has.

| Crate | Version | Licence | What it is | Why not |
|---|---|---|---|---|
| `edi` | 0.4.0, August 2025 | GPL-3.0 (a licence file; crates.io shows "non-standard") | An X12 parser | Cannot be a dependency of an Apache-2.0 project. |
| `x12-types` | 0.12.0, June 2026 | MIT OR Apache-2.0 | Types generated for X12 transaction sets | Carries the standard's definitions, which is the licence question above; and one type per segment is not what a guide kept as data needs. |
| `x12` | 0.1.0, 2019 | MIT | An X12 parser | Not maintained. |
| `edifact` (was `edifact-rs`) | 0.20.0, November 2026 | MIT OR Apache-2.0 | An EDIFACT parser | Active, but a second tokenizer, with its own limits to audit. |
| `edifact-types` | 0.4.1, January 2025 | MIT OR Apache-2.0 | Types for EDIFACT | As `x12-types`. |

The tokenizer for both syntaxes is about 500 lines, the X12 envelopes 550 and the EDIFACT ones 250, and this module needs them bounded against hostile input in ways it must control: that decided it.

**For SFTP (phase 5): `russh` 0.65 with `russh-sftp` 3.0**, both Apache-2.0, pure Rust, maintained (releases in November and September 2026), and with a server side that the tests can run in-process. `ssh2` (0.9.7) binds the C library libssh2; `openssh-sftp-client` (0.15, MIT) drives an `ssh` program on the machine. Neither fits a server that should carry no C library and start no process. Not checked: which cryptographic backend `russh` should be built with here; prefer the one the TLS stack already brings in.

**For AS2 (phase 6): one young crate exists**, `asx-rs` (0.19.0, first published June 2026, one author, MIT OR Apache-2.0, HTTP client on OpenSSL); the crate named `as2` is something else. Evaluate it when the phase starts. The plan assumes building from parts:

| Part | Choice | Notes |
|---|---|---|
| CMS structures (SignedData, EnvelopedData, CompressedData) | `cms` of RustCrypto, MIT OR Apache-2.0 | The stable line is 0.2.3 (January 2024); 0.3.0 is at its second release candidate (9 November 2026) and has a `builder` feature. Use it for the ASN.1 structures. Take 0.3 when it is released. |
| Certificates | `x509-cert` 0.3.0 | Parsed and pinned, never validated up a chain. Whether it pairs with `cms` 0.2 or only 0.3 is to be checked when phase 6 starts. |
| RSA signing, verifying and key decryption; AES | `aws-lc-rs` 1.19, already in the workspace through the TLS stack | It has PKCS#1 v1.5 decryption (`rsa::Pkcs1PrivateDecryptingKey`, read in its source), which AS2 partners still use for key transport. |
| MIME | written here, bounded | A signature covers the exact bytes of a MIME part, so the reader must hand back byte ranges; it is a boundary scanner, small. `mail-parser` 1.0 (Apache-2.0 OR MIT) can serve as the oracle in tests. |
| Inflating | `flate2`, with a limit, as `einvoice` does | |

The RustCrypto `rsa` crate was not chosen for private-key operations: its timing side channel (RUSTSEC-2023-0071, the "Marvin attack") is patched only from 0.10.0, released on 8 November 2026 (read in the advisory). It may be right later. `cryptographic-message-syntax` (0.28.0) is MPL-2.0, and `openssl` binds a C library the server does not otherwise carry.

**For reference, not for use**: OpenAS2 (Java) and pyas2-lib (Python) are the open implementations to test AS2 against, and both are GPL-3.0, so they are run as separate programs and nothing is copied from them. Bots (Python, GPL-3.0, archived) is the long-standing open EDI translator, built around mapping scripts per partner, which is the model this module replaces with guides as data.

## What the spike checks

`tests/spike.rs`, 13 tests on a made-up 850 and ORDERS:

- the envelope is read with the separators of the ISA, and the same order with four other sets of separators reads to the same segments;
- what was read is written back byte for byte;
- the 850 becomes a typed order (header, dates, terms, parties with addresses, lines with their product identifiers, case packs, a store allocation), and a segment with no field is reported, not dropped;
- a 997 accepts a good order, is written exactly as expected, and reads back as it was built;
- a 997 rejects a broken order and names the segment and element; counts and control numbers that do not agree are read and reported with the standard's codes;
- version 00501 states a repetition separator and 00401 does not;
- a file cut off at any length is refused, as are bad separators, control characters, too many segments or components, and a file over the size limit;
- every byte of both fixtures replaced by each of twelve bytes: reading may refuse, and returns;
- the writer refuses data that contains a separator;
- EDIFACT with a UNA (decimal comma, released characters) and without, with CONTRL's codes for counts that do not agree;
- text as UTF-8, else ISO 8859-1;
- SSCC check digits.

Not checked by anything yet: a real partner's file. Before phase 1 is called done, read sample files from at least two partners' public guides through `x12::read` and `po850::read` by hand; do not commit them.

## Fuzzing

`fuzz/` holds two `cargo-fuzz` targets: `x12_read` (any bytes through the X12 reader, the 850 reader, the acknowledgement built for them and its writer) and `edifact_read`. They are not run in CI, and the fuzz crate is outside the workspace. To run one (it needs a nightly toolchain):

```
cargo install cargo-fuzz
cd modules/edi/fuzz
cargo +nightly fuzz run x12_read -- -max_len=65536 -max_total_time=600
```

Seed the corpus with `tests/fixtures/`. A crash is a bug in the reader: add the input it found to the mutation tests (`tests/spike.rs`, `tests/acknowledgements.rs`), which every build runs as the cheap stand-in, and fix it there.
