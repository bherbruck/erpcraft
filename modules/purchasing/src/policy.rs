//! The business judgements purchasing leaves to the company.

use erpcraft_kernel::{Decimal, PolicyPoint};
use serde_json::{Value, json};

pub(crate) const OVER_RECEIPT: &str = "purchase_receipt.over_receipt";
pub(crate) const PRICE_DIFFERS: &str = "vendor_invoice.price_differs";
pub(crate) const INACTIVE_VENDOR: &str = "vendor.inactive";
pub(crate) const CLOSED_ORDER: &str = "purchase_order.closed";
pub(crate) const CREDIT_WITHOUT_RETURN: &str = "vendor_credit_note.without_return";
pub(crate) const REOPEN_ON_RETURN: &str = "purchase_order.reopen_on_return";
pub(crate) const WITHOUT_ORDER: &str = "vendor_invoice.without_order";
pub(crate) const WITHOUT_INVOICE: &str = "vendor_credit.without_invoice";
pub(crate) const APPROVE_ORDER: &str = "purchase_order.approve";

pub(crate) fn points() -> Vec<PolicyPoint> {
    vec![
        PolicyPoint::new(
            WITHOUT_INVOICE,
            "Asked when a vendor credit not tied to an invoice (a volume rebate, goodwill) is \
             posted: may it be posted? Unless configured otherwise it is allowed; a company \
             that wants such credits checked can require approval, for example above an \
             amount with `facts.total > 1000`.",
        )
        .number(
            "total",
            "What the vendor credits, tax included, in the credit's currency",
        )
        .as_money(Some("currency"))
        .text("currency", "The credit's currency")
        .text("vendor_code", "The vendor's code")
        .refusal("a credit without an invoice needs approval"),
        PolicyPoint::new(
            WITHOUT_ORDER,
            "Asked when a vendor invoice without a purchase order (rent, a service, a machine) \
             is posted: may it be posted? No order was approved for it, so this is where a \
             company that wants such invoices checked says so, for example requiring approval \
             or refusing them above an amount with `facts.total > 1000`. Unless configured \
             otherwise it is allowed.",
        )
        .number(
            "total",
            "What the invoice comes to, tax included, in its currency",
        )
        .as_money(Some("currency"))
        .text("currency", "The invoice's currency")
        .text("vendor_code", "The vendor's code")
        .refusal("an invoice without a purchase order needs approval"),
        PolicyPoint::new(
            APPROVE_ORDER,
            "Asked when a purchase order is approved: may it be, as it stands? Unless configured \
             otherwise any order is approved. A company that wants a second person to approve \
             large orders adds a rule with effect approve when, for example, \
             `facts.total_net > 10000 && facts.currency == 'USD'`, naming who approves.",
        )
        .number(
            "total_net",
            "The order's total without tax, in its currency",
        )
        .as_money(Some("currency"))
        .labelled("Net total")
        .text("currency", "The order's currency")
        .number("lines", "How many lines the order has"),
        PolicyPoint::new(
            OVER_RECEIPT,
            "Asked for every line of a purchase receipt when it is posted: may this quantity be \
             received against its order line? Unless configured otherwise, a receipt that takes \
             the line above the quantity ordered is refused. A company that accepts a little \
             more than it ordered allows it up to a limit, for example with an allow rule when \
             `facts.over_percent <= 5` followed by a refuse rule. Goods received beyond the \
             order come into stock at the order price and can be invoiced like the rest.",
        )
        .number("ordered", "Quantity ordered on the order line")
        .as_quantity()
        .number(
            "already_received",
            "Quantity received on the order line before this receipt",
        )
        .as_quantity()
        .number("receiving", "Quantity this receipt line brings in")
        .as_quantity()
        .number(
            "over_by",
            "By how much the line would be received beyond the quantity ordered; zero when it \
             stays within the order",
        )
        .as_quantity()
        .number(
            "over_percent",
            "over_by as a percentage of the quantity ordered",
        )
        .as_percent()
        .refuse_when("facts.over_by > 0")
        .refusal("more would be received than was ordered"),
        PolicyPoint::new(
            PRICE_DIFFERS,
            "Asked for every line of a vendor invoice when it is posted: may the vendor charge \
             this unit price for goods ordered at the order price? Unless configured otherwise \
             any price is accepted, and the difference from what the goods were received for \
             goes to purchase price variance. A company that wants invoices checked refuses \
             them beyond a limit, for example when `facts.difference_percent > 2`.",
        )
        .number("order_price", "Unit price on the order line")
        .as_unit_price(None)
        .number("invoice_price", "Unit price on the invoice line")
        .as_unit_price(None)
        .number(
            "difference",
            "Invoice price minus order price, per unit; negative when the vendor charges less",
        )
        .as_unit_price(None)
        .number(
            "difference_percent",
            "difference as a percentage of the order price; 100 when the order price is zero \
             and the invoice price is not",
        )
        .as_percent()
        .refusal("the invoice price differs too much from the order price"),
        PolicyPoint::new(
            INACTIVE_VENDOR,
            "Asked when a purchase order is approved, and when a receipt or a vendor invoice \
             against an order is posted: may this be done for the order's vendor? Unless \
             configured otherwise an order for an inactive vendor is not approved, while goods \
             ordered before the vendor became inactive can still be received and invoiced. A \
             company that wants nothing more from an inactive vendor refuses receipts and \
             invoices too, with a refuse rule when `!facts.active`.",
        )
        .text(
            "doc_type",
            "What is being done: purchase_order (approving it), purchase_receipt or \
             vendor_invoice (posting it)",
        )
        .text("vendor_code", "The vendor's code")
        .boolean("active", "Whether the vendor is active")
        .refuse_when("facts.doc_type == 'purchase_order' && !facts.active")
        .refusal("the vendor is inactive"),
        PolicyPoint::new(
            CLOSED_ORDER,
            "Asked when a receipt or a vendor invoice is drafted against, or posted against, a \
             purchase order that is closed, whether it closed by itself when it was complete or \
             was closed by hand. Unless configured otherwise a receipt is refused, and an \
             invoice only for goods the order still holds received and not invoiced. A company \
             that takes late deliveries on an order it closed short allows receipts, for \
             example when `facts.doc_type == 'purchase_receipt' && !facts.complete`. The order \
             stays closed: what arrives is counted on its lines, and it can be invoiced against \
             the closed order.",
        )
        .text(
            "doc_type",
            "The document drafted or posted: purchase_receipt or vendor_invoice",
        )
        .boolean(
            "complete",
            "Whether everything ordered had been received and everything received invoiced: \
             true when the order closed by itself, false when it was closed short by hand",
        )
        .number("ordered", "Quantity ordered, all lines together")
        .as_quantity()
        .number("received", "Quantity received so far, all lines together")
        .as_quantity()
        .number("invoiced", "Quantity invoiced so far, all lines together")
        .as_quantity()
        .number(
            "to_invoice",
            "Quantity received and not yet invoiced, all lines together",
        )
        .as_quantity()
        .refuse_when("facts.doc_type == 'purchase_receipt' || facts.to_invoice <= 0")
        .refusal("the purchase order is closed"),
        PolicyPoint::new(
            CREDIT_WITHOUT_RETURN,
            "Asked for every line of a vendor credit note when it is posted: may the vendor \
             credit goods that were not returned? The part of the line covered by goods \
             returned and not yet credited clears goods received not invoiced; the rest is a \
             credit of price only (a discount, an allowance) and goes to purchase price \
             variance. Unless configured otherwise a credit beyond what was returned is \
             refused. A company that accepts allowances from its vendors allows it.",
        )
        .number("crediting", "Quantity this credit note line credits")
        .as_quantity()
        .number(
            "returned",
            "Quantity returned on the order line and not yet credited",
        )
        .as_quantity()
        .number(
            "without_return",
            "crediting less returned: what would be credited for price only; zero when the \
             returns cover the line",
        )
        .as_quantity()
        .number("invoice_price", "Unit price on the invoice line credited")
        .as_unit_price(None)
        .number("credit_price", "Unit price on the credit note line")
        .as_unit_price(None)
        .refuse_when("facts.without_return > 0")
        .refusal("goods can be credited only once they have been returned"),
        PolicyPoint::new(
            REOPEN_ON_RETURN,
            "Asked when a purchase return is posted against an order that is closed: should \
             the order open again, so that replacement goods can be received against it? \
             Unless configured otherwise it stays closed; replacements are then ordered anew. \
             A company that has vendors replace what it sends back allows it.",
        )
        .number("ordered", "Quantity ordered, all lines together")
        .as_quantity()
        .number(
            "received",
            "Quantity received and kept after this return, all lines together",
        )
        .as_quantity()
        .number(
            "returning",
            "Quantity this return sends back, all lines together",
        )
        .as_quantity()
        .refuse_by_default()
        .refusal("the order stays closed"),
    ]
}

/// The facts of [`CREDIT_WITHOUT_RETURN`] for one credit note line.
pub(crate) fn credit_without_return(
    crediting: Decimal,
    returned: Decimal,
    invoice_price: Decimal,
    credit_price: Decimal,
) -> Value {
    json!({
        "crediting": shown(crediting),
        "returned": shown(returned),
        "without_return": shown((crediting - returned).max(Decimal::ZERO)),
        "invoice_price": shown(invoice_price),
        "credit_price": shown(credit_price),
    })
}

/// The facts of [`REOPEN_ON_RETURN`].
pub(crate) fn reopen_on_return(ordered: Decimal, received: Decimal, returning: Decimal) -> Value {
    json!({
        "ordered": shown(ordered),
        "received": shown(received),
        "returning": shown(returning),
    })
}

/// The facts of [`INACTIVE_VENDOR`].
pub(crate) fn inactive_vendor(doc_type: &str, vendor_code: &str, active: bool) -> Value {
    json!({ "doc_type": doc_type, "vendor_code": vendor_code, "active": active })
}

/// The facts of [`CLOSED_ORDER`], from each line's quantity ordered,
/// received and invoiced.
pub(crate) fn closed_order(
    doc_type: &str,
    complete: bool,
    lines: impl IntoIterator<Item = (Decimal, Decimal, Decimal)>,
) -> Value {
    let (mut ordered, mut received, mut invoiced) = (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
    for (qty, qty_received, qty_invoiced) in lines {
        ordered += qty;
        received += qty_received;
        invoiced += qty_invoiced;
    }
    json!({
        "doc_type": doc_type,
        "complete": complete,
        "ordered": shown(ordered),
        "received": shown(received),
        "invoiced": shown(invoiced),
        "to_invoice": shown(received - invoiced),
    })
}

/// The facts of [`APPROVE_ORDER`], from an order's summary.
pub(crate) fn approve_order(summary: &Value, lines: usize) -> Value {
    json!({
        "total_net": summary["total_net"].as_str().unwrap_or("0"),
        "currency": summary["currency"].as_str().unwrap_or_default(),
        "lines": lines,
    })
}

fn shown(value: Decimal) -> String {
    value.normalize().to_string()
}

fn percent(part: Decimal, whole: Decimal) -> Decimal {
    if part.is_zero() {
        Decimal::ZERO
    } else if whole.is_zero() {
        Decimal::ONE_HUNDRED
    } else {
        (part * Decimal::ONE_HUNDRED / whole).round_dp(6)
    }
}

/// The facts of [`WITHOUT_INVOICE`] for a vendor credit.
pub(crate) fn without_invoice(total: Decimal, currency: &str, vendor_code: &str) -> Value {
    json!({ "total": shown(total), "currency": currency, "vendor_code": vendor_code })
}

/// The facts of [`WITHOUT_ORDER`] for a direct vendor invoice.
pub(crate) fn without_order(total: Decimal, currency: &str, vendor_code: &str) -> Value {
    json!({ "total": shown(total), "currency": currency, "vendor_code": vendor_code })
}

/// The facts of [`OVER_RECEIPT`] for one receipt line.
pub(crate) fn over_receipt(
    ordered: Decimal,
    already_received: Decimal,
    receiving: Decimal,
) -> Value {
    let over_by = (already_received + receiving - ordered).max(Decimal::ZERO);
    json!({
        "ordered": shown(ordered),
        "already_received": shown(already_received),
        "receiving": shown(receiving),
        "over_by": shown(over_by),
        "over_percent": shown(percent(over_by, ordered)),
    })
}

/// The facts of [`PRICE_DIFFERS`] for one invoice line.
pub(crate) fn price_differs(order_price: Decimal, invoice_price: Decimal) -> Value {
    let difference = invoice_price - order_price;
    json!({
        "order_price": shown(order_price),
        "invoice_price": shown(invoice_price),
        "difference": shown(difference),
        "difference_percent": shown(percent(difference, order_price)),
    })
}
