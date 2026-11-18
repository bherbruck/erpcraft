//! What purchasing and sales documents share about their lines: a line is
//! stock (an item and its warehouse), a charge, or an account; its net
//! amount is its quantity times its price less its discounts, rounded per
//! line.

use erpcraft_kernel::document;
use erpcraft_kernel::{Alternatives, Ctx, Decimal, Error, FieldMeta, FieldType, Result};
use serde_json::Value;
use uuid::Uuid;

use crate::line_net;

/// Check what a document line is and its price and discounts: exactly one
/// of stock, a charge or an account; a quantity above zero; a price not
/// below zero with at most 6 decimal places; discounts from zero (zero says
/// "none") that do not come to more than the line; an account a plain line
/// may post to; an active charge. `n` is the line's position.
#[allow(clippy::too_many_arguments)]
pub async fn check_line(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    n: usize,
    stock: bool,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    qty: Decimal,
    unit_price: Decimal,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
) -> Result<()> {
    let refuse = |field: &str, why: String| Error::line("lines", n, Some(field), why);
    let kinds =
        usize::from(stock) + usize::from(charge_id.is_some()) + usize::from(account_id.is_some());
    if kinds != 1 {
        return Err(refuse(
            "item_id",
            "a line is an item with its warehouse, a charge, or an account: exactly one".into(),
        ));
    }
    if qty <= Decimal::ZERO {
        return Err(refuse("qty", "the quantity must be more than zero".into()));
    }
    if unit_price.is_sign_negative() {
        return Err(refuse(
            "unit_price",
            "the unit price cannot be negative".into(),
        ));
    }
    if unit_price.normalize().scale() > 6 {
        return Err(refuse(
            "unit_price",
            "a unit price has at most 6 decimal places".into(),
        ));
    }
    if discount_percent.is_some_and(|p| p < Decimal::ZERO || p > Decimal::ONE_HUNDRED) {
        return Err(refuse(
            "discount_percent",
            "a discount percent is from 0 to 100".into(),
        ));
    }
    if discount_amount.is_some_and(|a| a < Decimal::ZERO || a.normalize().scale() > 4) {
        return Err(refuse(
            "discount_amount",
            "a discount amount is not below zero, with at most 4 decimal places".into(),
        ));
    }
    if line_net(qty, unit_price, discount_percent, discount_amount, 4).is_none() {
        return Err(refuse(
            "discount_amount",
            "the discounts come to more than the line".into(),
        ));
    }
    if let Some(account) = account_id
        && let Some(why) =
            erpcraft_gl::line_account_refusal(&mut *ctx.conn, company_id, account).await?
    {
        return Err(refuse("account_id", why));
    }
    if let Some(charge) = charge_id {
        let active: Option<bool> = sqlx::query_scalar("SELECT active FROM charge WHERE id = $1")
            .bind(charge)
            .fetch_optional(&mut *ctx.conn)
            .await?;
        if active != Some(true) {
            return Err(refuse(
                "charge_id",
                "the charge does not exist or is inactive".into(),
            ));
        }
    }
    Ok(())
}

/// What a line of an order's `collection` is, as [`check_line`] holds it:
/// stock (an item and its warehouse), a charge, or an account. For the
/// `alternatives` of a document type whose lines have
/// [`line_kind_fields`].
pub fn line_kinds(collection: &str) -> Alternatives {
    Alternatives::new(
        collection,
        &[
            ("Stock item", &["item_id", "warehouse_id"]),
            ("Charge", &["charge_id"]),
            ("Account", &["account_id"]),
        ],
    )
}

/// What a line of an invoice's `collection` is: a line of its order (stock
/// comes through the order), a charge, or an account.
pub fn invoice_line_kinds(collection: &str) -> Alternatives {
    Alternatives::new(
        collection,
        &[
            ("Order line", &["order_line_id"]),
            ("Charge", &["charge_id"]),
            ("Account", &["account_id"]),
        ],
    )
}

/// The fields of a line that is not stock: a charge or an account, and
/// what it is. `account_help` says what the account is for.
pub fn line_kind_fields(account_help: &str) -> Vec<FieldMeta> {
    vec![
        FieldMeta::new("charge_id", "Charge", FieldType::link("charge"))
            .help("For a line that is not stock: freight, a service."),
        FieldMeta::new("account_id", "Account", FieldType::link("account")).help(account_help),
        FieldMeta::new("description", "Description", FieldType::Text),
    ]
}

/// The discount fields of a line, in the document's currency.
pub fn discount_fields() -> Vec<FieldMeta> {
    vec![
        FieldMeta::new("discount_percent", "Discount %", FieldType::Decimal).percent(6),
        FieldMeta::new("discount_amount", "Discount", FieldType::Decimal)
            .money()
            .currency_of_document("currency")
            .help("Off the whole line, after the percent discount."),
    ]
}

/// The net amount of a line of a body as it was read: quantity times
/// price, less its discounts, rounded as posting rounds it.
pub fn body_line_net(line: &Value, decimals: u32) -> Decimal {
    use document::body::{amount, decimal};
    line_net(
        amount(&line["qty"]),
        amount(&line["unit_price"]),
        decimal(&line["discount_percent"]),
        decimal(&line["discount_amount"]),
        decimals,
    )
    .unwrap_or_default()
}

/// Zero discounts are no discounts.
pub fn no_zero(value: Option<Decimal>) -> Option<Decimal> {
    value.filter(|v| !v.is_zero())
}

/// What a body's lines give off in discounts, and come to in charges, in
/// the document's currency: each line's discount is its quantity times its
/// price, rounded, less its net amount; a charge line counts with its net
/// amount. `is_charge` says whether a line is a charge (on the line itself,
/// or on the order line it follows).
pub fn discount_and_charges(
    lines: &[Value],
    decimals: u32,
    is_charge: impl Fn(&Value) -> bool,
) -> (Decimal, Decimal) {
    use document::body::{amount, decimal};
    let (mut discount, mut charges) = (Decimal::ZERO, Decimal::ZERO);
    for line in lines {
        let net = decimal(&line["net_amount"]).unwrap_or_else(|| body_line_net(line, decimals));
        let gross = erpcraft_kernel::round_money(
            amount(&line["qty"]) * amount(&line["unit_price"]),
            decimals,
        );
        discount += gross - net;
        if is_charge(line) {
            charges += net;
        }
    }
    (discount, charges)
}

/// The summary fields for [`discount_and_charges`], in the document's
/// currency.
pub fn discount_and_charge_fields() -> Vec<FieldMeta> {
    vec![
        FieldMeta::new("total_discount", "Discounts", FieldType::Decimal)
            .money()
            .currency_in("currency")
            .readonly()
            .help("What the lines' discounts take off, in the document's currency."),
        FieldMeta::new("total_charges", "Charges", FieldType::Decimal)
            .money()
            .currency_in("currency")
            .readonly()
            .help("Of the net total, what charge lines (freight, handling) come to."),
    ]
}
