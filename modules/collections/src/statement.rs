//! Customer statements: a customer's opening balance, what happened in a
//! period, its closing balance, the items still open and how old they
//! are, worked out from the open items ledger.

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, Error, FieldMeta, FieldType,
    JsonSchema, PgConnection, Result, StateMeta, async_trait, company, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) const STATEMENT: &str = "customer_statement";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    customer_id: Uuid,
    currency: Option<String>,
    from_date: NaiveDate,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Activity {
    id: Uuid,
    line_no: i32,
    entry_date: NaiveDate,
    kind: String,
    source_doc_type: Option<String>,
    number: Option<String>,
    debit: Decimal,
    credit: Decimal,
    balance: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Item {
    id: Uuid,
    line_no: i32,
    open_item_id: Uuid,
    source_doc_type: Option<String>,
    number: Option<String>,
    doc_date: NaiveDate,
    due_date: Option<NaiveDate>,
    days_overdue: i32,
    amount: Decimal,
    open: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Header {
    customer_id: Uuid,
    currency: String,
    from_date: NaiveDate,
    opening: Decimal,
    closing: Decimal,
    current_due: Decimal,
    overdue_1_30: Decimal,
    overdue_31_60: Decimal,
    overdue_61_90: Decimal,
    overdue_over_90: Decimal,
}

pub(crate) struct Statement;

fn money(name: &str, label: &str) -> FieldMeta {
    FieldMeta::new(name, label, FieldType::Decimal)
        .money()
        .currency_in("currency")
}

#[async_trait]
impl DocumentHandler for Statement {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: STATEMENT.into(),
            module: String::new(),
            label: "Customer statement".into(),
            plural: "Customer statements".into(),
            number_prefix: "CS".into(),
            fields: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).required(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .help("The items in this currency; empty: the accounting currency."),
                FieldMeta::new("from_date", "From", FieldType::Date)
                    .required()
                    .help("The period runs from this day to the statement's date."),
                money("opening", "Opening balance").readonly(),
                money("closing", "Closing balance").readonly(),
                money("current_due", "Not yet due").readonly(),
                money("overdue_1_30", "1-30 days overdue").readonly(),
                money("overdue_31_60", "31-60 days overdue").readonly(),
                money("overdue_61_90", "61-90 days overdue").readonly(),
                money("overdue_over_90", "Over 90 days overdue").readonly(),
            ],
            collections: vec![
                CollectionMeta {
                    name: "activity".into(),
                    label: "Activity".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new("entry_date", "Date", FieldType::Date).readonly(),
                        FieldMeta::new(
                            "kind",
                            "What",
                            FieldType::labelled(&[
                                ("item", "Invoice or credit"),
                                ("settlement", "Payment or credit applied"),
                                ("cancelled", "Reversed"),
                            ]),
                        )
                        .readonly(),
                        FieldMeta::new("source_doc_type", "Type", FieldType::Text).readonly(),
                        FieldMeta::new("number", "Number", FieldType::Text).readonly(),
                        money("debit", "Charged").readonly(),
                        money("credit", "Paid or credited").readonly(),
                        money("balance", "Balance").readonly(),
                    ],
                },
                CollectionMeta {
                    name: "open_items".into(),
                    label: "Open items".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new(
                            "open_item_id",
                            "Open item",
                            FieldType::Lookup(
                                erpcraft_openitems::lookup().field("company_id", "company_id"),
                            ),
                        )
                        .readonly(),
                        FieldMeta::new("source_doc_type", "Type", FieldType::Text).readonly(),
                        FieldMeta::new("number", "Number", FieldType::Text).readonly(),
                        FieldMeta::new("doc_date", "Date", FieldType::Date).readonly(),
                        FieldMeta::new("due_date", "Due", FieldType::Date).readonly(),
                        FieldMeta::new("days_overdue", "Days overdue", FieldType::Int).readonly(),
                        money("amount", "Amount").readonly(),
                        money("open", "Open").readonly(),
                    ],
                },
            ],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("issued", "Issued"),
            ],
            initial_state: "draft".into(),
            transitions: vec![("draft".into(), "issued".into())],
            summary: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).readonly(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly(),
                money("closing", "Closing balance").readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "customer statement body")?;
        ctx.require("open_item:read", doc.company_id)?;
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customer WHERE id = $1)")
                .bind(body.customer_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        if !known {
            return Err(Error::validation("the customer does not exist").with_field("customer_id"));
        }
        if body.from_date > doc.doc_date {
            return Err(
                Error::validation("the period starts on or before the statement's date")
                    .with_field("from_date"),
            );
        }
        let currency = match body.currency {
            Some(c) => c,
            None => company::base_currency(&mut *ctx.conn, doc.company_id).await?,
        };
        work_out(
            &mut *ctx.conn,
            doc,
            body.customer_id,
            &currency,
            body.from_date,
        )
        .await
    }

    async fn summary_of(
        &self,
        _conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        Ok(json!({
            "customer_id": body["customer_id"],
            "currency": body["currency"],
            "closing": body["closing"],
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let header: Option<Header> = sqlx::query_as(
            "SELECT customer_id, currency, from_date, opening, closing, current_due, overdue_1_30,
                    overdue_31_60, overdue_61_90, overdue_over_90
             FROM customer_statement WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *conn)
        .await?;
        let activity: Vec<Activity> = sqlx::query_as(
            "SELECT id, line_no, entry_date, kind, source_doc_type, number,
                    debit, credit, balance
             FROM customer_statement_activity WHERE document_id = $1 ORDER BY line_no",
        )
        .bind(doc.id)
        .fetch_all(&mut *conn)
        .await?;
        let items: Vec<Item> = sqlx::query_as(
            "SELECT id, line_no, open_item_id, source_doc_type, number,
                    doc_date, due_date, days_overdue, amount, open
             FROM customer_statement_item WHERE document_id = $1 ORDER BY line_no",
        )
        .bind(doc.id)
        .fetch_all(&mut *conn)
        .await?;
        let mut body = serde_json::to_value(&header).map_err(Error::internal)?;
        if body.is_null() {
            body = json!({});
        }
        body["activity"] = serde_json::to_value(&activity).map_err(Error::internal)?;
        body["open_items"] = serde_json::to_value(&items).map_err(Error::internal)?;
        Ok(body)
    }
}

/// One change of the balance in the period.
#[derive(sqlx::FromRow)]
struct Movement {
    entry_date: NaiveDate,
    kind: String,
    source_doc_type: Option<String>,
    source_document_id: Option<Uuid>,
    number: Option<String>,
    /// What it adds to what the customer owes.
    change: Decimal,
}

/// An item open on the statement's date: the item, its document's type,
/// id and number, its date and due date, its amount and what is open.
type OpenRow = (
    Uuid,
    Option<String>,
    Option<Uuid>,
    Option<String>,
    NaiveDate,
    Option<NaiveDate>,
    Decimal,
    Decimal,
);

/// Work the statement out from the ledger and store it.
async fn work_out(
    conn: &mut PgConnection,
    doc: &Document,
    customer: Uuid,
    currency: &str,
    from: NaiveDate,
) -> Result<()> {
    let to = doc.doc_date;
    let base = company::base_currency(&mut *conn, doc.company_id).await?;
    // The customer's receivables in the currency, as a table expression:
    // $1 company, $2 customer, $3 currency, $4 accounting currency.
    let items = "SELECT i.* FROM open_item i
                 WHERE i.company_id = $1 AND i.kind = 'receivable' AND i.party_type = 'customer'
                   AND i.party_id = $2 AND COALESCE(i.currency, $4) = $3";
    // What is open of the items dated before a day, settled before it.
    let open_before = |day: &str| {
        format!(
            "SELECT COALESCE(sum(COALESCE(i.txn_amount, i.amount)
                     - COALESCE((SELECT sum(COALESCE(s.txn_amount, s.amount))
                                 FROM settlement s JOIN posting p ON p.id = s.posting_id
                                 WHERE s.open_item_id = i.id AND p.posting_date < {day}), 0)), 0)
             FROM ({items}) i WHERE i.doc_date < {day}"
        )
    };
    let opening: Decimal = sqlx::query_scalar(&open_before("$5"))
        .bind(doc.company_id)
        .bind(customer)
        .bind(currency)
        .bind(&base)
        .bind(from)
        .fetch_one(&mut *conn)
        .await?;
    let movements: Vec<Movement> = sqlx::query_as(&format!(
        "SELECT * FROM (
             SELECT i.doc_date AS entry_date, 'item' AS kind, d.doc_type AS source_doc_type,
                    d.id AS source_document_id, d.number, COALESCE(i.txn_amount, i.amount) AS change,
                    0 AS sort
             FROM ({items}) i JOIN posting p ON p.id = i.posting_id
             LEFT JOIN document d ON d.id = p.document_id
             WHERE i.doc_date BETWEEN $5 AND $6
             UNION ALL
             SELECT p.posting_date, CASE WHEN bool_or(s.cancels) THEN 'cancelled' ELSE 'settlement' END,
                    d.doc_type, d.id, d.number, -sum(COALESCE(s.txn_amount, s.amount)), 1
             FROM ({items}) i JOIN settlement s ON s.open_item_id = i.id
             JOIN posting p ON p.id = s.posting_id
             LEFT JOIN document d ON d.id = p.document_id
             WHERE p.posting_date BETWEEN $5 AND $6
             GROUP BY p.id, p.posting_date, d.doc_type, d.id, d.number
         ) m WHERE change <> 0
         ORDER BY entry_date, sort, number NULLS LAST"
    ))
    .bind(doc.company_id)
    .bind(customer)
    .bind(currency)
    .bind(&base)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await?;
    let open_items: Vec<OpenRow> = sqlx::query_as(&format!(
        "SELECT * FROM (
             SELECT i.id, d.doc_type, d.id AS doc_id, d.number, i.doc_date, i.due_date,
                    COALESCE(i.txn_amount, i.amount) AS amount,
                    COALESCE(i.txn_amount, i.amount)
                    - COALESCE((SELECT sum(COALESCE(s.txn_amount, s.amount))
                                FROM settlement s JOIN posting sp ON sp.id = s.posting_id
                                WHERE s.open_item_id = i.id AND sp.posting_date <= $5), 0) AS open
             FROM ({items}) i JOIN posting p ON p.id = i.posting_id
             LEFT JOIN document d ON d.id = p.document_id
             WHERE i.doc_date <= $5
         ) o WHERE open <> 0
         ORDER BY COALESCE(due_date, doc_date), doc_date, number NULLS LAST"
    ))
    .bind(doc.company_id)
    .bind(customer)
    .bind(currency)
    .bind(&base)
    .bind(to)
    .fetch_all(&mut *conn)
    .await?;

    let mut buckets = [Decimal::ZERO; 5];
    let mut closing_open = Decimal::ZERO;
    for item in &open_items {
        let days = (to - item.5.unwrap_or(item.4)).num_days();
        let bucket = match days {
            d if d <= 0 => 0,
            1..=30 => 1,
            31..=60 => 2,
            61..=90 => 3,
            _ => 4,
        };
        buckets[bucket] += item.7;
        closing_open += item.7;
    }
    let closing = opening + movements.iter().map(|m| m.change).sum::<Decimal>();
    if closing != closing_open {
        return Err(Error::internal(format!(
            "the statement does not add up: {opening} and the period's activity come to \
             {closing}, the open items to {closing_open}"
        )));
    }

    sqlx::query(
        "INSERT INTO customer_statement (document_id, customer_id, currency, from_date, opening,
             closing, current_due, overdue_1_30, overdue_31_60, overdue_61_90, overdue_over_90)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT (document_id) DO UPDATE SET customer_id = EXCLUDED.customer_id,
             currency = EXCLUDED.currency, from_date = EXCLUDED.from_date,
             opening = EXCLUDED.opening, closing = EXCLUDED.closing,
             current_due = EXCLUDED.current_due, overdue_1_30 = EXCLUDED.overdue_1_30,
             overdue_31_60 = EXCLUDED.overdue_31_60, overdue_61_90 = EXCLUDED.overdue_61_90,
             overdue_over_90 = EXCLUDED.overdue_over_90",
    )
    .bind(doc.id)
    .bind(customer)
    .bind(currency)
    .bind(from)
    .bind(opening)
    .bind(closing)
    .bind(buckets[0])
    .bind(buckets[1])
    .bind(buckets[2])
    .bind(buckets[3])
    .bind(buckets[4])
    .execute(&mut *conn)
    .await?;
    for table in ["customer_statement_activity", "customer_statement_item"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE document_id = $1"))
            .bind(doc.id)
            .execute(&mut *conn)
            .await?;
    }
    let mut balance = opening;
    for (n, m) in movements.iter().enumerate() {
        balance += m.change;
        let (debit, credit) = if m.change.is_sign_negative() {
            (Decimal::ZERO, -m.change)
        } else {
            (m.change, Decimal::ZERO)
        };
        sqlx::query(
            "INSERT INTO customer_statement_activity (id, document_id, line_no, entry_date, kind,
                 source_doc_type, source_document_id, number, debit, credit, balance)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(erpcraft_kernel::new_id())
        .bind(doc.id)
        .bind(n as i32 + 1)
        .bind(m.entry_date)
        .bind(&m.kind)
        .bind(&m.source_doc_type)
        .bind(m.source_document_id)
        .bind(&m.number)
        .bind(debit)
        .bind(credit)
        .bind(balance)
        .execute(&mut *conn)
        .await?;
    }
    for (n, item) in open_items.iter().enumerate() {
        let days = (to - item.5.unwrap_or(item.4)).num_days().max(0);
        sqlx::query(
            "INSERT INTO customer_statement_item (id, document_id, line_no, open_item_id,
                 source_doc_type, source_document_id, number, doc_date, due_date, days_overdue,
                 amount, open)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(erpcraft_kernel::new_id())
        .bind(doc.id)
        .bind(n as i32 + 1)
        .bind(item.0)
        .bind(&item.1)
        .bind(item.2)
        .bind(&item.3)
        .bind(item.4)
        .bind(item.5)
        .bind(days as i32)
        .bind(item.6)
        .bind(item.7)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdInput {
    /// Customer statement id.
    id: Uuid,
}

/// Works a statement out once more and locks it.
pub(crate) struct Issue;

#[async_trait]
impl Command for Issue {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "customer_statement.issue",
            "Issue a draft customer statement: work it out once more from the ledgers as of its \
             date and lock it, so that it says what it said when it was sent. Send it with \
             document.send.",
        )
        .permission("customer_statement:post")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let doc = document::lock(ctx, STATEMENT, input.id, "post").await?;
        issue(ctx, doc).await
    }
}

async fn issue(ctx: &mut Ctx<'_>, mut doc: Document) -> Result<DocumentView> {
    if doc.status == "issued" {
        ctx.no_change();
        return document::view(ctx, doc).await;
    }
    let found: Option<(Uuid, String, NaiveDate)> = sqlx::query_as(
        "SELECT customer_id, currency, from_date FROM customer_statement WHERE document_id = $1",
    )
    .bind(doc.id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let (customer, currency, from) =
        found.ok_or_else(|| Error::validation("the statement names no customer yet"))?;
    ctx.require("open_item:read", doc.company_id)?;
    work_out(&mut *ctx.conn, &doc, customer, &currency, from).await?;
    document::transition(ctx, &mut doc, "issued").await?;
    document::view(ctx, doc).await
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IssueAllInput {
    /// The company whose customers get statements.
    company_id: Uuid,
    /// The first day of the period.
    from_date: NaiveDate,
    /// The statements' date: the last day of the period.
    date: NaiveDate,
    /// The currency of the items; the accounting currency when omitted.
    currency: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct IssueAllOutput {
    /// The statements issued, one per customer.
    statements: Vec<Uuid>,
}

/// A statement for every customer with something to say.
pub(crate) struct IssueAll;

#[async_trait]
impl Command for IssueAll {
    type Input = IssueAllInput;
    type Output = IssueAllOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "customer_statement.issue_all",
            "Draft and issue a customer statement for every customer of the company that has \
             something open on the date, or activity in the period, in the currency. Returns the \
             statements; send them with document.send.",
        )
        .permission("customer_statement:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IssueAllInput) -> Result<IssueAllOutput> {
        ctx.require("customer_statement:create", input.company_id)?;
        ctx.require("customer_statement:post", input.company_id)?;
        ctx.require("open_item:read", input.company_id)?;
        let base = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let currency = input.currency.unwrap_or_else(|| base.clone());
        let customers: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT i.party_id FROM open_item i
             WHERE i.company_id = $1 AND i.kind = 'receivable' AND i.party_type = 'customer'
               AND COALESCE(i.currency, $2) = $3 AND i.doc_date <= $5
               AND (i.doc_date >= $4
                    OR COALESCE(i.txn_amount, i.amount) <> COALESCE(i.txn_settled, i.settled)
                    OR EXISTS (SELECT 1 FROM settlement s JOIN posting p ON p.id = s.posting_id
                               WHERE s.open_item_id = i.id AND p.posting_date >= $4))
             ORDER BY i.party_id",
        )
        .bind(input.company_id)
        .bind(&base)
        .bind(&currency)
        .bind(input.from_date)
        .bind(input.date)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut statements = vec![];
        for customer in customers {
            let draft = document::create(
                ctx,
                document::NewDocument {
                    doc_type: STATEMENT.into(),
                    company_id: input.company_id,
                    doc_date: input.date,
                    memo: None,
                    body: json!({ "customer_id": customer, "currency": currency,
                                  "from_date": input.from_date }),
                },
            )
            .await?;
            // A customer that turns out to have nothing in the period and
            // nothing open is not sent a statement.
            let empty = draft.body["activity"].as_array().is_none_or(Vec::is_empty)
                && draft.body["open_items"]
                    .as_array()
                    .is_none_or(Vec::is_empty);
            if empty {
                document::delete(ctx, STATEMENT, draft.header.id).await?;
                continue;
            }
            let doc = document::lock(ctx, STATEMENT, draft.header.id, "post").await?;
            issue(ctx, doc).await?;
            statements.push(draft.header.id);
        }
        Ok(IssueAllOutput { statements })
    }
}
