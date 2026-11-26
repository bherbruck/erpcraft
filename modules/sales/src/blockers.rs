//! For `document.blockers`: what keeps an order's goods from leaving, line
//! by line, before a delivery is tried (which stops at its first line).

use erpcraft_kernel::blockers::{Blocker, BlockerCheck, Resolution};
use erpcraft_kernel::document::Document;
use erpcraft_kernel::{Ctx, Decimal, Decision, Result, async_trait};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::{SALES_ORDER, order_lines, policy, trim};

pub(crate) struct OrderCheck;

#[async_trait]
impl BlockerCheck for OrderCheck {
    async fn check(&self, ctx: &mut Ctx<'_>, doc: &Document, step: &str) -> Result<Vec<Blocker>> {
        if doc.doc_type != SALES_ORDER || step != "sales_order.deliver" {
            return Ok(vec![]);
        }
        let negative = ctx.policy(erpcraft_stock::NEGATIVE, doc.company_id).await?;
        let elsewhere = ctx
            .policy(policy::RESERVED_ELSEWHERE, doc.company_id)
            .await?;
        let mut blockers = Vec::new();
        // What earlier lines take of the same item and warehouse.
        let mut taking: BTreeMap<(Uuid, Uuid), Decimal> = BTreeMap::new();
        for line in order_lines(&mut *ctx.conn, doc.id).await? {
            let (Some(item_id), Some(warehouse_id)) = (line.item_id, line.warehouse_id) else {
                continue;
            };
            // In stock units, as stock and reservations are.
            let remaining = (line.qty - line.qty_delivered) * line.factor;
            if remaining <= Decimal::ZERO {
                continue;
            }
            let taken = taking.entry((item_id, warehouse_id)).or_default();
            let before = *taken;
            *taken += remaining;
            let (on_hand, _) =
                erpcraft_stock::on_hand(&mut *ctx.conn, item_id, warehouse_id).await?;
            let others =
                erpcraft_stock::reserved_for_others(&mut *ctx.conn, item_id, warehouse_id, doc.id)
                    .await?;
            let left = on_hand - before;
            let free = (left - others).max(Decimal::ZERO);
            if remaining <= free {
                continue;
            }
            let (item_code, warehouse_code): (String, String) = sqlx::query_as(
                "SELECT i.code, w.code FROM item i, warehouse w WHERE i.id = $1 AND w.id = $2",
            )
            .bind(item_id)
            .bind(warehouse_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
            let n = line.line_no as usize;
            let figures = json!({
                "item_id": item_id, "item_code": item_code,
                "warehouse_id": warehouse_id, "warehouse_code": warehouse_code,
                "to_deliver": trim(remaining), "on_hand": trim(on_hand),
                "reserved_for_others": trim(others), "free": trim(free),
            });
            if remaining > left.max(Decimal::ZERO) {
                let short = remaining - left.max(Decimal::ZERO);
                let facts = json!({
                    "on_hand": trim(left), "issuing": trim(remaining), "short_by": trim(short),
                    "item_code": item_code, "warehouse_code": warehouse_code,
                    "context": "sales_delivery",
                });
                if negative.evaluate(facts)? == Decision::Allowed {
                    continue;
                }
                let mut figures = figures;
                figures["short"] = json!(trim(short));
                let mut blocker = Blocker::new(
                    "stock_short",
                    format!(
                        "line {n}: {} of item {item_code} to deliver from {warehouse_code}, {} on \
                         hand there: {} short",
                        trim(remaining),
                        trim(left.max(Decimal::ZERO)),
                        trim(short)
                    ),
                )
                .line("lines", n)
                .covers_refusal();
                let stocked = stocked_elsewhere(ctx, doc.company_id, item_id, warehouse_id).await?;
                if !stocked.is_empty() {
                    figures["elsewhere"] = json!(stocked);
                    blocker = blocker.resolve(
                        Resolution::new(
                            "document.create",
                            "move it from a warehouse that has it (a stock transfer)",
                        )
                        .input(
                            json!({ "doc_type": "stock_transfer", "company_id": doc.company_id }),
                        ),
                    );
                }
                blocker = blocker
                    .resolve(
                        Resolution::new("document.create", "buy it (a purchase order)").input(
                            json!({ "doc_type": "purchase_order", "company_id": doc.company_id }),
                        ),
                    )
                    .resolve(
                        Resolution::new(
                            "planning.suggestions",
                            "see what to buy or make for open demand",
                        )
                        .input(json!({ "company_id": doc.company_id })),
                    );
                blockers.push(blocker.facts(figures));
            } else {
                let facts = policy::reserved_elsewhere(remaining, free, &item_code);
                if elsewhere.evaluate(facts)? == Decision::Allowed {
                    continue;
                }
                let holders = holders(ctx, item_id, warehouse_id, doc.id).await?;
                let mut figures = figures;
                figures["held_by"] = json!(holders);
                blockers.push(
                    Blocker::new(
                        "reserved_elsewhere",
                        format!(
                            "line {n}: {} of item {item_code} is on hand in {warehouse_code}, but {} \
                             of it is reserved for other orders; {} is free",
                            trim(on_hand),
                            trim(others),
                            trim(free)
                        ),
                    )
                    .line("lines", n)
                    .covers_refusal()
                    .facts(figures)
                    .resolve(
                        Resolution::new("stock.reservations", "see which orders hold it")
                            .input(json!({ "company_id": doc.company_id, "item_id": item_id,
                                           "warehouse_id": warehouse_id })),
                    )
                    .resolve(
                        Resolution::new(
                            "entity.create",
                            "let deliveries take stock other orders reserved (a policy rule)",
                        )
                        .input(json!({ "entity": "policy_rule", "data": {
                            "company_id": doc.company_id, "point": policy::RESERVED_ELSEWHERE,
                            "effect": "allow" } })),
                    ),
                );
            }
        }
        Ok(blockers)
    }
}

/// Other warehouses of the company with the item on hand.
async fn stocked_elsewhere(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    item_id: Uuid,
    warehouse_id: Uuid,
) -> Result<Vec<Value>> {
    let rows: Vec<(Uuid, String, Decimal)> = sqlx::query_as(
        "SELECT w.id, w.code, l.qty FROM stock_level l JOIN warehouse w ON w.id = l.warehouse_id
         WHERE l.company_id = $1 AND l.item_id = $2 AND l.warehouse_id <> $3 AND l.qty > 0
         ORDER BY l.qty DESC LIMIT 5",
    )
    .bind(company_id)
    .bind(item_id)
    .bind(warehouse_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, code, qty)| json!({ "warehouse_id": id, "warehouse_code": code, "on_hand": trim(qty) }))
        .collect())
}

/// The other documents holding the item in the warehouse, largest first.
async fn holders(
    ctx: &mut Ctx<'_>,
    item_id: Uuid,
    warehouse_id: Uuid,
    document_id: Uuid,
) -> Result<Vec<Value>> {
    let rows: Vec<(String, Uuid, Option<String>, Decimal)> = sqlx::query_as(
        "SELECT d.doc_type, d.id, d.number, sum(r.qty) FROM stock_reservation r
         JOIN document d ON d.id = r.document_id
         WHERE r.item_id = $1 AND r.warehouse_id = $2 AND r.document_id <> $3
         GROUP BY d.doc_type, d.id, d.number ORDER BY sum(r.qty) DESC LIMIT 10",
    )
    .bind(item_id)
    .bind(warehouse_id)
    .bind(document_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(doc_type, id, number, qty)| {
            json!({ "doc_type": doc_type, "id": id, "number": number, "qty": trim(qty) })
        })
        .collect())
}
