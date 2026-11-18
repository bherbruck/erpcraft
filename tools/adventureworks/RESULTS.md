# AdventureWorks import: results

Run on 2026-11-16 (evening, US time; the server clock said 2026-11-17 02:09 to 02:19 UTC) against the live server at `http://127.0.0.1:8600`, as the administrator, 4 requests in flight. The server was shared: other people were building Rust code and a UI on the same machine during the runs, so single latencies are noisy. Three runs, each continuing the one before in the same company `AWC`:

| Run | Orders asked for | What it added |
|---|---|---|
| 1 | 5 + 5 | All master data, then 5 purchase and 5 sales orders. |
| 2 | 300 + 300 (the default) | 295 + 295 more orders, 187 customers. |
| 3 | 3,000 + 3,000 | 2,700 + 2,700 more orders, 2,019 customers. |

**All three ran to the end. The only refusals were the server declining to approve purchase orders of inactive vendors (147 in total). Every consistency check on the books passed after every run.**

The full numbers of each run are in `data/run-<label>-<time>.json` (not committed).

## What was loaded

State of company `AWC` after run 3:

| Kind | Count | Notes |
|---|---:|---|
| Accounts | 11 | |
| Fiscal periods | 4 | FY2022 to FY2025: the data's dates are 2022-04-15 to 2025-09-21, not 2011 to 2014 |
| Warehouse, tax code, settings records | 1 each | |
| Items | 504 | all products |
| Vendors | 104 | 4 of them inactive, as in the source |
| Customers | 2,841 | 635 stores, 2,206 others that placed a loaded order |
| Bills of materials | 238 | with 2,383 component lines |
| Purchase orders | 3,000 | 2,765 closed, 9 open, 78 cancelled, 148 draft (1 pending in the source, 147 refused) |
| Purchase receipts | 2,765 | posted |
| Vendor invoices | 2,765 | posted |
| Sales orders | 3,000 | all closed (delivered and invoiced in full); 11,895 lines |
| Sales deliveries | 3,000 | posted |
| Sales invoices | 3,000 | posted |
| Payments | 5,527 | posted; 183 payables and 55 receivables left open on purpose (not yet due) |
| Stock adjustments | 3 | opening stock, one per run: 42 + 50 + 132 lines |

Purchase orders run from 2022-04-15 to 2025-06-07 and sales orders from 2022-05-30 to 2023-06-06: the earliest 3,000 of each do not cover the same time span, because the source has eight times as many sales orders as purchase orders.

## Time and throughput

| Step | Run 1 (5) | Run 2 (300) | Run 3 (3,000) |
|---|---|---|---|
| download and parse | about 30 s (first download, 39 MB) | 0.7 s | 0.7 s |
| setup | 0.6 s, 32 requests | 0.1 s, 10 | 0.1 s, 10 |
| items | 3.8 s, 505 requests, 134/s | (lookup only) | (lookup only) |
| vendors | 1.9 s, 105 requests, 56/s | (lookup only) | (lookup only) |
| customers | 9.5 s, 636 requests, 67/s | 0.7 s, 189, 288/s | 7.4 s, 2,021, 275/s |
| boms | 29.2 s, 2,623 requests, 90/s | (lookup only) | (lookup only) |
| purchases | 0.5 s, 30 | 41.4 s, 2,741 requests, 66/s | 177.8 s, 25,929 requests, 146/s |
| opening_stock | 0.4 s, 5 | 0.4 s, 5 | 0.7 s, 5 |
| sales | 1.9 s, 34 | 39.3 s, 2,454 requests, 62/s | 228.1 s, 24,439 requests, 107/s |
| checks | 0.7 s, 42 | 0.4 s, 42 | 1.2 s, 42 |
| **total** | **78 s, 4,012 requests, 51/s** | **85 s, 5,450 requests, 64/s** | **423 s, 52,455 requests, 124/s** |

- A complete purchase order costs 9.6 requests (order, approve, read back, receipt, post, invoice, post, find open item, payment, post, sometimes close) and took 66 ms of wall time at 4 in parallel: about **15 purchase orders per second**.
- A sales order costs 9.1 requests and took 84 ms: about **12 sales orders per second**, with 4 lines per order on average.
- At that rate the whole data set (4,012 purchase and 31,465 sales orders) would take about 50 minutes. It was not run.
- Run 2 was slower per request than run 3 (medians about twice as high). The data was smaller, so this is the shared machine, not the server.

## Latency by endpoint

Run 3, sorted by 95th percentile. Milliseconds, measured at the client. "First / last quarter" is the median of the first and of the last quarter of the calls: it shows whether an endpoint slowed down as the tables grew.

| Endpoint | Calls | Median | p95 | Max | Median, first / last quarter |
|---|---:|---:|---:|---:|---|
| `stock_adjustment.post` (132 lines) | 1 | 507 | | 507 | |
| `sales_invoice.post` | 2,700 | 42 | 322 | 3,724 | 41 / 44 |
| `gl.trial_balance` | 1 | 296 | | 296 | |
| `sales_delivery.post` | 2,700 | 36 | 208 | 3,451 | 35 / 37 |
| `vendor_invoice.post` | 2,508 | 54 | 108 | 511 | 57 / 55 |
| `purchase_receipt.post` | 2,508 | 42 | 80 | 284 | 45 / 43 |
| `stock.on_hand` | 2 | 14 | | 76 | |
| `POST documents/sales_invoice` | 2,700 | 24 | 47 | 637 | 23 / 25 |
| `POST documents/sales_delivery` | 2,700 | 24 | 45 | 596 | 23 / 25 |
| `POST documents/sales_order` | 2,700 | 22 | 45 | 689 | 21 / 23 |
| `payment.post` | 5,049 | 28 | 36 | 144 | 28 / 29 |
| `POST documents/vendor_invoice` | 2,508 | 25 | 33 | 115 | 26 / 26 |
| `POST documents/purchase_receipt` | 2,508 | 24 | 32 | 101 | 25 / 25 |
| `sales_order.approve` | 2,700 | 22 | 30 | 255 | 21 / 23 |

Master data, from run 1: `POST entities/item` median 17 ms, p95 67; `POST entities/bom_line` 20 / 150; `POST entities/bom` 19 / 110; `POST entities/customer` 61 / 149 in run 1 and about 3.6 ms per record of wall time in run 3; `POST entities/vendor` 72 / 134.

What the numbers say:

- **Nothing got slower with volume.** For every endpoint the median of the last quarter of calls equals that of the first, up to 3,000 orders of each kind, 24,000 documents and about 230 million in ledger turnover.
- **Sales postings have a long tail.** `sales_delivery.post` and `sales_invoice.post` have a median near 40 ms and a 95th percentile of 200 to 320 ms, with single calls of 3.5 s. The purchase postings do not (p95 80 to 110 ms). The cause was not measured. Sales orders have up to 72 lines where purchase orders have at most 30, and a delivery locks one stock level row per line while other deliveries want the same rows, so both line count and lock waits are candidates. The importer now records line counts so that the next run can tell them apart.
- `gl.trial_balance` took 296 ms over 11 accounts, the slowest read. It is a full aggregation and will grow with the ledger.
- Posting commands cost 30 to 55 ms each at the median, about twice a plain create (22 to 25 ms).

## Errors returned by the server

One kind, in all three runs together:

| Count | Status | Message | Endpoint | Example |
|---:|---|---|---|---|
| 159 requests (147 orders; 12 were tried in both run 2 and run 3) | 422 | `the vendor is inactive` | `POST /api/commands/purchase_order.approve` | `AW PO 7`, order date 2022-04-15, source status 4 (complete), vendor with `ActiveFlag = 0` |

No 5xx answer, no timeout, no deadlock, no conflict, no transport failure in 61,917 requests. No request was retried.

The refusal is correct for a live system and wrong for history: the source has four inactive vendors (Gardner Touring Cycles, Circuit Cycles, Reliance Fitness, Proseware) with 203 purchase orders, 185 of them complete. Those orders were placed while the vendor was active. They were left as drafts, so about 1.9 million of purchases that the source has are not in the books (run 3).

## The books

Checked after each run with the server's own reports. All passed, three times:

| Check | Result after run 3 |
|---|---|
| `gl.trial_balance`: total debit = total credit | 227,771,827.30 both |
| `stock.on_hand` total value = inventory account 1300 | 41,987,028.72 both |
| `open_item.list` payables = payables account 2000 | 183 items, 3,122,896.96 both |
| `open_item.list` receivables = receivables account 1200 | 55 items, 1,926,767.28 both |
| Goods received not invoiced (2100) is zero, as every receipt was invoiced | 0 |
| No item with negative quantity or value | 0 rows |
| `tax.report` agrees with the tax accounts (2200 collected, 5300 paid) | 2,011,567.13 and 3,358,962.13, both equal |
| Cost of goods sold = opening stock brought in (everything sold came from opening stock at standard cost) | 22,915,101.45 both |

Trial balance after run 3:

| Account | Balance (debit positive) |
|---|---:|
| 1000 Cash at bank | −16,993,722.06 |
| 1200 Accounts receivable | 1,926,767.28 |
| 1300 Inventory | 41,987,028.72 |
| 2000 Accounts payable | −3,122,896.96 |
| 2100 Goods received not invoiced | 0.00 |
| 2200 Sales tax payable | −2,011,567.13 |
| 3000 Opening balance equity | −22,915,101.45 |
| 4000 Sales revenue | −25,144,571.98 |
| 5000 Cost of goods sold | 22,915,101.45 |
| 5200 Purchase price variance | 0.00 |
| 5300 Sales tax on purchases | 3,358,962.13 |

Nothing in the books is wrong in the sense of a ledger disagreeing with another. These things look odd and come from the import, not from the server:

- **The bank account is 17 million overdrawn.** The company starts with no capital, and the load buys 44 million of components (three years of purchase orders) while selling 25 million (one year of sales orders). A believable demo needs either a matching time window for both or an opening bank balance.
- **Inventory of 42 million never moves.** Purchased components are not consumed, because no production orders are loaded; the bicycles that are sold come from opening stock instead.
- **Revenue is 2.37 higher than the source subtotals** (25,144,571.98 against 25,144,569.61) over 11,895 lines: the server rounds each line to cents, the source sums 6-decimal line totals.
- **Tax collected is 2,011,567.13 where the source says 2,339,513.95.** Reseller orders carry 8.7 % to 9.9 % in the source and were loaded at 8 % (see README).
- **Purchases are 1.9 million short of the source's stocked value** (41,987,028.72 against 43,902,819.66): the 147 refused orders.
- Amounts are shown with four decimals (`29395.3800`) for a currency with two. Cosmetic.

## What a faithful import would need

Bugs: none found in these runs. Every refusal was a rule working as written, and the ledgers agreed throughout. (The second round below found one design problem: document numbering serialises all postings of a kind.)

Missing features, in the order they cost fidelity here:

1. **Historical documents for a party that is inactive today.** Approval checks the vendor's current `active` flag. This is a hard-coded check that should become a policy point (it is on the team lead's list), like over-receipt already is. An import (or a correction of an old order) needs either "active as of the document date" or a way to deactivate after loading. 147 of 3,000 purchase orders were lost to this.
2. **Charges that are not items: freight.** Every order in the source has freight (2.5 % of the subtotal). Orders and invoices cannot carry it, so every invoice total is lower than the source's.
3. **Tax by jurisdiction, or a stated tax amount.** One code with one rate per line cannot reproduce reseller orders taxed at blended rates. Either rates by ship-to region or letting the invoice state the tax amount would do. Tax on freight is also missing.
4. **Sales in a foreign currency.** 44 % of sales orders have a currency rate; sales is accounting currency only.
5. **Line discounts.** Loaded as net prices; the discount and its reason (special offer) are lost.
6. **Rejected goods and returns.** `RejectedQty` (445 lines in run 3) can only be modelled by receiving less. There is no return to vendor and no inspection status.
7. **Payment terms and due dates.** Open items have a `due_date` that nothing sets; the importer invented 30 days. There is also no payment on account and no opening balance for the bank other than a journal entry.
8. **Production.** Work orders were not loaded. With them, purchased components would be consumed and bicycles would enter stock at cost, which removes the need for opening stock. The bills of materials are in place; bill lines have no unit, and 86 current source rows are in ounces or inches.
9. **Master data depth.** Items have a code, a name and one unit: no cost, list price, category, make-or-buy flag, or purchase unit. Vendors and customers have a code and a name: no address, contact, territory, credit rating or terms. Most of what makes AdventureWorks recognisable is therefore not visible.

What would make an import like this easier and faster:

- **A reference field on documents, unique per company and type.** The importer keeps its source key in `memo` and finds documents again with `q`, which is a substring search. An external reference with exact lookup would make resuming exact and cheap.
- **The open item on the invoice.** After `vendor_invoice.post` or `sales_invoice.post` the importer has to call `open_item.list` for the party and search for the document. Returning the open item id with the posted invoice, or a `document_id` filter on `open_item.list`, saves one request per invoice (5,500 here).
- **Bulk creation.** 3,000 orders of each kind took 52,455 requests. A batch endpoint for master data (2,383 single `bom_line` requests) and a "create and post" in one request would cut the count by more than half.
- **Idempotency keys expire after 24 hours** and failed commands do not keep theirs, which is right; it means a rerun must look things up, as this importer does, and cannot rely on keys alone.

Slowness, in order: the tail of `sales_delivery.post` and `sales_invoice.post` (seconds, where the median is 40 ms); `gl.trial_balance` at 0.3 s for a year of data; one HTTP request and one transaction per record for master data. None of it is a problem at this size.

---

# Second round: after the server restart (new build)

Run on 2026-11-16 after the server was restarted on a build with account rules, policies, year-end close and the new reports. The importer was changed to match (README has the details): no `inventory_account_id` in the settings, one `account_rule` for inventory, no `manufacturing_settings`.

**Which company.** The first company, `AWC`, was left as it is, with 3,000 + 3,000 orders and no opening capital. A second company, **`AWC2`**, was loaded from nothing with the updated importer. Items, vendors, customers, bills of materials and the tax code are shared between companies and were reused, so this round has no master data timings.

New in this round:

- **Opening capital:** a journal entry of 25,000,000.00 on 2022-04-15, bank against share capital.
- **Inactive vendors:** the importer switches the four inactive vendors to active for the purchase step and back afterwards. Checked after the run: all four are inactive again. With that, no purchase order is refused.
- **Year-end close** (`--close-years`) and checks against `gl.balance_sheet` and `gl.income_statement`.
- **Line counts** are recorded with every posting, to see what the slow calls have in common.

## Runs

All in `AWC2`, each continuing the one before:

| Run | Orders | In parallel | Time | Requests | Errors |
|---|---|---:|---:|---:|---:|
| A | first 300 + 300 (the default) | 4 | 68 s | 5,465, 80/s | 0 |
| B | next 300 + 300 (`--limit-orders 600`) | 1 | 215 s | 5,379 | 0 |
| C | next 300 + 300 (`--limit-orders 900`) | 8 | 78 s | 5,511, 71/s | 0 |
| D | same 900, `--close-years` | 4 | 5 s | 109 | 0 |

Run D found everything already there (109 requests, all lookups and checks) and posted two year-end closes. That is the resumability test: nothing was created twice.

Run A, the default limits on a clean company:

| Step | Time | Requests | Per second |
|---|---:|---:|---:|
| setup (company, 13 accounts, periods, capital, settings) | 0.7 s | 36 | 52 |
| purchases (300 orders: 269 complete, 30 cancelled, 1 draft) | 24.3 s | 2,879 | 118 |
| opening_stock (50 lines) | 0.4 s | 5 | |
| sales (300 orders, 1,324 lines) | 40.2 s | 2,479 | 62 |
| checks | 0.6 s | 51 | 80 |

Loaded by run A: 300 purchase orders, 269 receipts, 269 vendor invoices, 300 sales orders, 300 deliveries, 300 sales invoices, 490 payments, 1 stock adjustment, 1 journal entry. After run D the company holds 900 purchase orders (845 closed, 54 cancelled, 1 draft), 845 receipts and vendor invoices, 900 sales orders, deliveries and invoices, 1,315 payments, 3 stock adjustments, 2 year-end closes.

No refusal of any kind in 16,464 requests.

## The books of AWC2

All nine checks passed after each of the four runs. After run D:

| Check | Result |
|---|---|
| Trial balance: debit = credit | 96,232,593.13 both |
| Stock value = inventory account 1300 (now found through the account rule) | 12,359,838.14 both |
| Open payables = account 2000 | 352 items, 5,476,969.88 both |
| Open receivables = account 1200 | 78 items, 2,142,081.88 both |
| Goods received not invoiced = 0 | 0 |
| No negative stock | 0 rows |
| `gl.balance_sheet`: assets = liabilities + equity + unclosed result | 38,115,889.37 both |
| `gl.income_statement`: the four years add up to the whole range | 821,140.87 − 277,666.89 − 680,074.66 + 0 = −136,600.68 |
| Result of all years = unclosed result + retained earnings | −136,600.68 = −680,074.66 + 543,473.98 |

**Year-end close.** FY2022 and FY2023 were closed in order to 3100 Retained earnings (543,473.98 = 821,140.87 − 277,666.89). Afterwards the trial balance shows revenue and cost of goods sold at zero, 2024's expense (680,074.66 of sales tax on purchases) still open, and the income statement for 2022 still reports 821,140.87, so the report leaves the closing entries out as documented. FY2024 was not closed because the load reaches into it (purchase orders run to 2024-09-19; sales only to 2022-10-07). `year_end_close.post` took 106 and 128 ms.

**The bank account** stands at 23,613,969.35 instead of being overdrawn.

The years' results look strange (a loss in 2023 and 2024) for the reason given above: with equal counts the sales stop in October 2022 while purchases and their non-reclaimable sales tax go on to 2024.

## Why the sales postings have a slow tail

Two causes, both measured. A third is noise from the shared machine.

**1. Cost grows with the number of lines.** One request at a time (run B), so nothing waits for anything:

| Lines on the order | `sales_delivery.post` median / p95 | `sales_invoice.post` median / p95 | Calls |
|---|---|---|---:|
| 1 to 2 | 51 / 159 ms | 54 / 174 ms | 254 |
| 3 to 9 | 89 / 151 ms | 104 / 196 ms | 21 |
| 10 to 29 | 170 / 302 ms | 248 / 509 ms | 24 |
| 30 and more | 344 ms | 397 ms | 1 |

That is very roughly 8 ms per line for a delivery and 12 ms for an invoice, on top of about 40 ms. The purchase postings behave the same way (`vendor_invoice.post`: 48 ms for 1 to 2 lines, 73 for 3 to 9, 165 for 10 to 29) but purchase orders are short: 2.2 lines on average, against 4 for sales, with a fifth of the first 300 sales orders at 10 lines or more. So the tail in the first report was mostly the long reseller orders. In Postgres, the sessions doing this work were seen between single-row statements (`INSERT INTO gl_entry ...`, `UPDATE sales_delivery_line SET value = ...`, one round trip per line, each firing the ledger triggers), which fits a per-line cost.

**2. Postings of one kind queue behind the document number.** Same data, different concurrency (each run is the next 300 orders, so compare within a line-count band):

| | 1 in parallel | 4 in parallel | 8 in parallel |
|---|---|---|---|
| `sales_delivery.post`, 1 to 2 lines, median / p95 | 51 / 159 | 47 / 151 | 88 / 404 |
| `sales_delivery.post`, 10 to 29 lines | 170 / 302 | 221 / 893 | 365 / 1,763 |
| `sales_invoice.post`, 1 to 2 lines | 54 / 174 | 49 / 181 | 97 / 891 |
| `sales_invoice.post`, 10 to 29 lines | 248 / 509 | 375 / 1,140 | 722 / 2,124 |
| `sales_invoice.post`, all, median / p95 / max | 58 / 252 / 535 | 55 / 725 / 1,546 | 123 / 1,588 / 4,331 |
| `purchase_receipt.post`, all, median / p95 | 45 / 100 | 53 / 126 | 108 / 556 |
| `payment.post`, all, median / p95 | 35 / 111 | 32 / 56 | 68 / 811 |
| Sales step, 300 orders | 130.5 s (2.3 orders/s) | 40.2 s (7.5/s) | 39.6 s (7.6/s) |
| Purchase step, about 290 orders | 78.7 s | 24.3 s | 31.7 s |

Going from 4 to 8 in parallel bought nothing: the same throughput, with every posting two to three times slower. The reason is visible in `pg_stat_activity`, sampled about four times a second during run C (202 samples):

- In 140 of 202 samples at least one session was waiting for a lock, and up to 5 of the 8 at once.
- **Every one of the 324 waiting sessions seen was waiting on the same statement:** `INSERT INTO number_series ... ON CONFLICT (company_id, series) DO UPDATE SET last_value = last_value + 1` (314 on `transactionid`, 10 on `tuple`). The longest wait seen was 4.0 s, the average 0.42 s.
- The sessions holding that lock were in the middle of their posting: mostly inserting `gl_entry` rows one at a time, writing the audit record, or committing.
- With 1 in parallel there was no lock wait in 352 samples.

So the number series row is the lock. `document::assign_number` takes it before the stock and ledger entries are written (`modules/sales/src/lib.rs` lines 796 and 1148, the same in purchasing) and it is held until commit, which is what guarantees numbers without gaps (ARCHITECTURE.md, "A number series under load"). The series is per company, document type and year, so all sales invoices of a year in one company post strictly one after another, and a 30-line invoice holds everyone else up for its whole 400 ms. Posting throughput per document type is therefore capped at one transaction at a time whatever the hardware: here about 8 sales orders per second.

What would help, cheapest first: take the number as the last statement before commit, so the lock is held for one round trip and not for the whole posting; write the ledger and stock lines of a posting in one statement instead of one per line; and decide whether gap-free numbers are needed for every document type (invoices, by law in some countries; deliveries and receipts, probably not), using a sequence where they are not.

**3. Noise.** While these runs were in flight, other people's test runs created and dropped test databases on the same Postgres. `DROP DATABASE ... WITH (FORCE)` was seen waiting on a checkpoint in 280 samples, up to 2.3 s, and forced checkpoints slow every commit. Some of the single multi-second calls in the first report may be this and not the server. The medians are not affected much; treat every maximum in this document with suspicion.

## What changed in the list of problems

- **New, the most important finding so far: document numbering serialises all postings of a document type per company and year.** A design limit and not a wrong result. Details above.
- Inactive vendors: worked around in the importer as agreed; the server check is unchanged.
- Decimals finer than a column keeps are now refused. The source did not trigger it: the importer already rounded net prices to 6 decimals, and quantities are whole numbers.
- Over-receipt policy: not needed. No source line receives more than was ordered.
- The year-end close, the balance sheet and the income statement worked first time and agree with each other and with the trial balance.

---

# Third round: after numbering was moved to the end of a posting

Run on 2026-11-17 between 12:43 and 12:51 UTC against the live server, restarted on a build where a posting takes its document number as its last statement, writes its ledger lines in one statement per ledger, and has the ledger's rules checked once per statement instead of once per row. The question: does posting now scale from 4 to 8 requests in parallel?

**Which company.** A third company, **`AWC3`**, loaded from nothing with the same importer and the same four runs as `AWC2` in the second round, so each step processes exactly the same orders at the same concurrency as before: A the first 300 + 300 at 4 in parallel, B the next 300 + 300 at 1, C the next 300 + 300 at 8, D `--close-years`. `AWC` and `AWC2` were not touched. The importer needed no change for the current API.

The machine was shared as before: other agents were editing and building code at the same time, and the runs were not isolated from that. Single calls and maxima are noise; the medians and the ratios between concurrency levels are what to read.

## Runs

| Run | Orders | In parallel | Time, before → now | Requests per second, before → now | Errors |
|---|---|---:|---:|---:|---:|
| A | first 300 + 300 | 4 | 68.3 s → 72.9 s | 80 → 75 | 0 |
| B | next 300 + 300 | 1 | 215.1 s → 207.1 s | 25 → 26 | 0 |
| C | next 300 + 300 | 8 | 77.8 s → **42.2 s** | 71 → **131** | 0 |
| D | `--close-years` | 4 | 5 s → 2.8 s | | 0 |

By step:

| Step | 1 in parallel, before → now | 4 in parallel, before → now | 8 in parallel, before → now |
|---|---|---|---|
| Purchases (about 290 orders) | 78.7 s → 96.8 s (31 → 26 req/s) | 24.3 s → 31.7 s (118 → 91 req/s) | 31.7 s → **17.3 s** (86 → **157** req/s) |
| Sales (300 orders) | 130.5 s → 107.4 s (22 → 26 req/s) | 40.2 s → 38.1 s (62 → 65 req/s) | 39.6 s → **22.4 s** (68 → **121** req/s) |

**The answer is yes.** Before, 8 in parallel was no faster than 4 (sales 39.6 s against 40.2 s). Now 8 in parallel loads the same kind of slice in 22.4 s against 38.1 s at 4: 1.7 times the throughput, and purchases 1.8 times. Sales orders went from about 7.5 per second to about 13.4.

## Latency of the postings

Milliseconds at the client, median / p95, all calls of the step (about 300 each), before → now:

| | 1 in parallel | 4 in parallel | 8 in parallel |
|---|---|---|---|
| `sales_delivery.post` | 54 / 194 → 51 / 143 | 52 / 536 → 68 / 296 | 115 / 924 → **75 / 497** |
| `sales_invoice.post` | 58 / 252 → 58 / 154 | 55 / 725 → 76 / 279 | 123 / 1,588 → **83 / 334** |
| `purchase_receipt.post` | 45 / 100 → 56 / 111 | 53 / 126 → 67 / 125 | 108 / 556 → **77 / 136** |
| `vendor_invoice.post` | 54 / 120 → 67 / 124 | 66 / 164 → 82 / 151 | 144 / 536 → **94 / 159** |
| `payment.post` | 35 / 111 → 39 / 63 | 32 / 56 → 45 / 81 | 68 / 811 → 55 / 259 |
| Max of `sales_invoice.post` | 535 → 348 | 1,546 → 743 | 4,331 → 504 |

By lines on the order (the same orders in both rounds):

| | 1 in parallel | 4 in parallel | 8 in parallel |
|---|---|---|---|
| `sales_delivery.post`, 1 to 2 lines | 51 / 159 → 49 / 80 | 47 / 151 → 58 / 144 | 88 / 404 → 68 / 113 |
| `sales_delivery.post`, 10 to 29 lines | 170 / 302 → 141 / 227 | 221 / 893 → 201 / 636 | 365 / 1,763 → 333 / 691 |
| `sales_invoice.post`, 1 to 2 lines | 54 / 174 → 56 / 80 | 49 / 181 → 67 / 163 | 97 / 891 → 78 / 104 |
| `sales_invoice.post`, 10 to 29 lines | 248 / 509 → 158 / 213 | 375 / 1,140 → 224 / 620 | 722 / 2,124 → 261 / 371 |

What this says:

- **The tail at 8 in parallel is gone for short postings and much smaller for long ones.** The p95 of `sales_invoice.post` at 8 fell from 1.6 s to 0.33 s, and its maximum from 4.3 s to 0.5 s. At 8 in parallel the latencies are now close to those at 4, which is what a server that is not serialised looks like.
- **Long invoices are cheaper per line.** One at a time, an invoice of 10 to 29 lines went from 248 to 158 ms at the median (about 6 ms per line above the short ones, against 12 before): the batched ledger lines. Deliveries improved less (170 → 141 ms): the stock side still writes one statement per line.
- **Short postings at 1 and 4 in parallel did not get faster; the purchase postings are about 10 ms slower at the median** (receipt 45 → 56 ms at 1 in parallel, vendor invoice 54 → 67). Whether that is the machine or the server's added work since (policy points asked on each posting, summaries) was not separated; it is the same size in every run of this round, which argues for the server. Payments are 4 to 13 ms slower at the median at 1 and 4 for the same unknown reason.
- **Deliveries of many lines still slow down with concurrency** (10 to 29 lines: 141 ms alone, 333 ms at 8). That is the next limit, below.

## Lock waits at 8 in parallel

`pg_stat_activity` was sampled during run C with a read-only query through `docker exec ... psql`, about twice a second this time (each sample starts a `psql`; the first round managed four a second), 80 samples over the 42 s:

| | Before (run C of round 2) | Now |
|---|---:|---:|
| Samples | 202 | 80 |
| Samples with at least one session waiting for a lock | 140 (69 %) | 44 (55 %) |
| Most sessions waiting at once | 5 of 8 | 4 of 8 |
| Waiting sessions seen, on the number series | 324 (all) | 46 (59 %) |
| Waiting sessions seen, on stock levels | 0 | 32 (41 %): `INSERT INTO stock_level ... ON CONFLICT` and `SELECT 1 FROM stock_level ... FOR UPDATE` |
| Longest wait seen | 4.0 s | 0.45 s |
| Average wait seen (time since the statement started) | 0.42 s | 0.085 s |

Sessions still queue on the `number_series` row, as they must for gap-free numbers: a posting holds it from its last statement until it commits. But a wait now lasts the length of a commit, not of a posting: five times shorter on average, ten times at the longest. The second cause of waiting is now the stock levels: two deliveries of the same items lock the same `stock_level` rows, in a consistent order (no deadlock was seen), for the length of the posting. That is inherent to keeping a running quantity per item and warehouse and is what makes long deliveries slower under concurrency. AdventureWorks sells the same few dozen bicycles in most orders, so this data meets it more than a typical company would.

## The books of AWC3

All nine checks passed after each of the four runs. After run D they are equal, to the cent, to those of `AWC2` after its run D, as they should be for the same orders:

| Check | AWC3 |
|---|---|
| Trial balance: debit = credit | 96,232,593.13 both |
| Stock value = inventory account 1300 | 12,359,838.14 both |
| Open payables = account 2000 | 352 items, 5,476,969.88 both |
| Open receivables = account 1200 | 78 items, 2,142,081.88 both |
| Goods received not invoiced = 0 | 0 |
| No negative stock | 0 rows |
| `gl.balance_sheet`: assets = liabilities + equity + unclosed result | 38,115,889.37 both |
| `gl.income_statement`: the years add up to the whole range | 821,140.87 − 277,666.89 − 680,074.66 + 0 = −136,600.68 |
| Result of all years = unclosed result + retained earnings | −136,600.68 = −680,074.66 + 543,473.98 |

FY2022 and FY2023 were closed; `year_end_close.post` took 45 and 71 ms (106 and 128 before). No request failed, was retried or was refused in 16,464 requests.

## What changed in the list of problems

- **Document numbering no longer caps throughput.** From 4 to 8 in parallel throughput now rises by 1.7 to 1.8 times instead of not at all, and the posting tail at 8 is a fifth of what it was. Numbers are still gap-free.
- **The next contention is stock levels**, for deliveries of items that many orders share. Writing a delivery's stock entries in one statement, as the ledger lines now are, would shorten the time the rows are held; the locks themselves are what keeps the running quantity right.
- **Short postings did not get cheaper, and purchase postings and payments may have got about 10 ms slower at the median** one or four at a time. Worth a profile on a quiet machine before reading more into it.
