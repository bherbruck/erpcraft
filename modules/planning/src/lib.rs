//! Planning: what stock a company will have once the orders it has
//! accepted, placed and released are done.
//!
//! The module keeps nothing of its own. It reads stock on hand from the
//! stock ledger and asks sales, purchasing and manufacturing, through
//! their public functions, what their open documents will still move:
//! [`erpcraft_sales::open_demand`], [`erpcraft_purchasing::open_supply`]
//! and [`erpcraft_manufacturing::released`].

use std::collections::BTreeMap;
use std::sync::Arc;

use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DynCommand, JsonSchema, Migration, Module, Result,
    async_trait, command, schemars,
};
use serde::{Deserialize, Serialize};
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
        vec![]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Availability)]
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
        let company = input.company_id;
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
                if input.item_id.is_some_and(|i| i != item_id)
                    || input.warehouse_id.is_some_and(|w| w != warehouse_id)
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
                if let Some((_, code, name)) = item_names.iter().find(|(id, ..)| *id == row.item_id)
                {
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
            .filter(|row| !input.short_only || row.projected < Decimal::ZERO)
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
        Ok(AvailabilityOutput {
            company_id: company,
            rows: out,
        })
    }
}
