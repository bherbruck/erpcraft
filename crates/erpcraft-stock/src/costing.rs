//! How an item is valued in a company: at moving average (the default) or
//! at a standard cost. The stock ledger's trigger values standard items at
//! their standard (stock migration 14); here are the records that say
//! which method and which standard, and the revaluation entries that move
//! stock on hand to a new standard.

use erpcraft_kernel::{
    Decimal, EntityMeta, FieldMeta, FieldType, PgConnection, Posting, Result, new_id,
};
use uuid::Uuid;

use crate::{ENTRY_COLUMNS, StockEntry, lock_levels};

/// The two ways an item is valued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Average,
    Standard,
}

/// How `item_id` is valued in `company_id`: its own setting, else the
/// company's default, else moving average.
pub async fn method(conn: &mut PgConnection, company_id: Uuid, item_id: Uuid) -> Result<Method> {
    let method: String = sqlx::query_scalar("SELECT stock_costing_method($1, $2)")
        .bind(company_id)
        .bind(item_id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(if method == "standard" {
        Method::Standard
    } else {
        Method::Average
    })
}

/// An item's standard cost in a company, and its parts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Standard {
    pub unit_cost: Decimal,
    pub material_cost: Decimal,
    pub labour_cost: Decimal,
    pub overhead_cost: Decimal,
}

/// The standard of `item_id` in `company_id`, if it has one.
pub async fn standard(
    conn: &mut PgConnection,
    company_id: Uuid,
    item_id: Uuid,
) -> Result<Option<Standard>> {
    Ok(sqlx::query_as(
        "SELECT unit_cost, material_cost, labour_cost, overhead_cost FROM item_standard
         WHERE company_id = $1 AND item_id = $2",
    )
    .bind(company_id)
    .bind(item_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// For the rest of the transaction, standards may be written and items
/// switched to standard with stock on hand: what activating a cost version
/// does, which revalues that stock in the same transaction.
pub async fn begin_activation(conn: &mut PgConnection) -> Result<()> {
    sqlx::query("SELECT set_config('erpcraft.cost_activation', 'on', true)")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Write a standard (within [`begin_activation`]).
pub async fn set_standard(
    conn: &mut PgConnection,
    company_id: Uuid,
    item_id: Uuid,
    standard: &Standard,
    version_id: Uuid,
    since: chrono::NaiveDate,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO item_standard (company_id, item_id, unit_cost, material_cost, labour_cost,
             overhead_cost, version_id, since)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (company_id, item_id) DO UPDATE SET unit_cost = EXCLUDED.unit_cost,
             material_cost = EXCLUDED.material_cost, labour_cost = EXCLUDED.labour_cost,
             overhead_cost = EXCLUDED.overhead_cost, version_id = EXCLUDED.version_id,
             since = EXCLUDED.since",
    )
    .bind(company_id)
    .bind(item_id)
    .bind(standard.unit_cost)
    .bind(standard.material_cost)
    .bind(standard.labour_cost)
    .bind(standard.overhead_cost)
    .bind(version_id)
    .bind(since)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Switch an item to standard cost in a company (within
/// [`begin_activation`] when it has stock).
pub async fn set_method(
    conn: &mut PgConnection,
    company_id: Uuid,
    item_id: Uuid,
    method: Method,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO item_costing (id, company_id, item_id, method) VALUES ($1, $2, $3, $4)
         ON CONFLICT (company_id, item_id) DO UPDATE SET method = EXCLUDED.method",
    )
    .bind(new_id())
    .bind(company_id)
    .bind(item_id)
    .bind(match method {
        Method::Average => "average",
        Method::Standard => "standard",
    })
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A level that a new standard revalues: how much is there, what it is
/// worth, and what it is worth at `unit_cost`.
#[derive(Debug, Clone)]
pub struct Revaluation {
    pub item_id: Uuid,
    pub warehouse_id: Uuid,
    pub qty: Decimal,
    pub value: Decimal,
    pub new_value: Decimal,
}

/// Every level of `item_id` in `company_id` with stock, locked, and what it
/// would be worth at `unit_cost` (rounded to `decimals`).
pub async fn levels_to_revalue(
    conn: &mut PgConnection,
    company_id: Uuid,
    item_id: Uuid,
    unit_cost: Decimal,
    decimals: u32,
) -> Result<Vec<Revaluation>> {
    let pairs: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT item_id, warehouse_id FROM stock_level
         WHERE company_id = $1 AND item_id = $2 AND qty <> 0",
    )
    .bind(company_id)
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await?;
    if pairs.is_empty() {
        return Ok(vec![]);
    }
    lock_levels(conn, company_id, pairs).await?;
    let rows: Vec<(Uuid, Decimal, Decimal)> = sqlx::query_as(
        "SELECT warehouse_id, qty, value FROM stock_level
         WHERE company_id = $1 AND item_id = $2 AND qty <> 0 ORDER BY warehouse_id",
    )
    .bind(company_id)
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(warehouse_id, qty, value)| Revaluation {
            item_id,
            warehouse_id,
            qty,
            value,
            new_value: (qty * unit_cost).round_dp(decimals),
        })
        .collect())
}

/// Record the revaluations under `posting`: an entry per level, no
/// quantity, the difference in value. The caller posts the same amounts to
/// the general ledger. Levels already at their new value are left out.
pub async fn revalue(
    conn: &mut PgConnection,
    posting: &Posting,
    revaluations: &[Revaluation],
) -> Result<Vec<StockEntry>> {
    let moving: Vec<&Revaluation> = revaluations
        .iter()
        .filter(|r| r.new_value != r.value)
        .collect();
    if moving.is_empty() {
        return Ok(vec![]);
    }
    let numbered: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(line_no), 0) FROM stock_entry WHERE posting_id = $1",
    )
    .bind(posting.id)
    .fetch_one(&mut *conn)
    .await?;
    let mut entries = vec![];
    for (n, r) in moving.iter().enumerate() {
        let entry: StockEntry = sqlx::query_as(&format!(
            "INSERT INTO stock_entry (id, company_id, posting_id, line_no, item_id, warehouse_id,
                posting_date, qty, value, unit_cost, qty_after, value_after, revaluation)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, 0, 0, 0, true)
             RETURNING {ENTRY_COLUMNS}"
        ))
        .bind(new_id())
        .bind(posting.company_id)
        .bind(posting.id)
        .bind(numbered + n as i32 + 1)
        .bind(r.item_id)
        .bind(r.warehouse_id)
        .bind(posting.posting_date)
        .bind(r.new_value - r.value)
        .fetch_one(&mut *conn)
        .await?;
        entries.push(entry);
    }
    Ok(entries)
}

pub(crate) fn company_entity() -> EntityMeta {
    EntityMeta::new("stock_costing", "Costing")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new(
                "default_method",
                "Items are valued at",
                FieldType::options(&["average", "standard"]),
            )
            .required()
            .help(
                "Moving average, or a standard cost, for items that do not say otherwise. \
                 Standard only while the items it would change have no stock.",
            ),
        )
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["company_id"])
        .company_field("company_id")
}

pub(crate) fn item_entity() -> EntityMeta {
    EntityMeta::new("item_costing", "Item costing")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new("item_id", "Item", FieldType::link("item"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new(
                "method",
                "Valued at",
                FieldType::options(&["average", "standard"]),
            )
            .required()
            .help(
                "To standard only while the item has no stock in the company, else by a cost \
                 version that switches it; back to average at any time.",
            ),
        )
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["item_id"])
        .company_field("company_id")
}

pub(crate) fn standard_entity() -> EntityMeta {
    let cost = |name: &str, label: &str| {
        FieldMeta::new(name, label, FieldType::Decimal)
            .unit_price()
            .readonly()
    };
    EntityMeta::new("item_standard", "Standard cost")
        .field(FieldMeta::new("company_id", "Company", FieldType::link("company")).readonly())
        .field(FieldMeta::new("item_id", "Item", FieldType::link("item")).readonly())
        .field(cost("unit_cost", "Standard cost"))
        .field(cost("material_cost", "Material"))
        .field(cost("labour_cost", "Labour"))
        .field(cost("overhead_cost", "Overhead"))
        .field(
            FieldMeta::new("version_id", "Cost version", FieldType::Uuid)
                .readonly()
                .help("The cost version whose activation set it."),
        )
        .field(FieldMeta::new("since", "Since", FieldType::Date).readonly())
        .order_by(&["item_id"])
        .read_only()
        .company_field("company_id")
}
