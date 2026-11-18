# EDI research, November 2026

What was looked up before `modules/edi` was designed: the Rust crates, the open implementations in other languages, and the facts of the standards the design rests on. Checked on 18 November 2026. The design is in ARCHITECTURE.md, "Electronic data interchange (EDI)"; the build order in `modules/edi/PLAN.md`.

Each statement carries a mark:

- **V**: verified against the source cited;
- **V2**: verified only from a secondary or near-primary source (a reference site, a member organisation, a partner's guide read as a search snippet);
- **U**: unverified; no primary source was reached.

UNECE, gs1.org and ref.gs1.org refused automated requests (403), so several items rest on secondary sources. The X12 standard itself is licensed and was not consulted.

**One thing is left out on purpose.** The research returned the full code lists of the 997 and TA1 with their descriptions, from a vendor's documentation. They are not reproduced here: X12 counts code lists among its licensed products (section C.10), and this repository carries none. The sources are linked; the codes the module raises are named, in our own words, in `modules/edi/src/x12/ack.rs`.

## A. Rust crates

From the crates.io API, **V** unless noted.

### X12 and EDIFACT

| Crate | Version, date | Licence | Downloads | Notes |
|---|---|---|---|---|
| `edi` (sezna) | 0.4.0, 2025-08-12 | **GPL-3.0** (a licence file; crates.io shows "non-standard") | 154k | Loose X12 parser into serde structs. Cannot be a dependency of an Apache-2.0 project. |
| `x12-types` (apimeister) | 0.12.0, 2026-06-25 | MIT OR Apache-2.0 | 27k | Typed structs for X12 transaction sets; companion `serde_x12` 0.7.2 (2023). Embeds definitions derived from the standard. |
| `x12` | 0.1.0, 2019 | MIT | 2k | Dead. |
| `x12-stream-parser` | 0.1.0, 2025-04 | MIT OR Apache-2.0 | 0.7k | Low-level tokenizer; one release. |
| `x12pp` | 0.3.0, 2019 | GPL-3.0 | 5k | Pretty-printer; dead. |
| `edi-format` | 0.2.3, 2026-06 | MIT | 5k | EDIFACT formatter, a command-line tool. |
| `edifact-types` (apimeister) | 0.4.1, 2025-01 | MIT OR Apache-2.0 | 11k | Typed bindings of the D95B era. |
| `edifact` (hupe1980; was `edifact-rs`) | 0.20.0, 2026-11-15 | MIT OR Apache-2.0 | 0.4k (4k under the old name) | Zero-copy ISO 9735 parser and writer with validation. Very new, one author. |
| `edifact-parser` (Hochfrequenz) | 0.52.0, 2026-11-16 | MIT | 8k | SAX-style tokenizer for the German energy market. |

`rust-edifact`, `x12-parser` and `edi-parser` are not on crates.io. Permissively licensed references in other languages: `moov-io/x12` (Go, Apache-2.0) and `walmartlabs/gozer` (Java, Apache-2.0, retail X12).

### AS2

- **One crate exists: `asx-rs`** (hupe1980), 0.19.0, 2026-11-16, MIT OR Apache-2.0, 3.4k downloads, first published June 2026, 22 releases. AS2 (RFC 4130) and AS4 with signed MDNs, as a library; its HTTP client uses OpenSSL. Very young, one author. <https://github.com/hupe1980/asx-rs>
- The crate named `as2` is unrelated. Nothing else was found.

### S/MIME and CMS

- **`cms`** (RustCrypto): stable 0.2.3 (2024-01-08); 0.3.0-rc.2 (2026-11-09) is a pre-release. Apache-2.0 OR MIT, 17.5M downloads, pure Rust. The `builder` feature has `SignedDataBuilder`, `SignerInfoBuilder`, `EnvelopedDataBuilder`, `KeyTransRecipientInfoBuilder` and others (**V**, docs.rs 0.2.3). Its README says decoding "does not verify signatures or establish certificate trust": verification is the caller's. Audit status: **U**.
- `x509-cert` 0.3.0 (2026-07), `der` 0.8.3 (2026-10), `rsa` 0.10.0 (2026-11-08): MIT or Apache, pure Rust. The `rsa` crate's timing side channel (RUSTSEC-2023-0071) is patched from 0.10.0 (**V**, the advisory).
- `cryptographic-message-syntax` 0.28.0 (2025-08): **MPL-2.0**, pure Rust, signing and verifying.
- `openssl` 0.10.82: Apache-2.0, a C binding. Its PKCS7 and CMS functions are the most used route (**U**, from memory).
- `rasn-cms` 0.30.0: data types only. The `pkcs7` crate is deprecated in favour of `cms`.
- `aws-lc-rs` 1.19.0: no PKCS7 or CMS (**V**, by absence from its documentation), but RSA PKCS#1 v1.5 decryption (`rsa::Pkcs1PrivateDecryptingKey`), RSA signatures and AES (**V**, read in its source).

### MIME

`mail-parser` 1.0.1 and `mail-builder` 2.0.1 (Apache-2.0 OR MIT, 2026-11): general MIME. `mime` 0.3.18: media types only. `multipart` (dead) and `multer`: form data only. A signature covers the exact bytes of a part, so whatever reads the MIME must hand back byte ranges; whether `mail-parser` does was not checked (**U**).

### SFTP

`russh` 0.65.2 (2026-11-16) with `russh-sftp` 3.0.1 (2026-09): Apache-2.0, pure Rust, async. `ssh2` 0.9.7: a binding to the C library libssh2. `openssh-sftp-client` 0.15.12 (MIT): drives the machine's `ssh` program.

## B. Open implementations in other languages

From the GitHub API, **V** unless noted.

- **OpenAS2**: Java, **GPL-3.0** since 4.0.0, BSD before. <https://openas2.sourceforge.net/>
- **django-pyas2**, **pyas2-lib**: Python, GPL-3.0. The old `pyas2`: GPL-2.0, unmaintained.
- **Bots**: Python, GPL-3.0, archived (last push September 2025).
- **pyx12**: Python, BSD-style, health care, active.
- **node-x12**: TypeScript, MIT, last push 2022.
- **Smooks EDI cartridge**: Java, Apache-2.0 OR LGPL-3.0-or-later.
- **mendelson AS2**: Java, GPL-2.0 (**V2**, its SourceForge listing).
- **phase2** (was as2-lib): Java, believed Apache-2.0 (**U**).
- **imsweb/x12-parser**: Java, BSD.
- **Stedi**: a free online reference of X12 and EDIFACT and public partner guides at stedi.com/edi (**V2**).

They are test partners and reading matter, never dependencies: the GPL ones cannot be linked, and several ship tables of the standard.

## C. Facts of the standards

### 1. AS2

RFC 4130, <https://www.rfc-editor.org/rfc/rfc4130.txt> (**V**).

- Digest: SHA-1 is recommended for sending, and receivers are recommended to support MD5 and SHA-1 (section 5). The grammar knows only `sha1` and `md5`; SHA-2 is practice, not the RFC.
- Receipt: asynchronous when the sender gives `Receipt-Delivery-Option` with a URL, otherwise in the HTTP response. A signed one is asked for with `Disposition-Notification-Options`; it returns `Received-Content-MIC`.
- `AS2-Version` 1.1 means compression is supported; a receiver must not fail when the header is absent.
- A `Message-ID` may be reused only when all the content is identical (section 5.5).
- Compression: RFC 5402, <https://www.rfc-editor.org/rfc/rfc5402.txt> (**V**). ZLIB; before signing or around the signed part.
- **Certification.** Drummond's own page says "Walmart, Target, and Amazon require their trading partners to use Drummond-certified AS2 solutions". It tests SHA-256, 384 and 512 with AES in one profile and SHA-1 with 3DES in a legacy one, at two events a year; the price is not published. <https://www.drummondgroup.com/services/as2-testing-and-certification/> (**V** as Drummond's claim; it is an interested party.) A Walmart supplier guide asks for "an interoperable-tested AS2 communication package" (**V2**, a snippet of a copy). Which algorithms Walmart asks for: **U**.

### 2. The ISA

- Widths in 004010, and ISA11 as the standards identifier there: <https://www.stedi.com/edi/x12-004010/segment/ISA> (**V2**).
- ISA11 is the repetition separator from 00402: <https://www.ibm.com/support/pages/node/771387> (**V2**).
- The offsets were recomputed from the widths: element separator at index 3, ISA11 at 82, ISA16 at 104, the terminator at 105, 106 in all, as `modules/edi/src/x12.rs` has them.
- The values of ISA14 and ISA15: **U**.

### 3. 997, 999 and TA1

- 997: its segments and their codes are tabulated at <https://learn.microsoft.com/en-us/azure/logic-apps/logic-apps-enterprise-integration-x12-997-acknowledgment> (**V2**). The numbers `modules/edi` raises agree with it. That table lists `P` as a status of a set; it belongs to the group only.
- 999: other segment names for the same roles, a context segment, and a status for errors against an implementation guide. Required in US health care; retail still uses the 997. <https://edi.stedi.com/x12-005040/999> (**V2**)
- TA1: echoes the interchange's control number, date and time, with a status and a note code. <https://learn.microsoft.com/en-us/azure/logic-apps/logic-apps-enterprise-integration-x12-ta1-acknowledgment> (**V2**)

### 4. EDIFACT

- UNA: `UNA` and six characters; by default component separator `:`, element separator `+`, decimal mark `.`, release character `?`, a reserved space, terminator `'`. A GS1 example in syntax version 4 has `*` in the fifth place as the repetition separator (**V2**; ISO 9735 was not read).
- CONTRL actions, from the UN service code list at <https://service.gefeg.com/jwg1/Archive/cl/v3/18b/cl15.htm> (**V**): 4 rejects a level and all below it, 7 acknowledges a level, 8 says the interchange was received. It reports per interchange, group, message, segment and element (**V2**).

### 5. SSCC

- Eighteen digits: an extension digit, the GS1 company prefix, a serial reference, a check digit; application identifier (00). <https://www.gs1au.org/standards/id-keys/sscc> (**V2**; it says a prefix of 7 to 9 digits, others 7 to 10).
- Check digit: weights 3, 1, 3 … from the right, to the next multiple of ten (**V2**; hand-checked on 39501101000000418, which gives 5, and by the tests of `modules/edi/src/gs1.rs`).
- In an 856 the carton's number goes in a `MAN` segment with the qualifier for an SSCC with its application identifier, 20 characters, at the pack or pallet level (**V2**).

### 6. The hierarchy of an 856

BSN05 `0001` is shipment, order, pack, item (pick and pack); `0002` is shipment, order, item, pack (standard carton). A grocer's 5010 guide and <https://stedi.com/edi/x12/element/1005> (**V**, read as snippets). A pallet level sits between order and pack (**V2**).

### 7. Chargebacks

All secondary; the figures differ by buyer and year, and no buyer's own document was found.

- One large retailer's on-time-in-full rule: 3 % of the cost of goods of the cases that miss (**V2**, <https://supplierwiki.supplypike.com/articles/walmart-otif-dispute-process>).
- A ship notice that is late, 1 to 3 % of the invoice; one that is wrong, 2 to 5 %; or a flat 250 to 500 dollars a shipment (**U**, vendors' blogs).

### 8. What a VAN costs

Vendors' own pages and blogs, all **V2** or **U**.

- By the kilocharacter, for example 0.20 dollars; a ship notice is 5 to 10 of them. <https://www.orderful.com/blog/edi-pricing-guide>
- Setting up and mapping a partner: 750 to 2,500 dollars.
- Flat prices: around 190 to 200 dollars a month per partner.
- The largest providers publish no prices.

### 9. D.96A

From a mirror of the UN directory (**V2**).

- ORDERS: <https://www.stedi.com/edi/edifact/D96A/messages/ORDERS>. BGM and DTM, references, parties, then lines (LIN with PIA, IMD, QTY, PRI), then UNS, MOA, CNT.
- DESADV: <https://www.stedi.com/edi/edifact/D96A/messages/DESADV>. CPS, then PAC, PCI and GIN for packages, then LIN. It has no UNS.
- ORDRSP and INVOIC: not fetched (**U**).
- EANCOM 1997 is a subset of D.96A, EANCOM 2002 of D.01B (**V2**).

### 10. Licences

- **X12**, <https://x12.org/products/ip-use> and <https://x12.org/products/licensing-program> (**V**): "X12 manages the exclusive copyright to all standards, publications, and products". The products named include the standard, its implementation guides (TR3), table data, schemas and code lists, "regardless of whether X12 makes those products available for use at no cost". "An active license agreement with X12 is required to incorporate or use the X12 Standard" in software; an organisation that puts X12's work in a solution for third parties is a "Commercial Use Licensing Partner". Companion guides need X12's written permission. Open source is not addressed.
- **What follows for this repository** (an inference, not legal advice, and a hard constraint of the design until a lawyer or X12 says otherwise): a generic tokenizer and envelope reader; segments described in our own words only as far as the code acts on them; a partner's guide as data the company enters; no dictionary, no code-list descriptions, no TR3 text, and fixtures we wrote ourselves. That other open projects ship such tables is their risk, not a precedent.
- **UN/EDIFACT**: the directories are published without charge by the UNECE. The terms themselves: **U** (403). The syntax, ISO 9735, is sold by ISO.

## What the design took from this

- No mature X12 crate can be used: the tokenizer and envelopes are our own, and small.
- AS2 is built from `cms`, `x509-cert` and `aws-lc-rs`, or on `asx-rs` if it has matured by then; to decide when that phase starts.
- The largest retailers ask for certified AS2 software. They are reached by SFTP or through a certified gateway first; our own AS2 is for OEMs and smaller partners, and certifying it is a later decision of its own.
