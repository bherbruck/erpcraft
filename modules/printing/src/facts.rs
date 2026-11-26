//! What a printed invoice states besides its lines, as a German invoice
//! must (§ 14 Abs. 4 UStG) and a French one (CGI art. 242 nonies A): the
//! date of the supply, the net amount and tax by rate, the reason for an
//! exemption, and what the payment terms agree about paying early.
//!
//! These are facts of the document, which the law asks every copy of it
//! to state: they are read from the database as they stand, not through
//! the caller's rights, so a clerk who may print an invoice prints the
//! same legal content as the accountant does. Nothing here is more than
//! the invoice itself tells its customer.

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

/// The tax of a document by code and rate, from its postings that stand.
type TaxRow = (
    Uuid,
    Decimal,
    Decimal,
    Decimal,
    Decimal,
    Decimal,
    String,
    String,
    Option<String>,
    Option<String>,
    bool,
);

/// `facts` of the template context:
///
/// - `service_dates`: for an invoice of an order, the dates of the posted
///   deliveries whose quantities this invoice bills (the Leistungsdatum):
///   per order line, the deliveries in their order, of which the earlier
///   invoices billed the first and this one the next; empty when there
///   were none, when the date of the supply is the invoice's;
/// - `tax`: per code and rate, the net amount taxed and the tax in the
///   document's currency (`base`, `amount`), and in the accounting
///   currency (`accounting_base`, `accounting_amount`, which a German
///   invoice in another currency states, § 16 Abs. 6 UStG), with the
///   code's VAT category and exemption reason, and whether the buyer
///   accounts for it (reverse charge);
/// - `payment`: the item the document opened: `due_date`, and with an
///   early-payment discount `discount_date` and `discount_amount`, in the
///   document's currency.
pub(crate) async fn of(ctx: &mut Ctx<'_>, view: &DocumentView) -> Result<Value> {
    let doc = &view.header;
    let mut service_dates: Vec<String> = vec![];
    if doc.doc_type == "sales_invoice" && has_table(ctx, "sales_invoice_line").await? {
        let dates: Vec<chrono::NaiveDate> = sqlx::query_scalar(
            "WITH me AS (SELECT id, doc_date, created_at FROM document WHERE id = $1),
             billed AS (SELECT order_line_id, qty FROM sales_invoice_line WHERE document_id = $1),
             before AS (
                SELECT il.order_line_id, sum(il.qty) AS qty
                FROM sales_invoice_line il JOIN document d ON d.id = il.document_id, me
                WHERE d.status = 'posted' AND d.id <> me.id
                  AND (d.doc_date, d.created_at, d.id) < (me.doc_date, me.created_at, me.id)
                  AND il.order_line_id IN (SELECT order_line_id FROM billed)
                GROUP BY il.order_line_id),
             delivered AS (
                SELECT dl.order_line_id, d.doc_date, dl.qty,
                       sum(dl.qty) OVER (PARTITION BY dl.order_line_id
                                         ORDER BY d.doc_date, d.created_at, d.id) - dl.qty AS start
                FROM sales_delivery_line dl JOIN document d ON d.id = dl.document_id
                WHERE d.status = 'posted'
                  AND dl.order_line_id IN (SELECT order_line_id FROM billed))
             SELECT DISTINCT v.doc_date
             FROM delivered v JOIN billed b USING (order_line_id)
             LEFT JOIN before p USING (order_line_id)
             WHERE v.start < coalesce(p.qty, 0) + b.qty AND v.start + v.qty > coalesce(p.qty, 0)
             ORDER BY 1",
        )
        .bind(doc.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        service_dates = dates.iter().map(|d| d.to_string()).collect();
    }

    // The postings that stand: neither a reversal nor reversed.
    let standing: Vec<Uuid> = sqlx::query_scalar(
        "SELECT p.id FROM posting p
         WHERE p.document_id = $1 AND p.reverses_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)",
    )
    .bind(doc.id)
    .fetch_all(&mut *ctx.conn)
    .await?;

    let mut tax: Vec<Value> = vec![];
    if !standing.is_empty() && has_table(ctx, "tax_entry").await? {
        // Tax the buyer accounts for is recorded twice on a purchase, paid
        // and collected: the document states it once.
        let rows: Vec<TaxRow> = sqlx::query_as(
            "SELECT e.tax_code_id, e.rate,
                    sum(coalesce(e.txn_base, e.base)), sum(coalesce(e.txn_amount, e.amount)),
                    sum(e.base), sum(e.amount),
                    c.code, c.name, c.vat_category, c.exemption_reason, c.self_assessed
             FROM tax_entry e JOIN tax_code c ON c.id = e.tax_code_id
             WHERE e.posting_id = ANY ($1) AND NOT (e.self_assessed AND e.direction = 'collected')
             GROUP BY e.tax_code_id, e.rate, c.code, c.name, c.vat_category,
                      c.exemption_reason, c.self_assessed
             ORDER BY c.code, e.rate",
        )
        .bind(&standing)
        .fetch_all(&mut *ctx.conn)
        .await?;
        tax = rows
            .into_iter()
            .map(
                |(id, rate, base, amount, value_base, value, code, name, category, reason, rc)| {
                    json!({
                        "tax_code_id": id,
                        "code": code,
                        "name": name,
                        "rate": rate.normalize().to_string(),
                        "base": base.to_string(),
                        "amount": amount.to_string(),
                        "accounting_base": value_base.to_string(),
                        "accounting_amount": value.to_string(),
                        "vat_category": category,
                        "exemption_reason": reason,
                        "reverse_charge": rc,
                    })
                },
            )
            .collect();
    }

    let mut payment = Value::Null;
    if !standing.is_empty() && has_table(ctx, "open_item").await? {
        let found: Option<(
            Option<chrono::NaiveDate>,
            Option<chrono::NaiveDate>,
            Option<Decimal>,
        )> = sqlx::query_as(
            "SELECT due_date, discount_date, discount_amount FROM open_item
             WHERE posting_id = ANY ($1) ORDER BY due_date NULLS LAST, id LIMIT 1",
        )
        .bind(&standing)
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
