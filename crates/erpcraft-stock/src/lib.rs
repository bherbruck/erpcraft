//! The stock ledger.
//!
//! Other modules write to it with [`post`], under a posting the kernel
//! opened for one of their documents. The ledger's rules are enforced by
//! database triggers, so they hold for every module:
//!
//! - stock on hand never goes below zero, unless a policy rule of the
//!   company allows it (the policy point `stock.negative`, asked by
//!   [`post`]); the entry then names the rule;
//! - a receipt comes in at the cost the caller states, an issue goes out
//!   at the moving average cost of what is on hand, and the ledger, not the
//!   caller, works that out;
//! - goods that are not on hand leave at the last cost known for them, an
//!   estimate. When goods arrive on a level below zero they first cover
//!   what is missing, and the difference between their real cost and that
//!   estimate is a variance, not stock value: see [`StockEntry::variance`];
//! - the last of the stock takes the last of the value with it, so no
//!   rounding remainder is ever left on an empty shelf;
//! - entries are never edited, and a reversal takes back exactly what the
//!   original recorded.
//!
//! The stock ledger knows nothing about the general ledger. A module that
//! moves stock is responsible for posting its value there too, in the same
//! posting; [`post`] returns the values for that purpose.

use std::sync::Arc;

use erpcraft_kernel::policy::Policy;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Decision, DynCommand, EntityMeta, Error, FieldMeta,
    FieldType, JsonSchema, Ledger, Migration, Module, PgConnection, PolicyPoint, Posting, Result,
    async_trait, command, company, new_id, round_money, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod locations;
pub mod lots;

pub use locations::{Locations, line_scopes, locations};
pub use lots::{LineLot, LotQty, Tracking};

pub struct StockModule;

impl Module for StockModule {
    fn name(&self) -> &'static str {
        "stock"
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "stock",
                sql: include_str!("../migrations/0001_stock.sql"),
            },
            Migration {
                version: 2,
                name: "negative_stock",
                sql: include_str!("../migrations/0002_negative_stock.sql"),
            },
            Migration {
                version: 3,
                name: "last_cost_and_facts",
                sql: include_str!("../migrations/0003_last_cost_and_facts.sql"),
            },
            Migration {
                version: 4,
                name: "cost_adjustment",
                sql: include_str!("../migrations/0004_cost_adjustment.sql"),
            },
            Migration {
                version: 5,
                name: "item_weight",
                sql: include_str!("../migrations/0005_item_weight.sql"),
            },
            Migration {
                version: 6,
                name: "reservation",
                sql: include_str!("../migrations/0006_reservation.sql"),
            },
            Migration {
                version: 7,
                name: "list_indexes",
                sql: include_str!("../migrations/0007_list_indexes.sql"),
            },
            Migration {
                version: 8,
                name: "sites",
                sql: include_str!("../migrations/0008_sites.sql"),
            },
            Migration {
                version: 9,
                name: "warehouse_guards",
                sql: include_str!("../migrations/0009_warehouse_guards.sql"),
            },
            Migration {
                version: 10,
                name: "lots",
                sql: include_str!("../migrations/0010_lots.sql"),
            },
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                NEGATIVE,
                "Asked whenever stock going out would take an item in a warehouse below zero: \
                 may it? Unless configured otherwise it may not. A company whose goods are \
                 often on the shelf before their receipt is entered allows it, for everything \
                 or for some movements only, for example with an allow rule when \
                 `facts.context == 'sales_delivery'`. Goods that are not on hand leave at the \
                 last cost known for the item in that warehouse; when the missing goods are \
                 received, the difference from their real cost is posted as a stock variance, \
                 which needs an account rule with the purpose `stock_variance`.",
            )
            .number(
                "on_hand",
                "Quantity on hand before this movement; below zero if the level already is",
            )
            .as_quantity()
            .number("issuing", "Quantity this movement takes out")
            .as_quantity()
            .number(
                "short_by",
                "How far below zero the level would stand after this movement",
            )
            .as_quantity()
            .text("item_code", "Code of the item")
            .text("warehouse_code", "Code of the warehouse")
            .text(
                "context",
                "What takes the stock out: sales_delivery, production, adjustment or transfer; \
                 other modules name their own",
            )
            .refuse_by_default()
            .refusal("there is not enough stock on hand")
            // The ledger checks the allowing rule itself.
            .not_approvable(),
            lots::policy(),
        ]
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
                .field(
                    FieldMeta::new("weight", "Weight", FieldType::Decimal)
                        .quantity()
                        .help(
                            "Of one unit, in the company's unit of weight: what freight is \
                             shared out by when it is allocated by weight.",
                        ),
                )
                .field(
                    FieldMeta::new(
                        "tracking",
                        "Tracking",
                        FieldType::options(&["none", "lot", "serial"]),
                    )
                    .help(
                        "Whether every movement names its lots or serial numbers. Changes only \
                         while the item has no stock anywhere.",
                    ),
                )
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"]),
            EntityMeta::new("stock_lot", "Lot")
                .field(company())
                .field(
                    FieldMeta::new("item_id", "Item", FieldType::link("item"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("code", "Lot or serial number", FieldType::Text)
                        .required()
                        .immutable(),
                )
                .field(FieldMeta::new("expiry_date", "Expires", FieldType::Date))
                .field(FieldMeta::new(
                    "manufactured_date",
                    "Manufactured",
                    FieldType::Date,
                ))
                .field(
                    FieldMeta::new("attributes", "Attributes", FieldType::Json)
                        .help("Free attributes, such as the supplier's own lot number."),
                )
                .field(created())
                .order_by(&["code"])
                .search(&["code"])
                .display(&["code"])
                .company_field("company_id"),
            EntityMeta::new("site", "Site")
                .field(company())
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .company_field("company_id"),
            EntityMeta::new("warehouse", "Warehouse")
                .field(company())
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new("site_id", "Site", FieldType::link("site")).help(
                        "The plant or place the warehouse is at. A location inside another \
                         warehouse is at that warehouse's site.",
                    ),
                )
                .field(
                    FieldMeta::new("parent_id", "Part of", FieldType::link("warehouse"))
                        .help("For a location or bin: the warehouse it is part of."),
                )
                .field(
                    FieldMeta::new("transit", "In transit", FieldType::Bool).help(
                        "Holds goods on their way from one site to another, between the two \
                         steps of a transfer.",
                    ),
                )
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .company_field("company_id"),
            EntityMeta::new("stock_entry", "Stock entry")
                .field(company())
                .field(FieldMeta::new("posting_id", "Posting", FieldType::Posting))
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
                .field(
                    FieldMeta::new("site_id", "Site", FieldType::link("site"))
                        .help("The warehouse's site when the entry was recorded."),
                )
                .field(FieldMeta::new(
                    "posting_date",
                    "Posting date",
                    FieldType::Date,
                ))
                .field(
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom"),
                )
                .field(FieldMeta::new("value", "Value", FieldType::Decimal).money())
                .field(FieldMeta::new("unit_cost", "Unit cost", FieldType::Decimal).unit_price())
                .field(
                    FieldMeta::new("qty_after", "Quantity after", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom"),
                )
                .field(FieldMeta::new("value_after", "Value after", FieldType::Decimal).money())
                .field(FieldMeta::new(
                    "reverses_id",
                    "Reverses",
                    FieldType::link("stock_entry"),
                ))
                .field(FieldMeta::new("variance", "Variance", FieldType::Decimal).money())
                .field(FieldMeta::new(
                    "negative_rule_id",
                    "Went below zero by policy rule",
                    FieldType::link("policy_rule"),
                ))
                .field(FieldMeta::new(
                    "negative_facts",
                    "Facts the rule was asked with",
                    FieldType::Json,
                ))
                .field(FieldMeta::new(
                    "negative_condition",
                    "Condition of the rule then",
                    FieldType::Text,
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
        vec![
            command(OnHand),
            command(Reservations),
            command(lots::Lots),
            command(lots::Proposal),
            command(lots::Trace),
        ]
    }
}

/// The policy point asked when an issue would take a level below zero.
pub const NEGATIVE: &str = "stock.negative";

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
    /// The lots or serial numbers moved, quantities above zero, adding up
    /// to the quantity: required for a tracked item, none otherwise
    /// ([`lots::resolve`] makes them from what a document line names).
    pub lots: Vec<LotQty>,
}

/// A change in the value of what is on hand of an item in a warehouse,
/// without any quantity moving: a landed cost allocated to goods received.
#[derive(Debug, Clone)]
pub struct Adjustment {
    pub item_id: Uuid,
    pub warehouse_id: Uuid,
    /// In the company's accounting currency, rounded to its decimals; more
    /// than zero raises the value.
    pub value: Decimal,
    pub source_line_id: Option<Uuid>,
}

/// Record cost adjustments under `posting`: entries with no quantity that
/// change the value of the stock on hand. The ledger refuses one on a level
/// with nothing on hand, or that would take its value below zero.
/// Reversing the posting takes them back, which the ledger refuses once the
/// stock has gone.
pub async fn adjust(
    conn: &mut PgConnection,
    posting: &Posting,
    adjustments: &[Adjustment],
) -> Result<Vec<StockEntry>> {
    if adjustments.is_empty() {
        return Ok(vec![]);
    }
    lock_levels(
        conn,
        posting.company_id,
        adjustments
            .iter()
            .map(|a| (a.item_id, a.warehouse_id))
            .collect(),
    )
    .await?;
    let numbered: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(line_no), 0) FROM stock_entry WHERE posting_id = $1",
    )
    .bind(posting.id)
    .fetch_one(&mut *conn)
    .await?;
    let ids: Vec<Uuid> = adjustments.iter().map(|_| new_id()).collect();
    let line_nos: Vec<i32> = (1..=adjustments.len() as i32)
        .map(|n| numbered + n)
        .collect();
    // In the order given; see `post` on relying on it.
    let mut entries: Vec<StockEntry> = sqlx::query_as(&format!(
        "INSERT INTO stock_entry (id, company_id, posting_id, line_no, source_line_id, item_id,
            warehouse_id, posting_date, qty, value, unit_cost, qty_after, value_after)
         SELECT r.id, $1, $2, r.line_no, r.source_line_id, r.item_id, r.warehouse_id, $3,
            0, r.value, 0, 0, 0
         FROM unnest($4::uuid[], $5::int4[], $6::uuid[], $7::uuid[], $8::uuid[], $9::numeric[])
            AS r (id, line_no, source_line_id, item_id, warehouse_id, value)
         ORDER BY r.line_no
         RETURNING {ENTRY_COLUMNS}"
    ))
    .bind(posting.company_id)
    .bind(posting.id)
    .bind(posting.posting_date)
    .bind(&ids)
    .bind(&line_nos)
    .bind(
        adjustments
            .iter()
            .map(|a| a.source_line_id)
            .collect::<Vec<_>>(),
    )
    .bind(adjustments.iter().map(|a| a.item_id).collect::<Vec<_>>())
    .bind(
        adjustments
            .iter()
            .map(|a| a.warehouse_id)
            .collect::<Vec<_>>(),
    )
    .bind(adjustments.iter().map(|a| a.value).collect::<Vec<_>>())
    .fetch_all(&mut *conn)
    .await?;
    entries.sort_by_key(|e| e.line_no);
    Ok(entries)
}

/// What is on hand of an item in a warehouse now: quantity and value.
pub async fn on_hand(
    conn: &mut PgConnection,
    item_id: Uuid,
    warehouse_id: Uuid,
) -> Result<(Decimal, Decimal)> {
    let level: Option<(Decimal, Decimal)> = sqlx::query_as(
        "SELECT qty, value FROM stock_level WHERE item_id = $1 AND warehouse_id = $2",
    )
    .bind(item_id)
    .bind(warehouse_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(level.unwrap_or_default())
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
#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
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
    /// Not zero only for goods that arrived on a level below zero: what
    /// the goods covering the shortfall really cost, less the estimate the
    /// shortfall left stock at. It is not stock value. The caller posts it
    /// to the account for the purpose `stock_variance`: a debit when
    /// positive, a credit when negative.
    pub variance: Decimal,
    /// The policy rule that allowed this issue to take the level below
    /// zero. `None` for every entry that did not.
    pub negative_rule_id: Option<Uuid>,
}

impl StockEntry {
    /// What the movement cost in all: the change in stock value plus the
    /// variance. For a receipt it is the cost the caller stated, and what
    /// the other side of the receipt is posted at.
    pub fn cost(&self) -> Decimal {
        self.value + self.variance
    }
}

const ENTRY_COLUMNS: &str = "id, line_no, source_line_id, item_id, warehouse_id, qty, value, \
                             unit_cost, qty_after, value_after, variance, negative_rule_id";

// ------------------------------------------------------------- reservations

/// What a line of an order still needs from stock: a sales order line not
/// yet delivered, a production order's component not yet consumed.
#[derive(Debug, Clone)]
pub struct Need {
    pub line_id: Uuid,
    pub item_id: Uuid,
    pub warehouse_id: Uuid,
    /// Zero or less: nothing, and its reservation goes.
    pub qty: Decimal,
}

/// Set aside stock for the lines of an order (`document_id`): each line's
/// reservation becomes what it still needs, as far as stock on hand that
/// is not reserved for anything else covers it. A line that needs nothing
/// more, or for which nothing is free, holds no reservation. Returns what
/// each need holds now, in the order given. The levels are locked, so two
/// orders do not set aside the same goods.
pub async fn reserve(
    conn: &mut PgConnection,
    company_id: Uuid,
    document_id: Uuid,
    needs: &[Need],
) -> Result<Vec<Decimal>> {
    let pairs = needs.iter().map(|n| (n.item_id, n.warehouse_id)).collect();
    let on_hand = lock_and_read_levels(conn, company_id, pairs).await?;
    let mut held = Vec::with_capacity(needs.len());
    for need in needs {
        let others: Decimal = sqlx::query_scalar(
            "SELECT COALESCE(sum(qty), 0) FROM stock_reservation
             WHERE company_id = $1 AND item_id = $2 AND warehouse_id = $3 AND line_id <> $4",
        )
        .bind(company_id)
        .bind(need.item_id)
        .bind(need.warehouse_id)
        .bind(need.line_id)
        .fetch_one(&mut *conn)
        .await?;
        let here = on_hand
            .get(&(need.item_id, need.warehouse_id))
            .copied()
            .unwrap_or_default();
        let free = (here - others).max(Decimal::ZERO);
        let qty = need.qty.min(free).max(Decimal::ZERO);
        if qty.is_zero() {
            sqlx::query("DELETE FROM stock_reservation WHERE line_id = $1")
                .bind(need.line_id)
                .execute(&mut *conn)
                .await?;
        } else {
            sqlx::query(
                "INSERT INTO stock_reservation (id, company_id, item_id, warehouse_id, qty,
                     document_id, line_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (line_id) DO UPDATE SET qty = EXCLUDED.qty,
                     item_id = EXCLUDED.item_id, warehouse_id = EXCLUDED.warehouse_id,
                     updated_at = now()",
            )
            .bind(erpcraft_kernel::new_id())
            .bind(company_id)
            .bind(need.item_id)
            .bind(need.warehouse_id)
            .bind(qty)
            .bind(document_id)
            .bind(need.line_id)
            .execute(&mut *conn)
            .await?;
        }
        held.push(qty);
    }
    Ok(held)
}

/// Cut reservations back to what is on hand at these levels, which the
/// caller holds locked: when goods leave other than for the order that
/// reserved them (another order's delivery, an adjustment, a transfer, a
/// return, production), or a receipt is reversed, a level can hold less
/// than is reserved of it. The earliest reservations are kept whole and
/// the latest cut back first, so a promise made first is kept first; one
/// cut to nothing is removed. Reserved never exceeds on hand afterwards.
pub async fn cap_reservations(
    conn: &mut PgConnection,
    company_id: Uuid,
    mut pairs: Vec<(Uuid, Uuid)>,
) -> Result<()> {
    pairs.sort();
    pairs.dedup();
    let (items, warehouses): (Vec<Uuid>, Vec<Uuid>) = pairs.into_iter().unzip();
    // What each reservation may keep: on hand less what the reservations
    // before it hold, at most its own quantity.
    let keep: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "WITH r AS (
             SELECT r.id, r.qty, greatest(l.qty, 0) AS on_hand,
                    sum(r.qty) OVER (PARTITION BY r.item_id, r.warehouse_id
                                     ORDER BY r.created_at, r.id) - r.qty AS before
             FROM stock_reservation r
             JOIN unnest($2::uuid[], $3::uuid[]) AS p (item_id, warehouse_id)
               ON p.item_id = r.item_id AND p.warehouse_id = r.warehouse_id
             JOIN stock_level l ON l.item_id = r.item_id AND l.warehouse_id = r.warehouse_id
             WHERE r.company_id = $1)
         SELECT id, greatest(least(qty, on_hand - before), 0) FROM r
         WHERE before + qty > on_hand",
    )
    .bind(company_id)
    .bind(&items)
    .bind(&warehouses)
    .fetch_all(&mut *conn)
    .await?;
    for (id, qty) in keep {
        if qty.is_zero() {
            sqlx::query("DELETE FROM stock_reservation WHERE id = $1")
                .bind(id)
                .execute(&mut *conn)
                .await?;
        } else {
            sqlx::query("UPDATE stock_reservation SET qty = $2, updated_at = now() WHERE id = $1")
                .bind(id)
                .bind(qty)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

/// Remove every reservation of an order: it is done, cancelled or closed.
pub async fn release(conn: &mut PgConnection, document_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM stock_reservation WHERE document_id = $1")
        .bind(document_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// What is reserved of each item in each warehouse of a company.
pub async fn reserved(
    conn: &mut PgConnection,
    company_id: Uuid,
) -> Result<Vec<(Uuid, Uuid, Decimal)>> {
    Ok(sqlx::query_as(
        "SELECT item_id, warehouse_id, sum(qty) FROM stock_reservation
         WHERE company_id = $1 GROUP BY item_id, warehouse_id ORDER BY item_id, warehouse_id",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// What of an item in a warehouse is reserved for orders other than
/// `document_id`.
pub async fn reserved_for_others(
    conn: &mut PgConnection,
    item_id: Uuid,
    warehouse_id: Uuid,
    document_id: Uuid,
) -> Result<Decimal> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(sum(qty), 0) FROM stock_reservation
         WHERE item_id = $1 AND warehouse_id = $2 AND document_id <> $3",
    )
    .bind(item_id)
    .bind(warehouse_id)
    .bind(document_id)
    .fetch_one(&mut *conn)
    .await?)
}

/// Lock the levels of these `(item, warehouse)` pairs, always in the same
/// order, so that two postings moving the same items cannot deadlock.
/// [`post`] does this for the lines it is given. A caller that makes
/// several calls to [`post`] under one posting locks everything first.
pub async fn lock_levels(
    conn: &mut PgConnection,
    company_id: Uuid,
    pairs: Vec<(Uuid, Uuid)>,
) -> Result<()> {
    lock_and_read_levels(conn, company_id, pairs)
        .await
        .map(|_| ())
}

/// [`lock_levels`], returning the quantity on hand of each level.
async fn lock_and_read_levels(
    conn: &mut PgConnection,
    company_id: Uuid,
    mut pairs: Vec<(Uuid, Uuid)>,
) -> Result<std::collections::HashMap<(Uuid, Uuid), Decimal>> {
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
    let levels: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
        "SELECT l.item_id, l.warehouse_id, l.qty FROM stock_level l
         JOIN unnest($1::uuid[], $2::uuid[]) AS p (item_id, warehouse_id)
           ON p.item_id = l.item_id AND p.warehouse_id = l.warehouse_id
         ORDER BY l.item_id, l.warehouse_id FOR UPDATE OF l",
    )
    .bind(&items)
    .bind(&warehouses)
    .fetch_all(&mut *conn)
    .await?;
    Ok(levels
        .into_iter()
        .map(|(item, warehouse, qty)| ((item, warehouse), qty))
        .collect())
}

/// Write stock entries under a posting, in the order given, and return
/// them as recorded, with the value the ledger gave each one. It can be
/// called more than once for the same posting; the entries are numbered
/// on from the ones already there.
///
/// `context` says what the movement is, in one word such as
/// `sales_delivery`. The company's policy on stock below zero
/// ([`NEGATIVE`]) can tell movements apart by it.
///
/// A caller that brings stock in must look at [`StockEntry::variance`].
pub async fn post(
    ctx: &mut Ctx<'_>,
    posting: &Posting,
    lines: &[StockLine],
    context: &str,
) -> Result<Vec<StockEntry>> {
    if lines.is_empty() {
        return Err(Error::validation(
            "a posting needs at least one stock entry",
        ));
    }
    let decimals = company::currency_decimals(&mut *ctx.conn, posting.company_id).await?;

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
            (true, Some(Cost::Unit(cost))) => Some(round_money(line.qty * cost, decimals)),
            (true, Some(Cost::Total(value))) => Some(value),
            (false, Some(_)) => {
                return Err(fail(
                    "stock going out takes no cost: it leaves at the average cost on hand",
                ));
            }
            (false, None) => None,
        });
    }

    // Goods in transit move only with transfers (shipped in, received
    // out) and adjustments, which carry them on the account for goods in
    // transit; anything else would value them on another account.
    if context != "transfer" && context != "adjustment" {
        let warehouses: Vec<Uuid> = lines.iter().map(|l| l.warehouse_id).collect();
        let transit: Option<String> = sqlx::query_scalar(
            "SELECT code FROM warehouse WHERE id = ANY ($1) AND transit ORDER BY code LIMIT 1",
        )
        .bind(&warehouses)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some(code) = transit {
            return Err(Error::validation(format!(
                "warehouse {code} holds goods in transit, which come and go only with stock \
                 transfers"
            )));
        }
    }

    // What is on hand, followed line by line as the ledger will see it:
    // the trigger takes the entries in the order given.
    let mut on_hand = lock_and_read_levels(
        &mut *ctx.conn,
        posting.company_id,
        lines.iter().map(|l| (l.item_id, l.warehouse_id)).collect(),
    )
    .await?;

    let numbered: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(line_no), 0) FROM stock_entry WHERE posting_id = $1",
    )
    .bind(posting.id)
    .fetch_one(&mut *ctx.conn)
    .await?;

    // Each lot going out, as the company's policy sees it.
    for line in lines.iter().filter(|l| l.qty.is_sign_negative()) {
        lots::ask_issue(
            ctx,
            posting.company_id,
            posting.posting_date,
            line.warehouse_id,
            &line.lots,
            context,
        )
        .await?;
    }

    // Loaded when the first line is short, so that a posting within what
    // is on hand asks nothing.
    let mut policy = None;
    let mut rule_ids = Vec::with_capacity(lines.len());
    let mut facts = Vec::with_capacity(lines.len());
    for line in lines {
        let level = on_hand
            .entry((line.item_id, line.warehouse_id))
            .or_default();
        let allowed = if line.qty.is_sign_negative() {
            allow_negative(ctx, &mut policy, posting.company_id, line, *level, context).await?
        } else {
            None
        };
        *level += line.qty;
        let (rule, fact) = allowed.unzip();
        rule_ids.push(rule);
        facts.push(fact.map(|f| f.to_string()));
    }

    // One statement for all entries. unit_cost and the running totals are
    // set by the ledger's trigger, row by row in the order given, as are the
    // value of an issue and the variance of a receipt. The trigger honours
    // a rule only if it is one that allows this, and only with the facts it
    // was asked with, which it checks.
    //
    // This relies on `INSERT ... SELECT ... ORDER BY` inserting the rows,
    // and firing the row triggers, in the order of the ORDER BY. PostgreSQL
    // does not document that; it holds in practice, because the insert
    // (ModifyTable) pulls rows from the Sort one at a time and an INSERT is
    // never run in parallel. The order of RETURNING is not relied on: the
    // entries are sorted by line number below.
    let ids: Vec<Uuid> = lines.iter().map(|_| new_id()).collect();
    let line_nos: Vec<i32> = (1..=lines.len() as i32).map(|n| numbered + n).collect();
    let mut entries: Vec<StockEntry> = sqlx::query_as(&format!(
        "INSERT INTO stock_entry (id, company_id, posting_id, line_no, source_line_id, item_id,
            warehouse_id, posting_date, qty, value, unit_cost, qty_after, value_after,
            negative_rule_id, negative_facts)
         SELECT r.id, $1, $2, r.line_no, r.source_line_id, r.item_id, r.warehouse_id, $3,
            r.qty, r.value, 0, 0, 0, r.rule_id, r.facts::jsonb
         FROM unnest($4::uuid[], $5::int4[], $6::uuid[], $7::uuid[], $8::uuid[],
                     $9::numeric[], $10::numeric[], $11::uuid[], $12::text[])
            AS r (id, line_no, source_line_id, item_id, warehouse_id, qty, value, rule_id, facts)
         ORDER BY r.line_no
         RETURNING {ENTRY_COLUMNS}"
    ))
    .bind(posting.company_id)
    .bind(posting.id)
    .bind(posting.posting_date)
    .bind(&ids)
    .bind(&line_nos)
    .bind(lines.iter().map(|l| l.source_line_id).collect::<Vec<_>>())
    .bind(lines.iter().map(|l| l.item_id).collect::<Vec<_>>())
    .bind(lines.iter().map(|l| l.warehouse_id).collect::<Vec<_>>())
    .bind(lines.iter().map(|l| l.qty).collect::<Vec<_>>())
    .bind(&values)
    .bind(&rule_ids)
    .bind(&facts)
    .fetch_all(&mut *ctx.conn)
    .await?;
    // Returned in the order given, by their numbers.
    entries.sort_by_key(|e| e.line_no);
    // The lots of each entry; the ledger keeps their levels and checks at
    // commit that a tracked item's entry names lots for all of it.
    let mut lot_entries = vec![];
    let mut lot_ids = vec![];
    let mut lot_qtys = vec![];
    for (line, entry) in lines.iter().zip(&entries) {
        for lot in &line.lots {
            lot_entries.push(entry.id);
            lot_ids.push(lot.lot_id);
            lot_qtys.push(if line.qty.is_sign_negative() {
                -lot.qty
            } else {
                lot.qty
            });
        }
    }
    if !lot_entries.is_empty() {
        sqlx::query(
            "INSERT INTO stock_entry_lot (entry_id, lot_id, qty)
             SELECT * FROM unnest($1::uuid[], $2::uuid[], $3::numeric[])",
        )
        .bind(&lot_entries)
        .bind(&lot_ids)
        .bind(&lot_qtys)
        .execute(&mut *ctx.conn)
        .await?;
    }
    // Goods that left may leave less on hand than is reserved. A sales
    // delivery and production take their own order's reservation first:
    // those commands settle it and then call `cap_reservations`
    // themselves, so that another order's reservation is not cut for
    // goods its own order was holding.
    let settles_own = matches!(context, "sales_delivery" | "production");
    let out: Vec<(Uuid, Uuid)> = lines
        .iter()
        .filter(|l| l.qty.is_sign_negative())
        .map(|l| (l.item_id, l.warehouse_id))
        .collect();
    if !out.is_empty() && !settles_own {
        cap_reservations(&mut *ctx.conn, posting.company_id, out).await?;
    }
    Ok(entries)
}

/// For an issue that would take its level below zero, ask the company's
/// policy. `on_hand` is the level as the entry will find it: read under
/// the level's lock and followed through the lines before it. Returns the
/// rule that allows it with the facts it was asked with, `None` when the
/// issue stays within what is on hand, and the refusal otherwise.
async fn allow_negative<'a>(
    ctx: &mut Ctx<'a>,
    policy: &mut Option<Policy<'a>>,
    company_id: Uuid,
    line: &StockLine,
    on_hand: Decimal,
    context: &str,
) -> Result<Option<(Uuid, Value)>> {
    let after = on_hand + line.qty;
    if !after.is_sign_negative() || after.is_zero() {
        return Ok(None);
    }
    // An unknown item or warehouse is left to the ledger to refuse.
    let named: Option<(String, String, String)> = sqlx::query_as(
        "SELECT i.code, i.uom, w.code FROM item i, warehouse w WHERE i.id = $1 AND w.id = $2",
    )
    .bind(line.item_id)
    .bind(line.warehouse_id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let Some((item_code, uom, warehouse_code)) = named else {
        return Ok(None);
    };
    let policy = match policy {
        Some(policy) => policy,
        None => policy.insert(ctx.policy(NEGATIVE, company_id).await?),
    };
    let shown = |qty: Decimal| qty.normalize().to_string();
    let facts = json!({
        "on_hand": shown(on_hand),
        "issuing": shown(-line.qty),
        "short_by": shown(-after),
        "item_code": item_code,
        "warehouse_code": warehouse_code,
        "context": context,
    });
    let ruling = policy.ruling(facts.clone())?;
    match (ruling.decision, ruling.rule_id) {
        (Decision::Allowed, Some(rule_id)) => Ok(Some((rule_id, facts))),
        (Decision::Refused(refusal), _) => {
            Err(Error::validation(refusal.message.unwrap_or_else(|| {
                format!(
                    "only {} {uom} of item {item_code} are on hand in warehouse {warehouse_code}",
                    shown(on_hand)
                )
            })))
        }
        // Allowed with no rule to name: the ledger could not verify it.
        (Decision::Allowed, None) => Err(Error::internal(
            "stock below zero must be allowed by a policy rule",
        )),
    }
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

/// The stock value the document's posting brought in and took out, both
/// as positive amounts: `(in, out)`. `None` for a document that has not
/// posted. This is what a document shows as its value once it is posted.
pub async fn value_moved(
    conn: &mut PgConnection,
    document_id: Uuid,
) -> Result<Option<(Decimal, Decimal)>> {
    let posting: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM posting WHERE document_id = $1 AND reverses_id IS NULL
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(document_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(posting) = posting else {
        return Ok(None);
    };
    let entries = entries_of(conn, posting).await?;
    let sum = |incoming: bool| -> Decimal {
        entries
            .iter()
            .filter(|e| e.value.is_sign_positive() == incoming && !e.value.is_zero())
            .map(|e| e.value.abs())
            .sum()
    };
    Ok(Some((sum(true), sum(false))))
}

struct StockLedger;

/// One stock entry as a document shows it under its posting. Quantities
/// and values are decimals as text. Codes and names are absent for a
/// reader who may not read items or warehouses.
#[derive(JsonSchema)]
#[allow(dead_code)] // describes the rows `entries` builds in SQL
struct StockEntryRow {
    /// Entry id.
    id: Option<Uuid>,
    /// Position within the posting, from 1.
    line_no: Option<i32>,
    /// The document line the entry comes from.
    source_line_id: Option<Uuid>,
    item_id: Option<Uuid>,
    /// Item code.
    item_code: Option<String>,
    /// Item name.
    item_name: Option<String>,
    /// Unit of measure.
    uom: Option<String>,
    warehouse_id: Option<Uuid>,
    /// Warehouse code.
    warehouse_code: Option<String>,
    /// Quantity in (positive) or out (negative).
    qty: Option<String>,
    /// Value in or out, in the accounting currency.
    value: Option<String>,
    /// Cost of one unit.
    unit_cost: Option<String>,
    /// Quantity on hand after the entry.
    qty_after: Option<String>,
    /// Value on hand after the entry.
    value_after: Option<String>,
    /// Value that went to stock variance.
    variance: Option<String>,
    /// The lots or serial numbers moved, for a tracked item: a list of
    /// `{ lot, qty }`, quantities as decimal text, the way the entry goes.
    lots: Option<Vec<Value>>,
}

#[async_trait]
impl Ledger for StockLedger {
    fn name(&self) -> &'static str {
        "stock"
    }

    fn label(&self) -> String {
        "Stock".into()
    }

    fn entry_schema(&self) -> Value {
        erpcraft_kernel::document::row_schema::<StockEntryRow>()
    }

    fn entry_tables(&self) -> &'static [&'static str] {
        &["stock_entry"]
    }

    /// Take back each original entry, last first, with its variance. The
    /// ledger refuses if the stock has since moved on: a receipt cannot be
    /// reversed once what it brought in has been issued. Taking back a
    /// receipt that arrived on a level below zero may leave the level as
    /// far below zero as it stood before, not further.
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
            let id = new_id();
            // The trigger replaces qty and value with the exact opposite of
            // the original's.
            sqlx::query(
                "INSERT INTO stock_entry (id, company_id, posting_id, line_no, source_line_id, item_id,
                    warehouse_id, posting_date, qty, value, unit_cost, qty_after, value_after, reverses_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 0, 0, 0, $11)",
            )
            .bind(id)
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
            // And exactly its lots.
            sqlx::query(
                "INSERT INTO stock_entry_lot (entry_id, lot_id, qty)
                 SELECT $1, lot_id, -qty FROM stock_entry_lot WHERE entry_id = $2
                 ORDER BY lot_id",
            )
            .bind(id)
            .bind(entry.id)
            .execute(&mut *conn)
            .await?;
        }
        // A receipt taken back may leave less on hand than is reserved.
        let back: Vec<(Uuid, Uuid)> = originals
            .iter()
            .filter(|e| e.qty.is_sign_positive())
            .map(|e| (e.item_id, e.warehouse_id))
            .collect();
        if !back.is_empty() {
            cap_reservations(conn, original.company_id, back).await?;
        }
        Ok(())
    }

    /// The movements of the posting with their items and warehouses.
    /// Names come only with the right to read what they name, and what is
    /// withheld on stock entries, such as their value, is withheld here.
    async fn entries(
        &self,
        ctx: &mut Ctx<'_>,
        posting: &Posting,
    ) -> Result<Option<Vec<serde_json::Value>>> {
        const READ: &str = "stock_entry:read";
        let actor = ctx.actor;
        if !actor.can(READ, posting.company_id) {
            return Ok(None);
        }
        let named = |permission: &str, allowed: bool| {
            allowed
                && !["code", "name"]
                    .iter()
                    .any(|field| actor.may_withhold(permission, field))
        };
        let items = named("item:read", actor.can_any("item:read"));
        let warehouses = named(
            "warehouse:read",
            actor.can("warehouse:read", posting.company_id),
        );
        let mut rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT jsonb_build_object(
                'id', e.id, 'line_no', e.line_no, 'source_line_id', e.source_line_id,
                'item_id', e.item_id,
                'item_code', CASE WHEN $2 THEN i.code END,
                'item_name', CASE WHEN $2 THEN i.name END,
                'uom', i.uom,
                'warehouse_id', e.warehouse_id,
                'warehouse_code', CASE WHEN $3 THEN w.code END,
                'qty', e.qty::text, 'value', e.value::text, 'unit_cost', e.unit_cost::text,
                'qty_after', e.qty_after::text, 'value_after', e.value_after::text,
                'variance', e.variance::text,
                'lots', (SELECT jsonb_agg(jsonb_build_object('lot', sl.code, 'qty', el.qty::text)
                                          ORDER BY sl.code)
                         FROM stock_entry_lot el JOIN stock_lot sl ON sl.id = el.lot_id
                         WHERE el.entry_id = e.id))
             FROM stock_entry e
             JOIN item i ON i.id = e.item_id
             JOIN warehouse w ON w.id = e.warehouse_id
             WHERE e.posting_id = $1 ORDER BY e.line_no",
        )
        .bind(posting.id)
        .bind(items)
        .bind(warehouses)
        .fetch_all(&mut *ctx.conn)
        .await?;
        erpcraft_kernel::document::withhold(actor, READ, &mut rows);
        Ok(Some(rows))
    }
}

// ------------------------------------------------------------------ on hand

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OnHandInput {
    /// The company to report on.
    company_id: Uuid,
    /// Only this item.
    item_id: Option<Uuid>,
    /// Only this warehouse and the locations inside it.
    warehouse_id: Option<Uuid>,
    /// Only the warehouses of this site.
    site_id: Option<Uuid>,
    /// Only warehouses that hold goods in transit (true), or none of them
    /// (false).
    transit: Option<bool>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct OnHandRow {
    item_id: Uuid,
    /// Absent for a caller who may not read items.
    item_code: Option<String>,
    item_name: Option<String>,
    uom: String,
    warehouse_id: Uuid,
    /// Absent for a caller who may not read warehouses.
    warehouse_code: Option<String>,
    site_id: Option<Uuid>,
    /// Whether the warehouse holds goods in transit.
    transit: bool,
    qty: Decimal,
    /// Absent for a caller from whom the value of stock entries is withheld.
    value: Option<Decimal>,
    /// Value divided by quantity: what the next issue goes out at. For a
    /// quantity below zero, what the missing goods are carried at per unit.
    average_cost: Option<Decimal>,
}

#[derive(Serialize, JsonSchema)]
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
             in its accounting currency. Items with nothing on hand are left out. An item that \
             the company's policy let go below zero is listed with a negative quantity and a \
             negative value: the estimate its missing goods left stock at, so that the total \
             still equals the inventory accounts. Values are left out for a caller from whom \
             the value of stock entries is withheld. warehouse_id takes the locations inside \
             the warehouse with it; site_id gives one site's warehouses; transit true gives \
             the goods on their way between sites.",
        )
        .report("Stock on hand")
        .permission("stock_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: OnHandInput) -> Result<OnHandOutput> {
        ctx.require("stock_entry:read", input.company_id)?;
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        // This report is built from stock entries, so what is withheld on
        // them is withheld here, and names come only with the right to
        // read what they name.
        let actor = ctx.actor;
        let values = !["value", "unit_cost", "value_after", "variance"]
            .iter()
            .any(|field| actor.may_withhold("stock_entry:read", field));
        let items = actor.can_any("item:read");
        let warehouses = actor.can("warehouse:read", input.company_id);
        let rows: Vec<OnHandRow> = sqlx::query_as(
            "SELECT l.item_id,
                    CASE WHEN $5 THEN i.code END AS item_code,
                    CASE WHEN $5 THEN i.name END AS item_name, i.uom,
                    l.warehouse_id, CASE WHEN $6 THEN w.code END AS warehouse_code,
                    w.site_id, w.transit, l.qty,
                    CASE WHEN $4 THEN l.value END AS value,
                    CASE WHEN $4 THEN round(l.value / l.qty, 6) END AS average_cost
             FROM stock_level l
             JOIN item i ON i.id = l.item_id
             JOIN warehouse w ON w.id = l.warehouse_id
             WHERE l.company_id = $1 AND l.qty <> 0
               AND ($2::uuid IS NULL OR l.item_id = $2)
               AND ($3::uuid IS NULL OR l.warehouse_id IN (
                   WITH RECURSIVE down AS (
                       SELECT id FROM warehouse WHERE id = $3
                       UNION
                       SELECT c.id FROM warehouse c JOIN down ON c.parent_id = down.id
                   )
                   SELECT id FROM down))
               AND ($7::uuid IS NULL OR w.site_id = $7)
               AND ($8::bool IS NULL OR w.transit = $8)
             ORDER BY i.code, w.code",
        )
        .bind(input.company_id)
        .bind(input.item_id)
        .bind(input.warehouse_id)
        .bind(values)
        .bind(items)
        .bind(warehouses)
        .bind(input.site_id)
        .bind(input.transit)
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReservationsInput {
    /// The company whose reservations are listed.
    company_id: Uuid,
    /// Only this item.
    item_id: Option<Uuid>,
    /// Only this warehouse.
    warehouse_id: Option<Uuid>,
    /// Only those of this order.
    document_id: Option<Uuid>,
    /// At most this many rows: 50 by default, 500 at most.
    limit: Option<i64>,
    /// How many rows to skip first, for the next page.
    offset: Option<i64>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct ReservationRow {
    item_id: Uuid,
    warehouse_id: Uuid,
    qty: Decimal,
    /// What kind of order it is set aside for.
    doc_type: String,
    /// The order, its number and its line: only for a caller who may read
    /// that kind of order in the company.
    document_id: Option<Uuid>,
    number: Option<String>,
    line_id: Option<Uuid>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, JsonSchema)]
struct ReservationsOutput {
    company_id: Uuid,
    /// How many reservations match, whatever the page.
    total: i64,
    rows: Vec<ReservationRow>,
}

struct Reservations;

#[async_trait]
impl Command for Reservations {
    type Input = ReservationsInput;
    type Output = ReservationsOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "stock.reservations",
            "Stock on hand set aside for lines of open sales orders and released production \
             orders, per order line: what each holds now. An order reserves what free stock \
             covers when it is approved or released, holds less as its goods leave, and lets \
             go when it is done, cancelled or closed; when other movements leave less on hand \
             than is reserved, the latest reservations are cut back first. The order and its \
             number are shown only to someone who may read that kind of order. Paged: \
             limit (50, at most 500) and offset.",
        )
        .report("Reservations")
        .permission("stock_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReservationsInput) -> Result<ReservationsOutput> {
        ctx.require("stock_entry:read", input.company_id)?;
        const FILTER: &str = "WHERE r.company_id = $1 AND ($2::uuid IS NULL OR r.item_id = $2)
               AND ($3::uuid IS NULL OR r.warehouse_id = $3)
               AND ($4::uuid IS NULL OR r.document_id = $4)";
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM stock_reservation r {FILTER}"
        ))
        .bind(input.company_id)
        .bind(input.item_id)
        .bind(input.warehouse_id)
        .bind(input.document_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let mut rows: Vec<ReservationRow> = sqlx::query_as(&format!(
            "SELECT r.item_id, r.warehouse_id, trim_scale(r.qty) AS qty, d.doc_type,
                    r.document_id, d.number, r.line_id, r.updated_at
             FROM stock_reservation r JOIN document d ON d.id = r.document_id
             {FILTER}
             ORDER BY r.item_id, r.warehouse_id, r.created_at, r.id
             LIMIT $5 OFFSET $6"
        ))
        .bind(input.company_id)
        .bind(input.item_id)
        .bind(input.warehouse_id)
        .bind(input.document_id)
        .bind(input.limit.unwrap_or(50).clamp(1, 500))
        .bind(input.offset.unwrap_or(0).max(0))
        .fetch_all(&mut *ctx.conn)
        .await?;
        // Which order holds the stock is the order's to tell.
        for row in &mut rows {
            if !ctx
                .actor
                .can(&format!("{}:read", row.doc_type), input.company_id)
            {
                row.document_id = None;
                row.number = None;
                row.line_id = None;
            }
        }
        Ok(ReservationsOutput {
            company_id: input.company_id,
            total,
            rows,
        })
    }
}
