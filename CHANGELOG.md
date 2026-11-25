# Changelog

What changed, by area, from the commit history (`python3 tools/changelog.py`). Upgrading between versions: see [docs/ops/UPGRADING.md](docs/ops/UPGRADING.md).

## 0.1.0 (unreleased)

103 commits, 2026-11-15 to 2026-11-25.

### Admin UI

- Add Astryx UI spike (ui-astryx) (06f6515, 2026-11-18)
- Add @erpcraft/grid: headless Excel-style data grid (08165d8, 2026-11-18)
- Generate the Astryx UI from /api/meta (39eae01, 2026-11-18)
- Make @erpcraft/grid fully unstyled with render-prop slots (a58f138, 2026-11-18)
- Astryx UI: typed filter bar, mobile nav, smoke test (8b44c24, 2026-11-18)
- Astryx UI: self-hosted font, tablet nav, DecimalInput, compact errors (64c59a6, 2026-11-18)
- Astryx UI: lazy-load every route (c14aba2, 2026-11-18)
- Astryx UI: reports, approvals, printing, commands, views, palette (93cdcbb, 2026-11-18)
- Astryx UI: axe audit with zero violations (a7f9f1e, 2026-11-18)
- Astryx UI: parity with ui/ (53bc00c, 2026-11-18)
- Grid: toErpcraftQuery uses the new list conditions (8f9226e, 2026-11-18)
- Astryx UI: document lines on @erpcraft/grid (e412380, 2026-11-18)
- Astryx UI: lists on @erpcraft/grid in server mode (4469cf2, 2026-11-18)
- Astryx UI: editable line links, fitted widths, reports on the grid (53787b7, 2026-11-18)
- Add Astryx vs shadcn comparison for the UI decision (d29d024, 2026-11-18)
- Switch the admin UI to real shadcn/ui (5e6e2d6, 2026-11-18)
- Astryx UI: regenerate API types from the server (1cf142f, 2026-11-18)
- Admin UI: retake screenshots; sidebar starts closed below 1024px (eef85bd, 2026-11-18)
- Admin UI: regenerate API types, drop removed VAT category seed (1900946, 2026-11-18)
- Astryx UI: polish pass with Astryx's own agent guidance (7659f6b, 2026-11-18)
- Astryx UI: regenerate API types after the server upgrade (cc1e76c, 2026-11-18)
- Admin UI: shadcn Field, Checkbox, RadioGroup, toasts and date picker (daa6773, 2026-11-18)
- Grid: virtual columns for wide grids; Ctrl+Enter fills the selection (40e665f, 2026-11-18)
- Astryx UI: guidance-first rebuild of the sales order page (b6021ce, 2026-11-18)
- Admin UI: PDFs, sending documents, logo and mail settings, EDI inbox (392b45e, 2026-11-18)
- Admin UI: frontend-design polish (theme.css, IBM Plex Sans) (f32ad3d, 2026-11-18)
- Admin UI: the grid on lists and lines, one primary action, help behind ⓘ (6a014f0, 2026-11-25)
- Admin UI: payment runs, sites and locations, two-step transfers, delegation (a2d8663, 2026-11-25)
- Admin UI: lots and serial numbers on stock lines, lots on hand, lot trace (1af3a0e, 2026-11-25)

### Tax

- Support tax the buyer accounts for: reverse charge and use tax (74e5048, 2026-11-18)
- Propose a line's tax code from tax rules (1dc3dd3, 2026-11-18)
- E-invoices: VAT category per tax code; K, G and AE written correctly (74ed4ad, 2026-11-18)
- Tax: a group the buyer accounts for makes its components so; reversed adjustments stay adjustments (a8e1144, 2026-11-18)
- Tax by destination: US sales tax, Canada by province, EU One-Stop Shop (eb91c38, 2026-11-25)

### Electronic invoices and EDI

- Add payments, banking, assets, e-invoicing, party data and admin UI (4cfb393, 2026-11-18)
- EDI: decide the design; spike the X12 and EDIFACT format layer (8644556, 2026-11-18)
- EDI: keep the research; X12's licence as a constraint; SFTP before AS2 (1d51109, 2026-11-18)
- EDI phase 1: partners, the interchange log, 850 in, 997/999/TA1 out, proposal and draft (6d86c81, 2026-11-18)
- Write ZUGFeRD PDFs and send invoices in each customer's e-invoice format (7164c0f, 2026-11-18)
- EDI phase 2: the 855, 856 and 810 out, packing with SSCCs, shipping, 997s matched (6b25faa, 2026-11-18)
- EDI: fixes from the review of phase 1 (a92ba2c, 2026-11-18)
- EDI: drafts of one order number one at a time (78eeed7, 2026-11-25)
- The month scenario names the company of its EDI cross-references (3b17803, 2026-11-25)
- EDI 0003: drop the old xref indexes before copying partner-less references (5df884f, 2026-11-25)

### Accounting

- Add erpcraft kernel, general ledger and reference server (3118608, 2026-11-15)
- Close a year of monthly periods in one step (f9aa256, 2026-11-18)
- Use an exchange rate recorded the other way round (c6697b8, 2026-11-18)
- Refuse a year-end close over dates another close already covers (a3dad9c, 2026-11-18)
- Keep exchange rates to 18 places, so an inverse rate converts exactly (5947c3a, 2026-11-18)
- Intercompany: mirror sales to a sister company as drafts; balances of both sides (0181be9, 2026-11-18)
- Consolidation: a group's statements in one currency, intercompany eliminated (4c1c4b1, 2026-11-18)
- Month-end gaps: monthly periods, splitting a year, as-of open items, manufacturer templates (d33195a, 2026-11-25)
- Budgets by account, month and dimension; the cash flow statement (7c13797, 2026-11-25)

### Manufacturing and planning

- Absorb labor, machine time and overhead into production (7a842ed, 2026-11-18)
- Suggest what to buy or make from reorder rules (a0c9801, 2026-11-18)
- Draft production orders for what planning says to make (621c2f0, 2026-11-18)
- Add work centers and routings with actual time and variance (bce5f49, 2026-11-18)
- Refuse absorbed cost credited to a stock account (1d45381, 2026-11-18)
- Planning: drafts count as planned; the rule says buy or make (9287e7f, 2026-11-18)
- Review 7: routing cost never credits stock; net time decides variance (d1ab1a4, 2026-11-18)

### Inventory and warehousing

- Add stock ledger, inventory, purchasing and sales (b926e15, 2026-11-16)
- Add stock availability: committed, on order, in production (9942a11, 2026-11-18)
- Architecture: decide multi-site design (dimensions, location tree, transit) (f465059, 2026-11-18)
- Reserve stock for sales and production orders (5c0199e, 2026-11-18)
- Stock 0007: trigram indexes for finding items by part of code or name (a4c6300, 2026-11-18)
- Several sites: GL dimensions, sites and location tree, two-step transfers (de2c750, 2026-11-18)
- Availability shows what is reserved and what is free of reservations (8f9249d, 2026-11-18)
- Multi-site review fixes: warehouse guards, close short, rules by site (fe5f19b, 2026-11-18)
- Reservations: lock before checking, one lock order, never more than on hand (2a365bb, 2026-11-18)
- Lots and serial numbers, slice 1: the stock ledger, adjustments and transfers (dbe3a15, 2026-11-25)
- Lots and serial numbers, slice 2: receipts, deliveries, returns, production, traceability, the 856 (bb2d2d9, 2026-11-25)
- ARCHITECTURE: how the 856 carries lots (93cc19f, 2026-11-25)
- Physical inventory and cycle counts, ABC classes and a count schedule (a08f717, 2026-11-25)
- Units of measure: a stock unit per item, purchase and sales units by exact factor (dc775f2, 2026-11-25)

### Purchasing and payables

- Take a credit from a vendor without an invoice (52c7e85, 2026-11-18)
- Report returns that still await a credit note (7b20657, 2026-11-18)
- Sales 0013, purchasing 0014: indexes for party lists and summary sorts (77dc719, 2026-11-18)
- Payment runs: vendors paid in a batch, SEPA pain.001 and NACHA files, remittance by mail (380135d, 2026-11-25)

### Sales and receivables

- Credit a customer without an invoice (a5adef1, 2026-11-18)
- Collections: customer statements and dunning runs with letters by mail (6610622, 2026-11-25)
- Sales orders keep the customer's reference and requested date (bce40c0, 2026-11-25)
- Price lists: proposed prices on order lines, quantity breaks, groups, last price paid (b6a53bf, 2026-11-25)
- Sales quotes won into orders or lost; recurring invoices on a schedule (cff1cfa, 2026-11-25)

### Payments and banking

- Make invoices due in parts (f3dda82, 2026-11-18)
- Open items: read parts after the lock; lock terms when adding parts (f79c405, 2026-11-18)
- Payments: write off without money, credits and parts; hide open amounts (c94048b, 2026-11-18)

### Printing and mail

- Print on the letterhead, make PDFs on the server, send documents by email (0042238, 2026-11-18)
- Server CSP: allow data: images so a printed document shows its logo (1f0beef, 2026-11-18)
- Review 7: bound the logo, the PDF and mail (c47bf93, 2026-11-18)

### Platform

- Add list queries, record history, outbox delivery and UI guidelines (06c6993, 2026-11-16)
- License under Apache-2.0 (1739b7d, 2026-11-18)
- Keep the parties a document was posted with (1082ebe, 2026-11-18)
- List conditions: text tests, params, relative dates, time zone, summary fields (fb678ec, 2026-11-18)
- List sorting: choices in declared order, links by name, nulls first or last (1393433, 2026-11-18)
- Let an approver delegate their approvals for a span of days (e7ec9c6, 2026-11-18)
- Fill draft line figures in views and previews; lookups take limit (1692d55, 2026-11-18)
- Lists: plan each query with its values; sort id links through a join (b477de4, 2026-11-18)
- Kernel 0011: date index for document lists; find stale summaries by index (d09f828, 2026-11-18)
- Delegating approvals takes approval:delegate; limited tokens cannot (8fbfb89, 2026-11-18)
- Scenario: a month in the life of Ohio Widgets, with every ledger tied (0e8da8d, 2026-11-18)
- Ending a delegation keeps its record; delegation days are UTC days (51a9511, 2026-11-18)
- CommandSpec::redact: large inputs reach the audit log as a hash (fbe5480, 2026-11-25)
- Commands say when they are a record's primary action (bf5a9da, 2026-11-25)
