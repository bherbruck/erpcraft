# MCP trial: results

Run on 2026-11-25 against the month scenario (Ohio Widgets, March 2026). **Before** is main at 3b17803 plus the per-command request limits. **After** has the changes listed below. Both runs used the same `trial.ts`, so the agent behaves the same way and only the server differs.

| Task | Requests | Errors | Tool-description tokens | Result tokens | Stuck | Outcome after |
|---|---|---|---|---|---|---|
| create a customer and a sales order for 3 widgets | 12 → 10 | 1 → 1 | 73,495 → 20,211 | 155,319 → 4,090 | 3 → 0 | customer GLOBEX (code, name required), order 01a21550-1698-7026-8eed-8a74480c2b65 for 37.5000 |
| what's overdue for Acme | 5 → 6 | 0 → 0 | 3,243 → 8,372 | 1,598 → 2,717 | 2 → 0 | 0 overdue item(s),  |
| post this vendor invoice from an e-invoice file | 8 → 13 | 2 → 2 | 9,623 → 14,041 | 4,587 → 8,511 | 3 → 0 | posted VI-2026-00005 |
| why didn't order SO-2026-00004 ship | 7 → 8 | 0 → 0 | 41,326 → 22,771 | 3,459 → 6,310 | 3 → 0 | status open; 0 delivery(ies); ordered 500.000000, delivered 0, on hand 22 |
| close March | 50 → 8 | 5 → 0 | 29,732 → 6,687 | 5,579 → 7,331 | 3 → 0 | 1 draft(s) in March (journal_entry); the close would go through |
| **All five** | 82 → 45 | 8 → 3 | 157,419 → 72,082 | 170,542 → 28,959 | 14 → 0 | |

The whole `tools/list`: 213 tools, 456,662 → 159,172 tokens.

The order costs 37.50 because the agent was told the price. With no price list for widgets, the first `document_create` is refused with "line 1: give a unit price", and the agent asks its user. Acme owes nothing: its one invoice is paid.

## Where the agent got stuck before, and what changed

1. **Most of the tool tokens were output schemas** (366k of 457k). Every tool that returns a document carried the full document view schema, about 3,500 tokens, and actions carried it twice (plain and preview). Now there is one short document schema, and a preview's result is not described a second time. The full list went from 457k to 159k tokens. The tools for sales orders (`erpcraft_tools(resource: sales_order)`) went from 36.6k to 13.1k tokens, and those now include the generic tools and the order's fields.
2. **Creating a customer or a sales order could not be found from the resource.** `erpcraft_tools(resource: customer)` returned nothing, because creating a record is the kernel's generic `entity_create`. For the fields, the agent had to read `meta_describe(sales)`, which is 77k tokens. Now `erpcraft_tools` with a resource returns the generic tools the caller may use on it, plus the resource's fields to write, with types in words and which are required, its lines and its states.
3. **The core tools were not on the "start here" path.** `search_records`, `entity_list` and `document_get` are needed by every task, but no module lists them. `erpcraft_modules` now names them under `core_tools`.
4. **Document types and records were given by label** ("Customer credit"), and the tools take names (`sales_credit`). Guessing names from labels cost five failed calls in "close March". They now come as `{name, label}`.
5. **There was no way to fetch tools whose names the agent already had**, for example from a prompt. `erpcraft_tools` now takes `names`.
6. **Closing a period meant one `document_list` per document type** (24 calls) to find drafts. `fiscal_period.close` now returns `drafts`, and the `close_month` prompt previews the close to see them. That task went from 50 requests to 8.
7. **After importing an e-invoice, the agent did not know which tool posts the draft.** A tool that creates a document now names the next ones (`einvoice_import`: "next: vendor_invoice_post").
8. **A wrong list filter did not say which filters exist.** `entity_list` on `tax_code` with `company_id` now answers "tax_code records belong to no company; the fields of tax_code are ...".
9. **The descriptions of `preview`, `idempotency_key` and `view` were about 150 tokens in each of some 150 tools.** They are one line each now. The server's instructions say when to use them.

## Still open

- **What remains of the full list is mostly report output schemas** (about 80k). They describe real columns, so they were left as they are.
- **The description tokens for "what's overdue" went up** (3k to 8k). The agent now loads the core tools' schemas once at the start, which every later task reuses within a session. In this trial each task is a fresh session, so the cost is paid five times.
- **"Why didn't it ship" still leaves the diagnosis to the agent.** It compares ordered, delivered and on hand itself. A query that says what blocks an order's delivery (stock, credit hold, an approval) would answer it in one call.
- **The agent never needed** `meta_describe`, the guide resources or the `explain_refusal` prompt. The refusals it met were sentences it could act on.
