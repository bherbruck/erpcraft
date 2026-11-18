//! The business judgements sales leaves to the company.

use erpcraft_kernel::{Decimal, PolicyPoint};
use serde_json::{Value, json};

pub(crate) const OVER_DELIVERY: &str = "sales_delivery.over_delivery";
pub(crate) const PRICE_DIFFERS: &str = "sales_invoice.price_differs";
pub(crate) const INACTIVE_CUSTOMER: &str = "customer.inactive";
pub(crate) const CLOSED_ORDER: &str = "sales_order.closed";
pub(crate) const CREDIT_WITHOUT_RETURN: &str = "sales_credit_note.without_return";
pub(crate) const REOPEN_ON_RETURN: &str = "sales_order.reopen_on_return";

pub(crate) fn points() -> Vec<PolicyPoint> {
    vec![
        PolicyPoint::new(
            OVER_DELIVERY,
            "Asked for every line of a sales delivery when it is posted: may this quantity be \
             delivered against its order line? Unless configured otherwise, a delivery that \
             takes the line above the quantity ordered is refused. A company that ships a \
             little more than was ordered allows it up to a limit, for example with an allow \
             rule when `facts.over_percent <= 5` followed by a refuse rule. Goods delivered \
             beyond the order can be invoiced like the rest.",
        )
        .number("ordered", "Quantity ordered on the order line")
        .as_quantity()
        .number(
            "already_delivered",
            "Quantity delivered on the order line before this delivery",
        )
        .as_quantity()
        .number("delivering", "Quantity this delivery line sends out")
        .as_quantity()
        .number(
            "over_by",
            "By how much the line would be delivered beyond the quantity ordered; zero when it \
             stays within the order",
        )
        .as_quantity()
        .number(
            "over_percent",
            "over_by as a percentage of the quantity ordered",
        )
        .as_percent()
        .refuse_when("facts.over_by > 0")
        .refusal("more would be delivered than was ordered"),
        PolicyPoint::new(
            PRICE_DIFFERS,
            "Asked for every line of a sales invoice when it is posted: may the customer be \
             charged this unit price for goods ordered at the order price? Unless configured \
             otherwise any price is accepted. A company that wants invoices to keep to the \
             order refuses them beyond a limit, for example when \
             `facts.difference_percent < -2` to stop unagreed discounts.",
        )
        .number("order_price", "Unit price on the order line")
        .as_unit_price(None)
        .number("invoice_price", "Unit price on the invoice line")
        .as_unit_price(None)
        .number(
            "difference",
            "Invoice price minus order price, per unit; negative when the customer is charged \
             less",
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
            INACTIVE_CUSTOMER,
            "Asked when a sales order is approved, and when a delivery or a sales invoice \
             against an order is posted: may this be done for the order's customer? Unless \
             configured otherwise an order for an inactive customer is not approved, while what \
             was ordered before the customer became inactive can still be delivered and \
             invoiced. A company that ships nothing more to an inactive customer refuses \
             deliveries too, with a refuse rule when \
             `facts.doc_type == 'sales_delivery' && !facts.active`.",
        )
        .text(
            "doc_type",
            "What is being done: sales_order (approving it), sales_delivery or sales_invoice \
             (posting it)",
        )
        .text("customer_code", "The customer's code")
        .boolean("active", "Whether the customer is active")
        .refuse_when("facts.doc_type == 'sales_order' && !facts.active")
        .refusal("the customer is inactive"),
        PolicyPoint::new(
            CLOSED_ORDER,
            "Asked when a delivery or a sales invoice is drafted against, or posted against, a \
             sales order that is closed, whether it closed by itself when it was complete or \
             was closed by hand. Unless configured otherwise a delivery is refused, and an \
             invoice only for goods the order still holds delivered and not invoiced. A \
             company that still ships the rest of an order it closed short allows deliveries, \
             for example when `facts.doc_type == 'sales_delivery' && !facts.complete`. The \
             order stays closed: what goes out is counted on its lines, and it can be invoiced \
             against the closed order.",
        )
        .text(
            "doc_type",
            "The document drafted or posted: sales_delivery or sales_invoice",
        )
        .boolean(
            "complete",
            "Whether everything ordered had been delivered and everything delivered invoiced: \
             true when the order closed by itself, false when it was closed short by hand",
        )
        .number("ordered", "Quantity ordered, all lines together")
        .as_quantity()
        .number("delivered", "Quantity delivered so far, all lines together")
        .as_quantity()
        .number("invoiced", "Quantity invoiced so far, all lines together")
        .as_quantity()
        .number(
            "to_invoice",
            "Quantity delivered and not yet invoiced, all lines together",
        )
        .as_quantity()
        .refuse_when("facts.doc_type == 'sales_delivery' || facts.to_invoice <= 0")
        .refusal("the sales order is closed"),
        PolicyPoint::new(
            CREDIT_WITHOUT_RETURN,
            "Asked for every line of a sales credit note when it is posted: may the customer be \
             credited for goods that did not come back? The part of the line covered by goods \
             returned and not yet credited takes back revenue for goods the company has again; \
             the rest is a credit of price only (a discount, an allowance, a complaint settled) \
             and takes back revenue alone. Unless configured otherwise a credit beyond what came \
             back is refused. A company that grants allowances allows it, perhaps up to a \
             limit, for example when `facts.credit_price <= facts.invoice_price * 0.1`.",
        )
        .number("crediting", "Quantity this credit note line credits")
        .as_quantity()
        .number(
            "returned",
            "Quantity come back on the order line and not yet credited",
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
        .refusal("goods can be credited only once they have come back"),
        PolicyPoint::new(
            REOPEN_ON_RETURN,
            "Asked when a sales return is posted against an order that is closed: should the \
             order open again, so that replacement goods can be delivered against it? Unless \
             configured otherwise it stays closed; a replacement is then a new order. A \
             company that replaces what customers send back allows it.",
        )
        .number("ordered", "Quantity ordered, all lines together")
        .as_quantity()
        .number(
            "delivered",
            "Quantity delivered and kept after this return, all lines together",
        )
        .as_quantity()
        .number(
            "returning",
            "Quantity this return brings back, all lines together",
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
pub(crate) fn reopen_on_return(ordered: Decimal, delivered: Decimal, returning: Decimal) -> Value {
    json!({
        "ordered": shown(ordered),
        "delivered": shown(delivered),
        "returning": shown(returning),
    })
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

/// The facts of [`INACTIVE_CUSTOMER`].
pub(crate) fn inactive_customer(doc_type: &str, customer_code: &str, active: bool) -> Value {
    json!({ "doc_type": doc_type, "customer_code": customer_code, "active": active })
}

/// The facts of [`CLOSED_ORDER`], from each line's quantity ordered,
/// delivered and invoiced.
pub(crate) fn closed_order(
    doc_type: &str,
    complete: bool,
    lines: impl IntoIterator<Item = (Decimal, Decimal, Decimal)>,
) -> Value {
    let (mut ordered, mut delivered, mut invoiced) = (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
    for (qty, qty_delivered, qty_invoiced) in lines {
        ordered += qty;
        delivered += qty_delivered;
        invoiced += qty_invoiced;
    }
    json!({
        "doc_type": doc_type,
        "complete": complete,
        "ordered": shown(ordered),
        "delivered": shown(delivered),
        "invoiced": shown(invoiced),
        "to_invoice": shown(delivered - invoiced),
    })
}

fn shown(value: Decimal) -> String {
    value.normalize().to_string()
}

/// The facts of [`OVER_DELIVERY`] for one delivery line.
pub(crate) fn over_delivery(
    ordered: Decimal,
    already_delivered: Decimal,
    delivering: Decimal,
) -> Value {
    let over_by = (already_delivered + delivering - ordered).max(Decimal::ZERO);
    let over_percent = percent(over_by, ordered);
    json!({
        "ordered": shown(ordered),
        "already_delivered": shown(already_delivered),
        "delivering": shown(delivering),
        "over_by": shown(over_by),
        "over_percent": shown(over_percent),
    })
}
