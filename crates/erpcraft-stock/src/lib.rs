//! The stock ledger.
//!
//! Other modules write to it with [`post`], under a posting the kernel
//! opened for one of their documents. The ledger's rules are enforced by
//! database triggers, so they hold for every module:
//!
//! - stock on hand never goes below zero;
//! - a receipt comes in at the cost the caller states, an issue goes out
//!   at the moving average cost of what is on hand, and the ledger, not the
//!   caller, works that out;
//! - the last of the stock takes the last of the value with it, so no
//!   rounding remainder is ever left on an empty shelf;
//! - entries are never edited, and a reversal takes back exactly what the
//!   original recorded.
//!
//! The stock ledger knows nothing about the general ledger. A module that
//! moves stock is responsible for posting its value there too, in the same
//! posting; [`post`] returns the values for that purpose.

use std::sync::Arc;

use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DynCommand, EntityMeta, Error, FieldMeta, FieldType,
    JsonSchema, Ledger, Migration, Module, PgConnection, Posting, Result, async_trait, command,
    new_id, round_money, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub struct StockModule;

impl Module for StockModule {
    fn name(&self) -> &'static str {
        "stock"
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "stock",
            sql: include_str!("../migrations/0001_stock.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let company = || {
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable()
        };
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        vec![
            EntityMeta::new("item", "Item")
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                // Quantities already recorded would change meaning.
                .field(FieldMeta::new("uom", "Unit of measure", FieldType::Text).immutable())
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"]),
            EntityMeta::new("warehouse", "Warehouse")
                .field(company())
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .company_field("company_id"),
            EntityMeta::new("stock_entry", "Stock entry")
                .field(company())
                .field(FieldMeta::new("posting_id", "Posting", FieldType::Uuid))
                .field(FieldMeta::new("line_no", "Line", FieldType::Int))
                .field(FieldMeta::new(
                    "source_line_id",
                    "Source line",
                    FieldType::Uuid,
                ))
                .field(FieldMeta::new("item_id", "Item", FieldType::link("item")))
                .field(FieldMeta::new(
                    "warehouse_id",
                    "Warehouse",
                    FieldType::link("warehouse"),
                ))
                .field(FieldMeta::new(
                    "posting_date",
                    "Posting date",
                    FieldType::Date,
                ))
                .field(FieldMeta::new("qty", "Quantity", FieldType::Decimal))
                .field(FieldMeta::new("value", "Value", FieldType::Decimal))
                .field(FieldMeta::new("unit_cost", "Unit cost", FieldType::Decimal))
                .field(FieldMeta::new(
                    "qty_after",
                    "Quantity after",
                    FieldType::Decimal,
                ))
                .field(FieldMeta::new(
                    "value_after",
                    "Value after",
                    FieldType::Decimal,
                ))
                .field(FieldMeta::new(
                    "reverses_id",
                    "Reverses",
                    FieldType::link("stock_entry"),
                ))
                .field(created())
                .order_by(&["created_at", "id"])
                .read_only()
                .company_field("company_id"),
        ]
    }

    fn ledgers(&self) -> Vec<Arc<dyn Ledger>> {
        vec![Arc::new(StockLedger)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(OnHand)]
    }
}

/// One movement to record.
#[derive(Debug, Clone)]
pub struct StockLine {
    pub item_id: Uuid,
    pub warehouse_id: Uuid,
    /// Positive: into the warehouse. Negative: out of it.
    pub qty: Decimal,
    /// What stock coming in costs, in the company's accounting currency.
    /// Required for a receipt. Must be absent for an issue: the ledger
    /// values issues.
    pub cost: Option<Cost>,
    /// The document line this movement comes from.
    pub source_line_id: Option<Uuid>,
}

/// The cost of stock coming in.
#[derive(Debug, Clone, Copy)]
pub enum Cost {
    /// Per unit. The value is this times the quantity, rounded to the
    /// currency's decimals.
    Unit(Decimal),
    /// For the whole quantity, taken as it is. Use it to bring stock in at
    /// exactly the value it left somewhere else for.
    Total(Decimal),
}

/// A movement as the ledger recorded it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StockEntry {
    pub id: Uuid,
    pub line_no: i32,
    pub source_line_id: Option<Uuid>,
    pub item_id: Uuid,
    pub warehouse_id: Uuid,
    pub qty: Decimal,
    /// The change in stock value: positive for a receipt, negative for an
    /// issue. This is the amount to post to the general ledger.
    pub value: Decimal,
    pub unit_cost: Decimal,
    pub qty_after: Decimal,
    pub value_after: Decimal,
}

const ENTRY_COLUMNS: &str = "id, line_no, source_line_id, item_id, warehouse_id, qty, value, \
                             unit_cost, qty_after, value_after";

/// Lock the levels of these `(item, warehouse)` pairs, always in the same
/// order, so that two postings moving the same items cannot deadlock.
/// [`post`] does this for the lines it is given. A caller that makes
/// several calls to [`post`] under one posting locks everything first.
pub async fn lock_levels(
    conn: &mut PgConnection,
    company_id: Uuid,
    mut pairs: Vec<(Uuid, Uuid)>,
) -> Result<()> {
    pairs.sort();
    pairs.dedup();
    let (items, warehouses): (Vec<Uuid>, Vec<Uuid>) = pairs.into_iter().unzip();
    sqlx::query(
        "INSERT INTO stock_level (item_id, warehouse_id, company_id)
         SELECT item_id, warehouse_id, $3 FROM unnest($1::uuid[], $2::uuid[]) AS p (item_id, warehouse_id)
         ON CONFLICT DO NOTHING",
    )
    .bind(&items)
    .bind(&warehouses)
    .bind(company_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "SELECT 1 FROM stock_level l
         JOIN unnest($1::uuid[], $2::uuid[]) AS p (item_id, warehouse_id)
           ON p.item_id = l.item_id AND p.warehouse_id = l.warehouse_id
         ORDER BY l.item_id, l.warehouse_id FOR UPDATE OF l",
    )
    .bind(&items)
    .bind(&warehouses)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Write stock entries under a posting, in the order given, and return
/// them as recorded, with the value the ledger gave each one. It can be
/// called more than once for the same posting; the entries are numbered
/// on from the ones already there.
pub async fn post(
    conn: &mut PgConnection,
    posting: &Posting,
    lines: &[StockLine],
) -> Result<Vec<StockEntry>> {
    if lines.is_empty() {
        return Err(Error::validation(
            "a posting needs at least one stock entry",
        ));
    }
    let decimals: i16 = sqlx::query_scalar(
        "SELECT cur.decimals FROM company c
         JOIN currency cur ON cur.code = c.base_currency WHERE c.id = $1",
    )
    .bind(posting.company_id)
    .fetch_one(&mut *conn)
    .await?;

    let mut values = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let n = index + 1;
        let fail = |msg: &str| Error::validation(format!("line {n}: {msg}"));
        if line.qty.is_zero() {
            return Err(fail("the quantity cannot be zero"));
        }
        values.push(match (line.qty.is_sign_positive(), line.cost) {
            (true, None) => return Err(fail("stock coming in needs a cost")),
            (true, Some(Cost::Unit(cost) | Cost::Total(cost))) if cost.is_sign_negative() => {
                return Err(fail("the cost cannot be negative"));
            }
            (true, Some(Cost::Unit(cost))) => Some(round_money(line.qty * cost, decimals as u32)),
            (true, Some(Cost::Total(value))) => Some(value),
            (false, Some(_)) => {
                return Err(fail(
                    "stock going out takes no cost: it leaves at the average cost on hand",
                ));
            }
            (false, None) => None,
        });
    }

    lock_levels(
        conn,
        posting.company_id,
        lines.iter().map(|l| (l.item_id, l.warehouse_id)).collect(),
    )
    .await?;

    let numbered: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(line_no), 0) FROM stock_entry WHERE posting_id = $1",
    )
    .bind(posting.id)
    .fetch_one(&mut *conn)
    .await?;

    let mut entries = Vec::with_capacity(lines.len());
    for (index, (line, value)) in lines.iter().zip(values).enumerate() {
        // unit_cost and the running totals are set by the ledger's trigger,
        // as is the value of an issue.
        let entry: StockEntry = sqlx::query_as(&format!(
            "INSERT INTO stock_entry (id, company_id, posting_id, line_no, source_line_id, item_id,
                warehouse_id, posting_date, qty, value, unit_cost, qty_after, value_after)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 0, 0, 0)
             RETURNING {ENTRY_COLUMNS}"
        ))
        .bind(new_id())
        .bind(posting.company_id)
        .bind(posting.id)
        .bind(numbered + index as i32 + 1)
        .bind(line.source_line_id)
        .bind(line.item_id)
        .bind(line.warehouse_id)
        .bind(posting.posting_date)
        .bind(line.qty)
        .bind(value)
        .fetch_one(&mut *conn)
        .await?;
        entries.push(entry);
    }
    Ok(entries)
}

/// The entries recorded under a posting, in order.
pub async fn entries_of(conn: &mut PgConnection, posting_id: Uuid) -> Result<Vec<StockEntry>> {
    let entries = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLUMNS} FROM stock_entry WHERE posting_id = $1 ORDER BY line_no"
    ))
    .bind(posting_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(entries)
}

struct StockLedger;

#[async_trait]
impl Ledger for StockLedger {
    fn name(&self) -> &'static str {
        "stock"
    }

    /// Take back each original entry, last first. The ledger refuses if the
    /// stock has since moved on: a receipt cannot be reversed once what it
    /// brought in has been issued.
    async fn reverse(
        &self,
        conn: &mut PgConnection,
        original: &Posting,
        reversal: &Posting,
    ) -> Result<()> {
        let originals = entries_of(conn, original.id).await?;
        if originals.is_empty() {
            return Ok(());
        }
        lock_levels(
            conn,
            original.company_id,
            originals
                .iter()
                .map(|e| (e.item_id, e.warehouse_id))
                .collect(),
        )
        .await?;
        for entry in originals.iter().rev() {
            // The trigger replaces qty and value with the exact opposite of
            // the original's.
            sqlx::query(
                "INSERT INTO stock_entry (id, company_id, posting_id, line_no, source_line_id, item_id,
                    warehouse_id, posting_date, qty, value, unit_cost, qty_after, value_after, reverses_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 0, 0, 0, $11)",
            )
            .bind(new_id())
            .bind(reversal.company_id)
            .bind(reversal.id)
            .bind(entry.line_no)
            .bind(entry.source_line_id)
            .bind(entry.item_id)
            .bind(entry.warehouse_id)
            .bind(reversal.posting_date)
            .bind(-entry.qty)
            .bind(-entry.value)
            .bind(entry.id)
            .execute(&mut *conn)
            .await?;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ on hand

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OnHandInput {
    company_id: Uuid,
    /// Only this item.
    item_id: Option<Uuid>,
    /// Only this warehouse.
    warehouse_id: Option<Uuid>,
}

#[derive(Serialize, sqlx::FromRow)]
struct OnHandRow {
    item_id: Uuid,
    /// Absent for a caller who may not read items.
    item_code: Option<String>,
    item_name: Option<String>,
    uom: String,
    warehouse_id: Uuid,
    /// Absent for a caller who may not read warehouses.
    warehouse_code: Option<String>,
    qty: Decimal,
    /// Absent for a caller from whom the value of stock entries is withheld.
    value: Option<Decimal>,
    /// Value divided by quantity: what the next issue goes out at.
    average_cost: Option<Decimal>,
}

#[derive(Serialize)]
struct OnHandOutput {
    company_id: Uuid,
    currency: String,
    total_value: Option<Decimal>,
    rows: Vec<OnHandRow>,
}

struct OnHand;

#[async_trait]
impl Command for OnHand {
    type Input = OnHandInput;
    type Output = OnHandOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "stock.on_hand",
            "Quantity, value and average cost of every item on hand in a company's warehouses, \
             in its accounting currency. Items with nothing on hand are left out. Values are \
             left out for a caller from whom the value of stock entries is withheld.",
        )
        .permission("stock_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: OnHandInput) -> Result<OnHandOutput> {
        ctx.require("stock_entry:read", input.company_id)?;
        let currency: String =
            sqlx::query_scalar("SELECT base_currency FROM company WHERE id = $1")
                .bind(input.company_id)
                .fetch_optional(&mut *ctx.conn)
                .await?
                .ok_or_else(|| Error::not_found("company"))?;
        // This report is built from stock entries, so what is withheld on
        // them is withheld here, and names come only with the right to
        // read what they name.
        let actor = ctx.actor;
        let values = !["value", "unit_cost", "value_after"]
            .iter()
            .any(|field| actor.may_withhold("stock_entry:read", field));
        let items = actor.can_any("item:read");
        let warehouses = actor.can("warehouse:read", input.company_id);
        let rows: Vec<OnHandRow> = sqlx::query_as(
            "SELECT l.item_id,
                    CASE WHEN $5 THEN i.code END AS item_code,
                    CASE WHEN $5 THEN i.name END AS item_name, i.uom,
                    l.warehouse_id, CASE WHEN $6 THEN w.code END AS warehouse_code, l.qty,
                    CASE WHEN $4 THEN l.value END AS value,
                    CASE WHEN $4 THEN round(l.value / l.qty, 6) END AS average_cost
             FROM stock_level l
             JOIN item i ON i.id = l.item_id
             JOIN warehouse w ON w.id = l.warehouse_id
             WHERE l.company_id = $1 AND l.qty > 0
               AND ($2::uuid IS NULL OR l.item_id = $2)
               AND ($3::uuid IS NULL OR l.warehouse_id = $3)
             ORDER BY i.code, w.code",
        )
        .bind(input.company_id)
        .bind(input.item_id)
        .bind(input.warehouse_id)
        .bind(values)
        .bind(items)
        .bind(warehouses)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(OnHandOutput {
            company_id: input.company_id,
            currency,
            total_value: values.then(|| rows.iter().filter_map(|r| r.value).sum()),
            rows,
        })
    }
}
