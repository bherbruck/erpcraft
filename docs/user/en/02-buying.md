# 2. Buying

## Purchase order

1. **Purchasing → Purchase orders → New.** Choose the vendor, then add lines with an item, the warehouse it goes to, the quantity and the price. Leave the price empty and the vendor's price list, or else the last price you paid, fills it in.
2. **Approve** (`purchase_order.approve`). The order gets its number and is sent to the vendor with *Send by email* (`document.send`) or printed.

## Receiving the goods

- On the open order, **Receive** (`purchase_order.receive`) drafts a receipt with everything still to come. Change the quantities to what arrived, and give lot or serial numbers for tracked items.
- **Post** it (`purchase_receipt.post`). Stock goes up at the order's price, and the value waits on *goods received not invoiced* until the invoice comes.

## The vendor's invoice

- **Invoice** (`purchase_order.invoice`) drafts the vendor invoice from what was received. Enter the vendor's invoice number, check the prices and the tax codes, add freight as a charge line.
- **Post** it (`vendor_invoice.post`). The amount owed becomes an open item on accounts payable, due as the payment terms say.
- An e-invoice (XRechnung, ZUGFeRD) can be read straight in with *Import e-invoice* (`einvoice.import`).
- An invoice without an order (rent, a service) is a vendor invoice with charge or account lines.

## Paying

- **Payment run** (`payment_run.propose`, then `approve` and `post`). It proposes everything due by a date and takes discounts still open, then writes a SEPA or NACHA file for your bank.
- Or pay one invoice: **Payments → New** with the vendor's open items (`payment.post`).

Returns go back with **Return** on the order, and the vendor's credit note with **Credit** on the invoice.
