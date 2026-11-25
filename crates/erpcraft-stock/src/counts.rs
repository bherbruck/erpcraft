//! What physical counts need of the stock ledger: holding the levels a
//! count in progress counts (and asking the company's policy before goods
//! move on them), the book quantity of a level or lot at a count's cut-off,
//! and ABC classes of items.

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Decision, Error, JsonSchema, PgConnection, PolicyPoint,
    Result, async_trait, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

/// The policy point asked when goods move on a level a count holds.
pub const COUNT_MOVEMENT: &str = "stock.count_movement";

pub(crate) fn policy() -> PolicyPoint {
    PolicyPoint::new(
        COUNT_MOVEMENT,
        "Asked for every movement of an item in a warehouse that a physical count in \
         progress is counting: may goods move while they are being counted? Unless \
         configured otherwise they may, and the count is taken at its cut-off: the \
         difference found is adjusted on top of what moved since. A company that freezes \
         its warehouse during a count refuses it, perhaps except for `facts.context == \
         'purchase_receipt'`.",
    )
    .text(
        "context",
        "What moves the goods: sales_delivery, purchase_receipt, production, transfer, ...",
    )
    .text("item_code", "Code of the item")
    .text("warehouse_code", "Code of the warehouse")
    .text("count", "Number of the count in progress")
    .refusal("these goods are being counted")
}

/// Hold the levels `pairs` (item, warehouse) for the count `document_id`.
pub async fn hold(
    conn: &mut PgConnection,
    company_id: Uuid,
    document_id: Uuid,
    pairs: &[(Uuid, Uuid)],
) -> Result<()> {
    let (items, warehouses): (Vec<Uuid>, Vec<Uuid>) = pairs.iter().copied().unzip();
    sqlx::query(
        "INSERT INTO stock_count_hold (item_id, warehouse_id, document_id, company_id)
         SELECT item_id, warehouse_id, $3, $4
         FROM unnest($1::uuid[], $2::uuid[]) AS p (item_id, warehouse_id)
         ON CONFLICT DO NOTHING",
    )
    .bind(&items)
    .bind(&warehouses)
    .bind(document_id)
    .bind(company_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Release every level the count `document_id` holds.
pub async fn release(conn: &mut PgConnection, document_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM stock_count_hold WHERE document_id = $1")
        .bind(document_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Ask [`COUNT_MOVEMENT`] for each level of `pairs` a count holds.
pub(crate) async fn ask(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    pairs: &[(Uuid, Uuid)],
    context: &str,
) -> Result<()> {
    let (items, warehouses): (Vec<Uuid>, Vec<Uuid>) = pairs.iter().copied().unzip();
    let held: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT DISTINCT i.code, w.code, COALESCE(d.number, d.id::text)
         FROM stock_count_hold h
         JOIN unnest($1::uuid[], $2::uuid[]) AS p (item_id, warehouse_id)
             ON p.item_id = h.item_id AND p.warehouse_id = h.warehouse_id
         JOIN item i ON i.id = h.item_id
         JOIN warehouse w ON w.id = h.warehouse_id
         JOIN document d ON d.id = h.document_id
         ORDER BY 1, 2, 3",
    )
    .bind(&items)
    .bind(&warehouses)
    .fetch_all(&mut *ctx.conn)
    .await?;
    if held.is_empty() {
        return Ok(());
    }
    let policy = ctx.policy(COUNT_MOVEMENT, company_id).await?;
    for (item_code, warehouse_code, count) in held {
        let facts = json!({ "context": context, "item_code": item_code,
                            "warehouse_code": warehouse_code, "count": count });
        if let Decision::Refused(refusal) = policy.evaluate(facts)? {
            return Err(Error::validation(refusal.message.unwrap_or_else(|| {
                format!("item {item_code} in warehouse {warehouse_code} is being counted ({count})")
            })));
        }
    }
    Ok(())
}

/// The ledger's order number now: the `seq` of the last entry recorded.
/// Taken under the locks of the levels a count covers, every entry of
/// those levels up to it is committed.
pub async fn cut_off(conn: &mut PgConnection) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM stock_entry")
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// What the book held of an item in a warehouse (of one lot, when given)
/// at the cut-off `seq`: every entry recorded up to it.
pub async fn book_at(
    conn: &mut PgConnection,
    item_id: Uuid,
    warehouse_id: Uuid,
    lot_id: Option<Uuid>,
    seq: i64,
) -> Result<Decimal> {
    let qty: Option<Decimal> = match lot_id {
        None => {
            sqlx::query_scalar(
                "SELECT sum(qty) FROM stock_entry
                 WHERE item_id = $1 AND warehouse_id = $2 AND COALESCE(seq, 0) <= $3",
            )
            .bind(item_id)
            .bind(warehouse_id)
            .bind(seq)
            .fetch_one(&mut *conn)
            .await?
        }
        Some(lot) => sqlx::query_scalar(
            "SELECT sum(el.qty) FROM stock_entry_lot el JOIN stock_entry e ON e.id = el.entry_id
                 WHERE el.lot_id = $1 AND e.warehouse_id = $2 AND COALESCE(e.seq, 0) <= $3",
        )
        .bind(lot)
        .bind(warehouse_id)
        .bind(seq)
        .fetch_one(&mut *conn)
        .await?,
    };
    Ok(qty.unwrap_or_default())
}

/// Net quantity moved on a level (or lot) after the cut-off `seq`.
pub async fn moved_since(
    conn: &mut PgConnection,
    item_id: Uuid,
    warehouse_id: Uuid,
    lot_id: Option<Uuid>,
    seq: i64,
) -> Result<Decimal> {
    let now = book_at(conn, item_id, warehouse_id, lot_id, i64::MAX).await?;
    Ok(now - book_at(conn, item_id, warehouse_id, lot_id, seq).await?)
}

// ------------------------------------------------------------- ABC

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Basis {
    /// The value on hand now.
    #[default]
    Value,
    /// The value issued (gone out of stock) over a period.
    Movement,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClassifyInput {
    /// The company whose items are classed.
    company_id: Uuid,
    /// `value` (the default): by value on hand; `movement`: by value issued
    /// between `from` and `to`.
    #[serde(default)]
    basis: Basis,
    /// For `movement`: the first day of the period.
    from: Option<NaiveDate>,
    /// For `movement`: the last day of the period.
    to: Option<NaiveDate>,
    /// Items are A while the running share stays within this, 0.80 by
    /// default.
    a_share: Option<Decimal>,
    /// And B within this, 0.95 by default; C after.
    b_share: Option<Decimal>,
}

#[derive(Serialize, JsonSchema, Clone)]
pub(crate) struct ClassRow {
    item_id: Uuid,
    item_code: String,
    class: String,
    amount: Decimal,
    cumulative_share: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ClassifyOutput {
    company_id: Uuid,
    /// How many items fell in each class.
    a: usize,
    b: usize,
    c: usize,
    rows: Vec<ClassRow>,
}

pub(crate) struct Classify;

#[async_trait]
impl Command for Classify {
    type Input = ClassifyInput;
    type Output = ClassifyOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock.abc_classify",
            "Class a company's items A, B or C by value on hand (basis value) or by the value \
             issued over a period (basis movement, from and to): ranked by that value, items \
             are A while the running share of the total stays within a_share (0.80), B within \
             b_share (0.95), C after; items with no value are C. Replaces the company's classes \
             (item_abc), which cycle counts and their schedule read. Takes the right to change \
             items' classes (item_abc:update) and to read stock entries.",
        )
        .permission("item_abc:update")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ClassifyInput) -> Result<ClassifyOutput> {
        ctx.require("item_abc:update", input.company_id)?;
        ctx.require("stock_entry:read", input.company_id)?;
        let a_share = input.a_share.unwrap_or(Decimal::new(80, 2));
        let b_share = input.b_share.unwrap_or(Decimal::new(95, 2));
        if !(Decimal::ZERO < a_share && a_share <= b_share && b_share <= Decimal::ONE) {
            return Err(Error::validation(
                "the shares must be above zero, a_share no more than b_share, both at most 1",
            ));
        }
        // Every item the company has stock of, or moved, with its amount.
        let amounts: Vec<(Uuid, String, Decimal)> = match input.basis {
            Basis::Value => {
                sqlx::query_as(
                    "SELECT i.id, i.code, COALESCE(sum(GREATEST(l.value, 0)), 0)
                     FROM item i JOIN stock_level l ON l.item_id = i.id
                     WHERE l.company_id = $1
                     GROUP BY i.id, i.code",
                )
                .bind(input.company_id)
                .fetch_all(&mut *ctx.conn)
                .await?
            }
            Basis::Movement => {
                let (Some(from), Some(to)) = (input.from, input.to) else {
                    return Err(Error::validation(
                        "a class by movement needs the period: from and to",
                    ));
                };
                sqlx::query_as(
                    "SELECT i.id, i.code,
                            COALESCE(sum(-e.value) FILTER (
                                WHERE e.qty < 0 AND e.reverses_id IS NULL
                                  AND e.posting_date BETWEEN $2 AND $3
                                  AND NOT EXISTS (SELECT 1 FROM stock_entry r
                                                  WHERE r.reverses_id = e.id)), 0)
                     FROM item i JOIN stock_entry e ON e.item_id = i.id
                     WHERE e.company_id = $1
                     GROUP BY i.id, i.code",
                )
                .bind(input.company_id)
                .bind(from)
                .bind(to)
                .fetch_all(&mut *ctx.conn)
                .await?
            }
        };
        let mut ranked = amounts;
        ranked.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        let total: Decimal = ranked.iter().map(|r| r.2.max(Decimal::ZERO)).sum();
        let mut running = Decimal::ZERO;
        let mut rows = vec![];
        for (item_id, item_code, amount) in ranked {
            let before = if total.is_zero() {
                Decimal::ONE
            } else {
                running / total
            };
            running += amount.max(Decimal::ZERO);
            let share = if total.is_zero() {
                Decimal::ONE
            } else {
                (running / total).round_dp(6)
            };
            // An item is in the class its share starts in: the first item
            // is A even when it alone is more than a_share.
            let class = if amount <= Decimal::ZERO {
                "C"
            } else if before < a_share {
                "A"
            } else if before < b_share {
                "B"
            } else {
                "C"
            };
            rows.push(ClassRow {
                item_id,
                item_code,
                class: class.into(),
                amount,
                cumulative_share: share,
            });
        }
        sqlx::query("DELETE FROM item_abc WHERE company_id = $1")
            .bind(input.company_id)
            .execute(&mut *ctx.conn)
            .await?;
        let basis = match input.basis {
            Basis::Value => "value",
            Basis::Movement => "movement",
        };
        sqlx::query(
            "INSERT INTO item_abc (id, company_id, item_id, class, basis, amount, cumulative_share)
             SELECT id, $1, item_id, class, $2, amount, share
             FROM unnest($7::uuid[], $3::uuid[], $4::text[], $5::numeric[], $6::numeric[])
                 AS r (id, item_id, class, amount, share)",
        )
        .bind(input.company_id)
        .bind(basis)
        .bind(rows.iter().map(|r| r.item_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|r| r.class.clone()).collect::<Vec<_>>())
        .bind(rows.iter().map(|r| r.amount).collect::<Vec<_>>())
        .bind(rows.iter().map(|r| r.cumulative_share).collect::<Vec<_>>())
        .bind(rows.iter().map(|_| new_id()).collect::<Vec<_>>())
        .execute(&mut *ctx.conn)
        .await?;
        let count = |class: &str| rows.iter().filter(|r| r.class == class).count();
        Ok(ClassifyOutput {
            company_id: input.company_id,
            a: count("A"),
            b: count("B"),
            c: count("C"),
            rows,
        })
    }
}
