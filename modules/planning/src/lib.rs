//! Planning: what stock a company will have once the orders it has
//! accepted, placed and released are done, and what to order or make so
//! that it has what it wants.
//!
//! It reads stock on hand from the stock ledger and asks sales, purchasing
//! and manufacturing, through their public functions, what their open
//! documents will still move: [`erpcraft_sales::open_demand`],
//! [`erpcraft_purchasing::open_supply`] and
//! [`erpcraft_manufacturing::released`]. What draft purchase and production
//! orders would bring in (`planned`) it reads itself. Its own records are
//! reorder rules (`reorder_rule`): the least of an item a company wants in
//! a warehouse, how far to fill up, in what multiples, whether it is bought
//! or made, and from whom. It suggests; it orders nothing by itself, but
//! drafts purchase and production orders when asked.

pub mod mrp;

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
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("de", include_str!("../i18n/de.json")),
            ("fr", include_str!("../i18n/fr.json")),
        ]
    }

    fn name(&self) -> &'static str {
        "planning"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["stock", "inventory", "sales", "purchasing", "manufacturing"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "reorder_rule",
                sql: include_str!("../migrations/0001_reorder_rule.sql"),
            },
            Migration {
                version: 2,
                name: "procure",
                sql: include_str!("../migrations/0002_procure.sql"),
            },
            Migration {
                version: 3,
                name: "purchase_unit",
                sql: include_str!("../migrations/0003_purchase_unit.sql"),
            },
            Migration {
                version: 4,
                name: "mrp",
                sql: include_str!("../migrations/0004_mrp.sql"),
            },
        ]
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
                    FieldMeta::new(
                        "procure",
                        "Bought or made",
                        FieldType::labelled(&[("buy", "Bought"), ("make", "Made")]),
                    )
                    .help(
                        "Empty: bought when the rule names a vendor, made when the item has \
                         an active bill of materials and the rule names no vendor, bought \
                         otherwise.",
                    ),
                )
                .field(
                    FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor"))
                        .help("Whom it is bought from: drafted purchase orders go to it."),
                )
                .field(
                    FieldMeta::new("purchase_uom", "Bought in", FieldType::Text).help(
                        "The unit the vendor sells it in: one of the item's purchase units \
                         (item_uom). Drafted purchase orders are in it, rounded up to whole \
                         units of it. Empty: the stock unit.",
                    ),
                )
                .field(
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                        .unit_price()
                        .help(
                            "What a drafted purchase order asks per stock unit, in the \
                             accounting currency; the last price paid when left out.",
                        ),
                )
                .field(
                    FieldMeta::new(
                        "method",
                        "Planned by",
                        FieldType::labelled(&[
                            ("reorder", "Reorder point"),
                            ("mrp", "MRP, by date"),
                        ]),
                    )
                    .help(
                        "Reorder point: keep at least the minimum, whenever. MRP: plan by date, \
                         so that each order is met on its day, with lead times, safety stock \
                         and the bills' components.",
                    ),
                )
                .field(
                    FieldMeta::new("safety_stock", "Safety stock", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .help("MRP: what is kept on hand beyond every need."),
                )
                .field(
                    FieldMeta::new("min_order_qty", "Minimum order", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .help("MRP: the least a planned order is for."),
                )
                .field(
                    FieldMeta::new("lead_time_days", "Lead time (days)", FieldType::Int).help(
                        "MRP: days from release to receipt. Left out, the vendor's (item_vendor) \
                         for an item bought, the bill's for one made.",
                    ),
                )
                .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
                .order_by(&["item_id", "warehouse_id"])
                .company_field("company_id"),
            EntityMeta::new("item_vendor", "Item vendor")
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
                    FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("lead_time_days", "Lead time (days)", FieldType::Int)
                        .help("Days from ordering until the goods arrive."),
                )
                .field(
                    FieldMeta::new("min_order_qty", "Minimum order", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .help("The least the vendor sells, in the item's stock unit."),
                )
                .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
                .order_by(&["item_id", "vendor_id"])
                .plural("Item vendors")
                .company_field("company_id"),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Availability),
            command(Suggestions),
            command(DraftPurchaseOrders),
            command(DraftProductionOrders),
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
    /// Of what is on hand, what is set aside for particular open sales
    /// order lines and released production orders (stock reservations).
    /// It is part of what is committed and needed, not on top of it.
    pub reserved: Decimal,
    /// On hand less what is reserved: what can be taken now without
    /// touching another order's reservation.
    pub free: Decimal,
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
    /// Shipped to this warehouse on transfers and not yet received.
    pub in_transit: Decimal,
    /// Available plus what is on order, in production and in transit: what is left
    /// once every open order is done. Below zero, the item falls short.
    pub projected: Decimal,
    /// What draft purchase orders and draft production orders would bring
    /// in: not on order yet, and not in `projected`, but planned, so that
    /// planning does not suggest it again.
    pub planned: Decimal,
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
            "For each item and warehouse of a company: what is on hand, how much of it is \
             reserved for particular orders and what is free of reservations, what open sales orders \
             still have to deliver (committed), what released production orders still have to \
             consume, what can still be promised (available), what open purchase orders and \
             released production orders will bring in, what transfers shipped to it have on \
             their way (in_transit; goods in a transit warehouse count there, not where they \
             wait), and what is left once every open order is done (projected; below zero the \
             item falls short); and apart from that, what \
             draft purchase and production orders would bring in (planned). Quantities are in \
             each item's unit. Takes the right to read stock entries, sales orders, purchase orders \
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
        "SELECT l.item_id, l.warehouse_id, l.qty FROM stock_level l
         JOIN warehouse w ON w.id = l.warehouse_id
         WHERE l.company_id = $1 AND l.qty <> 0 AND NOT w.transit",
    )
    .bind(company)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let committed = erpcraft_sales::open_demand(&mut *ctx.conn, company).await?;
    let on_order = erpcraft_purchasing::open_supply(&mut *ctx.conn, company).await?;
    let (needed, making) = erpcraft_manufacturing::released(&mut *ctx.conn, company).await?;
    let planned = drafted(&mut *ctx.conn, company).await?;
    let travelling: Vec<(Uuid, Uuid, Decimal)> =
        erpcraft_inventory::in_transit(&mut *ctx.conn, company)
            .await?
            .into_iter()
            .map(|t| (t.item_id, t.warehouse_id, t.qty))
            .collect();
    let reserved = erpcraft_stock::reserved(&mut *ctx.conn, company).await?;

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
    add(planned, |r| &mut r.planned);
    add(travelling, |r| &mut r.in_transit);
    add(reserved, |r| &mut r.reserved);

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
            row.free = row.on_hand - row.reserved;
            row.available = row.on_hand - row.committed - row.needed_for_production;
            row.projected = row.available + row.on_order + row.in_production + row.in_transit;
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
                &mut row.reserved,
                &mut row.free,
                &mut row.committed,
                &mut row.needed_for_production,
                &mut row.available,
                &mut row.on_order,
                &mut row.in_production,
                &mut row.in_transit,
                &mut row.projected,
                &mut row.planned,
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

/// What draft purchase orders and draft production orders of `company`
/// would bring in, per item and warehouse: planned, not ordered. A draft
/// production order's components are not counted: they are worked out when
/// it is released.
async fn drafted(
    conn: &mut erpcraft_kernel::PgConnection,
    company: Uuid,
) -> Result<Vec<(Uuid, Uuid, Decimal)>> {
    Ok(sqlx::query_as(
        "SELECT item_id, warehouse_id, sum(qty) FROM (
             SELECT l.item_id, l.warehouse_id, l.qty * l.factor AS qty
             FROM purchase_order_line l JOIN document d ON d.id = l.document_id
             WHERE d.company_id = $1 AND d.status = 'draft' AND l.item_id IS NOT NULL
               AND l.warehouse_id IS NOT NULL
             UNION ALL
             SELECT b.item_id, o.warehouse_id, o.qty
             FROM production_order o JOIN bom b ON b.id = o.bom_id
             JOIN document d ON d.id = o.document_id
             WHERE d.company_id = $1 AND d.status = 'draft'
         ) drafts
         GROUP BY item_id, warehouse_id HAVING sum(qty) > 0",
    )
    .bind(company)
    .fetch_all(&mut *conn)
    .await?)
}

/// Planning's draft commands of one company run one at a time: each
/// counts what the other drafted, so two runs at once do not draft the same
/// need twice. Held until the transaction ends.
async fn one_draft_at_a_time(
    conn: &mut erpcraft_kernel::PgConnection,
    company: Uuid,
) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('planning.draft'), hashtext($1::text))")
        .bind(company)
        .execute(&mut *conn)
        .await?;
    Ok(())
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
    /// What draft orders would bring in, netted off before suggesting.
    pub planned: Decimal,
    /// The reorder rule's minimum; zero without a rule.
    pub min_qty: Decimal,
    /// What to order or make: up to the rule's fill-up quantity (or its
    /// minimum, or zero without a rule), less what is planned, rounded up
    /// to its multiple. In stock units.
    pub qty: Decimal,
    /// For an item bought in another unit (the rule's `purchase_uom`):
    /// that unit, how many stock units one is, and how many of it to buy,
    /// rounded up to whole units.
    pub purchase_uom: Option<String>,
    pub purchase_factor: Option<Decimal>,
    pub purchase_qty: Option<Decimal>,
    /// What buying whole purchase units brings in beyond `qty`, in stock
    /// units.
    pub extra: Decimal,
    /// `buy` or `make`: what the rule says; else `buy` when the rule names a
    /// vendor, `make` when the item has an active bill of materials, else
    /// `buy`.
    pub source: String,
    /// The vendor the rule names, for an item bought. Absent for a caller
    /// who may not read reorder rules.
    pub vendor_id: Option<Uuid>,
    /// The price a purchase order would ask: the rule's, else the last
    /// price paid on an open or closed purchase order of the company.
    /// Absent when there is none, and for a caller who may not read reorder
    /// rules.
    pub unit_price: Option<Decimal>,
    /// Why the suggestion cannot be drafted as it stands, such as an item
    /// to be made that has no active bill of materials.
    pub note: Option<String>,
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
    procure: Option<String>,
    purchase_uom: Option<String>,
    purchase_factor: Option<Decimal>,
}

/// What to order or make so that every item ends up at least at its
/// reorder rule's minimum, or not below zero where it has no rule. The
/// rule's vendor and price are shown only to a caller who may read reorder
/// rules.
pub async fn suggestions(
    ctx: &mut Ctx<'_>,
    company: Uuid,
    only_item: Option<Uuid>,
    only_warehouse: Option<Uuid>,
) -> Result<Vec<Suggestion>> {
    let mut out = plan(ctx, company, only_item, only_warehouse).await?;
    if !ctx.actor.can("reorder_rule:read", company) {
        for s in &mut out {
            s.vendor_id = None;
            s.unit_price = None;
        }
    }
    Ok(out)
}

/// The suggestions in full, with the rules' vendors and prices, for the
/// draft commands, which require the right to read the rules.
async fn plan(
    ctx: &mut Ctx<'_>,
    company: Uuid,
    only_item: Option<Uuid>,
    only_warehouse: Option<Uuid>,
) -> Result<Vec<Suggestion>> {
    let rows = availability(ctx, company, only_item, only_warehouse).await?;
    let rules: Vec<Rule> = sqlx::query_as(
        "SELECT r.item_id, r.warehouse_id, r.min_qty, r.max_qty, r.multiple, r.vendor_id,
                r.unit_price, r.procure, r.purchase_uom, u.factor AS purchase_factor
         FROM reorder_rule r
         LEFT JOIN item_uom u ON u.item_id = r.item_id AND u.uom = r.purchase_uom
         WHERE r.company_id = $1",
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
        // Drafts count: what they would bring in is not suggested again.
        let expected = row.projected + row.planned;
        if expected >= min {
            continue;
        }
        let target = rule.map_or(Decimal::ZERO, |r| r.max_qty.unwrap_or(r.min_qty));
        let mut qty = target - expected;
        if let Some(multiple) = rule.and_then(|r| r.multiple) {
            qty = (qty / multiple).ceil() * multiple;
        }
        if qty <= Decimal::ZERO {
            continue;
        }
        let has_bill = made.contains(&row.item_id);
        let source = match rule.and_then(|r| r.procure.as_deref()) {
            Some("make") => "make",
            Some(_) => "buy",
            None if rule.is_some_and(|r| r.vendor_id.is_some()) => "buy",
            None if has_bill => "make",
            None => "buy",
        };
        let note = (source == "make" && !has_bill)
            .then(|| "to be made, but the item has no active bill of materials".to_owned());
        // The last price paid: on an order placed, not on a draft.
        let unit_price = match rule.and_then(|r| r.unit_price) {
            Some(price) => Some(price),
            None if source == "buy" => {
                sqlx::query_scalar(
                    // Per stock unit, whatever unit it was bought in.
                    "SELECT round(l.unit_price / l.factor, 6) FROM purchase_order_line l
                     JOIN document d ON d.id = l.document_id
                     JOIN purchase_order o ON o.document_id = l.document_id
                     WHERE d.company_id = $1 AND l.item_id = $2 AND o.currency IS NULL
                       AND d.status IN ('open', 'closed')
                     ORDER BY d.doc_date DESC, d.created_at DESC LIMIT 1",
                )
                .bind(company)
                .bind(row.item_id)
                .fetch_optional(&mut *ctx.conn)
                .await?
            }
            None => None,
        };
        // Bought in another unit: whole units of it, and what that brings
        // in beyond the need.
        let unit = rule
            .filter(|_| source == "buy")
            .and_then(|r| r.purchase_uom.clone().zip(r.purchase_factor));
        let (purchase_qty, extra) = match &unit {
            Some((_, factor)) => {
                let whole = (qty / factor).ceil();
                (Some(whole.normalize()), (whole * factor - qty).normalize())
            }
            None => (None, Decimal::ZERO),
        };
        out.push(Suggestion {
            purchase_uom: unit.as_ref().map(|u| u.0.clone()),
            purchase_factor: unit.as_ref().map(|u| u.1.normalize()),
            purchase_qty,
            extra,
            item_id: row.item_id,
            item_code: row.item_code.clone(),
            warehouse_id: row.warehouse_id,
            warehouse_code: row.warehouse_code.clone(),
            projected: row.projected,
            planned: row.planned,
            min_qty: min.normalize(),
            qty: qty.normalize(),
            source: source.into(),
            vendor_id: if source == "buy" {
                rule.and_then(|r| r.vendor_id)
            } else {
                None
            },
            unit_price: unit_price.map(|p: Decimal| p.normalize()),
            note,
        });
    }
    // Codes for rows that came from rules alone.
    let items = ctx.actor.can_any("item:read");
    let warehouses = ctx.actor.can("warehouse:read", company);
    for s in out.iter_mut() {
        if items && s.item_code.is_none() {
            s.item_code = sqlx::query_scalar("SELECT code FROM item WHERE id = $1")
                .bind(s.item_id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        }
        if warehouses && s.warehouse_code.is_none() {
            s.warehouse_code =
                sqlx::query_scalar("SELECT code FROM warehouse WHERE id = $1 AND company_id = $2")
                    .bind(s.warehouse_id)
                    .bind(company)
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
             the rule's fill-up quantity, less what draft orders already plan, rounded up to its \
             multiple. Each suggestion says whether the item is bought or made (the rule's \
             choice; else bought from the rule's vendor, made when it has an active bill of \
             materials, bought otherwise), and for one bought the rule's vendor and the price a \
             purchase order would ask (shown with the right to read reorder rules). Orders \
             nothing; planning.draft_purchase_orders and planning.draft_production_orders draft \
             the orders.",
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
    /// Suggestions to buy with no price: neither the rule's nor one paid
    /// before. No line is drafted at a price nobody agreed to.
    without_price: Vec<Suggestion>,
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
             them as any other; until then they count as planned, so running it again drafts \
             nothing more. Suggestions without a vendor or a price are returned, not ordered. \
             Takes the right to read reorder rules.",
        )
        .permission("purchase_order:create")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftInput) -> Result<DraftOutput> {
        let company = input.company_id;
        ctx.require("purchase_order:create", company)?;
        ctx.require("reorder_rule:read", company)?;
        let doc_date = input
            .doc_date
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        one_draft_at_a_time(&mut *ctx.conn, company).await?;
        let buy: Vec<Suggestion> = plan(ctx, company, None, None)
            .await?
            .into_iter()
            .filter(|s| s.source == "buy")
            .collect();
        let mut by_vendor: BTreeMap<Uuid, Vec<&Suggestion>> = BTreeMap::new();
        let mut without_vendor = vec![];
        let mut without_price = vec![];
        for s in &buy {
            match s.vendor_id {
                Some(vendor) if input.vendor_id.is_none_or(|v| v == vendor) => {
                    if s.unit_price.is_some() {
                        by_vendor.entry(vendor).or_default().push(s)
                    } else {
                        without_price.push(s.clone())
                    }
                }
                Some(_) => {}
                None => without_vendor.push(s.clone()),
            }
        }
        if by_vendor.is_empty() {
            ctx.no_change();
        }
        let mut ids = vec![];
        for (vendor, lines) in by_vendor {
            let lines: Vec<_> = lines
                .iter()
                .map(|s| {
                    match (&s.purchase_uom, s.purchase_factor, s.purchase_qty) {
                        // In the vendor's unit, at the price per stock unit
                        // times the unit's factor.
                        (Some(uom), Some(factor), Some(qty)) => json!({
                            "item_id": s.item_id,
                            "warehouse_id": s.warehouse_id,
                            "qty": qty,
                            "uom": uom,
                            "unit_price": s.unit_price.map(|p| (p * factor).round_dp(6).normalize()),
                        }),
                        _ => json!({
                            "item_id": s.item_id,
                            "warehouse_id": s.warehouse_id,
                            "qty": s.qty,
                            "unit_price": s.unit_price,
                        }),
                    }
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
            without_price,
        })
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftProductionInput {
    /// The company to make for.
    company_id: Uuid,
    /// The date of the drafted orders. Today when omitted.
    doc_date: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema)]
pub struct DraftProductionOutput {
    /// The production orders drafted, one per item and warehouse.
    production_order_ids: Vec<Uuid>,
    /// Suggestions to make for items without an active bill of materials:
    /// no order was drafted for them.
    without_bill: Vec<Suggestion>,
}

pub struct DraftProductionOrders;

#[async_trait]
impl Command for DraftProductionOrders {
    type Input = DraftProductionInput;
    type Output = DraftProductionOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "planning.draft_production_orders",
            "Draft a production order for everything planning.suggestions says to make, by the \
             item's active bill of materials (the first by code when it has several), in the \
             warehouse that needs it. The orders are drafts: check and release them as any \
             other; until then what they make counts as planned, so running it again drafts \
             nothing more. Their components are not planned until they are released. Items to \
             be made without an active bill are returned, not drafted. Takes the right to read \
             reorder rules and bills of materials.",
        )
        .permission("production_order:create")
    }

    async fn run(
        &self,
        ctx: &mut Ctx<'_>,
        input: DraftProductionInput,
    ) -> Result<DraftProductionOutput> {
        let company = input.company_id;
        ctx.require("production_order:create", company)?;
        ctx.require("reorder_rule:read", company)?;
        // Bills of materials are shared by all companies.
        ctx.actor.require_global("bom:read")?;
        let doc_date = input
            .doc_date
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        one_draft_at_a_time(&mut *ctx.conn, company).await?;
        let (make, without_bill): (Vec<Suggestion>, Vec<Suggestion>) =
            plan(ctx, company, None, None)
                .await?
                .into_iter()
                .filter(|s| s.source == "make")
                .partition(|s| s.note.is_none());
        if make.is_empty() {
            ctx.no_change();
        }
        let mut ids = vec![];
        for s in make {
            let bom: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM bom WHERE item_id = $1 AND active ORDER BY code LIMIT 1",
            )
            .bind(s.item_id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
            // Retired since the suggestion was worked out, a moment ago.
            let Some(bom) = bom else {
                return Err(erpcraft_kernel::Error::state(format!(
                    "item {} has no active bill of materials any more; nothing was drafted, \
                     run planning again",
                    s.item_code.as_deref().unwrap_or(&s.item_id.to_string())
                )));
            };
            let view = document::create(
                ctx,
                NewDocument {
                    doc_type: "production_order".into(),
                    company_id: company,
                    doc_date,
                    memo: Some("Drafted by planning".into()),
                    body: json!({ "bom_id": bom, "qty": s.qty, "warehouse_id": s.warehouse_id }),
                },
            )
            .await?;
            ids.push(view.header.id);
        }
        Ok(DraftProductionOutput {
            production_order_ids: ids,
            without_bill,
        })
    }
}
