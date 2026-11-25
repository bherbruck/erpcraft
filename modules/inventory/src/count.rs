//! Physical inventory and cycle counts.
//!
//! A count (`stock_count`) covers a warehouse with the locations inside it,
//! narrowed for a cycle count to an ABC class and to items. Starting it
//! locks the levels it covers, takes the stock ledger's order number (the
//! cut-off) and writes a line per item, warehouse and lot on hand with the
//! book quantity at that moment; the levels are held, so the stock ledger
//! asks the company's policy (`stock.count_movement`) before goods move on
//! them. The count is taken at the cut-off: the difference is what was
//! counted less the book at the cut-off, and it is adjusted on top of what
//! moved since. Lines too far off are recounted (the policy
//! `stock_count.tolerance`). Posting turns the differences into stock
//! adjustments against the account for `inventory_count_variance`.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, Decision, DocTypeMeta, EntityMeta, Error,
    FieldMeta, FieldType, JsonSchema, PgConnection, PolicyPoint, Result, StateMeta, async_trait,
    company, new_id, schemars,
};
use erpcraft_stock::counts;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) const STOCK_COUNT: &str = "stock_count";
/// The policy point asked for a counted line that differs from the book.
pub(crate) const TOLERANCE: &str = "stock_count.tolerance";
/// The account purpose of differences found by counting.
const VARIANCE_ACCOUNT: &str = "inventory_count_variance";

pub(crate) fn tolerance_point() -> PolicyPoint {
    PolicyPoint::new(
        TOLERANCE,
        "Asked for every counted line of a physical count that differs from the book: may \
         the difference stand, or must the line be counted again? Unless configured otherwise \
         it stands. A company that recounts large differences refuses, for example when \
         `facts.difference_percent > 5 || facts.value_difference > 500`; the line is then \
         marked for a recount, and the count does not post until it has one.",
    )
    .number("book_qty", "What the book held at the count's cut-off")
    .as_quantity()
    .number("counted_qty", "What was counted")
    .as_quantity()
    .number(
        "difference",
        "Counted less book: below zero when goods are missing",
    )
    .as_quantity()
    .number(
        "difference_percent",
        "The difference as a percentage of the book, unsigned; 100 when the book held nothing",
    )
    .as_percent()
    .number(
        "value_difference",
        "What the difference is worth at the level's average cost, unsigned",
    )
    .as_company_money()
    .text("item_code", "Code of the item")
    .text("abc_class", "The item's ABC class, or empty")
    .refusal("this line must be counted again")
}

pub(crate) fn cycle_rule_entity() -> EntityMeta {
    EntityMeta::new("cycle_count_rule", "Cycle count rule")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new("class", "ABC class", FieldType::options(&["A", "B", "C"])).required(),
        )
        .field(
            FieldMeta::new("every_days", "Every (days)", FieldType::Int)
                .required()
                .help("How often items of the class are counted."),
        )
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["class"])
        .company_field("company_id")
}

// ------------------------------------------------------------ the document

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    warehouse_id: Uuid,
    abc_class: Option<String>,
    #[serde(default)]
    item_ids: Vec<Uuid>,
    #[serde(default)]
    blind: bool,
    #[serde(default)]
    lines: Vec<LineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    id: Option<Uuid>,
    item_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    lot: Option<String>,
    counted_qty: Option<Decimal>,
    recount_qty: Option<Decimal>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Header {
    warehouse_id: Uuid,
    abc_class: Option<String>,
    item_ids: Vec<Uuid>,
    blind: bool,
    cut_off_seq: Option<i64>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct Line {
    id: Uuid,
    line_no: i32,
    item_id: Uuid,
    warehouse_id: Uuid,
    lot: Option<String>,
    book_qty: Decimal,
    counted_qty: Option<Decimal>,
    recount: bool,
    recount_qty: Option<Decimal>,
}

impl Line {
    /// What stands: the recount, else the count.
    fn counted(&self) -> Option<Decimal> {
        self.recount_qty.or(self.counted_qty)
    }
}

async fn header(conn: &mut PgConnection, id: Uuid) -> Result<Option<Header>> {
    Ok(sqlx::query_as(
        "SELECT warehouse_id, abc_class, item_ids, blind, cut_off_seq FROM stock_count
         WHERE document_id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?)
}

async fn lines(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, item_id, warehouse_id, lot, book_qty, counted_qty, recount,
                recount_qty
         FROM stock_count_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The warehouse and every location inside it.
async fn subtree(conn: &mut PgConnection, warehouse_id: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "WITH RECURSIVE down AS (
             SELECT id FROM warehouse WHERE id = $1
             UNION SELECT c.id FROM warehouse c JOIN down ON c.parent_id = down.id)
         SELECT id FROM down",
    )
    .bind(warehouse_id)
    .fetch_all(&mut *conn)
    .await?)
}

async fn lot_id(
    conn: &mut PgConnection,
    company_id: Uuid,
    item_id: Uuid,
    lot: Option<&str>,
) -> Result<Option<Uuid>> {
    let Some(lot) = lot else { return Ok(None) };
    Ok(sqlx::query_scalar(
        "SELECT id FROM stock_lot WHERE company_id = $1 AND item_id = $2 AND code = $3",
    )
    .bind(company_id)
    .bind(item_id)
    .bind(lot)
    .fetch_optional(&mut *conn)
    .await?)
}

pub(crate) struct StockCount;

#[async_trait]
impl DocumentHandler for StockCount {
    fn meta(&self) -> DocTypeMeta {
        let qty = |name: &str, label: &str| {
            FieldMeta::new(name, label, FieldType::Decimal)
                .quantity()
                .unit_of("item_id", "uom")
        };
        DocTypeMeta {
            name: STOCK_COUNT.into(),
            module: String::new(),
            label: "Stock count".into(),
            plural: "Stock counts".into(),
            number_prefix: "PC".into(),
            fields: vec![
                FieldMeta::new("warehouse_id", "Warehouse", FieldType::link("warehouse"))
                    .required()
                    .help("Counted with the locations inside it."),
                FieldMeta::new(
                    "abc_class",
                    "ABC class",
                    FieldType::options(&["A", "B", "C"]),
                )
                .help("For a cycle count: only items of this class."),
                FieldMeta::new("item_ids", "Items", FieldType::Json)
                    .help("For a cycle count: only these items (a list of item ids)."),
                FieldMeta::new("blind", "Blind", FieldType::Bool)
                    .help("Hide the book quantity from the count sheet until it is posted."),
                FieldMeta::new("cut_off_seq", "Cut-off", FieldType::Int)
                    .readonly()
                    .help("The stock ledger's order number when the count started."),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Count".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("item_id", "Item", FieldType::link("item")),
                    FieldMeta::new("warehouse_id", "Location", FieldType::link("warehouse")),
                    FieldMeta::new("lot", "Lot or serial", FieldType::Text),
                    qty("book_qty", "Book")
                        .readonly()
                        .help("At the cut-off. Hidden while a blind count is in progress."),
                    qty("counted_qty", "Counted"),
                    FieldMeta::new("recount", "Recount", FieldType::Bool)
                        .readonly()
                        .help("Too far off: count it again."),
                    qty("recount_qty", "Recounted"),
                    qty("difference", "Difference").readonly(),
                    qty("moved_since", "Moved since").readonly().help(
                        "What the book moved after the cut-off; the count does not adjust it.",
                    ),
                ],
            }],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::editable("counting", "Counting"),
                StateMeta::locked("posted", "Posted"),
                StateMeta::locked("cancelled", "Cancelled").unnumbered(),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "counting".into()),
                ("counting".into(), "posted".into()),
                ("draft".into(), "cancelled".into()),
                ("counting".into(), "cancelled".into()),
            ],
            summary: vec![
                FieldMeta::new("warehouse_id", "Warehouse", FieldType::link("warehouse"))
                    .readonly(),
                FieldMeta::new("lines", "Lines", FieldType::Int).readonly(),
                FieldMeta::new("counted", "Counted", FieldType::Int).readonly(),
                FieldMeta::new("differences", "Differences", FieldType::Int).readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "stock count body")?;
        if !company::owns_all(
            &mut *ctx.conn,
            "warehouse",
            &[body.warehouse_id],
            doc.company_id,
        )
        .await?
        {
            return Err(
                Error::validation("the warehouse must belong to the document's company")
                    .with_field("warehouse_id"),
            );
        }
        if let Some(class) = &body.abc_class
            && !["A", "B", "C"].contains(&class.as_str())
        {
            return Err(Error::validation("the class is A, B or C").with_field("abc_class"));
        }
        let current = header(&mut *ctx.conn, doc.id).await?;
        if doc.status == "counting" {
            // What is counted is fixed once the count started.
            let current = current.ok_or_else(|| Error::internal("a count without its header"))?;
            let mut ids = body.item_ids.clone();
            ids.sort();
            let mut was = current.item_ids.clone();
            was.sort();
            if current.warehouse_id != body.warehouse_id
                || current.abc_class != body.abc_class
                || was != ids
                || current.blind != body.blind
            {
                return Err(Error::validation(
                    "what a count covers is fixed once it started; cancel it and start another",
                ));
            }
            return save_counting(ctx, doc, &current, &body.lines).await;
        }
        if !body.lines.is_empty() {
            return Err(Error::validation(
                "a count's lines are written when it starts (stock_count.start)",
            )
            .with_field("lines"));
        }
        sqlx::query(
            "INSERT INTO stock_count (document_id, warehouse_id, abc_class, item_ids, blind)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (document_id) DO UPDATE SET warehouse_id = EXCLUDED.warehouse_id,
                 abc_class = EXCLUDED.abc_class, item_ids = EXCLUDED.item_ids,
                 blind = EXCLUDED.blind",
        )
        .bind(doc.id)
        .bind(body.warehouse_id)
        .bind(&body.abc_class)
        .bind(&body.item_ids)
        .bind(body.blind)
        .execute(&mut *ctx.conn)
        .await?;
        Ok(())
    }

    async fn summary_of(
        &self,
        _conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let lines = document::body::lines(body, "lines");
        let counted = lines
            .iter()
            .filter(|l| !l["counted_qty"].is_null() || !l["recount_qty"].is_null())
            .count();
        let differences = lines
            .iter()
            .filter(|l| document::body::decimal(&l["difference"]).is_some_and(|d| !d.is_zero()))
            .count();
        Ok(json!({
            "warehouse_id": body["warehouse_id"],
            "lines": lines.len(),
            "counted": counted,
            "differences": differences,
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let Some(head) = header(conn, doc.id).await? else {
            return Ok(json!({ "lines": [] }));
        };
        let hide = head.blind && doc.status == "counting";
        let mut rows = vec![];
        for line in lines(conn, doc.id).await? {
            let moved = match head.cut_off_seq {
                Some(seq) if !hide => {
                    let lot =
                        lot_id(conn, doc.company_id, line.item_id, line.lot.as_deref()).await?;
                    Some(
                        counts::moved_since(conn, line.item_id, line.warehouse_id, lot, seq)
                            .await?,
                    )
                }
                _ => None,
            };
            let mut row = serde_json::to_value(&line).unwrap_or_default();
            if hide {
                row["book_qty"] = Value::Null;
            } else {
                row["difference"] = json!(line.counted().map(|c| c - line.book_qty));
                row["moved_since"] = json!(moved);
            }
            rows.push(row);
        }
        Ok(json!({
            "warehouse_id": head.warehouse_id,
            "abc_class": head.abc_class,
            "item_ids": head.item_ids,
            "blind": head.blind,
            "cut_off_seq": head.cut_off_seq,
            "lines": rows,
        }))
    }
}

/// Saving a count in progress: what was counted on the lines it has, and
/// lines for goods found that the book did not show. Lines are not removed.
async fn save_counting(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    head: &Header,
    input: &[LineInput],
) -> Result<()> {
    let existing = lines(&mut *ctx.conn, doc.id).await?;
    for line in &existing {
        if !input.iter().any(|l| l.id == Some(line.id)) {
            return Err(Error::line(
                "lines",
                line.line_no as usize,
                None,
                "a line of a count in progress is not removed: leave it uncounted, or count \
                 nothing (0)",
            ));
        }
    }
    let places = subtree(&mut *ctx.conn, head.warehouse_id).await?;
    let seq = head
        .cut_off_seq
        .ok_or_else(|| Error::internal("a count in progress without its cut-off"))?;
    let mut next_no = existing.iter().map(|l| l.line_no).max().unwrap_or(0);
    for (index, given) in input.iter().enumerate() {
        let n = index + 1;
        for qty in [given.counted_qty, given.recount_qty].into_iter().flatten() {
            if qty.is_sign_negative() {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("counted_qty"),
                    "a count is zero or more",
                ));
            }
        }
        match given.id.and_then(|id| existing.iter().find(|l| l.id == id)) {
            Some(line) => {
                if given.recount_qty.is_some() && !line.recount {
                    return Err(Error::line(
                        "lines",
                        n,
                        Some("recount_qty"),
                        "only a line marked for a recount takes one",
                    ));
                }
                sqlx::query(
                    "UPDATE stock_count_line SET counted_qty = $2, recount_qty = $3 WHERE id = $1",
                )
                .bind(line.id)
                .bind(given.counted_qty)
                .bind(given.recount_qty)
                .execute(&mut *ctx.conn)
                .await?;
            }
            None => {
                // Goods found: a line of their own, its book at the cut-off.
                let (Some(item_id), Some(warehouse_id)) = (given.item_id, given.warehouse_id)
                else {
                    return Err(Error::line(
                        "lines",
                        n,
                        Some("item_id"),
                        "a line added to a count names its item and location",
                    ));
                };
                if !places.contains(&warehouse_id) {
                    return Err(Error::line(
                        "lines",
                        n,
                        Some("warehouse_id"),
                        "the location must be the count's warehouse or inside it",
                    ));
                }
                let tracking = erpcraft_stock::lots::tracking(&mut *ctx.conn, item_id).await?;
                let lot = given
                    .lot
                    .as_deref()
                    .map(str::trim)
                    .filter(|l| !l.is_empty());
                if (tracking == erpcraft_stock::Tracking::None) != lot.is_none() {
                    return Err(Error::line(
                        "lines",
                        n,
                        Some("lot"),
                        if lot.is_none() {
                            "the item is tracked by lot or serial number: name the lot counted"
                        } else {
                            "the item is not tracked by lot: name no lot"
                        },
                    ));
                }
                let lot_id = lot_id(&mut *ctx.conn, doc.company_id, item_id, lot).await?;
                let book = match (lot, lot_id) {
                    (Some(_), None) => Decimal::ZERO,
                    (_, lot_id) => {
                        counts::book_at(&mut *ctx.conn, item_id, warehouse_id, lot_id, seq).await?
                    }
                };
                next_no += 1;
                sqlx::query(
                    "INSERT INTO stock_count_line (id, document_id, line_no, item_id, warehouse_id,
                         lot, book_qty, counted_qty)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(new_id())
                .bind(doc.id)
                .bind(next_no)
                .bind(item_id)
                .bind(warehouse_id)
                .bind(lot)
                .bind(book)
                .bind(given.counted_qty)
                .execute(&mut *ctx.conn)
                .await
                .map_err(|e| match Error::from(e) {
                    Error::Conflict(_) => Error::line(
                        "lines",
                        n,
                        Some("item_id"),
                        "the count has a line for this item, location and lot already",
                    ),
                    other => other,
                })?;
                counts::hold(
                    &mut *ctx.conn,
                    doc.company_id,
                    doc.id,
                    &[(item_id, warehouse_id)],
                )
                .await?;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------ commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdInput {
    /// Stock count id.
    id: Uuid,
}

/// Starts a count: the book frozen at the cut-off, a line per level.
pub(crate) struct Start;

#[async_trait]
impl Command for Start {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_count.start",
            "Start a draft stock count: the levels it covers (its warehouse and the locations \
             inside it, narrowed to its ABC class and items) are locked, the stock ledger's \
             order number is taken as the cut-off, and a line is written per item, location \
             and lot or serial number on hand, with the book quantity then. Until the count is \
             posted or cancelled the levels are held: the stock ledger asks the policy \
             stock.count_movement before goods move on them (allowed by default; the count is \
             taken at the cut-off).",
        )
        .permission("stock_count:start")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, STOCK_COUNT, input.id, "start").await?;
        if doc.status == "counting" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} stock count cannot be started",
                doc.status
            )));
        }
        let head = header(&mut *ctx.conn, doc.id)
            .await?
            .ok_or_else(|| Error::validation("the count has no warehouse yet"))?;
        let places = subtree(&mut *ctx.conn, head.warehouse_id).await?;
        // The levels it covers, now: every item with stock or value there.
        let levels: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT l.item_id, l.warehouse_id FROM stock_level l
             LEFT JOIN item_abc a ON a.company_id = $2 AND a.item_id = l.item_id
             WHERE l.warehouse_id = ANY ($1) AND l.qty <> 0
               AND ($3::text IS NULL OR a.class = $3)
               AND (cardinality($4::uuid[]) = 0 OR l.item_id = ANY ($4))
             ORDER BY l.item_id, l.warehouse_id",
        )
        .bind(&places)
        .bind(doc.company_id)
        .bind(&head.abc_class)
        .bind(&head.item_ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
        erpcraft_stock::lock_levels(&mut *ctx.conn, doc.company_id, levels.clone()).await?;
        // Under the levels' locks every entry of theirs up to it is in.
        let cut_off = counts::cut_off(&mut *ctx.conn).await?;
        let (items, warehouses): (Vec<Uuid>, Vec<Uuid>) = levels.iter().copied().unzip();
        // Tracked items by lot on hand; the others by level.
        let rows: Vec<(Uuid, Uuid, Option<String>, Decimal)> = sqlx::query_as(
            "SELECT l.item_id, l.warehouse_id, NULL::text, l.qty
             FROM stock_level l JOIN item i ON i.id = l.item_id
             JOIN unnest($1::uuid[], $2::uuid[]) AS p (item_id, warehouse_id)
                 ON p.item_id = l.item_id AND p.warehouse_id = l.warehouse_id
             WHERE i.tracking = 'none'
             UNION ALL
             SELECT v.item_id, v.warehouse_id, s.code, v.qty
             FROM stock_lot_level v JOIN stock_lot s ON s.id = v.lot_id
             JOIN item i ON i.id = v.item_id
             JOIN unnest($1::uuid[], $2::uuid[]) AS p (item_id, warehouse_id)
                 ON p.item_id = v.item_id AND p.warehouse_id = v.warehouse_id
             WHERE i.tracking <> 'none' AND v.qty > 0",
        )
        .bind(&items)
        .bind(&warehouses)
        .fetch_all(&mut *ctx.conn)
        .await?;
        // In the order of the shelves: location, then item code, then lot.
        let mut sorted: Vec<(String, String, Uuid, Uuid, Option<String>, Decimal)> = vec![];
        for (item, warehouse, lot, qty) in rows {
            let (item_code, warehouse_code): (String, String) = sqlx::query_as(
                "SELECT (SELECT code FROM item WHERE id = $1),
                        (SELECT code FROM warehouse WHERE id = $2)",
            )
            .bind(item)
            .bind(warehouse)
            .fetch_one(&mut *ctx.conn)
            .await?;
            sorted.push((warehouse_code, item_code, item, warehouse, lot, qty));
        }
        sorted.sort_by(|a, b| (&a.0, &a.1, &a.4).cmp(&(&b.0, &b.1, &b.4)));
        sqlx::query(
            "UPDATE stock_count SET cut_off_seq = $2, started_at = now() WHERE document_id = $1",
        )
        .bind(doc.id)
        .bind(cut_off)
        .execute(&mut *ctx.conn)
        .await?;
        for (n, (_, _, item, warehouse, lot, qty)) in sorted.iter().enumerate() {
            sqlx::query(
                "INSERT INTO stock_count_line (id, document_id, line_no, item_id, warehouse_id,
                     lot, book_qty)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(n as i32 + 1)
            .bind(item)
            .bind(warehouse)
            .bind(lot)
            .bind(qty)
            .execute(&mut *ctx.conn)
            .await?;
        }
        counts::hold(&mut *ctx.conn, doc.company_id, doc.id, &levels).await?;
        document::transition(ctx, &mut doc, "counting").await?;
        document::view(ctx, doc).await
    }
}

/// A line's difference as the tolerance policy sees it.
async fn tolerance_facts(
    conn: &mut PgConnection,
    company_id: Uuid,
    line: &Line,
    counted: Decimal,
) -> Result<Value> {
    let (item_code, average, class): (String, Option<Decimal>, Option<String>) = sqlx::query_as(
        "SELECT i.code,
                (SELECT CASE WHEN l.qty > 0 THEN l.value / l.qty END FROM stock_level l
                 WHERE l.item_id = i.id AND l.warehouse_id = $2),
                (SELECT class FROM item_abc WHERE company_id = $3 AND item_id = i.id)
         FROM item i WHERE i.id = $1",
    )
    .bind(line.item_id)
    .bind(line.warehouse_id)
    .bind(company_id)
    .fetch_one(&mut *conn)
    .await?;
    let decimals = company::currency_decimals(&mut *conn, company_id).await?;
    let difference = counted - line.book_qty;
    let percent = if line.book_qty.is_zero() {
        Decimal::ONE_HUNDRED
    } else {
        (difference.abs() * Decimal::ONE_HUNDRED / line.book_qty.abs()).round_dp(2)
    };
    let value = (difference.abs() * average.unwrap_or_default()).round_dp(decimals);
    let shown = |d: Decimal| d.normalize().to_string();
    Ok(json!({
        "book_qty": shown(line.book_qty),
        "counted_qty": shown(counted),
        "difference": shown(difference),
        "difference_percent": shown(percent),
        "value_difference": shown(value),
        "item_code": item_code,
        "abc_class": class.unwrap_or_default(),
    }))
}

/// The lines whose difference the tolerance refuses and that have no
/// recount yet.
async fn over_tolerance(ctx: &mut Ctx<'_>, doc: &Document, lines: &[Line]) -> Result<Vec<i32>> {
    let policy = ctx.policy(TOLERANCE, doc.company_id).await?;
    let mut refused = vec![];
    for line in lines {
        let (Some(counted), None) = (line.counted_qty, line.recount_qty) else {
            continue;
        };
        if counted == line.book_qty {
            continue;
        }
        let facts = tolerance_facts(&mut *ctx.conn, doc.company_id, line, counted).await?;
        if let Decision::Refused(_) = policy.evaluate(facts)? {
            refused.push(line.line_no);
        }
    }
    Ok(refused)
}

/// Marks what must be counted again.
pub(crate) struct Review;

#[async_trait]
impl Command for Review {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_count.review",
            "Review a count in progress: every counted line that differs from the book is put \
             to the policy stock_count.tolerance, and a line it refuses is marked for a recount \
             (recount), to be counted again in recount_qty. The count does not post until those \
             lines have their recount.",
        )
        .permission("stock_count:update")
        .from_states(&["counting"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let doc = document::lock(ctx, STOCK_COUNT, input.id, "update").await?;
        if doc.status != "counting" {
            return Err(Error::state("only a count in progress is reviewed"));
        }
        let all = lines(&mut *ctx.conn, doc.id).await?;
        let refused = over_tolerance(ctx, &doc, &all).await?;
        sqlx::query(
            "UPDATE stock_count_line SET recount = (line_no = ANY ($2)) OR recount
             WHERE document_id = $1",
        )
        .bind(doc.id)
        .bind(&refused)
        .execute(&mut *ctx.conn)
        .await?;
        document::view(ctx, doc).await
    }
}

/// Posts the differences as stock adjustments.
pub(crate) struct Post;

#[async_trait]
impl Command for Post {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_count.post",
            "Post a count in progress: every line must be counted, and a line the policy \
             stock_count.tolerance refuses must have its recount. The holds are released and \
             the differences (counted, or recounted, less the book at the cut-off) are posted \
             as stock adjustments, one per offset account, the account rule \
             inventory_count_variance; goods found come in at the level's average cost (else \
             its last cost), goods missing go out at the average. The adjustments are linked to \
             the count. Takes the rights to create and post stock adjustments.",
        )
        .permission("stock_count:post")
        .from_states(&["counting"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, STOCK_COUNT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "counting" {
            return Err(Error::state(format!(
                "a {} stock count cannot be posted: start it first",
                doc.status
            )));
        }
        let all = lines(&mut *ctx.conn, doc.id).await?;
        if let Some(line) = all.iter().find(|l| l.counted().is_none()) {
            return Err(Error::line(
                "lines",
                line.line_no as usize,
                Some("counted_qty"),
                "this line is not counted yet",
            ));
        }
        if let Some(line) = all.iter().find(|l| l.recount && l.recount_qty.is_none()) {
            return Err(Error::line(
                "lines",
                line.line_no as usize,
                Some("recount_qty"),
                "this line must be counted again",
            ));
        }
        let refused = over_tolerance(ctx, &doc, &all).await?;
        if let Some(first) = refused.first() {
            return Err(Error::line(
                "lines",
                *first as usize,
                Some("counted_qty"),
                "this line is too far off the book and must be counted again: review the count \
                 (stock_count.review) and recount it",
            ));
        }
        counts::release(&mut *ctx.conn, doc.id).await?;

        // The differences, per offset account, item and location, and the
        // way they go: what is found comes in, what is missing goes out.
        type Key = (Uuid, Uuid, Uuid, bool);
        let mut moves: BTreeMap<Key, (Decimal, Vec<Value>)> = BTreeMap::new();
        let mut accounts: HashMap<(Uuid, Uuid), Uuid> = HashMap::new();
        for line in &all {
            let difference = line.counted().unwrap_or_default() - line.book_qty;
            if difference.is_zero() {
                continue;
            }
            let pair = (line.item_id, line.warehouse_id);
            let account = match accounts.get(&pair) {
                Some(account) => *account,
                None => {
                    let account = erpcraft_gl::account_for(
                        &mut *ctx.conn,
                        doc.company_id,
                        VARIANCE_ACCOUNT,
                        &[("item", line.item_id), ("warehouse", line.warehouse_id)],
                    )
                    .await?;
                    accounts.insert(pair, account);
                    account
                }
            };
            let entry = moves
                .entry((
                    account,
                    line.item_id,
                    line.warehouse_id,
                    difference > Decimal::ZERO,
                ))
                .or_default();
            entry.0 += difference;
            if let Some(lot) = &line.lot {
                entry
                    .1
                    .push(json!({ "lot": lot, "qty": difference.abs().normalize() }));
            }
        }
        let mut by_account: BTreeMap<Uuid, Vec<Value>> = BTreeMap::new();
        for ((account, item, warehouse, found), (qty, lots)) in moves {
            let unit_cost = if found {
                let (avg, last): (Option<Decimal>, Option<Decimal>) = sqlx::query_as(
                    "SELECT CASE WHEN qty > 0 THEN round(value / qty, 6) END, last_cost
                     FROM stock_level WHERE item_id = $1 AND warehouse_id = $2",
                )
                .bind(item)
                .bind(warehouse)
                .fetch_optional(&mut *ctx.conn)
                .await?
                .unwrap_or((None, None));
                Some(avg.or(last).unwrap_or_default())
            } else {
                None
            };
            by_account.entry(account).or_default().push(json!({
                "item_id": item, "warehouse_id": warehouse, "qty": qty.normalize(),
                "unit_cost": unit_cost, "lots": lots,
            }));
        }
        let post = ctx
            .kernel
            .registry()
            .command("stock_adjustment.post")?
            .handler
            .clone();
        for (account, lines) in by_account {
            let adjustment = document::create(
                ctx,
                document::NewDocument {
                    doc_type: crate::STOCK_ADJUSTMENT.into(),
                    company_id: doc.company_id,
                    doc_date: doc.doc_date,
                    memo: doc.number.as_ref().map(|n| format!("Count {n}")),
                    body: json!({ "offset_account_id": account, "lines": lines }),
                },
            )
            .await?;
            post.run_json(ctx, json!({ "id": adjustment.header.id }))
                .await?;
            document::link(ctx, doc.id, adjustment.header.id, "adjustment").await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

/// Cancels a count, releasing what it holds.
pub(crate) struct Cancel;

#[async_trait]
impl Command for Cancel {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_count.cancel",
            "Cancel a stock count that is not posted: nothing is adjusted and the levels it held \
             are released.",
        )
        .permission("stock_count:cancel")
        .from_states(&["draft", "counting"])
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, STOCK_COUNT, input.id, "cancel").await?;
        if doc.status == "cancelled" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if !["draft", "counting"].contains(&doc.status.as_str()) {
            return Err(Error::state(format!(
                "a {} stock count cannot be cancelled",
                doc.status
            )));
        }
        counts::release(&mut *ctx.conn, doc.id).await?;
        document::transition(ctx, &mut doc, "cancelled").await?;
        document::view(ctx, doc).await
    }
}

// ------------------------------------------------------------ schedule

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProposeInput {
    /// The company counting.
    company_id: Uuid,
    /// The warehouse, with the locations inside it.
    warehouse_id: Uuid,
    /// The day the count would be taken. Today when omitted.
    date: Option<NaiveDate>,
    /// At most this many items, 50 by default.
    limit: Option<i64>,
    /// Also draft a count of the items proposed.
    #[serde(default)]
    draft: bool,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct DueRow {
    item_id: Uuid,
    item_code: String,
    /// The item's ABC class; C when it has none.
    class: String,
    every_days: i32,
    /// When it was last counted (a posted count), if ever.
    last_counted: Option<NaiveDate>,
    /// Days past due; null when never counted.
    days_overdue: Option<i32>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ProposeOutput {
    due: Vec<DueRow>,
    /// The count drafted, when `draft` was asked.
    draft: Option<DocumentView>,
}

pub(crate) struct Propose;

#[async_trait]
impl Command for Propose {
    type Input = ProposeInput;
    type Output = ProposeOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_count.propose",
            "Propose what to count next in a warehouse: the items on hand there whose ABC class \
             (C when an item has none) has a cycle_count_rule and that are due, never counted \
             first, then the longest overdue; at most limit. With draft, a count of them is \
             drafted. Nothing is counted by itself.",
        )
        .permission("stock_count:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ProposeInput) -> Result<ProposeOutput> {
        ctx.require("stock_count:read", input.company_id)?;
        let date = input
            .date
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        let places = subtree(&mut *ctx.conn, input.warehouse_id).await?;
        let limit = input.limit.unwrap_or(50).clamp(1, 1000);
        let due: Vec<DueRow> = sqlx::query_as(
            "WITH items AS (
                 SELECT DISTINCT l.item_id FROM stock_level l
                 WHERE l.warehouse_id = ANY ($2) AND l.qty <> 0
             ), classed AS (
                 SELECT it.item_id, COALESCE(a.class, 'C') AS class
                 FROM items it
                 LEFT JOIN item_abc a ON a.company_id = $1 AND a.item_id = it.item_id
             ), last AS (
                 SELECT cl.item_id, max(d.doc_date) AS counted
                 FROM stock_count_line cl JOIN document d ON d.id = cl.document_id
                 WHERE d.company_id = $1 AND d.status = 'posted' AND cl.warehouse_id = ANY ($2)
                 GROUP BY cl.item_id
             )
             SELECT c.item_id, i.code AS item_code, c.class, r.every_days,
                    last.counted AS last_counted,
                    ($3::date - (last.counted + r.every_days))::int AS days_overdue
             FROM classed c
             JOIN item i ON i.id = c.item_id
             JOIN cycle_count_rule r ON r.company_id = $1 AND r.class = c.class
             LEFT JOIN last ON last.item_id = c.item_id
             WHERE last.counted IS NULL OR last.counted + r.every_days <= $3
             ORDER BY last.counted IS NOT NULL, ($3::date - (last.counted + r.every_days)) DESC,
                      i.code
             LIMIT $4",
        )
        .bind(input.company_id)
        .bind(&places)
        .bind(date)
        .bind(limit)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let draft = if input.draft && !due.is_empty() {
            Some(
                document::create(
                    ctx,
                    document::NewDocument {
                        doc_type: STOCK_COUNT.into(),
                        company_id: input.company_id,
                        doc_date: date,
                        memo: Some("Cycle count".into()),
                        body: json!({
                            "warehouse_id": input.warehouse_id,
                            "item_ids": due.iter().map(|d| d.item_id).collect::<Vec<_>>(),
                        }),
                    },
                )
                .await?,
            )
        } else {
            None
        };
        Ok(ProposeOutput { due, draft })
    }
}
