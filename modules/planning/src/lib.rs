//! Planning: what stock a company will have once the orders it has
//! accepted, placed and released are done, and what to order or make so
//! that it has what it wants.
//!
//! It reads stock on hand from the stock ledger and asks sales, purchasing
//! and manufacturing, through their public functions, what their open
//! documents will still move: [`erpcraft_sales::open_demand`],
//! [`erpcraft_purchasing::open_supply`] and
//! [`erpcraft_manufacturing::released`]. Its own records are reorder rules
//! (`reorder_rule`): the least of an item a company wants in a warehouse,
//! how far to fill up, in what multiples, and from whom. It suggests; it
//! orders nothing by itself, but drafts purchase orders when asked.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, NewDocument};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DynCommand, EntityMeta, FieldMeta, FieldType, JsonSchema,
    Migration, Module, Result, async_trait, command, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

pub struct PlanningModule;

impl Module for PlanningModule {
    fn name(&self) -> &'static str {
        "planning"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["stock", "sales", "purchasing", "manufacturing"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "reorder_rule",
            sql: include_str!("../migrations/0001_reorder_rule.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![
            EntityMeta::new("reorder_rule", "Reorder rule")
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
                    FieldMeta::new("warehouse_id", "Warehouse", FieldType::link("warehouse"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("min_qty", "Minimum", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .help(
                            "The least the company wants once every open order is done. Below \
                             it, planning suggests ordering or making more.",
                        ),
                )
                .field(
                    FieldMeta::new("max_qty", "Fill up to", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .help("How far a suggestion fills up; the minimum when left out."),
                )
                .field(
                    FieldMeta::new("multiple", "In multiples of", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .help("A pack or batch size: suggestions are rounded up to it."),
                )
                .field(
                    FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor"))
                        .help("Whom it is bought from: drafted purchase orders go to it."),
                )
                .field(
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                        .unit_price()
                        .help(
                            "What a drafted purchase order asks, in the accounting currency; \
                             the last price paid when left out.",
                        ),
                )
                .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
                .order_by(&["item_id", "warehouse_id"])
                .company_field("company_id"),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Availability),
            command(Suggestions),
            command(DraftPurchaseOrders),
        ]
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityInput {
    /// The company whose stock and orders are counted.
    company_id: Uuid,
    /// Only this item.
    item_id: Option<Uuid>,
    /// Only this warehouse.
    warehouse_id: Option<Uuid>,
    /// Only the items that will fall short: whose projected quantity is
    /// below zero.
    #[serde(default)]
    short_only: bool,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct AvailabilityRow {
    pub item_id: Uuid,
    /// Absent for a caller who may not read items.
    pub item_code: Option<String>,
    pub item_name: Option<String>,
    pub warehouse_id: Uuid,
    /// Absent for a caller who may not read warehouses.
    pub warehouse_code: Option<String>,
    /// In the warehouse now.
    pub on_hand: Decimal,
    /// Still to be delivered on open sales orders.
    pub committed: Decimal,
    /// Still to be consumed by released production orders.
    pub needed_for_production: Decimal,
    /// On hand less what is committed and needed: what can still be
    /// promised from stock.
    pub available: Decimal,
    /// Still to come in on open purchase orders.
    pub on_order: Decimal,
    /// Still to be made by released production orders.
    pub in_production: Decimal,
    /// Available plus what is on order and in production: what is left
    /// once every open order is done. Below zero, the item falls short.
    pub projected: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub struct AvailabilityOutput {
    company_id: Uuid,
    rows: Vec<AvailabilityRow>,
}

pub struct Availability;

#[async_trait]
impl Command for Availability {
    type Input = AvailabilityInput;
    type Output = AvailabilityOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "stock.availability",
            "For each item and warehouse of a company: what is on hand, what open sales orders \
             still have to deliver (committed), what released production orders still have to \
             consume, what can still be promised (available), what open purchase orders and \
             released production orders will bring in, and what is left once every open order \
             is done (projected; below zero the item falls short). Quantities are in each \
             item's unit. Takes the right to read stock entries, sales orders, purchase orders \
             and production orders of the company.",
        )
        .report("Stock availability")
        .permission("stock_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: AvailabilityInput) -> Result<AvailabilityOutput> {
        let rows = availability(ctx, input.company_id, input.item_id, input.warehouse_id)
            .await?
            .into_iter()
            .filter(|row| !input.short_only || row.projected < Decimal::ZERO)
            .collect();
        Ok(AvailabilityOutput {
            company_id: input.company_id,
            rows,
        })
    }
}

/// Every item and warehouse of `company` that has stock or open orders,
/// with what is on hand, committed, needed, on order and in production, as
/// [`Availability`] shows them. Takes the rights it names.
pub async fn availability(
    ctx: &mut Ctx<'_>,
    company: Uuid,
    only_item: Option<Uuid>,
    only_warehouse: Option<Uuid>,
) -> Result<Vec<AvailabilityRow>> {
    for right in [
        "stock_entry:read",
        "sales_order:read",
        "purchase_order:read",
        "production_order:read",
    ] {
        ctx.require(right, company)?;
    }
    let on_hand: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
        "SELECT item_id, warehouse_id, qty FROM stock_level
         WHERE company_id = $1 AND qty <> 0",
    )
    .bind(company)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let committed = erpcraft_sales::open_demand(&mut *ctx.conn, company).await?;
    let on_order = erpcraft_purchasing::open_supply(&mut *ctx.conn, company).await?;
    let (needed, making) = erpcraft_manufacturing::released(&mut *ctx.conn, company).await?;

    let mut rows: BTreeMap<(Uuid, Uuid), AvailabilityRow> = BTreeMap::new();
    let mut add = |list: Vec<(Uuid, Uuid, Decimal)>,
                   field: fn(&mut AvailabilityRow) -> &mut Decimal| {
        for (item_id, warehouse_id, qty) in list {
            if only_item.is_some_and(|i| i != item_id)
                || only_warehouse.is_some_and(|w| w != warehouse_id)
            {
                continue;
            }
            let row = rows
                .entry((item_id, warehouse_id))
                .or_insert_with(|| AvailabilityRow {
                    item_id,
                    warehouse_id,
                    ..AvailabilityRow::default()
                });
            *field(row) += qty;
        }
    };
    add(on_hand, |r| &mut r.on_hand);
    add(committed, |r| &mut r.committed);
    add(needed, |r| &mut r.needed_for_production);
    add(on_order, |r| &mut r.on_order);
    add(making, |r| &mut r.in_production);

    let items = ctx.actor.can_any("item:read");
    let warehouses = ctx.actor.can("warehouse:read", company);
    let ids: Vec<Uuid> = rows.keys().map(|(item, _)| *item).collect();
    let item_names: Vec<(Uuid, String, String)> = if items {
        sqlx::query_as("SELECT id, code, name FROM item WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(&mut *ctx.conn)
            .await?
    } else {
        vec![]
    };
    let warehouse_codes: Vec<(Uuid, String)> = if warehouses {
        sqlx::query_as("SELECT id, code FROM warehouse WHERE company_id = $1")
            .bind(company)
            .fetch_all(&mut *ctx.conn)
            .await?
    } else {
        vec![]
    };
    let mut out: Vec<AvailabilityRow> = rows
        .into_values()
        .map(|mut row| {
            row.available = row.on_hand - row.committed - row.needed_for_production;
            row.projected = row.available + row.on_order + row.in_production;
            if let Some((_, code, name)) = item_names.iter().find(|(id, ..)| *id == row.item_id) {
                row.item_code = Some(code.clone());
                row.item_name = Some(name.clone());
            }
            row.warehouse_code = warehouse_codes
                .iter()
                .find(|(id, _)| *id == row.warehouse_id)
                .map(|(_, code)| code.clone());
            for qty in [
                &mut row.on_hand,
                &mut row.committed,
                &mut row.needed_for_production,
                &mut row.available,
                &mut row.on_order,
                &mut row.in_production,
                &mut row.projected,
            ] {
                *qty = qty.normalize();
            }
            row
        })
        .collect();
    out.sort_by(|a, b| {
        (
            a.item_code.as_deref(),
            a.item_id,
            a.warehouse_code.as_deref(),
        )
            .cmp(&(
                b.item_code.as_deref(),
                b.item_id,
                b.warehouse_code.as_deref(),
            ))
    });
    Ok(out)
}

// ------------------------------------------------------------- suggestions

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestionsInput {
    /// The company to plan for.
    company_id: Uuid,
    /// Only this item.
    item_id: Option<Uuid>,
    /// Only this warehouse.
    warehouse_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Suggestion {
    pub item_id: Uuid,
    /// Absent for a caller who may not read items.
    pub item_code: Option<String>,
    pub warehouse_id: Uuid,
    /// Absent for a caller who may not read warehouses.
    pub warehouse_code: Option<String>,
    /// What is left once every open order is done.
    pub projected: Decimal,
    /// The reorder rule's minimum; zero without a rule.
    pub min_qty: Decimal,
    /// What to order or make: up to the rule's fill-up quantity (or its
    /// minimum, or zero without a rule), rounded up to its multiple.
    pub qty: Decimal,
    /// `make` when the item has an active bill of materials, else `buy`.
    pub source: String,
    /// The vendor the rule names, for an item bought.
    pub vendor_id: Option<Uuid>,
    /// The price a purchase order would ask: the rule's, else the last
    /// price paid on a purchase order of the company.
    pub unit_price: Option<Decimal>,
}

#[derive(Serialize, JsonSchema)]
pub struct SuggestionsOutput {
    company_id: Uuid,
    suggestions: Vec<Suggestion>,
}

#[derive(sqlx::FromRow)]
struct Rule {
    item_id: Uuid,
    warehouse_id: Uuid,
    min_qty: Decimal,
    max_qty: Option<Decimal>,
    multiple: Option<Decimal>,
    vendor_id: Option<Uuid>,
    unit_price: Option<Decimal>,
}

/// What to order or make so that every item ends up at least at its
/// reorder rule's minimum, or not below zero where it has no rule.
pub async fn suggestions(
    ctx: &mut Ctx<'_>,
    company: Uuid,
    only_item: Option<Uuid>,
    only_warehouse: Option<Uuid>,
) -> Result<Vec<Suggestion>> {
    let rows = availability(ctx, company, only_item, only_warehouse).await?;
    let rules: Vec<Rule> = sqlx::query_as(
        "SELECT item_id, warehouse_id, min_qty, max_qty, multiple, vendor_id, unit_price
         FROM reorder_rule WHERE company_id = $1",
    )
    .bind(company)
    .fetch_all(&mut *ctx.conn)
    .await?;
    // Items with a rule and no stock or orders yet are planned too.
    let mut rows = rows;
    for rule in &rules {
        if only_item.is_some_and(|i| i != rule.item_id)
            || only_warehouse.is_some_and(|w| w != rule.warehouse_id)
            || rows
                .iter()
                .any(|r| r.item_id == rule.item_id && r.warehouse_id == rule.warehouse_id)
        {
            continue;
        }
        rows.push(AvailabilityRow {
            item_id: rule.item_id,
            warehouse_id: rule.warehouse_id,
            ..AvailabilityRow::default()
        });
    }
    let made: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT item_id FROM bom WHERE active")
        .fetch_all(&mut *ctx.conn)
        .await?;
    let mut out = vec![];
    for row in rows {
        let rule = rules
            .iter()
            .find(|r| r.item_id == row.item_id && r.warehouse_id == row.warehouse_id);
        let min = rule.map_or(Decimal::ZERO, |r| r.min_qty);
        if row.projected >= min {
            continue;
        }
        let target = rule.map_or(Decimal::ZERO, |r| r.max_qty.unwrap_or(r.min_qty));
        let mut qty = target - row.projected;
        if let Some(multiple) = rule.and_then(|r| r.multiple) {
            qty = (qty / multiple).ceil() * multiple;
        }
        if qty <= Decimal::ZERO {
            continue;
        }
        let source = if made.contains(&row.item_id) {
            "make"
        } else {
            "buy"
        };
        let unit_price = match rule.and_then(|r| r.unit_price) {
            Some(price) => Some(price),
            None if source == "buy" => {
                sqlx::query_scalar(
                    "SELECT l.unit_price FROM purchase_order_line l
                     JOIN document d ON d.id = l.document_id
                     JOIN purchase_order o ON o.document_id = l.document_id
                     WHERE d.company_id = $1 AND l.item_id = $2 AND o.currency IS NULL
                       AND d.status <> 'cancelled'
                     ORDER BY d.doc_date DESC, d.created_at DESC LIMIT 1",
                )
                .bind(company)
                .bind(row.item_id)
                .fetch_optional(&mut *ctx.conn)
                .await?
            }
            None => None,
        };
        out.push(Suggestion {
            item_id: row.item_id,
            item_code: row.item_code.clone(),
            warehouse_id: row.warehouse_id,
            warehouse_code: row.warehouse_code.clone(),
            projected: row.projected,
            min_qty: min.normalize(),
            qty: qty.normalize(),
            source: source.into(),
            vendor_id: rule.and_then(|r| r.vendor_id),
            unit_price: unit_price.map(|p: Decimal| p.normalize()),
        });
    }
    // Codes for rows that came from rules alone.
    for s in out.iter_mut().filter(|s| s.item_code.is_none()) {
        if ctx.actor.can_any("item:read") {
            s.item_code = sqlx::query_scalar("SELECT code FROM item WHERE id = $1")
                .bind(s.item_id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        }
    }
    Ok(out)
}

pub struct Suggestions;

#[async_trait]
impl Command for Suggestions {
    type Input = SuggestionsInput;
    type Output = SuggestionsOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "planning.suggestions",
            "What to order or make, per item and warehouse, so that what is left once every \
             open order is done (see stock.availability) is at least the reorder rule's \
             minimum, or not below zero for an item without a rule: the quantity fills up to \
             the rule's fill-up quantity, rounded up to its multiple. Each suggestion says \
             whether the item is made (it has an active bill of materials) or bought, and for \
             one bought the rule's vendor and the price a purchase order would ask. Orders \
             nothing; planning.draft_purchase_orders drafts the purchase orders.",
        )
        .report("Planning suggestions")
        .permission("stock_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SuggestionsInput) -> Result<SuggestionsOutput> {
        Ok(SuggestionsOutput {
            company_id: input.company_id,
            suggestions: suggestions(ctx, input.company_id, input.item_id, input.warehouse_id)
                .await?,
        })
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftInput {
    /// The company to order for.
    company_id: Uuid,
    /// The date of the drafted orders. Today when omitted.
    doc_date: Option<NaiveDate>,
    /// Only for this vendor.
    vendor_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema)]
pub struct DraftOutput {
    /// The purchase orders drafted, one per vendor.
    purchase_order_ids: Vec<Uuid>,
    /// Suggestions to buy that name no vendor: no order was drafted for
    /// them.
    without_vendor: Vec<Suggestion>,
}

pub struct DraftPurchaseOrders;

#[async_trait]
impl Command for DraftPurchaseOrders {
    type Input = DraftInput;
    type Output = DraftOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "planning.draft_purchase_orders",
            "Draft a purchase order per vendor for what planning.suggestions says to buy, from \
             the vendor each item's reorder rule names, at the rule's price or else the last \
             price paid. The orders are drafts in the accounting currency: check and approve \
             them as any other. Suggestions without a vendor are returned, not ordered.",
        )
        .permission("purchase_order:create")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftInput) -> Result<DraftOutput> {
        let company = input.company_id;
        ctx.require("purchase_order:create", company)?;
        let doc_date = input
            .doc_date
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        let buy: Vec<Suggestion> = suggestions(ctx, company, None, None)
            .await?
            .into_iter()
            .filter(|s| s.source == "buy")
            .collect();
        let mut by_vendor: BTreeMap<Uuid, Vec<&Suggestion>> = BTreeMap::new();
        let mut without_vendor = vec![];
        for s in &buy {
            match s.vendor_id {
                Some(vendor) if input.vendor_id.is_none_or(|v| v == vendor) => {
                    by_vendor.entry(vendor).or_default().push(s)
                }
                Some(_) => {}
                None => without_vendor.push(s.clone()),
            }
        }
        if by_vendor.is_empty() && without_vendor.is_empty() {
            ctx.no_change();
        }
        let mut ids = vec![];
        for (vendor, lines) in by_vendor {
            let lines: Vec<_> = lines
                .iter()
                .map(|s| {
                    json!({
                        "item_id": s.item_id,
                        "warehouse_id": s.warehouse_id,
                        "qty": s.qty,
                        "unit_price": s.unit_price.unwrap_or_default(),
                    })
                })
                .collect();
            let view = document::create(
                ctx,
                NewDocument {
                    doc_type: "purchase_order".into(),
                    company_id: company,
                    doc_date,
                    memo: Some("Drafted by planning".into()),
                    body: json!({ "vendor_id": vendor, "lines": lines }),
                },
            )
            .await?;
            ids.push(view.header.id);
        }
        Ok(DraftOutput {
            purchase_order_ids: ids,
            without_vendor,
        })
    }
}
