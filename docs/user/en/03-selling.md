# 3. Selling

## Quote and order

1. A **sales quote** (`sales_quote`) is optional. Issue it (`sales_quote.issue`) and send it. When the customer accepts, **Accept** (`sales_quote.accept`) turns it into a sales order; **Lose** keeps why it was lost.
2. A **sales order**: customer, lines with item, warehouse, quantity. Prices come from the price lists when left empty. **Approve** (`sales_order.approve`): the order is numbered and the stock reserved.

## Delivery

- **Deliver** (`sales_order.deliver`) drafts the delivery note (Lieferschein) with what is still to ship. Name lots or serial numbers for tracked items.
- **Post** it (`sales_delivery.post`): stock goes out at its cost, and cost of goods sold is booked.

## Invoice

- **Invoice** (`sales_order.invoice`) drafts the invoice for what was delivered and not yet invoiced. The tax code comes from the tax rules (the customer, the item, where the goods went), and can be changed.
- **Post** it (`sales_invoice.post`). Revenue and tax are booked and the customer owes an open item.
- **Send by email** (`document.send`) mails it as a PDF, or as an e-invoice if the customer takes one.

A German customer's invoice prints in German with what § 14 UStG requires: the invoice number and date, the delivery date (Leistungsdatum), your tax number or VAT ID, and tax by rate.

## Being paid

- **Payments → New**, incoming, with the customer's open items (`payment.post`), or match the line of an imported bank statement (*Banking → Import statement*, then *Match line*).
- An early-payment discount is taken automatically if the payment is dated within the discount days.

## When customers pay late

- **Customer statements** (`customer_statement.issue_all`) at month-end show each customer what is open.
- **Dunning runs** (`dunning_run.propose`, review, `post`, `send`) send reminders by level, with fees and interest as your dunning levels say.
