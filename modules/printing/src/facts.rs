//! What a printed invoice states besides its lines, as a German invoice
//! must (§ 14 Abs. 4 UStG): the date of the supply, the net amount and
//! tax by rate, the reason for an exemption, and what the payment terms
//! agree about paying early. Read from the document as the caller may read
//! it (its postings and their entries), so a template learns nothing the
//! caller could not.

use std::collections::BTreeMap;

use erpcraft_kernel::document::DocumentView;
use erpcraft_kernel::{Ctx, Decimal, Result};
use serde_json::{Value, json};
use uuid::Uuid;

/// Whether a table of another module is there to read.
async fn has_table(ctx: &mut Ctx<'_>, table: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&mut *ctx.conn)
            .await?,
    )
}

fn decimal(value: &Value) -> Decimal {
    value
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or_default()
}

/// `facts` of the template context:
///
/// - `service_dates`: for an invoice of an order, the dates of the order's
///   posted deliveries up to the invoice's date (the Leistungsdatum);
///   empty when there were none, when the date of the supply is the
///   invoice's;
/// - `tax`: per code and rate, the net amount taxed and the tax, with the
///   code's VAT category and exemption reason, and whether the buyer
///   accounts for it (reverse charge);
/// - `payment`: the item the document opened: `due_date`, and with an
///   early-payment discount `discount_date` and `discount_amount`.
pub(crate) async fn of(ctx: &mut Ctx<'_>, view: &DocumentView) -> Result<Value> {
    let doc = &view.header;
    let mut service_dates: Vec<String> = vec![];
    if doc.doc_type == "sales_invoice"
        && let Some(order) = view.body["sales_order_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
    {
        let dates: Vec<chrono::NaiveDate> = sqlx::query_scalar(
            "SELECT DISTINCT d.doc_date FROM document_link l JOIN document d ON d.id = l.to_id
             WHERE l.from_id = $1 AND d.doc_type = 'sales_delivery' AND d.status = 'posted'
               AND d.doc_date <= $2
             ORDER BY 1",
        )
        .bind(order)
        .bind(doc.doc_date)
        .fetch_all(&mut *ctx.conn)
        .await?;
        service_dates = dates.iter().map(|d| d.to_string()).collect();
    }

    // The entries of the postings that stand, as the caller may read them.
    let standing: Vec<&erpcraft_kernel::document::PostingView> = view
        .postings
        .iter()
        .filter(|p| p.posting.reverses_id.is_none())
        .collect();
    let mut by_rate: BTreeMap<(String, String), (Value, Decimal, Decimal)> = BTreeMap::new();
    let mut items: Vec<Uuid> = vec![];
    for posting in &standing {
        let entries = serde_json::to_value(&posting.entries).unwrap_or(Value::Null);
        for entry in entries["tax"].as_array().into_iter().flatten() {
            let key = (
                entry["tax_code_id"].as_str().unwrap_or_default().to_owned(),
                entry["rate"].as_str().unwrap_or_default().to_owned(),
            );
            let row = by_rate
                .entry(key)
                .or_insert_with(|| (entry.clone(), Decimal::ZERO, Decimal::ZERO));
            row.1 += decimal(&entry["base"]);
            row.2 += decimal(&entry["amount"]);
        }
        for entry in entries["open_items"].as_array().into_iter().flatten() {
            if let Some(id) = entry["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) {
                items.push(id);
            }
        }
    }
    let codes: Vec<Uuid> = by_rate
        .keys()
        .filter_map(|(code, _)| Uuid::parse_str(code).ok())
        .collect();
    let about: Vec<(Uuid, Option<String>, Option<String>, bool)> =
        if !codes.is_empty() && has_table(ctx, "tax_code").await? {
            sqlx::query_as(
                "SELECT id, vat_category, exemption_reason, self_assessed FROM tax_code
                 WHERE id = ANY ($1)",
            )
            .bind(&codes)
            .fetch_all(&mut *ctx.conn)
            .await?
        } else {
            vec![]
        };
    let tax: Vec<Value> = by_rate
        .into_iter()
        .map(|((code_id, rate), (entry, base, amount))| {
            let code = about.iter().find(|a| a.0.to_string() == code_id);
            json!({
                "tax_code_id": code_id,
                "code": entry["code"],
                "name": entry["name"],
                "rate": rate,
                "base": base.to_string(),
                "amount": amount.to_string(),
                "vat_category": code.and_then(|c| c.1.clone()),
                "exemption_reason": code.and_then(|c| c.2.clone()),
                "reverse_charge": code.is_some_and(|c| c.3),
            })
        })
        .collect();

    let mut payment = Value::Null;
    if let Some(item) = items.first()
        && has_table(ctx, "open_item").await?
    {
        let found: Option<(
            Option<chrono::NaiveDate>,
            Option<chrono::NaiveDate>,
            Option<Decimal>,
        )> = sqlx::query_as(
            "SELECT due_date, discount_date, discount_amount FROM open_item WHERE id = $1",
        )
        .bind(item)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some((due, discount_date, discount_amount)) = found {
            payment = json!({
                "due_date": due,
                "discount_date": discount_date,
                "discount_amount": discount_amount.map(|d| d.to_string()),
            });
        }
    }
    Ok(json!({ "service_dates": service_dates, "tax": tax, "payment": payment }))
}
