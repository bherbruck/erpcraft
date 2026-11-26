# erpcraft user guide

The guide walks through one month of a small trading company:

1. [Setting up a company](01-setup.md)
2. [Buying](02-buying.md)
3. [Selling](03-selling.md)
4. [Month-end](04-month-end.md)

Everything you do in the admin UI is a **command**. The same command can be run over the API (`POST /api/commands/{name}`) or by an AI agent over MCP, under the same rights. This guide names each command in brackets, such as (`sales_invoice.post`), so you can find it in either place.

**How documents work.**
- Orders, deliveries, invoices and payments are *documents*. A document starts as a *draft*, which you can change as often as you like.
- Posting it (or approving an order) locks it, gives it its number and writes it to the books.
- A posted document is never edited: it is *reversed*, and a corrected one is posted.
- On a draft, the button that comes first is the next step (approve, post, issue).

**Language.** The UI follows your browser's language. With German selected, labels, messages and the documents you print for a German customer are in German. Set a customer's or vendor's language in its *Party details* (profile → language).
