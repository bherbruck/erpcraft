# erpcraft-einvoice

Electronic invoices after the European standard EN 16931: XRechnung (in UBL 2.1 or UN/CEFACT CII syntax) and the XML inside a ZUGFeRD / Factur-X PDF.

- `einvoice.export` writes a posted sales invoice or sales credit note as an EN 16931 invoice. By default it writes XRechnung 3.0 in UBL 2.1 syntax; it can also write CII, or plain EN 16931 without the German rules. The XML is read back and checked before it is returned.
- `einvoice.read` reads an incoming invoice and reports what it says and which rules it breaks. It proposes the vendor (by VAT identifier, otherwise candidates by name), the open purchase order, and the order line and tax code for each line. It creates nothing.
- `einvoice.import` drafts the vendor invoice. The draft is checked and posted like one entered by hand.

## The legal and technical background, with sources

Everything below was read from the sources named, on 17 November 2026. Where an official page could not be reached, this says so.

**The German obligation.**
- § 27 Abs. 38 UStG sets the transition for issuing e-invoices ([gesetze-im-internet.de/ustg_1980/__27.html](https://www.gesetze-im-internet.de/ustg_1980/__27.html)).
  - For supplies made in 2025 and 2026, invoices may still be issued on paper or, with the recipient's consent, in another electronic format until 31 December 2026.
  - For supplies made in 2027, the same applies until 31 December 2027, but only for a business whose total turnover in the previous year was not more than 800,000 euros.
  - EDI invoices under Recommendation 94/820/EC may be issued until 31 December 2027.
- From 2028 an invoice between domestic businesses must be an e-invoice.
- Being able to receive e-invoices has been required since 1 January 2025. The BMF letters below set that out; the transition rules only concern issuing.
- The administration's view is in three BMF letters on "Einführung der obligatorischen elektronischen Rechnung bei Umsätzen zwischen inländischen Unternehmern ab dem 1. Januar 2025":
  - 15 October 2024 (BStBl I S. 1320);
  - 15 October 2025 (GZ III C 2 - S 7287-a/00019/007/243), read as published at [steuerschroeder.de](https://www.steuerschroeder.de/blog/wp-content/uploads/2025/10/2025-10-15-einfuehrung-obligatorische-e-rechnung.pdf);
  - 15 October 2026 (GZ III C 5 - S 7287-a/00019/021/017), read as published by the ZDH at [zdh.de](https://www.zdh.de/fileadmin/Oeffentlich/Steuern_und_Finanzen/Themen/E-Rechnung/20261015_BMF-Schreiben_E-Rechnung_final.pdf).
- The ministry's own FAQ page ([bundesfinanzministerium.de/Content/DE/FAQ/e-rechnung.html](https://www.bundesfinanzministerium.de/Content/DE/FAQ/e-rechnung.html)) answered automated requests with a security check and could not be read here.
- The 2025 letter (Rn. 13 and 14) says:
  - an invoice in the XRechnung standard meets EN 16931;
  - ZUGFeRD from version 2.0.1 meets it too, except the profiles MINIMUM and BASIC-WL.
- The 2026 letter adds a non-objection rule for gross-price invoices issued until 31 December 2027, because no gross-price format exists yet for XRechnung and ZUGFeRD.

**EN 16931.**
- The semantic model is EN 16931-1:2017 with its amendment A1:2019. The syntax bindings are CEN/TS 16931-3-2 (UBL 2.1) and -3-3 (CII D16B).
- Secondary sources report that CEN approved a revision of EN 16931-1 in February 2026, for the EU's ViDA package. This module does not follow that revision: XRechnung 3.0, which it writes, is built on the 2017 model.
- The official validation artefacts are published by the European Commission's eInvoicing team at [github.com/ConnectingEurope/eInvoicing-EN16931](https://github.com/ConnectingEurope/eInvoicing-EN16931). The latest release read was validation-1.3.17, of 12 October 2026. The rule texts and tolerances checked here are taken from its UBL Schematron.

**XRechnung.**
- KoSIT publishes the standard ([xeinkauf.de/xrechnung/versionen-und-bundles](https://xeinkauf.de/xrechnung/versionen-und-bundles/)).
- The current version is XRechnung 3.0, in force since 1 February 2024 and, the page says, until at least 31 July 2027.
- The latest bundle is "3.0.2 Summer 2026 Bugfix" of 31 August 2026.
- A preliminary XRechnung 4.0.0 is dated 15 September 2026; the final 4.0 is expected in spring 2027. This module writes 3.0: specification identifier `urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0`, business process `urn:fdc:peppol.eu:2017:poacc:billing:01:1.0`.
- The German rules (BR-DE-*) are taken from the XRechnung Schematron release v2.6.0 (for XRechnung 3.0.2), 2 September 2026, at [github.com/itplr-kosit/xrechnung-schematron](https://github.com/itplr-kosit/xrechnung-schematron).
- The element structure follows the KoSIT test suite's samples at [github.com/itplr-kosit/xrechnung-testsuite](https://github.com/itplr-kosit/xrechnung-testsuite).

**ZUGFeRD / Factur-X.**
- FeRD's publication list ([ferd-net.de/en/downloads/publications](https://www.ferd-net.de/en/downloads/publications)) shows ZUGFeRD 2.5.2 of 4 August 2026 as the newest.
- Before it came 2.5 (10 June 2026) and 2.4 (4 December 2025, technically identical to Factur-X 1.08, in force from 15 January 2026).
- A ZUGFeRD invoice is a PDF/A-3 with the CII XML embedded as `factur-x.xml` (`zugferd-invoice.xml` in 2.0, `xrechnung.xml` for the XRECHNUNG profile).

## What is checked, and what is not

Before `einvoice.export` returns an invoice, it writes the XML, reads it back with the same reader that reads incoming invoices, and checks the result. `einvoice.read` and `einvoice.import` run the same checks on what they receive.

Checked:
- **Well-formed XML**, read with `quick-xml`. A document type declaration (DTD), any entity but the five XML defines, more than 5 MiB, more than 200,000 elements and nesting deeper than 64 are refused. Nothing is expanded, resolved or fetched.
- **The root and the specification identifier**: a UBL 2.1 `Invoice` or `CreditNote`, or a CII `CrossIndustryInvoice`, whose BT-24 starts with `urn:cen.eu:en16931:2017`. That excludes ZUGFeRD MINIMUM and BASIC WL, which the BMF letter excludes too.
- **Mandatory terms**: BR-01 to BR-16, BR-21, BR-23, BR-25, BR-CO-18, BR-CO-26.
- **Line arithmetic**:
  - PEPPOL-EN16931-R120: line net = quantity × price ÷ base quantity + charges − allowances, within 0.02 as XRechnung allows;
  - R121;
  - R040: an allowance's percentage of its base;
  - BR-CO-23/24: allowances and charges give a reason.
- **Document arithmetic**: BR-CO-10 to BR-CO-16 exactly, as the Schematron does.
- **The VAT breakdown**:
  - BR-CO-17: a category's tax is its taxable amount × rate, within one currency unit, the Schematron's tolerance, which lets tax worked out per line stand;
  - the taxable amount of each category equals the lines' (BR-S-08 and the like, within one unit);
  - every category of a line has a breakdown;
  - BR-S-05, BR-x-05 for zero-rate categories, BR-x-10 exemption reasons.
- **Decimals**: amounts have at most two decimal places (BR-DEC-*).
- **XRechnung's German rules**:
  - BR-DE-1 payment instructions;
  - BR-DE-3/4/5/6/7 seller address and contact;
  - BR-DE-8/9 buyer city and post code;
  - BR-DE-14 VAT rate;
  - BR-DE-15 buyer reference;
  - BR-DE-16 seller VAT identifier or tax number;
  - BR-DE-17 type codes;
  - BR-DE-18 the `#SKONTO#` format;
  - BR-DE-19 a correct IBAN (mod 97) for SEPA transfers;
  - BR-DE-21;
  - PEPPOL-EN16931-R010 / R020 electronic addresses;
  - BR-DE-27/28 and BR-DE-TMP-32 as advice.

Requests reach the module only within the server's request size limit (axum's default of 2 MB today), which is below the module's own limits; those apply to whatever calls the commands with larger input.

Not checked here:
- **Schema validation against the UBL 2.1 and CII D16B XSDs.** The writer emits elements in schema order, but no XSD validator is run.
- **The many rules not listed above.** Among them: code lists (currencies, units, countries, the EAS and VATEX lists), the rules for the categories AE, K, G, L and M beyond rate and reason, and the rules for invoicing periods, payee and tax representative parties, attachments and direct debit.
- **The PDF/A-3 conformance of a ZUGFeRD PDF and its XMP metadata.** Only the embedded XML is read.

**The official test cases.** The reader and the checks pass every business case of the KoSIT XRechnung test suite: the 66 files in `src/test/business-cases/standard`, UBL and CII, read on 17 November 2026. To run them, point `EINVOICE_SAMPLES` at a directory of the files and run `cargo test -p erpcraft-einvoice --test samples`. The files are not copied into this repository.

**Run the official validator on what this module writes.** It could not be run where this module was built. It is a Java program:

1. Download the KoSIT validator (`validator-1.6.3-standalone.jar`, the latest release on 17 November 2026) from [github.com/itplr-kosit/validator](https://github.com/itplr-kosit/validator/releases).
2. Download the XRechnung configuration (`xrechnung-3.0.2-validator-configuration-2026-08-31.zip`) from [github.com/itplr-kosit/validator-configuration-xrechnung](https://github.com/itplr-kosit/validator-configuration-xrechnung/releases) and unpack it.
3. Save the `xml` of `einvoice.export` to a file, say `SI-2026-00001-ubl.xml`, and run:

   ```
   java -jar validator-1.6.3-standalone.jar -s scenarios.xml -r . -h SI-2026-00001-ubl.xml
   ```

   It writes an HTML report next to the file, and its result says whether the invoice is acceptable.

## Exporting: where the data comes from

| Business term | From |
|---|---|
| Seller name, address, VAT identifier, tax number, register entry, contact, electronic address (BG-4 to BG-6, BT-34) | the company's party data: `party_profile`, its default billing `party_address`, `party_tax_id` (schemes `eu_vat` and `tax_number`), its default `party_contact` and `party_electronic_address`; the name from the company when the profile gives none |
| IBAN, BIC, account holder (BG-17) | banking's `bank_account_number` of the bank account the company's `party_profile` names as `payment_account_id`: bank account numbers are kept in one place, for bank files and e-invoices alike |
| Buyer name, address, VAT identifier, electronic address, buyer reference / Leitweg-ID (BG-7, BT-10, BT-49) | the customer's party data, the buyer reference being `invoice_reference` of its `party_profile`; the name from the customer |
| Invoice number, date, currency, notes (BT-1, BT-2, BT-5, BT-22) | the document; its memo is a note |
| Due date (BT-9), payment terms and the early-payment discount as `#SKONTO#TAGE=n#PROZENT=n.nn#` (BT-20) | the open item the invoice opened and its payment terms |
| Delivery date (BT-72) | the last posted delivery of the order by the invoice's date |
| Lines: quantity, unit (UN/ECE Rec. 20 from the item's unit; C62 otherwise), price, net, item name and code (BG-25) | the document's lines and the item, charge or account they are |
| Line discounts (BG-27, reason code 95) | the line's percent and amount discounts, in that order |
| VAT category and rate (BT-151, BT-152), breakdown (BG-23) | the line's tax code. A code above 0 % is standard rated (S) unless an `einvoice_tax_category` says otherwise; one at 0 % must have one, with the exemption reason. The tax of each category is the tax recorded per line, so the invoice says what was posted |
| VAT in the accounting currency (BT-6, BT-111) | for an invoice in another currency: the accounting currency and the tax it posted |
| Credit note: the invoice credited (BT-25, BT-26) | the sales invoice the credit note credits; type code 381, payment means 97 (clearing between the parties) |

Anything missing is refused at once, with every missing item, its business term and the record and field to set.
- A tax group (several taxes on one line, as for Canadian GST plus PST) is refused: EN 16931 has one VAT rate per line.
- A line without a tax code is refused.

## Importing

1. **Format.** The invoice is given as XML text (`content`), or as a base64 file (`content_base64`), which may be a ZUGFeRD / Factur-X PDF. The PDF is handled as hostile input:
   - at most 20 MiB;
   - parsed with `lopdf` on a thread of its own with a large stack. It is waited for from Tokio's blocking pool, never on a runtime worker, for at most 20 seconds. A panic in the parser is a refusal;
   - every stream `lopdf` inflates while loading (object streams, cross-reference streams) may inflate to at most 4 MiB;
   - what the load keeps is budgeted at 200,000 objects and 64 MiB; beyond either, the file is refused;
   - refused if it contains JavaScript anywhere;
   - never rendered;
   - the embedded file is found by its name and inflated by this module with a limit of 5 MiB.
2. **Numbers.** A number with more than 10 decimal places, or of 10^14 or more, is refused when the file is read: that is what the database's `numeric(20, 6)` holds. The arithmetic of the checks and of the draft is checked arithmetic, so a number that would overflow is a finding or a refusal, never a crash.
3. **Rights.** What `einvoice.read` proposes, it reads with the caller's rights: a vendor (by VAT identifier or by name), the party record it rests on, and the purchase order. One the caller may not read is not proposed.
4. **Vendor.** It is the one whose `party_tax_id` (scheme `eu_vat`) is the seller's VAT identifier. If none does, `einvoice.read` lists candidates by name, and `einvoice.import` needs `vendor_id`: a name never decides on its own.
5. **Buyer.** The invoice must be addressed to the company:
   - by VAT identifier where both carry one; a different one is refused;
   - otherwise by name or electronic address;
   - when nothing identifies the company, that is a difference (see 8).
6. **Purchase order.** When the buyer's order reference (BT-13) is the number of an open purchase order of that vendor, the invoice is drafted against it. Each line goes against the order line its line reference (BT-132, the line number) or its buyer item identifier (BT-156, the item's code) names.
   - Lines without an order line go to `account_id`.
   - An invoice without an order is a direct vendor invoice, with all its lines on `account_id`.
7. **Lines.** A draft line's net must be the invoice line's to the cent.
   - The unit price is the price per base quantity (BT-149), rounded up to six decimals.
   - What that and the line's allowances and charges leave between quantity times price and the invoice's line net is the line's discount.
8. **Tax, and the draft against the invoice.**
   - **Tax codes.** A line takes the company's one tax code for its VAT category and rate, or the one chosen in `line_tax_codes` (by the line's identifier). A line the invoice charges VAT on without a code is a problem that stops the import, listed with the candidate codes.
   - **Totals.** What the draft will come to is worked out as posting works it out (tax per line). It is set against the invoice's VAT (BT-110), total with VAT (BT-112) and amount due (BT-115), to the cent.
   - **Paid already.** An amount the invoice says was paid already (BT-113) is a difference too: the draft owes the full amount, and the payment is recorded against it.
   - **Differences.** `einvoice.read` lists every difference. `einvoice.import` refuses to draft unless `accept_differences` is given, and then writes them into the draft's memo.
9. **Refused:**
   - an invoice that breaks a checked rule;
   - one addressed to another VAT identifier than the company's;
   - a credit note (enter it against the invoice it credits);
   - document level allowances and charges;
   - an invoice number of the same vendor imported before. `einvoice_import` keeps every import with the SHA-256 of the file.

## Where the party data comes from

Addresses, VAT identifiers, register entries, contacts, electronic addresses and the buyer reference are party data. The party module keeps them for the company, customers and vendors alike, and printing and tax reports read them there too. This module reads them with `erpcraft_party::load` and keeps only what is e-invoicing's own: the VAT category of a tax code and the record of imported invoices.

Until the party module existed they were in this module's `einvoice_profile` and `einvoice_party`. Migration 3 moved them across and dropped those tables; the commands' outputs did not change.

## Not built

- **Writing a ZUGFeRD / Factur-X PDF.** That needs a PDF/A-3 with XMP metadata and an embedded file. Doing it properly means producing conforming PDF/A, which is not a small addition. The CII written here is what such a PDF would carry.
- **Sending and receiving.** There is no Peppol access point, e-mail inbox or upload to a public buyer's portal (ZRE, OZG-RE). The XML is returned and taken as a file.
- **Other countries' rules on top of EN 16931**, such as France's 2026 reform, Peppol BIS Billing 3.0's own rules and Italy's FatturaPA. EN 16931 alone can be written with `profile: en16931`.
- **Incoming credit notes, document level allowances and charges, attachments (BG-24), invoicing periods, and gross prices.**
