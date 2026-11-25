//! Lots and serial numbers: what a document line names, how it becomes the
//! lots of a stock entry, which lots to propose for goods going out, and
//! where a lot went or came from.
//!
//! The ledger's rules (stock migration 10) hold whatever a module does: an
//! entry of a tracked item carries lots adding up to its quantity, a lot
//! level never goes below zero, a serial number is one unit on hand once.
//! What is here gives modules good messages before the ledger refuses, and
//! keeps the lots a document names in one table for every document type.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Decision, Error, JsonSchema, PgConnection, PolicyPoint,
    Result, async_trait, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// The policy point asked for each lot going out.
pub const LOT_ISSUE: &str = "stock.lot_issue";

pub(crate) fn policy() -> PolicyPoint {
    PolicyPoint::new(
        LOT_ISSUE,
        "Asked for each lot or serial number going out of a warehouse (a delivery, \
         production, a transfer, an adjustment, a return to a vendor): may this lot go? \
         Unless configured otherwise it may. A company that ships nothing expired refuses \
         when `facts.expired`; one that keeps to first expiring, first out refuses when \
         `!facts.first_expiring`, or asks for approval.",
    )
    .text("context", "What takes the goods out, as for stock.negative")
    .text("item_code", "Code of the item")
    .text("warehouse_code", "Code of the warehouse")
    .text("lot", "The lot or serial number")
    .boolean(
        "expired",
        "Whether the lot's expiry date is before the movement's date",
    )
    .number(
        "days_to_expiry",
        "Days from the movement's date to the lot's expiry date, below zero once expired; \
         9999 for a lot without one",
    )
    .boolean(
        "first_expiring",
        "Whether no other lot of the item on hand in the warehouse, not expired by the \
         movement's date, expires sooner",
    )
    .refusal("this lot may not go out")
}

/// How an item is tracked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
    None,
    Lot,
    Serial,
}

impl Tracking {
    fn parse(text: &str) -> Self {
        match text {
            "lot" => Tracking::Lot,
            "serial" => Tracking::Serial,
            _ => Tracking::None,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Tracking::Serial => "serial number",
            _ => "lot",
        }
    }
}

/// How `item_id` is tracked.
pub async fn tracking(conn: &mut PgConnection, item_id: Uuid) -> Result<Tracking> {
    let text: Option<String> = sqlx::query_scalar("SELECT tracking FROM item WHERE id = $1")
        .bind(item_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(Tracking::parse(text.as_deref().unwrap_or("none")))
}

/// A lot a document line names: which, how much, and for goods coming in
/// under a new lot, its dates and attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct LineLot {
    /// The lot or serial number.
    pub lot: String,
    /// How much of the line is of this lot, above zero; one for a serial
    /// number.
    pub qty: Decimal,
    /// For a new lot coming in: when it expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_date: Option<NaiveDate>,
    /// For a new lot coming in: when it was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufactured_date: Option<NaiveDate>,
    /// For a new lot coming in: free attributes, such as the supplier's
    /// own lot number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,
}

/// The `lots` field of a document line that moves stock.
pub fn field() -> erpcraft_kernel::FieldMeta {
    erpcraft_kernel::FieldMeta::new("lots", "Lots", erpcraft_kernel::FieldType::Json).help(
        "For an item tracked by lot or serial number: the lots of this line, as a list of \
         { lot, qty }, adding up to its quantity. Goods coming in under a new lot may give its \
         expiry_date, manufactured_date and attributes.",
    )
}

/// Checks the lots of one line as a draft is saved: quantities above
/// zero, codes given once. Whether they fit the item is checked when the
/// document posts ([`resolve`]).
pub fn check(collection: &str, line_no: usize, lots: &[LineLot]) -> Result<()> {
    let fail = |msg: String| Error::line(collection, line_no, Some("lots"), msg);
    let mut seen = std::collections::HashSet::new();
    for lot in lots {
        if lot.lot.trim().is_empty() || lot.lot.trim() != lot.lot || lot.lot.chars().count() > 64 {
            return Err(fail(
                "a lot number is given without spaces around it, at most 64 characters".into(),
            ));
        }
        if lot.qty <= Decimal::ZERO {
            return Err(fail(format!(
                "lot {}: the quantity must be above zero",
                lot.lot
            )));
        }
        if !seen.insert(lot.lot.as_str()) {
            return Err(fail(format!("lot {} is named twice", lot.lot)));
        }
        if let Some(attributes) = &lot.attributes
            && !attributes.is_object()
        {
            return Err(fail(format!(
                "lot {}: attributes are an object of names and values",
                lot.lot
            )));
        }
        if let (Some(expiry), Some(made)) = (lot.expiry_date, lot.manufactured_date)
            && expiry < made
        {
            return Err(fail(format!(
                "lot {}: it cannot expire before it was made",
                lot.lot
            )));
        }
    }
    Ok(())
}

/// Replace the lots every line of a document names: `lines` is each line's
/// id and its lots. Lines not given name none.
pub async fn save(
    conn: &mut PgConnection,
    document_id: Uuid,
    lines: &[(Uuid, &[LineLot])],
) -> Result<()> {
    sqlx::query("DELETE FROM stock_line_lot WHERE document_id = $1")
        .bind(document_id)
        .execute(&mut *conn)
        .await?;
    for (line_id, lots) in lines {
        for (seq, lot) in lots.iter().enumerate() {
            sqlx::query(
                "INSERT INTO stock_line_lot (id, document_id, line_id, seq, lot, qty, expiry_date,
                     manufactured_date, attributes)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(new_id())
            .bind(document_id)
            .bind(line_id)
            .bind(seq as i32 + 1)
            .bind(&lot.lot)
            .bind(lot.qty)
            .bind(lot.expiry_date)
            .bind(lot.manufactured_date)
            .bind(&lot.attributes)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

/// The lots each line of a document names, by line id.
pub async fn of_document(
    conn: &mut PgConnection,
    document_id: Uuid,
) -> Result<HashMap<Uuid, Vec<LineLot>>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        line_id: Uuid,
        #[sqlx(flatten)]
        lot: LineLot,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT line_id, lot, qty, expiry_date, manufactured_date, attributes
         FROM stock_line_lot WHERE document_id = $1 ORDER BY line_id, seq",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: HashMap<Uuid, Vec<LineLot>> = HashMap::new();
    for row in rows {
        out.entry(row.line_id).or_default().push(row.lot);
    }
    Ok(out)
}

/// Put each line's lots on a loaded body: every object of `body[collection]`
/// with an `id` gets `lots`, empty when it names none.
pub async fn attach(
    conn: &mut PgConnection,
    document_id: Uuid,
    body: &mut Value,
    collection: &str,
) -> Result<()> {
    let lots = of_document(conn, document_id).await?;
    if let Some(lines) = body[collection].as_array_mut() {
        for line in lines {
            let id = line["id"].as_str().and_then(|s| s.parse::<Uuid>().ok());
            let named = id.and_then(|id| lots.get(&id)).cloned().unwrap_or_default();
            line["lots"] = json!(named);
        }
    }
    Ok(())
}

/// The lots that live stock entries of these document lines moved, by
/// lot code, signed: what came in less what went out. Entries taken back
/// are left out. A follow-on document drafts its lots from it, such as a
/// return from the receipts and returns of an order line.
pub async fn net_by_source(
    conn: &mut PgConnection,
    source_line_ids: &[Uuid],
) -> Result<BTreeMap<String, Decimal>> {
    let rows: Vec<(String, Decimal)> = sqlx::query_as(
        "SELECT l.code, sum(el.qty)
         FROM stock_entry e
         JOIN stock_entry_lot el ON el.entry_id = e.id
         JOIN stock_lot l ON l.id = el.lot_id
         WHERE e.source_line_id = ANY ($1) AND e.reverses_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM stock_entry r WHERE r.reverses_id = e.id)
         GROUP BY l.code ORDER BY l.code",
    )
    .bind(source_line_ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Up to `qty` taken from the lots of `available` that are above zero, in
/// the order of their codes, as a document line names them.
pub fn take(available: &BTreeMap<String, Decimal>, mut qty: Decimal) -> Vec<LineLot> {
    let mut out = vec![];
    for (code, left) in available {
        if qty <= Decimal::ZERO {
            break;
        }
        if *left <= Decimal::ZERO {
            continue;
        }
        let part = (*left).min(qty);
        qty -= part;
        out.push(LineLot {
            lot: code.clone(),
            qty: part.normalize(),
            expiry_date: None,
            manufactured_date: None,
            attributes: None,
        });
    }
    out
}

/// A lot an entry moves: the lot and how much of it, above zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LotQty {
    pub lot_id: Uuid,
    pub qty: Decimal,
}

/// Where a line's lots are, for messages.
#[derive(Debug, Clone, Copy)]
pub struct At<'a> {
    pub collection: &'a str,
    pub line_no: usize,
}

impl At<'_> {
    fn fail(&self, msg: String) -> Error {
        Error::line(self.collection, self.line_no, Some("lots"), msg)
    }
}

/// The lots a movement of `qty` (positive in, negative out) of `item_id`
/// moves, from what its document line names: checked against the item's
/// tracking, the quantity and, going out, the lots that exist. Lots coming
/// in are created when new, with the dates and attributes given; an
/// existing lot keeps its own, and a date that contradicts it is refused.
pub async fn resolve(
    conn: &mut PgConnection,
    company_id: Uuid,
    item_id: Uuid,
    qty: Decimal,
    lots: &[LineLot],
    at: At<'_>,
) -> Result<Vec<LotQty>> {
    let (code, tracking): (String, String) =
        sqlx::query_as("SELECT code, tracking FROM item WHERE id = $1")
            .bind(item_id)
            .fetch_one(&mut *conn)
            .await?;
    let tracking = Tracking::parse(&tracking);
    if tracking == Tracking::None {
        if !lots.is_empty() {
            return Err(at.fail(format!(
                "item {code} is not tracked by lot: the line names no lots"
            )));
        }
        return Ok(vec![]);
    }
    let noun = tracking.noun();
    if lots.is_empty() {
        return Err(at.fail(format!(
            "item {code} is tracked by {noun}: name the {noun}s of this line"
        )));
    }
    let total: Decimal = lots.iter().map(|l| l.qty).sum();
    if total != qty.abs() {
        return Err(at.fail(format!(
            "the {noun}s of this line add up to {}, not {}",
            total.normalize(),
            qty.abs().normalize()
        )));
    }
    let mut out = Vec::with_capacity(lots.len());
    for lot in lots {
        if tracking == Tracking::Serial && lot.qty != Decimal::ONE {
            return Err(at.fail(format!("serial number {} is one unit", lot.lot)));
        }
        let found: Option<(Uuid, Option<NaiveDate>, Option<NaiveDate>)> = sqlx::query_as(
            "SELECT id, expiry_date, manufactured_date FROM stock_lot
             WHERE company_id = $1 AND item_id = $2 AND code = $3",
        )
        .bind(company_id)
        .bind(item_id)
        .bind(&lot.lot)
        .fetch_optional(&mut *conn)
        .await?;
        let lot_id = match found {
            Some((id, expiry, made)) => {
                for (given, kept, what) in [
                    (lot.expiry_date, expiry, "expiry date"),
                    (lot.manufactured_date, made, "manufacturing date"),
                ] {
                    if let (Some(given), Some(kept)) = (given, kept)
                        && given != kept
                    {
                        return Err(at.fail(format!(
                            "{noun} {} of item {code} has the {what} {kept}, not {given}",
                            lot.lot
                        )));
                    }
                }
                if qty.is_sign_positive()
                    && ((lot.expiry_date.is_some() && expiry.is_none())
                        || (lot.manufactured_date.is_some() && made.is_none()))
                {
                    sqlx::query(
                        "UPDATE stock_lot SET expiry_date = COALESCE(expiry_date, $2),
                             manufactured_date = COALESCE(manufactured_date, $3)
                         WHERE id = $1",
                    )
                    .bind(id)
                    .bind(lot.expiry_date)
                    .bind(lot.manufactured_date)
                    .execute(&mut *conn)
                    .await?;
                }
                id
            }
            None if qty.is_sign_positive() => {
                let id = new_id();
                // Two documents bringing in the same new lot at once: the
                // second waits on the key and takes the first one's row.
                sqlx::query(
                    "INSERT INTO stock_lot (id, company_id, item_id, code, expiry_date,
                         manufactured_date, attributes)
                     VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, '{}'::jsonb))
                     ON CONFLICT (company_id, item_id, code) DO NOTHING",
                )
                .bind(id)
                .bind(company_id)
                .bind(item_id)
                .bind(&lot.lot)
                .bind(lot.expiry_date)
                .bind(lot.manufactured_date)
                .bind(&lot.attributes)
                .execute(&mut *conn)
                .await
                .map_err(|e| match Error::from(e) {
                    Error::Validation(v) => at.fail(format!("{noun} {}: {}", lot.lot, v.text)),
                    other => other,
                })?;
                sqlx::query_scalar(
                    "SELECT id FROM stock_lot WHERE company_id = $1 AND item_id = $2 AND code = $3",
                )
                .bind(company_id)
                .bind(item_id)
                .bind(&lot.lot)
                .fetch_one(&mut *conn)
                .await?
            }
            None => {
                return Err(at.fail(format!(
                    "item {code} has no {noun} {} in this company",
                    lot.lot
                )));
            }
        };
        out.push(LotQty {
            lot_id,
            qty: lot.qty,
        });
    }
    Ok(out)
}

/// Ask the policy [`LOT_ISSUE`] for each lot of an issue. Called by
/// [`crate::post`] for every line going out.
pub(crate) async fn ask_issue(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    posting_date: NaiveDate,
    warehouse_id: Uuid,
    lots: &[LotQty],
    context: &str,
) -> Result<()> {
    if lots.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = lots.iter().map(|l| l.lot_id).collect();
    let rows: Vec<(String, String, String, Option<NaiveDate>, bool)> = sqlx::query_as(
        "SELECT l.code, i.code, w.code, l.expiry_date,
                NOT EXISTS (
                    SELECT 1 FROM stock_lot_level o JOIN stock_lot ol ON ol.id = o.lot_id
                    WHERE o.item_id = l.item_id AND o.warehouse_id = $2 AND o.qty > 0
                      AND o.lot_id <> ALL ($1)
                      AND ol.expiry_date IS NOT NULL AND ol.expiry_date >= $3
                      AND (l.expiry_date IS NULL OR ol.expiry_date < l.expiry_date))
         FROM stock_lot l JOIN item i ON i.id = l.item_id, warehouse w
         WHERE l.id = ANY ($1) AND w.id = $2
         ORDER BY l.code",
    )
    .bind(&ids)
    .bind(warehouse_id)
    .bind(posting_date)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let policy = ctx.policy(LOT_ISSUE, company_id).await?;
    for (lot, item_code, warehouse_code, expiry, first) in rows {
        let days = expiry.map_or(9999, |e| (e - posting_date).num_days());
        let facts = json!({
            "context": context,
            "item_code": item_code,
            "warehouse_code": warehouse_code,
            "lot": lot,
            "expired": expiry.is_some_and(|e| e < posting_date),
            "days_to_expiry": days,
            "first_expiring": first,
        });
        if let Decision::Refused(refusal) = policy.evaluate(facts)? {
            return Err(Error::validation(refusal.message.unwrap_or_else(|| {
                format!(
                    "lot {lot} of item {item_code} may not go out of warehouse {warehouse_code}"
                )
            })));
        }
    }
    Ok(())
}

// ------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LotsInput {
    /// The company whose lots are listed.
    company_id: Uuid,
    /// Only this item.
    item_id: Option<Uuid>,
    /// Only this warehouse.
    warehouse_id: Option<Uuid>,
    /// Only lots whose code contains this.
    lot: Option<String>,
    /// Only lots that expire on or before this day.
    expiring_by: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct LotRow {
    lot_id: Uuid,
    lot: String,
    item_id: Uuid,
    item_code: String,
    warehouse_id: Uuid,
    warehouse_code: String,
    qty: Decimal,
    expiry_date: Option<NaiveDate>,
    manufactured_date: Option<NaiveDate>,
    attributes: Value,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LotsOutput {
    company_id: Uuid,
    rows: Vec<LotRow>,
}

pub(crate) struct Lots;

#[async_trait]
impl Command for Lots {
    type Input = LotsInput;
    type Output = LotsOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "stock.lots",
            "Lots and serial numbers on hand in a company's warehouses: quantity per lot and \
             warehouse, with the lot's expiry and manufacturing dates and attributes, first \
             expiring first. Lots with nothing on hand are left out.",
        )
        .report("Lots on hand")
        .permission("stock_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: LotsInput) -> Result<LotsOutput> {
        ctx.require("stock_entry:read", input.company_id)?;
        let rows = sqlx::query_as(
            "SELECT l.id AS lot_id, l.code AS lot, l.item_id, i.code AS item_code,
                    v.warehouse_id, w.code AS warehouse_code, v.qty, l.expiry_date,
                    l.manufactured_date, l.attributes
             FROM stock_lot_level v
             JOIN stock_lot l ON l.id = v.lot_id
             JOIN item i ON i.id = l.item_id
             JOIN warehouse w ON w.id = v.warehouse_id
             WHERE v.company_id = $1 AND v.qty > 0
               AND ($2::uuid IS NULL OR v.item_id = $2)
               AND ($3::uuid IS NULL OR v.warehouse_id = $3)
               AND ($4::text IS NULL OR l.code ILIKE '%' || $4 || '%')
               AND ($5::date IS NULL OR l.expiry_date <= $5)
             ORDER BY i.code, l.expiry_date NULLS LAST, l.code, w.code",
        )
        .bind(input.company_id)
        .bind(input.item_id)
        .bind(input.warehouse_id)
        .bind(input.lot.as_deref().map(escape_like))
        .bind(input.expiring_by)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(LotsOutput {
            company_id: input.company_id,
            rows,
        })
    }
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PickOrder {
    /// First expiring, first out; lots without an expiry date last.
    #[default]
    Fefo,
    /// First received, first out.
    Fifo,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProposalInput {
    /// The company the goods go out of.
    company_id: Uuid,
    /// The item going out.
    item_id: Uuid,
    /// The warehouse it goes out of.
    warehouse_id: Uuid,
    /// How much is to go out.
    qty: Decimal,
    /// `fefo` (the default) or `fifo`.
    #[serde(default)]
    order: PickOrder,
    /// Leave out lots that have expired by this day (the movement's date).
    not_expired_on: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ProposalOutput {
    /// The lots to name on the line, as a document line takes them.
    lots: Vec<LineLot>,
    /// What the lots on hand do not cover; zero when they do.
    short: Decimal,
}

pub(crate) struct Proposal;

#[async_trait]
impl Command for Proposal {
    type Input = ProposalInput;
    type Output = ProposalOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "stock.lot_proposal",
            "Propose the lots or serial numbers for a quantity of an item going out of a \
             warehouse: first expiring first (fefo, the default; lots without an expiry date \
             last) or first received first (fifo). A proposal only: the document line names \
             the lots, and may name others. `short` is what the lots on hand do not cover.",
        )
        .permission("stock_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ProposalInput) -> Result<ProposalOutput> {
        ctx.require("stock_entry:read", input.company_id)?;
        if input.qty <= Decimal::ZERO {
            return Err(Error::validation("the quantity must be above zero").with_field("qty"));
        }
        let order = match input.order {
            PickOrder::Fefo => "l.expiry_date NULLS LAST, first_in, l.code",
            PickOrder::Fifo => "first_in, l.code",
        };
        let rows: Vec<(String, Decimal)> = sqlx::query_as(&format!(
            "SELECT l.code, v.qty,
                    (SELECT min(e.seq) FROM stock_entry_lot el
                     JOIN stock_entry e ON e.id = el.entry_id
                     WHERE el.lot_id = l.id AND e.warehouse_id = v.warehouse_id AND el.qty > 0)
                        AS first_in
             FROM stock_lot_level v JOIN stock_lot l ON l.id = v.lot_id
             WHERE v.company_id = $1 AND v.item_id = $2 AND v.warehouse_id = $3 AND v.qty > 0
               AND ($4::date IS NULL OR l.expiry_date IS NULL OR l.expiry_date >= $4)
             ORDER BY {order}"
        ))
        .bind(input.company_id)
        .bind(input.item_id)
        .bind(input.warehouse_id)
        .bind(input.not_expired_on)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut left = input.qty;
        let mut lots = vec![];
        for (lot, qty) in rows {
            if left <= Decimal::ZERO {
                break;
            }
            let take = qty.min(left);
            left -= take;
            lots.push(LineLot {
                lot,
                qty: take.normalize(),
                expiry_date: None,
                manufactured_date: None,
                attributes: None,
            });
        }
        Ok(ProposalOutput {
            lots,
            short: left.max(Decimal::ZERO).normalize(),
        })
    }
}

// ------------------------------------------------------------- tracing

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Direction {
    /// Where the lot went: customers, and the lots made from it.
    #[default]
    Forward,
    /// Where it came from: vendors, and the lots it was made from.
    Backward,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TraceInput {
    /// The company whose lot is traced.
    company_id: Uuid,
    /// The item of the lot.
    item_id: Uuid,
    /// The lot or serial number to start from.
    lot: String,
    /// `forward` (the default) or `backward`.
    #[serde(default)]
    direction: Direction,
}

#[derive(Serialize, JsonSchema, Clone)]
pub(crate) struct TracedLot {
    lot_id: Uuid,
    item_id: Uuid,
    item_code: String,
    lot: String,
    expiry_date: Option<NaiveDate>,
    /// Steps from the lot traced from: 0 for itself, 1 for the lots made
    /// from it (forward) or that went into it (backward), and so on.
    depth: i32,
    /// The lot this one was reached from, through a production order.
    via_lot_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow, Clone)]
pub(crate) struct TraceMovement {
    lot_id: Uuid,
    lot: String,
    item_code: String,
    /// What kind of document moved it: purchase_receipt, sales_delivery,
    /// production_order, stock_transfer, ...
    doc_type: String,
    document_id: Uuid,
    number: Option<String>,
    posting_date: NaiveDate,
    warehouse_code: String,
    /// In (positive) or out (negative) of the warehouse.
    qty: Decimal,
    /// The customer or vendor of the document, when it has one.
    party_type: Option<String>,
    party_id: Option<Uuid>,
    party_name: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TraceOutput {
    lots: Vec<TracedLot>,
    /// Every movement of those lots that was not taken back, oldest first.
    movements: Vec<TraceMovement>,
    /// Forward: the customers who received any of the lots. Backward: the
    /// vendors who supplied any of them.
    parties: Vec<TraceParty>,
}

#[derive(Serialize, JsonSchema, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TraceParty {
    party_type: String,
    party_id: Uuid,
    name: Option<String>,
}

pub(crate) struct Trace;

/// The document types under which lots are made from other lots:
/// production. Every lot consumed under such a document went into every
/// lot it made.
const MADE_UNDER: &[&str] = &["production_order"];

#[async_trait]
impl Command for Trace {
    type Input = TraceInput;
    type Output = TraceOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "stock.trace",
            "Trace a lot or serial number. Forward: every movement of it (receipts, transfers, \
             deliveries with their customers, returns), and through production the lots of the \
             products it went into, followed in turn, with every customer who received any of \
             them. Backward: where it came from (receipts with their vendors) and, if it was \
             made, the lots of the components its production order consumed, followed in \
             turn, with every vendor who supplied any of them. Production links lots by \
             order: every lot an order consumed went into every lot it made. Movements that \
             were taken back are left out.",
        )
        .report("Lot traceability")
        .permission("stock_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: TraceInput) -> Result<TraceOutput> {
        ctx.require("stock_entry:read", input.company_id)?;
        let start: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM stock_lot WHERE company_id = $1 AND item_id = $2 AND code = $3",
        )
        .bind(input.company_id)
        .bind(input.item_id)
        .bind(&input.lot)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let start = start.ok_or_else(|| {
            Error::not_found(format!("the item has no lot {} in this company", input.lot))
        })?;
        // Breadth first over the production orders that link lots.
        let mut depth_of: BTreeMap<Uuid, (i32, Option<Uuid>)> = BTreeMap::new();
        depth_of.insert(start, (0, None));
        let mut frontier = vec![start];
        let mut depth = 0;
        while !frontier.is_empty() && depth < 50 {
            depth += 1;
            // Forward: documents of production that consumed a lot of the
            // frontier, and the lots they made. Backward: the other way.
            let (from_sign, to_sign) = match input.direction {
                Direction::Forward => (-1, 1),
                Direction::Backward => (1, -1),
            };
            let next: Vec<(Uuid, Uuid)> = sqlx::query_as(
                "SELECT DISTINCT ON (t.lot_id) f.lot_id, t.lot_id
                 FROM stock_entry_lot f
                 JOIN stock_entry fe ON fe.id = f.entry_id
                 JOIN posting fp ON fp.id = fe.posting_id
                 JOIN posting tp ON tp.document_id = fp.document_id
                 JOIN stock_entry te ON te.posting_id = tp.id
                 JOIN stock_entry_lot t ON t.entry_id = te.id
                 JOIN document d ON d.id = fp.document_id
                 WHERE f.lot_id = ANY ($1) AND fe.company_id = $2
                   AND sign(f.qty) = $3 AND sign(t.qty) = $4
                   AND fe.reverses_id IS NULL AND te.reverses_id IS NULL
                   AND NOT EXISTS (SELECT 1 FROM stock_entry r WHERE r.reverses_id = fe.id)
                   AND NOT EXISTS (SELECT 1 FROM stock_entry r WHERE r.reverses_id = te.id)
                   AND te.item_id <> fe.item_id
                   AND d.doc_type = ANY ($5)
                 ORDER BY t.lot_id, f.lot_id",
            )
            .bind(&frontier)
            .bind(input.company_id)
            .bind(from_sign)
            .bind(to_sign)
            .bind(MADE_UNDER)
            .fetch_all(&mut *ctx.conn)
            .await?;
            frontier = vec![];
            for (via, lot) in next {
                if let std::collections::btree_map::Entry::Vacant(slot) = depth_of.entry(lot) {
                    slot.insert((depth, Some(via)));
                    frontier.push(lot);
                }
            }
        }
        let ids: Vec<Uuid> = depth_of.keys().copied().collect();
        let described: Vec<(Uuid, Uuid, String, String, Option<NaiveDate>)> = sqlx::query_as(
            "SELECT l.id, l.item_id, i.code, l.code, l.expiry_date
             FROM stock_lot l JOIN item i ON i.id = l.item_id WHERE l.id = ANY ($1)",
        )
        .bind(&ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut lots: Vec<TracedLot> = described
            .into_iter()
            .map(|(lot_id, item_id, item_code, lot, expiry_date)| {
                let (depth, via_lot_id) = depth_of[&lot_id];
                TracedLot {
                    lot_id,
                    item_id,
                    item_code,
                    lot,
                    expiry_date,
                    depth,
                    via_lot_id,
                }
            })
            .collect();
        lots.sort_by(|a, b| (a.depth, &a.item_code, &a.lot).cmp(&(b.depth, &b.item_code, &b.lot)));
        let movements: Vec<TraceMovement> = sqlx::query_as(
            "SELECT el.lot_id, l.code AS lot, i.code AS item_code, d.doc_type,
                    d.id AS document_id, d.number, e.posting_date, w.code AS warehouse_code,
                    el.qty,
                    CASE WHEN d.summary ? 'customer_id' THEN 'customer'
                         WHEN d.summary ? 'vendor_id' THEN 'vendor' END AS party_type,
                    COALESCE(d.summary ->> 'customer_id', d.summary ->> 'vendor_id')::uuid
                        AS party_id,
                    COALESCE(c.name, v.name) AS party_name
             FROM stock_entry_lot el
             JOIN stock_entry e ON e.id = el.entry_id
             JOIN stock_lot l ON l.id = el.lot_id
             JOIN item i ON i.id = l.item_id
             JOIN warehouse w ON w.id = e.warehouse_id
             JOIN posting p ON p.id = e.posting_id
             JOIN document d ON d.id = p.document_id
             LEFT JOIN customer c ON c.id = (d.summary ->> 'customer_id')::uuid
             LEFT JOIN vendor v ON v.id = (d.summary ->> 'vendor_id')::uuid
             WHERE el.lot_id = ANY ($1) AND e.company_id = $2 AND e.reverses_id IS NULL
               AND NOT EXISTS (SELECT 1 FROM stock_entry r WHERE r.reverses_id = e.id)
             ORDER BY e.seq, el.lot_id",
        )
        .bind(&ids)
        .bind(input.company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        // Forward: who received goods (deliveries out). Backward: who
        // supplied them (receipts in).
        let mut parties: Vec<TraceParty> = movements
            .iter()
            .filter(|m| match input.direction {
                Direction::Forward => {
                    m.party_type.as_deref() == Some("customer") && m.qty.is_sign_negative()
                }
                Direction::Backward => {
                    m.party_type.as_deref() == Some("vendor") && m.qty.is_sign_positive()
                }
            })
            .filter_map(|m| {
                Some(TraceParty {
                    party_type: m.party_type.clone()?,
                    party_id: m.party_id?,
                    name: m.party_name.clone(),
                })
            })
            .collect();
        parties.sort();
        parties.dedup();
        Ok(TraceOutput {
            lots,
            movements,
            parties,
        })
    }
}
