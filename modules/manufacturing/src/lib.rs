//! Manufacturing module. Uses only the public module API of the kernel, the
//! stock ledger and the general ledger, as a module outside this
//! repository would.
//!
//! A bill of materials says what a product is made of. A production order
//! makes the product: completing it takes the components out of stock and
//! brings the product in, in one posting, so that the product is worth
//! exactly what was consumed.

use std::str::FromStr;
use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, Decision, DocTypeMeta, DynCommand,
    EntityMeta, Error, FieldMeta, FieldType, JsonSchema, Migration, Module, PgConnection,
    PolicyPoint, Registry, Result, StateMeta, async_trait, command, company, new_id, round_money,
    schemars,
};
use erpcraft_stock::{Cost, StockLine};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

mod routing;

const PRODUCTION_ORDER: &str = "production_order";

pub struct ManufacturingModule;

/// The policy point asked when a production order is released.
pub const INACTIVE_BOM: &str = "production_order.inactive_bom";

impl Module for ManufacturingModule {
    fn name(&self) -> &'static str {
        "manufacturing"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "stock"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "manufacturing",
                sql: include_str!("../migrations/0001_manufacturing.sql"),
            },
            Migration {
                version: 2,
                name: "inventory_account_rule",
                sql: include_str!("../migrations/0002_inventory_account_rule.sql"),
            },
            Migration {
                version: 3,
                name: "activities",
                sql: include_str!("../migrations/0003_activities.sql"),
            },
            Migration {
                version: 4,
                name: "routing",
                sql: include_str!("../migrations/0004_routing.sql"),
            },
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                INACTIVE_BOM,
                "Asked when a production order is released: may it be made by this bill of \
                 materials? Unless configured otherwise a bill that is inactive is refused. \
                 A company that retires a bill but still has orders drafted against it allows \
                 them, for example for that bill with `facts.bom_code == 'B-100'`.",
            )
            .text("bom_code", "The code of the bill of materials")
            .boolean("active", "Whether the bill is active")
            .refuse_when("!facts.active")
            .refusal("the bill of materials is inactive"),
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        let mut all = vec![
            EntityMeta::new("bom", "Bill of materials")
                // The product. Orders released against the bill would
                // otherwise make something else.
                .field(
                    FieldMeta::new("item_id", "Product", FieldType::link("item"))
                        .required()
                        .immutable(),
                )
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new("output_qty", "Output quantity", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom"),
                )
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .plural("Bills of materials")
                .validator(validate_bom),
            EntityMeta::new("bom_line", "Bill of materials line")
                .field(
                    FieldMeta::new("bom_id", "Bill of materials", FieldType::link("bom"))
                        .required()
                        .immutable(),
                )
                .field(FieldMeta::new("item_id", "Component", FieldType::link("item")).required())
                .field(
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .required(),
                )
                .field(created())
                .order_by(&["bom_id", "created_at", "id"])
                .display(&["item_id"])
                // A line is entered on the page of its bill.
                .child_of("bom", "bom_id")
                .validator(validate_bom_line),
            EntityMeta::new("cost_activity", "Activity")
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new(
                        "kind",
                        "Kind",
                        FieldType::options(&["labor", "machine", "overhead"]),
                    )
                    .help("What the cost is: people's time, a machine's, or overhead."),
                )
                .field(
                    FieldMeta::new("unit", "Unit", FieldType::Text)
                        .help("What its quantity counts, such as h for hours."),
                )
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .plural("Activities"),
            EntityMeta::new("cost_activity_rate", "Activity rate")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("activity_id", "Activity", FieldType::link("cost_activity"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("rate", "Rate", FieldType::Decimal)
                        .unit_price()
                        .required()
                        .help(
                            "Per unit of the activity, in the company's accounting currency: \
                             what a production order absorbs for it.",
                        ),
                )
                .field(created())
                .order_by(&["activity_id"])
                .company_field("company_id"),
            EntityMeta::new("bom_activity", "Bill of materials activity")
                .field(
                    FieldMeta::new("bom_id", "Bill of materials", FieldType::link("bom"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("activity_id", "Activity", FieldType::link("cost_activity"))
                        .required(),
                )
                .field(
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                        .quantity()
                        .required()
                        .help("Units of the activity one run of the bill takes."),
                )
                .field(created())
                .order_by(&["bom_id", "created_at", "id"])
                .display(&["activity_id"])
                .child_of("bom", "bom_id")
                .validator(validate_bom_line),
        ];
        all.extend(routing::entities());
        all
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(ProductionOrder)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Release),
            command(Cancel),
            command(Complete),
            command(Reverse),
            command(routing::RecordTime),
            command(routing::Load),
        ]
    }
}

// ------------------------------------------------------- bills of materials

/// A quantity of a document as its column keeps it: more than zero, at most
/// 6 decimal places. Postgres would round a longer one without saying so.
fn check_qty(qty: Decimal, what: &str) -> Result<()> {
    if qty <= Decimal::ZERO {
        return Err(Error::validation(format!("{what} must be more than zero")));
    }
    if qty.normalize().scale() > 6 {
        return Err(Error::validation(format!(
            "{what} has at most 6 decimal places"
        )));
    }
    Ok(())
}

/// A quantity field of a record must be more than zero, if the caller sent
/// it. The kernel has already refused one that is not a number or has more
/// decimal places than the column keeps.
fn check_qty_field(data: &Map<String, Value>, field: &str, what: &str) -> Result<()> {
    let text = match data.get(field) {
        Some(Value::String(text)) => text.trim().to_owned(),
        Some(Value::Number(number)) => number.to_string(),
        _ => return Ok(()),
    };
    let qty = Decimal::from_str(&text).or_else(|_| Decimal::from_scientific(&text));
    match qty {
        Ok(qty) if qty <= Decimal::ZERO => {
            Err(Error::validation(format!("{what} must be more than zero")))
        }
        _ => Ok(()),
    }
}

fn validate_bom(_: &Registry, data: &Map<String, Value>) -> Result<()> {
    check_qty_field(data, "output_qty", "the output quantity")
}

fn validate_bom_line(_: &Registry, data: &Map<String, Value>) -> Result<()> {
    check_qty_field(data, "qty", "the quantity")
}

// ------------------------------------------------------------ document type

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    bom_id: Uuid,
    qty: Decimal,
    warehouse_id: Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct Order {
    bom_id: Uuid,
    qty: Decimal,
    warehouse_id: Uuid,
    /// The product, from the bill.
    item_id: Uuid,
}

/// One activity of a released order: how much of it, at what rate.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct Activity {
    id: Uuid,
    line_no: i32,
    activity_id: Uuid,
    qty: Decimal,
    rate: Decimal,
}

async fn load_activities(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Activity>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, activity_id, qty, rate FROM production_order_activity
         WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// One component of a released order.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct Component {
    id: Uuid,
    line_no: i32,
    item_id: Uuid,
    qty: Decimal,
}

/// What released production orders of a company will take and make, per
/// item and warehouse: the components they will consume, and the products
/// they will bring in.
pub async fn released(
    conn: &mut PgConnection,
    company_id: Uuid,
) -> Result<(Vec<(Uuid, Uuid, Decimal)>, Vec<(Uuid, Uuid, Decimal)>)> {
    let needs = sqlx::query_as(
        "SELECT c.item_id, o.warehouse_id, sum(c.qty)
         FROM production_order_component c
         JOIN production_order o ON o.document_id = c.document_id
         JOIN document d ON d.id = c.document_id
         WHERE d.company_id = $1 AND d.status = 'released'
         GROUP BY c.item_id, o.warehouse_id",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    let makes = sqlx::query_as(
        "SELECT b.item_id, o.warehouse_id, sum(o.qty)
         FROM production_order o
         JOIN bom b ON b.id = o.bom_id
         JOIN document d ON d.id = o.document_id
         WHERE d.company_id = $1 AND d.status = 'released'
         GROUP BY b.item_id, o.warehouse_id",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok((needs, makes))
}

async fn load_order(conn: &mut PgConnection, document_id: Uuid) -> Result<Order> {
    let order = sqlx::query_as(
        "SELECT o.bom_id, o.qty, o.warehouse_id, b.item_id
         FROM production_order o JOIN bom b ON b.id = o.bom_id
         WHERE o.document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(order)
}

async fn load_components(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Component>> {
    let components = sqlx::query_as(
        "SELECT id, line_no, item_id, qty FROM production_order_component
         WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(components)
}

struct ProductionOrder;

#[async_trait]
impl DocumentHandler for ProductionOrder {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: PRODUCTION_ORDER.into(),
            module: String::new(),
            label: "Production order".into(),
            number_prefix: "MO".into(),
            fields: vec![
                FieldMeta::new("bom_id", "Bill of materials", FieldType::link("bom")).required(),
                FieldMeta::new("qty", "Quantity to make", FieldType::Decimal)
                    .quantity()
                    .unit_of("item_id", "uom")
                    .required(),
                FieldMeta::new("warehouse_id", "Warehouse", FieldType::link("warehouse"))
                    .required(),
                FieldMeta::new("item_id", "Product", FieldType::link("item")).readonly(),
            ],
            collections: vec![
                CollectionMeta {
                    name: "components".into(),
                    label: "Components".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new("item_id", "Item", FieldType::link("item")).readonly(),
                        FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                            .quantity()
                            .unit_of("item_id", "uom")
                            .readonly(),
                    ],
                },
                CollectionMeta {
                    name: "activities".into(),
                    label: "Labor and overhead".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new("activity_id", "Activity", FieldType::link("cost_activity"))
                            .readonly(),
                        FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                            .quantity()
                            .readonly(),
                        FieldMeta::new("rate", "Rate", FieldType::Decimal)
                            .unit_price()
                            .readonly()
                            .help("The company's rate when the order was released."),
                    ],
                },
                CollectionMeta {
                    name: "operations".into(),
                    label: "Operations".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new("seq", "Step", FieldType::Int).readonly(),
                        FieldMeta::new(
                            "work_center_id",
                            "Work center",
                            FieldType::link("work_center"),
                        )
                        .readonly(),
                        FieldMeta::new("description", "Description", FieldType::Text).readonly(),
                        FieldMeta::new("hours", "Standard hours", FieldType::Decimal)
                            .quantity()
                            .readonly(),
                        FieldMeta::new("rate", "Rate", FieldType::Decimal)
                            .unit_price()
                            .readonly(),
                        FieldMeta::new("actual_hours", "Hours recorded", FieldType::Decimal)
                            .quantity()
                            .readonly(),
                    ],
                },
            ],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("released", "Released"),
                StateMeta::locked("completed", "Completed"),
                StateMeta::locked("reversed", "Reversed"),
                StateMeta::locked("cancelled", "Cancelled").unnumbered(),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "released".into()),
                ("draft".into(), "cancelled".into()),
                ("released".into(), "cancelled".into()),
                ("released".into(), "completed".into()),
                ("completed".into(), "reversed".into()),
            ],
            summary: vec![
                FieldMeta::new("item_id", "Product", FieldType::link("item")).readonly(),
                FieldMeta::new("qty", "Quantity to make", FieldType::Decimal)
                    .quantity()
                    .unit_of("item_id", "uom")
                    .readonly(),
                FieldMeta::new("total_value", "Value", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help("Known once completed: what the product was made for."),
            ],
            ..DocTypeMeta::default()
        }
    }

    /// Whether the bill can be used and whether there is enough stock is
    /// decided later, at release and at completion.
    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "production order body")?;
        check_qty(body.qty, "the quantity to make").map_err(|e| e.with_field("qty"))?;
        let known: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM bom WHERE id = $1)")
            .bind(body.bom_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
        if !known {
            return Err(
                Error::validation("bom_id must be a bill of materials").with_field("bom_id")
            );
        }
        let warehouse = [body.warehouse_id];
        if !company::owns_all(&mut *ctx.conn, "warehouse", &warehouse, doc.company_id).await? {
            return Err(
                Error::validation("the warehouse must belong to the document's company")
                    .with_field("warehouse_id"),
            );
        }

        sqlx::query(
            "INSERT INTO production_order (document_id, bom_id, qty, warehouse_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (document_id) DO UPDATE SET
                bom_id = EXCLUDED.bom_id,
                qty = EXCLUDED.qty,
                warehouse_id = EXCLUDED.warehouse_id",
        )
        .bind(doc.id)
        .bind(body.bom_id)
        .bind(body.qty)
        .bind(body.warehouse_id)
        .execute(&mut *ctx.conn)
        .await?;
        Ok(())
    }

    /// The components are empty until the order is released.
    /// What is made and how much, and once the order is completed, the
    /// value the product came into stock with.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let moved = erpcraft_stock::value_moved(conn, doc.id).await?;
        Ok(json!({
            "item_id": body["item_id"],
            "qty": body["qty"],
            "total_value": moved.map(|(made, _)| made),
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let order = load_order(conn, doc.id).await?;
        Ok(json!({
            "bom_id": order.bom_id,
            "qty": order.qty,
            "warehouse_id": order.warehouse_id,
            "item_id": order.item_id,
            "components": load_components(conn, doc.id).await?,
            "activities": load_activities(conn, doc.id).await?,
            "operations": routing::operations(conn, doc.id).await?,
        }))
    }
}

// ----------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocInput {
    /// Production order id.
    id: Uuid,
}

struct Release;

#[async_trait]
impl Command for Release {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "production_order.release",
            "Release a draft production order. It receives its number, can no longer be \
             changed, and takes its components from the bill of materials as the bill is now, \
             scaled to the quantity to make. Later changes to the bill do not change the order. \
             Releasing an order that is already released returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("production_order:release")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PRODUCTION_ORDER, input.id, "release").await?;
        if doc.status == "released" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} production order cannot be released",
                doc.status
            )));
        }
        let order = load_order(&mut *ctx.conn, doc.id).await?;
        // The share lock holds the bill still while its lines are copied.
        let (code, output_qty, active): (String, Decimal, bool) =
            sqlx::query_as("SELECT code, output_qty, active FROM bom WHERE id = $1 FOR SHARE")
                .bind(order.bom_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let facts = json!({ "bom_code": code, "active": active });
        let ruling = ctx
            .policy(INACTIVE_BOM, doc.company_id)
            .await?
            .ruling(facts)?;
        if let Decision::Refused(refusal) = ruling.decision {
            return Err(Error::validation(refusal.message.unwrap_or_else(|| {
                format!("bill of materials {code} is inactive")
            })));
        }
        let lines: Vec<(Uuid, String, Decimal)> = sqlx::query_as(
            "SELECT l.item_id, i.code, l.qty FROM bom_line l JOIN item i ON i.id = l.item_id
             WHERE l.bom_id = $1 ORDER BY l.created_at, l.id",
        )
        .bind(order.bom_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        if lines.is_empty() {
            return Err(Error::validation(format!(
                "bill of materials {code} has no components"
            )));
        }
        // Written before the transition: a released order is locked. One
        // statement for all components.
        let mut items = Vec::with_capacity(lines.len());
        let mut quantities = Vec::with_capacity(lines.len());
        for (item_id, item, per_run) in &lines {
            let qty = per_run
                .checked_mul(order.qty)
                .and_then(|needed| needed.checked_div(output_qty))
                .ok_or_else(|| Error::validation("the quantity to make is too large"))?
                .normalize();
            if qty.scale() > 6 {
                return Err(Error::validation(format!(
                    "making {} takes {} of item {item}, which has more than 6 decimal places; \
                     change the quantity to make so that the components come out even",
                    order.qty.normalize(),
                    qty.round_dp(8),
                )));
            }
            items.push(*item_id);
            quantities.push(qty);
        }
        let ids: Vec<Uuid> = lines.iter().map(|_| new_id()).collect();
        let line_nos: Vec<i32> = (1..=lines.len() as i32).collect();
        sqlx::query(
            "INSERT INTO production_order_component (id, document_id, line_no, item_id, qty)
             SELECT r.id, $1, r.line_no, r.item_id, r.qty
             FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[])
                AS r (id, line_no, item_id, qty)",
        )
        .bind(doc.id)
        .bind(&ids)
        .bind(&line_nos)
        .bind(&items)
        .bind(&quantities)
        .execute(&mut *ctx.conn)
        .await?;
        // The labor and overhead it will absorb, at the company's rates now.
        let activities: Vec<(Uuid, String, Decimal, Option<Decimal>)> = sqlx::query_as(
            "SELECT a.activity_id, c.code, a.qty, r.rate
             FROM bom_activity a JOIN cost_activity c ON c.id = a.activity_id
             LEFT JOIN cost_activity_rate r ON r.activity_id = a.activity_id
                                           AND r.company_id = $2
             WHERE a.bom_id = $1 ORDER BY a.created_at, a.id",
        )
        .bind(order.bom_id)
        .bind(doc.company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        for (index, (activity, code, per_run, rate)) in activities.iter().enumerate() {
            let Some(rate) = rate else {
                return Err(Error::validation(format!(
                    "this company has no rate for activity {code}: add a cost_activity_rate"
                )));
            };
            let qty = per_run
                .checked_mul(order.qty)
                .and_then(|needed| needed.checked_div(output_qty))
                .ok_or_else(|| Error::validation("the quantity to make is too large"))?
                .round_dp(6);
            if qty.is_zero() {
                continue;
            }
            sqlx::query(
                "INSERT INTO production_order_activity (id, document_id, line_no, activity_id,
                     qty, rate)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(activity)
            .bind(qty)
            .bind(rate)
            .execute(&mut *ctx.conn)
            .await?;
        }

        // Its routing, at the company's work centers and rates now.
        routing::release(
            &mut *ctx.conn,
            doc.company_id,
            doc.id,
            order.bom_id,
            order.qty,
        )
        .await?;
        // The components are set aside for it, as far as free stock goes.
        let needs: Vec<erpcraft_stock::Need> = load_components(&mut *ctx.conn, doc.id)
            .await?
            .into_iter()
            .map(|c| erpcraft_stock::Need {
                line_id: c.id,
                item_id: c.item_id,
                warehouse_id: order.warehouse_id,
                qty: c.qty,
            })
            .collect();
        erpcraft_stock::reserve(&mut *ctx.conn, doc.company_id, doc.id, &needs).await?;
        document::transition(ctx, &mut doc, "released").await?;
        document::view(ctx, doc).await
    }
}

struct Cancel;

#[async_trait]
impl Command for Cancel {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "production_order.cancel",
            "Cancel a production order that is not completed. Nothing was posted for it, so \
             nothing is taken back.",
        )
        .from_states(&["draft", "released"])
        .destructive()
        .permission("production_order:cancel")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PRODUCTION_ORDER, input.id, "cancel").await?;
        if doc.status == "cancelled" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if !["draft", "released"].contains(&doc.status.as_str()) {
            return Err(Error::state(format!(
                "a {} production order cannot be cancelled",
                doc.status
            )));
        }
        erpcraft_stock::release(&mut *ctx.conn, doc.id).await?;
        document::transition(ctx, &mut doc, "cancelled").await?;
        document::view(ctx, doc).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompleteInput {
    /// Production order id.
    id: Uuid,
    /// Accounting date of the production. Defaults to the date of the
    /// order.
    posting_date: Option<NaiveDate>,
}

struct Complete;

#[async_trait]
impl Command for Complete {
    type Input = CompleteInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "production_order.complete",
            "Complete a released production order: every component leaves the warehouse at its \
             average cost and the product arrives worth what was consumed plus the labor and \
             overhead the order absorbs (each activity's quantity times its rate). In the \
             general ledger that value moves from each component's inventory account to the \
             product's, where the account rules make them differ, and the absorbed cost is \
             credited to the account for absorbed_cost. If any \
             component is short, nothing is posted, unless the company's policy lets stock go \
             below zero. Completing an order that is already \
             completed returns it unchanged.",
        )
        .from_states(&["released"])
        .permission("production_order:complete")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CompleteInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PRODUCTION_ORDER, input.id, "complete").await?;
        if doc.status == "completed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "released" {
            return Err(Error::state(format!(
                "a {} production order cannot be completed",
                doc.status
            )));
        }
        let order = load_order(&mut *ctx.conn, doc.id).await?;
        let components = load_components(&mut *ctx.conn, doc.id).await?;
        if components.is_empty() {
            return Err(Error::internal(
                "a released production order has no components",
            ));
        }
        // The inventory account of each component and of the product.
        let mut accounts = vec![];
        for item_id in components.iter().map(|c| c.item_id).chain([order.item_id]) {
            accounts.push(
                erpcraft_gl::account_for(
                    &mut *ctx.conn,
                    doc.company_id,
                    "inventory",
                    &[("item", item_id), ("warehouse", order.warehouse_id)],
                )
                .await?,
            );
        }
        let product_account = accounts.pop().unwrap_or_default();

        let posting_date = input.posting_date.unwrap_or(doc.doc_date);
        let posting = document::create_posting(ctx, &doc, posting_date, doc.memo.clone()).await?;

        // Every level this order touches, locked up front in one order, so
        // that two orders sharing items cannot deadlock.
        let pairs = components
            .iter()
            .map(|c| c.item_id)
            .chain([order.item_id])
            .map(|item_id| (item_id, order.warehouse_id))
            .collect();
        erpcraft_stock::lock_levels(&mut *ctx.conn, doc.company_id, pairs).await?;

        let issues: Vec<StockLine> = components
            .iter()
            .map(|component| StockLine {
                item_id: component.item_id,
                warehouse_id: order.warehouse_id,
                qty: -component.qty,
                cost: None,
                source_line_id: Some(component.id),
            })
            .collect();
        let consumed = erpcraft_stock::post(ctx, &posting, &issues, "production").await?;
        // The labor and overhead absorbed: each activity's quantity times
        // its rate, rounded, credited to the account for absorbed cost.
        let decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        let mut absorbed: Vec<(Uuid, Decimal, Uuid)> = vec![];
        for activity in load_activities(&mut *ctx.conn, doc.id).await? {
            let amount = round_money(activity.qty * activity.rate, decimals);
            if amount.is_zero() {
                continue;
            }
            let account = erpcraft_gl::account_for(
                &mut *ctx.conn,
                doc.company_id,
                "absorbed_cost",
                &[("cost_activity", activity.activity_id)],
            )
            .await?;
            absorbed.push((account, amount, activity.id));
        }
        // The routing's standard hours are absorbed the same way, at the
        // work center's activity; the difference from the hours recorded
        // is a variance, posted below, outside the product's value.
        let operations = routing::operations(&mut *ctx.conn, doc.id).await?;
        let mut variances: Vec<(Uuid, Decimal, Uuid, Uuid)> = vec![];
        for operation in &operations {
            let account = erpcraft_gl::account_for(
                &mut *ctx.conn,
                doc.company_id,
                "absorbed_cost",
                &[("cost_activity", operation.activity_id)],
            )
            .await?;
            let amount = round_money(operation.hours * operation.rate, decimals);
            if !amount.is_zero() {
                absorbed.push((account, amount, operation.id));
            }
            let recorded: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM production_operation_time WHERE operation_id = $1)",
            )
            .bind(operation.id)
            .fetch_one(&mut *ctx.conn)
            .await?;
            if recorded {
                let actual = round_money(operation.actual_hours * operation.rate, decimals);
                if actual != amount {
                    variances.push((
                        account,
                        actual - amount,
                        operation.id,
                        operation.work_center_id,
                    ));
                }
            }
        }
        // What was consumed and absorbed is what the product is worth, to
        // the cent.
        let total: Decimal = consumed.iter().map(|entry| -entry.value).sum::<Decimal>()
            + absorbed
                .iter()
                .map(|(_, amount, _)| *amount)
                .sum::<Decimal>();
        let made = erpcraft_stock::post(
            ctx,
            &posting,
            &[StockLine {
                item_id: order.item_id,
                warehouse_id: order.warehouse_id,
                qty: order.qty,
                cost: Some(Cost::Total(total)),
                source_line_id: None,
            }],
            "production",
        )
        .await?;
        // Not zero only where the product stood below zero, because some
        // of it had left before it was made: the part of its cost that
        // differs from what the missing goods left stock for.
        let variance: Decimal = made.iter().map(|entry| entry.variance).sum();

        // The value moves from each component's account to the product's.
        // A component kept on the product's own account moves nothing.
        let line = |account_id, debit, credit, source_line_id| GlLine {
            account_id,
            debit,
            credit,
            currency: None,
            fx_rate: None,
            memo: None,
            source_line_id,
        };
        let mut gl: Vec<GlLine> = consumed
            .iter()
            .zip(&accounts)
            .filter(|(entry, account)| !entry.value.is_zero() && **account != product_account)
            .map(|(entry, account)| {
                line(*account, Decimal::ZERO, -entry.value, entry.source_line_id)
            })
            .collect();
        gl.extend(absorbed.iter().map(|(account, amount, source)| {
            line(*account, Decimal::ZERO, *amount, Some(*source))
        }));
        // The product's account takes what came from other accounts, less
        // the variance, which is not stock value.
        let moved: Decimal = gl.iter().map(|l| l.credit).sum();
        let to_product = moved - variance;
        if !to_product.is_zero() {
            gl.push(line(
                product_account,
                to_product.max(Decimal::ZERO),
                (-to_product).max(Decimal::ZERO),
                None,
            ));
        }
        if !variance.is_zero() {
            let account = erpcraft_gl::account_for(
                &mut *ctx.conn,
                doc.company_id,
                "stock_variance",
                &[("item", order.item_id), ("warehouse", order.warehouse_id)],
            )
            .await?;
            gl.push(line(
                account,
                variance.max(Decimal::ZERO),
                (-variance).max(Decimal::ZERO),
                None,
            ));
        }
        // More time than standard: the difference is a cost of production
        // the product does not carry, and the absorbed cost account takes
        // what the time was really worth. Less time: the other way round.
        for (absorbed_account, difference, source, center) in variances {
            let account = erpcraft_gl::account_for(
                &mut *ctx.conn,
                doc.company_id,
                "production_variance",
                &[("work_center", center), ("item", order.item_id)],
            )
            .await?;
            let more = difference.max(Decimal::ZERO);
            let less = (-difference).max(Decimal::ZERO);
            gl.push(line(account, more, less, Some(source)));
            gl.push(line(absorbed_account, less, more, Some(source)));
        }
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }

        // Consumed: nothing is held for it any more.
        erpcraft_stock::release(&mut *ctx.conn, doc.id).await?;
        document::transition(ctx, &mut doc, "completed").await?;
        document::view(ctx, doc).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReverseInput {
    /// Production order id.
    id: Uuid,
    /// Accounting date of the reversal. Defaults to the date of the
    /// original posting.
    posting_date: Option<NaiveDate>,
    /// Memo of the reversing posting. Defaults to "Reversal of" and the
    /// document number.
    memo: Option<String>,
}

struct Reverse;

#[async_trait]
impl Command for Reverse {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "production_order.reverse",
            "Cancel a completed production order by a second posting that takes the product \
             out of stock and puts every component back at the value it left for, in both \
             ledgers. Refused if the product has since left the warehouse.",
        )
        .from_states(&["completed"])
        .destructive()
        .permission("production_order:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PRODUCTION_ORDER, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "completed" {
            return Err(Error::state(format!(
                "a {} production order cannot be reversed",
                doc.status
            )));
        }
        let original = document::original_posting(&mut *ctx.conn, &doc).await?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}
