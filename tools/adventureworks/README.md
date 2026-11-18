# AdventureWorks importer

Loads Microsoft's AdventureWorks sample company (a fictional bicycle manufacturer) into a running erpcraft server as the company **Adventure Works Cycles** (code `AWC`, USD). It uses only the public HTTP API, as any client would. It serves two purposes: a believable company to click through, and a first measurement of the server under volume. The measurements are in [RESULTS.md](RESULTS.md).

## Run

```sh
cd tools/adventureworks
bun run import.ts                       # 300 purchase orders and 300 sales orders
bun run import.ts --limit-orders 3000   # the earliest 3000 of each
bun run import.ts --all                 # everything: 4,012 purchase and 31,465 sales orders, all 19,820 customers
```

Needs `bun` (written against 1.3.9) and nothing else: there are no dependencies to install.

| Option | Default | Meaning |
|---|---|---|
| `--limit-orders N` | 300 | Load the earliest N purchase orders and the earliest N sales orders, by order date. |
| `--all` | | No limit, and every customer instead of only those needed. |
| `--company-code CODE` | `AWC` | Load into another company, for example `AWC2`. Items, vendors, customers, bills of materials and the tax code are shared by all companies and are reused. |
| `--close-years` | off | After loading, post a year-end close for every year before the last one the load reaches, oldest first, to account 3100 Retained earnings. A closed year refuses later postings, so do not use it if you mean to run again with a larger limit. |
| `--concurrency N` | 4 | Requests in flight at once. |
| `--base-url URL` | `http://127.0.0.1:8600` | The server. |
| `--env PATH` | `../../.env` | File with `ERPCRAFT_ADMIN_EMAIL` and `ERPCRAFT_ADMIN_PASSWORD`. Read at run time; the password is never written or printed. |
| `--steps a,b` | all | Any of `setup, items, vendors, customers, boms, purchases, opening_stock, sales, year_end, checks`. The company and its accounts are always looked up or created. |
| `--label NAME` | the limit | Name used in the report file. |

Each run prints a summary and writes the full numbers to `data/run-<label>-<time>.json`.

**Running it again is safe.** Master data is looked up by code and documents by memo (`AW PO 12`, `AW PO 12 receipt`, `AW SO43659 invoice`, ...) before anything is created, and a document found half-way through its workflow is continued from where it stopped. A larger limit after a smaller one adds only the difference. Every write also carries an `Idempotency-Key` derived from the request, so a retry after a lost answer cannot do the work twice. Requests that get no answer, or a 502, 503 or 504, are retried three times; anything else the server answers is recorded and not retried.

## Source and licence

- Data: AdventureWorks OLTP install script, <https://github.com/microsoft/sql-server-samples/tree/master/samples/databases/adventure-works/oltp-install-script>
- Licence: MIT, Copyright (c) Microsoft Corporation, <https://github.com/microsoft/sql-server-samples/blob/master/license.txt> (read on 2026-11-16).

The files are downloaded on the first run into `data/`, which `.gitignore` keeps out of the repository. If a download fails the script stops and says which one; it never makes data up.

### What the files look like

Eleven files are used: `Product`, `BillOfMaterials`, `Vendor`, `ProductVendor` (downloaded, not used), `PurchaseOrderHeader`, `PurchaseOrderDetail`, `Customer`, `Person`, `Store`, `SalesOrderHeader`, `SalesOrderDetail`.

- **No header rows.** Column names come from the `CREATE TABLE` statements in `instawdb.sql`. Computed columns (`LineTotal`, `StockedQty`, `TotalDue`, `SalesOrderNumber`, `AccountNumber`) are present in the files.
- **Two formats.** Nine files are tab-separated with one row per line. `Person.csv` and `Store.csv` hold XML in a field and use `+|` between fields and `&|` followed by a newline at the end of each row. The script recognises the format from the content.
- **Encoding.** In the current `master` branch none of the files used here is UTF-16: all are UTF-8 without a byte-order mark (`Person.csv` has accented names; the rest are plain ASCII), and the install script loads them with `CODEPAGE = '65001'`. The script still decodes UTF-16 if it finds a byte-order mark, in case another copy of the files is put in `data/`. `instawdb.sql` is UTF-8 with a byte-order mark and CRLF line ends.
- **Padding.** Unit codes are padded with spaces (`EA `, `CM `); fields are trimmed.
- **Decimals** are written without a leading zero (`.0000`).
- **The dates are not 2011 to 2014.** The current files run from **2022-04-15 to 2025-09-21**: Microsoft has moved the sample's dates forward. The script uses the dates as they are and opens the fiscal periods FY2022 to FY2025.
- Every row is checked for its number of fields; a row that does not fit stops the run.

## Mapping

| AdventureWorks | erpcraft | Notes |
|---|---|---|
| (none) | company `AWC`, 13 accounts, an opening capital journal entry, periods FY2022 to FY2025, warehouse `MAIN`, purchasing and sales settings, an account rule naming 1300 as the inventory account, tax code `AW-ST8` (8 %) with its tax settings | Created first. |
| `Product` (504) | `item` | `code` = `ProductNumber`, `name` = `Name`, `uom` = `EA`. |
| `Vendor` (104) | `vendor` | `code` = `AccountNumber`, `name`, `active` = `ActiveFlag`. |
| `Customer` + `Store` + `Person` | `customer` | `code` = `AccountNumber` (`AW00000001`). The name is the store's name if the customer has a store, else the person's first and last name. |
| `BillOfMaterials` | `bom` + `bom_line` | One `bom` per assembly, code `BOM-<ProductNumber>`, output quantity 1; one line per direct component with `PerAssemblyQty`. 238 bills, 2,383 lines. |
| `PurchaseOrderHeader` + `Detail` | `purchase_order`, then `purchase_receipt`, `vendor_invoice`, `payment` | By `Status`, below. |
| `SalesOrderHeader` + `Detail` | `sales_order`, then `sales_delivery`, `sales_invoice`, `payment` | All source orders have status 5, shipped. |
| (none) | `stock_adjustment` "AW opening stock" | See below. |

Chart of accounts: 1000 Cash at bank, 1200 Accounts receivable, 1300 Inventory, 2000 Accounts payable, 2100 Goods received not invoiced, 2200 Sales tax payable, 3000 Opening balance equity, 3100 Retained earnings, 3200 Share capital, 4000 Sales revenue, 5000 Cost of goods sold, 5200 Purchase price variance, 5300 Sales tax on purchases.

Purchase orders, by source status:

| Source `Status` | In erpcraft | Dates |
|---|---|---|
| 1 Pending | Drafted, not approved. | Order: `OrderDate` |
| 2 Approved | Approved and left open. | |
| 3 Rejected | Drafted, then cancelled. | |
| 4 Complete | Approved; received and invoiced for `StockedQty` (received minus rejected) at the order's unit price; closed short if less was stocked than ordered; paid. | Receipt and invoice: `ShipDate`. Payment: 30 days later. |

Sales orders: approved on `OrderDate`; delivered and invoiced in full on `ShipDate`; paid on `ShipDate` if the order was placed online (a card sale), otherwise 30 days later.

## Simplifications

Each of these is a difference between what the source says and what ends up in the books.

- **Opening capital is invented.** A journal entry on the first day of the data (2022-04-15) pays 25,000,000.00 into the bank account against 3200 Share capital, so the bank account can pay for the purchases. The company `AWC` loaded before this was added has none and is overdrawn.
- **Payment terms are invented.** AdventureWorks records no payments. The script pays every invoice in full, 30 days after it (card sales the same day), **unless that day lies after the last ship date of the orders being loaded**. Those invoices stay open, so payables and receivables are not empty. A later run with a larger limit pays them.
- **Currency.** 13,976 of the 31,465 sales orders carry a currency rate (they were sold in another currency). The server's sales and purchasing work in the accounting currency only, so every amount is loaded as US dollars as it stands and the rate is ignored.
- **Tax.** Orders carry no tax in erpcraft; invoices do, per line, by a tax code with one rate. Every invoice line gets the one code `AW-ST8` at 8 %. That matches the source for all purchase orders and for online sales (8 % of the subtotal). Reseller sales carry between 8.7 % and 9.9 % in the source, depending on where they ship; those are loaded at 8 % and so come out lower than the source. Tax on purchases goes to an expense account, as US sales tax cannot be reclaimed. The tax is rounded per line by the server, not per order.
- **Freight** (2.5 % of the subtotal on most orders) is not loaded: neither orders nor invoices have a place for a charge that is not an item.
- **Discounts and special offers.** 3,282 sales lines have a `UnitPriceDiscount`. There is no discount field, so the line is loaded at its net unit price, `UnitPrice × (1 − discount)`, rounded half up to 6 decimals by the importer, because the server refuses a unit price with more than 6 (source prices and discounts have 4 each, so a net price can have 8). The special offer that caused the discount is dropped.
- **Rounding.** Source line totals have up to 6 decimals and the order subtotal is their sum; the server rounds each line to cents. Revenue differs from the source subtotals by a fraction of a cent per line.
- **Order status.** Purchase orders are mapped as in the table above. The source gives received quantities even on pending, approved and rejected orders; those contradict the status and are ignored. Sales orders are all "shipped" and get the full flow.
- **Rejected goods.** `RejectedQty` has no counterpart (there is no return of goods, and no inspection step). Only `StockedQty` is received, and only that is invoiced, although the source header bills the full ordered quantity. Lines with nothing stocked are left out of the receipt; an order where nothing was stocked is approved and closed.
- **Products without cost.** 200 products have a standard cost of zero. None of them is sold. Purchased items enter stock at the purchase order price, so the standard cost is used only for opening stock, and never when it is zero.
- **Opening stock.** AdventureWorks sells bicycles that it makes, but its work orders are not loaded, and the purchase orders buy components. So before the sales step, one stock adjustment dated the day before the first sale (2022-05-29) brings in what the selected sales orders need and the warehouse does not have, at `StandardCost`, against Opening balance equity. A later, larger run adds a second adjustment for the difference. Purchased components pile up in stock for the same reason: nothing consumes them.
- **Bills of materials.** Only rows without an `EndDate` (2,480 of 2,679), and of those only rows with an assembly (97 rows have none: they mark the top of a tree). Each assembly becomes one single-level bill with its direct components; sub-assemblies appear as ordinary component items that have a bill of their own. The component's unit (`EA`, `OZ`, `IN`) is dropped, since an item has one unit. No production orders are created.
- **Units of measure.** Every item is `EA`. The vendor's purchase unit in `ProductVendor` (case, carton, ...) is not used.
- **Customers.** A limited run loads the 635 store customers that have a contact person (the ones that order) and every customer of an order being loaded. `--all` loads all 19,820. The 701 customer records that have a store and no person (635 of them the same store as a record that has one) never order and are loaded only with `--all`.
- **One warehouse, one inventory account.** AdventureWorks has 14 inventory locations; everything goes to `MAIN`. One account rule (`purpose: inventory`, no scope) makes account 1300 the inventory account for receipts, deliveries, adjustments and production, so the value of stock on hand can be checked against one balance.
- **Not loaded at all:** addresses, sales territories and salespeople, ship methods, credit cards, employees, work orders and routings, product categories, models, descriptions and photos, price and cost history, the `ProductVendor` price list.

## Where the server refuses the data

- **Purchase orders of inactive vendors.** Four vendors have `ActiveFlag = 0` and still have 203 purchase orders in the source, 185 of them complete. The server refuses to approve an order of an inactive vendor (`the vendor is inactive`); in the first runs those orders stayed drafts (147 of the first 3,000; see RESULTS.md). The check is hard-coded today and is meant to become a policy point. Until then the importer **switches those four vendors to active before the purchase step and back to inactive after it**, also when the step fails. While the step runs, anyone else on the server sees them as active.
- **Unit prices with more than 6 decimals** are refused, not rounded. The importer rounds net prices to 6 decimals itself (see Discounts above).
