//! Time-phased planning against the books: what `mrp` is given, read from
//! the company's rules, bills, stock and orders, and the `planning.mrp`
//! command that shows the plan.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, Utc};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::mrp::{
    self, Bill, Demand, Exception, Input, Node, Params, Peg, Plan, PlannedOrder, Receipt,
    ReceiptKind, Source,
};

/// The rights a plan is read with: what availability takes, and the
/// rules and bills.
pub const RIGHTS: [&str; 5] = [
    "stock_entry:read",
    "sales_order:read",
    "purchase_order:read",
    "production_order:read",
    "reorder_rule:read",
];

#[derive(sqlx::FromRow, Clone)]
struct RuleRow {
    item_id: Uuid,
    warehouse_id: Uuid,
    method: String,
    multiple: Option<Decimal>,
    vendor_id: Option<Uuid>,
    procure: Option<String>,
    safety_stock: Decimal,
    min_order_qty: Option<Decimal>,
    lead_time_days: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct BillRow {
    id: Uuid,
    item_id: Uuid,
    output: Decimal,
    lead_time_days: i32,
}

#[derive(sqlx::FromRow)]
struct VendorRow {
    item_id: Uuid,
    vendor_id: Uuid,
    lead_time_days: i32,
    min_order_qty: Option<Decimal>,
}

/// Everything planned by MRP and what it is given.
pub struct Gathered {
    pub input: Input,
    /// The bill each item is made by, and its lead time.
    pub bills_of: BTreeMap<Uuid, (Uuid, i32)>,
}

/// The nodes MRP plans: those whose rule says `mrp`, and every component
/// node their bills reach at the same warehouse.
pub async fn gather(ctx: &mut Ctx<'_>, company: Uuid, as_of: NaiveDate) -> Result<Gathered> {
    for right in RIGHTS {
        ctx.require(right, company)?;
    }
    collect(ctx, company, as_of).await
}

async fn collect(ctx: &mut Ctx<'_>, company: Uuid, as_of: NaiveDate) -> Result<Gathered> {
    let conn = &mut *ctx.conn;
    let rules: Vec<RuleRow> = sqlx::query_as(
        "SELECT item_id, warehouse_id, method, multiple, vendor_id, procure, safety_stock,
                min_order_qty, lead_time_days
         FROM reorder_rule WHERE company_id = $1",
    )
    .bind(company)
    .fetch_all(&mut *conn)
    .await?;
    let bill_rows: Vec<BillRow> = sqlx::query_as(
        "SELECT id, item_id, output_qty * output_factor AS output, lead_time_days
         FROM bom WHERE active ORDER BY code, id",
    )
    .fetch_all(&mut *conn)
    .await?;
    let lines: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
        "SELECT l.bom_id, l.item_id, sum(l.qty * l.factor) FROM bom_line l
         JOIN bom b ON b.id = l.bom_id WHERE b.active
         GROUP BY l.bom_id, l.item_id ORDER BY l.bom_id, l.item_id",
    )
    .fetch_all(&mut *conn)
    .await?;
    let vendors: Vec<VendorRow> = sqlx::query_as(
        "SELECT item_id, vendor_id, lead_time_days, min_order_qty FROM item_vendor
         WHERE company_id = $1 ORDER BY created_at, id",
    )
    .bind(company)
    .fetch_all(&mut *conn)
    .await?;

    // Each made item by its active bill, the first by code when it has
    // several (as the reorder drafts take it).
    let mut bills = BTreeMap::new();
    let mut bills_of: BTreeMap<Uuid, (Uuid, i32)> = BTreeMap::new();
    for b in &bill_rows {
        bills.insert(
            b.id,
            Bill {
                product: b.item_id,
                output: b.output,
                lines: lines
                    .iter()
                    .filter(|(bom, _, _)| *bom == b.id)
                    .map(|(_, item, qty)| (*item, *qty))
                    .collect(),
            },
        );
        bills_of
            .entry(b.item_id)
            .or_insert((b.id, b.lead_time_days));
    }

    let params_of = |item: Uuid, rule: Option<&RuleRow>| -> Params {
        let bill = bills_of.get(&item);
        let make = match rule.and_then(|r| r.procure.as_deref()) {
            Some("make") => true,
            Some(_) => false,
            None => rule.and_then(|r| r.vendor_id).is_none() && bill.is_some(),
        };
        let vendor = rule.and_then(|r| r.vendor_id).or_else(|| {
            vendors
                .iter()
                .find(|v| v.item_id == item)
                .map(|v| v.vendor_id)
        });
        let quote = vendors
            .iter()
            .find(|v| v.item_id == item && Some(v.vendor_id) == vendor);
        let (source, lead) = match (make, bill) {
            (true, Some((bom, lead))) => (Source::Make { bom: *bom }, *lead as i64),
            _ => (
                Source::Buy { vendor },
                quote.map_or(0, |q| q.lead_time_days as i64),
            ),
        };
        Params {
            source,
            lead_days: rule.and_then(|r| r.lead_time_days).map_or(lead, i64::from),
            safety_stock: rule.map_or(Decimal::ZERO, |r| r.safety_stock),
            min_order_qty: rule
                .and_then(|r| r.min_order_qty)
                .or_else(|| quote.and_then(|q| q.min_order_qty)),
            multiple: rule.and_then(|r| r.multiple),
        }
    };
    let mut params: BTreeMap<Node, Params> = BTreeMap::new();
    let mut todo: Vec<Node> = rules
        .iter()
        .filter(|r| r.method == "mrp")
        .map(|r| (r.item_id, r.warehouse_id))
        .collect();
    while let Some(node) = todo.pop() {
        if params.contains_key(&node) {
            continue;
        }
        let rule = rules
            .iter()
            .find(|r| r.item_id == node.0 && r.warehouse_id == node.1);
        let p = params_of(node.0, rule);
        if let Source::Make { bom } = &p.source
            && let Some(bill) = bills.get(bom)
        {
            for (component, _) in &bill.lines {
                todo.push((*component, node.1));
            }
        }
        params.insert(node, p);
    }

    let items: Vec<Uuid> = params
        .keys()
        .map(|n| n.0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let on_hand: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
        "SELECT l.item_id, l.warehouse_id, l.qty FROM stock_level l
         JOIN warehouse w ON w.id = l.warehouse_id
         WHERE l.company_id = $1 AND l.item_id = ANY($2) AND NOT w.transit AND l.qty > 0",
    )
    .bind(company)
    .bind(&items)
    .fetch_all(&mut *conn)
    .await?;
    let sales: Vec<(Uuid, Uuid, Uuid, Uuid, NaiveDate, Decimal)> = sqlx::query_as(
        "SELECT l.document_id, l.id, l.item_id, l.warehouse_id,
                COALESCE(o.requested_date, d.doc_date),
                GREATEST(l.qty - COALESCE(p.qty_delivered - p.qty_returned, 0), 0) * l.factor
         FROM sales_order_line l
         JOIN sales_order o ON o.document_id = l.document_id
         JOIN document d ON d.id = l.document_id
         LEFT JOIN sales_order_progress p ON p.line_id = l.id
         WHERE d.company_id = $1 AND d.status = 'open' AND l.item_id = ANY($2)
         ORDER BY d.doc_date, l.id",
    )
    .bind(company)
    .bind(&items)
    .fetch_all(&mut *conn)
    .await?;
    // Released orders need their components; drafts the bill's.
    let components: Vec<(Uuid, Uuid, Uuid, NaiveDate, Decimal)> = sqlx::query_as(
        "SELECT o.document_id, c.item_id, o.warehouse_id, d.doc_date, c.qty
         FROM production_order_component c
         JOIN production_order o ON o.document_id = c.document_id
         JOIN document d ON d.id = c.document_id
         WHERE d.company_id = $1 AND d.status = 'released' AND c.item_id = ANY($2)
         UNION ALL
         SELECT o.document_id, l.item_id, o.warehouse_id, d.doc_date,
                l.qty * l.factor * o.qty / (b.output_qty * b.output_factor)
         FROM production_order o
         JOIN document d ON d.id = o.document_id
         JOIN bom b ON b.id = o.bom_id
         JOIN bom_line l ON l.bom_id = b.id
         WHERE d.company_id = $1 AND d.status = 'draft' AND l.item_id = ANY($2)",
    )
    .bind(company)
    .bind(&items)
    .fetch_all(&mut *conn)
    .await?;
    let bought: Vec<(Uuid, Uuid, Uuid, NaiveDate, Decimal, String)> = sqlx::query_as(
        "SELECT l.document_id, l.item_id, l.warehouse_id,
                COALESCE(o.expected_date, d.doc_date + COALESCE(
                    (SELECT v.lead_time_days FROM item_vendor v
                     WHERE v.company_id = d.company_id AND v.item_id = l.item_id
                       AND v.vendor_id = o.vendor_id), 0)),
                CASE WHEN d.status = 'draft' THEN l.qty * l.factor
                     ELSE GREATEST(l.qty - COALESCE(p.qty_received - p.qty_returned, 0), 0)
                          * l.factor END,
                d.status
         FROM purchase_order_line l
         JOIN purchase_order o ON o.document_id = l.document_id
         JOIN document d ON d.id = l.document_id
         LEFT JOIN purchase_order_progress p ON p.line_id = l.id
         WHERE d.company_id = $1 AND d.status IN ('open', 'draft') AND l.item_id = ANY($2)",
    )
    .bind(company)
    .bind(&items)
    .fetch_all(&mut *conn)
    .await?;
    let made: Vec<(Uuid, Uuid, Uuid, NaiveDate, Decimal, String)> = sqlx::query_as(
        "SELECT o.document_id, b.item_id, o.warehouse_id,
                COALESCE(o.due_date, d.doc_date + b.lead_time_days), o.qty, d.status
         FROM production_order o
         JOIN bom b ON b.id = o.bom_id
         JOIN document d ON d.id = o.document_id
         WHERE d.company_id = $1 AND d.status IN ('released', 'draft') AND b.item_id = ANY($2)",
    )
    .bind(company)
    .bind(&items)
    .fetch_all(&mut *conn)
    .await?;

    let planned = |item: Uuid, warehouse: Uuid| params.contains_key(&(item, warehouse));
    let mut input = Input {
        as_of,
        on_hand: on_hand
            .into_iter()
            .filter(|(i, w, _)| planned(*i, *w))
            .map(|(i, w, q)| ((i, w), q))
            .collect(),
        params: params.clone(),
        bills,
        ..Input::default()
    };
    for (document_id, line_id, item, warehouse, date, qty) in sales {
        if planned(item, warehouse) && qty > Decimal::ZERO {
            input.demands.push(Demand {
                node: (item, warehouse),
                date,
                qty,
                peg: Peg::SalesOrder {
                    document_id,
                    line_id,
                },
            });
        }
    }
    for (document_id, item, warehouse, date, qty) in components {
        if planned(item, warehouse) && qty > Decimal::ZERO {
            input.demands.push(Demand {
                node: (item, warehouse),
                date,
                qty: qty.round_dp(6),
                peg: Peg::ProductionOrder { document_id },
            });
        }
    }
    for (rows, open, draft) in [
        (
            bought,
            ReceiptKind::PurchaseOrder,
            ReceiptKind::DraftPurchaseOrder,
        ),
        (
            made,
            ReceiptKind::ProductionOrder,
            ReceiptKind::DraftProductionOrder,
        ),
    ] {
        for (document_id, item, warehouse, date, qty, status) in rows {
            if planned(item, warehouse) && qty > Decimal::ZERO {
                input.receipts.push(Receipt {
                    node: (item, warehouse),
                    date,
                    qty,
                    kind: if status == "draft" {
                        draft.clone()
                    } else {
                        open.clone()
                    },
                    document_id,
                });
            }
        }
    }
    Ok(Gathered { input, bills_of })
}

/// The plan for a company on a day.
pub async fn run(ctx: &mut Ctx<'_>, company: Uuid, as_of: NaiveDate) -> Result<Plan> {
    let gathered = gather(ctx, company, as_of).await?;
    mrp::plan(&gathered.input).map_err(|cycle| {
        Error::validation(format!(
            "the bills of materials lead back to themselves through {} item(s): a product is \
             among its own components",
            cycle.len().saturating_sub(1)
        ))
    })
}

/// The nodes MRP plans, so that the reorder suggestions leave them out.
pub async fn nodes(ctx: &mut Ctx<'_>, company: Uuid) -> Result<BTreeSet<Node>> {
    let any_mrp: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM reorder_rule WHERE company_id = $1 AND method = 'mrp')",
    )
    .bind(company)
    .fetch_one(&mut *ctx.conn)
    .await?;
    if !any_mrp {
        return Ok(BTreeSet::new());
    }
    // Which nodes, not what they need: no right beyond the caller's own.
    let gathered = collect(ctx, company, Utc::now().date_naive()).await?;
    Ok(gathered.input.params.keys().copied().collect())
}

// ------------------------------------------------------------------ command

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MrpInput {
    /// The company planned.
    company_id: Uuid,
    /// The day planned from; today when omitted.
    as_of: Option<NaiveDate>,
    /// Only planned orders and exceptions of this item.
    item_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema)]
pub struct PlannedOut {
    #[serde(flatten)]
    pub order: PlannedOrder,
    pub item_code: Option<String>,
    pub warehouse_code: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct ExceptionOut {
    #[serde(flatten)]
    pub exception: Exception,
    pub item_code: Option<String>,
    /// The order's number.
    pub number: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct MrpOutput {
    pub company_id: Uuid,
    pub as_of: NaiveDate,
    pub planned_orders: Vec<PlannedOut>,
    pub exceptions: Vec<ExceptionOut>,
    /// Each item's low-level code.
    pub levels: BTreeMap<Uuid, u32>,
}

pub struct Mrp;

#[async_trait]
impl Command for Mrp {
    type Input = MrpInput;
    type Output = MrpOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "planning.mrp",
            "Time-phased planning, changing nothing: for every item and warehouse whose reorder \
             rule plans by MRP, and every component their bills reach, what to order or start \
             on which day so that each sales order line is met on its requested date: planned \
             orders with their quantity (in stock units), release and due dates and what each \
             serves (pegging), netted level by level against stock on hand and the orders due \
             in (open and draft purchase and production orders, at their expected and due \
             dates), with safety stock, minimum order quantities and lead times; and the \
             exceptions: an order to expedite or defer (to what date), to cancel, past due, \
             or a planned order whose release date has passed. Takes the rights to read \
             stock, sales, purchase and production orders and reorder rules.",
        )
        .report("Material requirements")
        .label("Material requirements")
        .permission("reorder_rule:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: MrpInput) -> Result<MrpOutput> {
        let as_of = input.as_of.unwrap_or_else(|| Utc::now().date_naive());
        let plan = run(ctx, input.company_id, as_of).await?;
        let item_ids: Vec<Uuid> = plan
            .planned_orders
            .iter()
            .map(|p| p.item_id)
            .chain(plan.exceptions.iter().map(|e| e.item_id))
            .collect();
        let codes: BTreeMap<Uuid, String> = if ctx.actor.can_any("item:read") {
            sqlx::query_as::<_, (Uuid, String)>("SELECT id, code FROM item WHERE id = ANY($1)")
                .bind(&item_ids)
                .fetch_all(&mut *ctx.conn)
                .await?
                .into_iter()
                .collect()
        } else {
            BTreeMap::new()
        };
        let warehouses: BTreeMap<Uuid, String> =
            if ctx.actor.can("warehouse:read", input.company_id) {
                sqlx::query_as::<_, (Uuid, String)>(
                    "SELECT id, code FROM warehouse WHERE company_id = $1",
                )
                .bind(input.company_id)
                .fetch_all(&mut *ctx.conn)
                .await?
                .into_iter()
                .collect()
            } else {
                BTreeMap::new()
            };
        let docs: Vec<Uuid> = plan
            .exceptions
            .iter()
            .filter_map(|e| e.document_id)
            .collect();
        let numbers: BTreeMap<Uuid, Option<String>> = sqlx::query_as::<_, (Uuid, Option<String>)>(
            "SELECT id, number FROM document WHERE id = ANY($1)",
        )
        .bind(&docs)
        .fetch_all(&mut *ctx.conn)
        .await?
        .into_iter()
        .collect();
        let wanted = |item: &Uuid| input.item_id.is_none_or(|i| i == *item);
        Ok(MrpOutput {
            company_id: input.company_id,
            as_of,
            planned_orders: plan
                .planned_orders
                .into_iter()
                .filter(|p| wanted(&p.item_id))
                .map(|order| PlannedOut {
                    item_code: codes.get(&order.item_id).cloned(),
                    warehouse_code: warehouses.get(&order.warehouse_id).cloned(),
                    order,
                })
                .collect(),
            exceptions: plan
                .exceptions
                .into_iter()
                .filter(|e| wanted(&e.item_id))
                .map(|exception| ExceptionOut {
                    item_code: codes.get(&exception.item_id).cloned(),
                    number: exception
                        .document_id
                        .and_then(|d| numbers.get(&d).cloned().flatten()),
                    exception,
                })
                .collect(),
            levels: plan.levels,
        })
    }
}

/// The planned orders of one source (`buy` or `make`) to release by a day.
pub async fn to_release(
    ctx: &mut Ctx<'_>,
    company: Uuid,
    as_of: NaiveDate,
    release_by: NaiveDate,
    source: &str,
) -> Result<Vec<PlannedOrder>> {
    let any_mrp: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM reorder_rule WHERE company_id = $1 AND method = 'mrp')",
    )
    .bind(company)
    .fetch_one(&mut *ctx.conn)
    .await?;
    if !any_mrp {
        return Ok(vec![]);
    }
    Ok(run(ctx, company, as_of)
        .await?
        .planned_orders
        .into_iter()
        .filter(|p| p.source == source && p.release_date <= release_by)
        .collect())
}
