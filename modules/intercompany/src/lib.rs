//! Intercompany trading: companies of one database that sell to each other.
//!
//! Each company keeps its own books. What one company sells to a customer
//! that stands for another company is mirrored, as a draft, in the buying
//! company: an approved sales order as a purchase order, a posted sales
//! invoice as a vendor invoice. A subscriber of the outbox hears the
//! seller's command after it committed and runs `intercompany.mirror` as a
//! user of the buyer through the command executor, so the buyer's rights
//! and policies apply. Nothing is posted in the buyer's books.
//!
//! The module reads and drafts documents only through the document
//! handlers and the kernel's document functions, as any module would.

use std::sync::Arc;

mod consolidation;

use chrono::NaiveDate;
use erpcraft_kernel::authz::load_actor;
use erpcraft_kernel::command::{Options, execute};
use erpcraft_kernel::document::{self, Document, DocumentView, NewDocument};
use erpcraft_kernel::outbox::{Event, Subscriber};
use erpcraft_kernel::{
    Actor, Command, CommandSpec, Ctx, Decimal, DynCommand, EntityMeta, Error, FieldMeta, FieldType,
    JsonSchema, Kernel, Migration, Module, PgConnection, Result, async_trait, command, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

const SALES_ORDER: &str = "sales_order";
const SALES_INVOICE: &str = "sales_invoice";
const PURCHASE_ORDER: &str = "purchase_order";
const VENDOR_INVOICE: &str = "vendor_invoice";
const MIRROR: &str = "intercompany.mirror";

pub struct IntercompanyModule;

impl Module for IntercompanyModule {
    fn name(&self) -> &'static str {
        "intercompany"
    }

    fn label(&self) -> String {
        "Intercompany".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["sales", "purchasing", "open_items"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "intercompany",
                sql: include_str!("../migrations/0001_intercompany.sql"),
            },
            Migration {
                version: 2,
                name: "consolidation",
                sql: include_str!("../migrations/0002_consolidation.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        let mut entities = vec![
            EntityMeta::new("intercompany_party", "Intercompany party")
                .field(
                    FieldMeta::new(
                        "party_type",
                        "Party type",
                        FieldType::options(&["customer", "vendor"]),
                    )
                    .required(),
                )
                .field(
                    FieldMeta::new(
                        "party_id",
                        "Party",
                        FieldType::entity_named_by("party_type"),
                    )
                    .required(),
                )
                .field(
                    FieldMeta::new("company_id", "Stands for", FieldType::link("company"))
                        .required()
                        .immutable()
                        .help("The company this customer or vendor is."),
                )
                .field(created())
                .order_by(&["party_type", "party_id"])
                .display(&["party_type", "party_id"])
                .plural("Intercompany parties")
                .written_together(&["party_type", "party_id"])
                .company_field("company_id"),
            EntityMeta::new("intercompany_rule", "Intercompany rule")
                .field(
                    FieldMeta::new("company_id", "Selling company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new(
                        "buyer_company_id",
                        "Buying company",
                        FieldType::link("company"),
                    )
                    .required()
                    .immutable(),
                )
                .field(
                    FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                        .required()
                        .help("The seller's customer that stands for the buying company."),
                )
                .field(
                    FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor"))
                        .required()
                        .help("The buyer's vendor that stands for the selling company."),
                )
                .field(
                    FieldMeta::new("run_as_user_id", "Drafted as", FieldType::link("user"))
                        .required()
                        .help(
                            "A user of the buying company: the drafts are made as them, with \
                             their rights and the buying company's policies.",
                        ),
                )
                .field(
                    FieldMeta::new(
                        "warehouse_id",
                        "Receiving warehouse",
                        FieldType::link("warehouse"),
                    )
                    .help("The buying company's warehouse goods are ordered into."),
                )
                .field(
                    FieldMeta::new("account_id", "Account", FieldType::link("account")).help(
                        "The buying company's account for lines the seller put on an account.",
                    ),
                )
                .field(FieldMeta::new(
                    "mirror_orders",
                    "Mirror sales orders",
                    FieldType::Bool,
                ))
                .field(FieldMeta::new(
                    "mirror_invoices",
                    "Mirror sales invoices",
                    FieldType::Bool,
                ))
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["company_id", "customer_id"])
                .display(&["company_id", "buyer_company_id"])
                .write_check(check_rule_write)
                .company_field("company_id"),
            EntityMeta::new("intercompany_document", "Intercompany document")
                .field(FieldMeta::new(
                    "source_id",
                    "Source",
                    FieldType::document(SALES_INVOICE),
                ))
                .field(
                    FieldMeta::new("company_id", "Selling company", FieldType::link("company"))
                        .immutable(),
                )
                .field(FieldMeta::new(
                    "buyer_company_id",
                    "Buying company",
                    FieldType::link("company"),
                ))
                .field(FieldMeta::new(
                    "rule_id",
                    "Rule",
                    FieldType::link("intercompany_rule"),
                ))
                .field(FieldMeta::new("mirror_id", "Mirror", FieldType::Uuid))
                .field(FieldMeta::new(
                    "status",
                    "Status",
                    FieldType::options(&["drafted", "refused"]),
                ))
                .field(FieldMeta::new("message", "Why not", FieldType::Text))
                .field(FieldMeta::new(
                    "updated_at",
                    "Updated",
                    FieldType::Timestamp,
                ))
                .order_by(&["updated_at"])
                .display(&["source_id", "status"])
                .read_only()
                .company_field("company_id"),
        ];
        entities.extend(consolidation::entities());
        entities
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Mirror),
            command(Balances),
            command(consolidation::Report),
        ]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(Mirroring)]
    }
}

/// A rule makes drafts in the buying company, so writing one takes the
/// right to change rules there too, not only in the selling company.
fn check_rule_write(
    actor: &Actor,
    before: Option<&Value>,
    fields: &Map<String, Value>,
) -> Result<()> {
    let buyer = fields
        .get("buyer_company_id")
        .or_else(|| before.and_then(|b| b.get("buyer_company_id")))
        .and_then(Value::as_str)
        .and_then(|id| id.parse::<Uuid>().ok());
    match buyer {
        Some(buyer) if actor.can("intercompany_rule:update", buyer) => Ok(()),
        Some(_) => Err(Error::forbidden(
            "an intercompany rule makes drafts in the buying company: it takes the right to \
             change intercompany rules there too",
        )),
        None => Ok(()),
    }
}

// ------------------------------------------------------------- the rule

#[derive(Debug, Clone, sqlx::FromRow)]
struct Rule {
    id: Uuid,
    buyer_company_id: Uuid,
    vendor_id: Uuid,
    run_as_user_id: Uuid,
    warehouse_id: Option<Uuid>,
    account_id: Option<Uuid>,
    mirror_orders: bool,
    mirror_invoices: bool,
}

async fn rule_for(
    conn: &mut PgConnection,
    company_id: Uuid,
    customer_id: Uuid,
) -> Result<Option<Rule>> {
    Ok(sqlx::query_as(
        "SELECT id, buyer_company_id, vendor_id, run_as_user_id, warehouse_id,
                account_id, mirror_orders, mirror_invoices
         FROM intercompany_rule WHERE company_id = $1 AND customer_id = $2 AND active",
    )
    .bind(company_id)
    .bind(customer_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// A source document with its body, read through its handler.
struct Source {
    doc: Document,
    body: Value,
    customer_id: Uuid,
}

async fn load_doc(conn: &mut PgConnection, id: Uuid) -> Result<Option<Document>> {
    Ok(sqlx::query_as("SELECT * FROM document WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?)
}

async fn load_source(kernel: &Kernel, conn: &mut PgConnection, id: Uuid) -> Result<Source> {
    let doc = load_doc(conn, id)
        .await?
        .ok_or_else(|| Error::not_found("document"))?;
    if doc.doc_type != SALES_ORDER && doc.doc_type != SALES_INVOICE {
        return Err(Error::validation(
            "only a sales order or a sales invoice is mirrored",
        ));
    }
    let handler = kernel.registry().doc_type(&doc.doc_type)?.handler.clone();
    let body = handler.load_body(conn, &doc).await?;
    let customer = match document::body::uuid(&body["customer_id"]) {
        Some(customer) => customer,
        None => {
            let order = document::body::uuid(&body["sales_order_id"])
                .ok_or_else(|| Error::internal("a sales invoice without customer or order"))?;
            let order = load_doc(conn, order)
                .await?
                .ok_or_else(|| Error::not_found("sales_order"))?;
            let order_body = kernel
                .registry()
                .doc_type(SALES_ORDER)?
                .handler
                .load_body(conn, &order)
                .await?;
            document::body::uuid(&order_body["customer_id"])
                .ok_or_else(|| Error::internal("a sales order without customer"))?
        }
    };
    Ok(Source {
        doc,
        body,
        customer_id: customer,
    })
}

// ------------------------------------------------------------ mirroring

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MirrorInput {
    /// The approved sales order or posted sales invoice to mirror.
    #[schemars(extend("x-field-type" = { "kind": "document", "doc_type": "sales_invoice", "states": [] }))]
    source_id: Uuid,
}

struct Mirror;

#[async_trait]
impl Command for Mirror {
    type Input = MirrorInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            MIRROR,
            "Draft in the buying company what another company of this database sold to it: \
             for an approved sales order a purchase order, for a posted sales invoice a vendor \
             invoice (against the mirrored purchase order where there is one), both of the \
             vendor the intercompany rule names, as a draft for the buyer to check and post. \
             Run by the outbox as the rule's user when the order is approved or the invoice \
             posted; run it by hand to try again once a refusal is fixed. A document mirrored \
             already returns its mirror.",
        )
        .permission("intercompany:mirror")
        .creates(VENDOR_INVOICE)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: MirrorInput) -> Result<DocumentView> {
        let kernel = ctx.kernel;
        let source = load_source(kernel, &mut *ctx.conn, input.source_id).await?;
        let wanted = match source.doc.doc_type.as_str() {
            SALES_ORDER => ["open", "closed"].contains(&source.doc.status.as_str()),
            _ => source.doc.status == "posted",
        };
        if !wanted {
            return Err(Error::state(format!(
                "a {} {} is not mirrored: only an approved order or a posted invoice is",
                source.doc.status,
                source.doc.doc_type.replace('_', " ")
            )));
        }
        let rule = rule_for(&mut *ctx.conn, source.doc.company_id, source.customer_id)
            .await?
            .ok_or_else(|| {
                Error::validation("no active intercompany rule covers the document's customer")
            })?;
        ctx.require("intercompany:mirror", rule.buyer_company_id)?;

        // Once: a mirror that still exists is the answer.
        let done: Option<Uuid> = sqlx::query_scalar(
            "SELECT mirror_id FROM intercompany_document
             WHERE source_id = $1 AND status = 'drafted' AND mirror_id IS NOT NULL
             FOR UPDATE",
        )
        .bind(source.doc.id)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .flatten();
        if let Some(mirror) = done {
            ctx.no_change();
            let doc_type = if source.doc.doc_type == SALES_ORDER {
                PURCHASE_ORDER
            } else {
                VENDOR_INVOICE
            };
            return document::get(ctx, doc_type, mirror).await;
        }

        let seller_code: String = sqlx::query_scalar("SELECT code FROM company WHERE id = $1")
            .bind(source.doc.company_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
        let memo = Some(format!(
            "Intercompany: {} {} of {seller_code}",
            if source.doc.doc_type == SALES_ORDER {
                "sales order"
            } else {
                "sales invoice"
            },
            source.doc.number.clone().unwrap_or_default()
        ));
        let (doc_type, body, source_lines) = if source.doc.doc_type == SALES_ORDER {
            order_mirror(&source, &rule)?
        } else {
            invoice_mirror(ctx, &source, &rule).await?
        };
        let view = document::create(
            ctx,
            NewDocument {
                doc_type: doc_type.into(),
                company_id: rule.buyer_company_id,
                doc_date: source.doc.doc_date,
                memo,
                body,
            },
        )
        .await?;
        let mirror_id = view.header.id;
        sqlx::query(
            "INSERT INTO intercompany_document (source_id, company_id, buyer_company_id, rule_id,
                 mirror_id, status, message, updated_at)
             VALUES ($1, $2, $3, $4, $5, 'drafted', NULL, now())
             ON CONFLICT (source_id) DO UPDATE SET mirror_id = EXCLUDED.mirror_id,
                 rule_id = EXCLUDED.rule_id, status = 'drafted', message = NULL, updated_at = now()",
        )
        .bind(source.doc.id)
        .bind(source.doc.company_id)
        .bind(rule.buyer_company_id)
        .bind(rule.id)
        .bind(mirror_id)
        .execute(&mut *ctx.conn)
        .await?;
        // The mirror's lines in the order they were given.
        let mirror_lines: Vec<Uuid> = view.body["lines"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|l| document::body::uuid(&l["id"]))
                    .collect()
            })
            .unwrap_or_default();
        if source.doc.doc_type == SALES_ORDER && mirror_lines.len() == source_lines.len() {
            sqlx::query(
                "INSERT INTO intercompany_line (source_line_id, mirror_line_id, source_id, mirror_id)
                 SELECT s, m, $3, $4 FROM unnest($1::uuid[], $2::uuid[]) AS u(s, m)
                 ON CONFLICT (source_line_id) DO UPDATE SET mirror_line_id = EXCLUDED.mirror_line_id,
                     mirror_id = EXCLUDED.mirror_id",
            )
            .bind(&source_lines)
            .bind(&mirror_lines)
            .bind(source.doc.id)
            .bind(mirror_id)
            .execute(&mut *ctx.conn)
            .await?;
        }
        document::link(ctx, source.doc.id, mirror_id, "intercompany").await?;
        Ok(view)
    }
}

/// The parts of a sold line a mirrored line keeps.
fn priced(line: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for field in [
        "description",
        "qty",
        "unit_price",
        "discount_percent",
        "discount_amount",
    ] {
        if !line[field].is_null() {
            out.insert(field.into(), line[field].clone());
        }
    }
    out
}

/// A line the seller put on its own account goes to the rule's account of
/// the buyer.
fn on_account(rule: &Rule, n: usize) -> Result<Value> {
    rule.account_id.map(|a| json!(a)).ok_or_else(|| {
        Error::validation(format!(
            "line {n} is on an account, and the intercompany rule names no account of the \
             buying company for it"
        ))
    })
}

/// A purchase order for an approved sales order.
fn order_mirror(source: &Source, rule: &Rule) -> Result<(&'static str, Value, Vec<Uuid>)> {
    if !rule.mirror_orders {
        return Err(Error::validation(
            "the intercompany rule does not mirror sales orders",
        ));
    }
    let mut lines = vec![];
    let mut ids = vec![];
    for (index, line) in source.body["lines"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let mut out = priced(line);
        // Bought in the unit it was sold in: items, and their units, are
        // shared by the two companies.
        if !line["uom"].is_null() {
            out.insert("uom".into(), line["uom"].clone());
        }
        if !line["item_id"].is_null() {
            let warehouse = rule.warehouse_id.ok_or_else(|| {
                Error::validation(
                    "the order has goods, and the intercompany rule names no receiving \
                     warehouse of the buying company",
                )
            })?;
            out.insert("item_id".into(), line["item_id"].clone());
            out.insert("warehouse_id".into(), json!(warehouse));
        } else if !line["charge_id"].is_null() {
            out.insert("charge_id".into(), line["charge_id"].clone());
        } else {
            out.insert("account_id".into(), on_account(rule, index + 1)?);
        }
        if let Some(id) = document::body::uuid(&line["id"]) {
            ids.push(id);
        }
        lines.push(Value::Object(out));
    }
    Ok((
        PURCHASE_ORDER,
        json!({
            "vendor_id": rule.vendor_id,
            "currency": source.body["currency"],
            "lines": lines,
        }),
        ids,
    ))
}

/// A vendor invoice for a posted sales invoice: against the purchase order
/// its sales order was mirrored into, where there is one.
async fn invoice_mirror(
    ctx: &mut Ctx<'_>,
    source: &Source,
    rule: &Rule,
) -> Result<(&'static str, Value, Vec<Uuid>)> {
    if !rule.mirror_invoices {
        return Err(Error::validation(
            "the intercompany rule does not mirror sales invoices",
        ));
    }
    let order = document::body::uuid(&source.body["sales_order_id"]);
    let mirrored_order: Option<Uuid> = match order {
        Some(order) => sqlx::query_scalar(
            "SELECT mirror_id FROM intercompany_document
             WHERE source_id = $1 AND status = 'drafted' AND mirror_id IS NOT NULL",
        )
        .bind(order)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .flatten(),
        None => None,
    };
    // The sold order's lines, to tell goods from charges.
    let order_lines: Vec<Value> = match order {
        Some(order) => {
            let doc = load_doc(&mut *ctx.conn, order)
                .await?
                .ok_or_else(|| Error::not_found("sales_order"))?;
            let handler = ctx.kernel.registry().doc_type(SALES_ORDER)?.handler.clone();
            handler.load_body(&mut *ctx.conn, &doc).await?["lines"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        }
        None => vec![],
    };
    let mut lines = vec![];
    for (index, line) in source.body["lines"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let n = index + 1;
        let mut out = priced(line);
        let sold = document::body::uuid(&line["order_line_id"]).and_then(|id| {
            order_lines
                .iter()
                .find(|l| document::body::uuid(&l["id"]) == Some(id))
        });
        let mirror_line: Option<Uuid> = match document::body::uuid(&line["order_line_id"]) {
            Some(id) if mirrored_order.is_some() => {
                sqlx::query_scalar(
                    "SELECT mirror_line_id FROM intercompany_line WHERE source_line_id = $1",
                )
                .bind(id)
                .fetch_optional(&mut *ctx.conn)
                .await?
            }
            _ => None,
        };
        let charge = sold
            .map(|l| l["charge_id"].clone())
            .unwrap_or_else(|| line["charge_id"].clone());
        if let Some(mirror_line) = mirror_line {
            out.insert("order_line_id".into(), json!(mirror_line));
        } else if sold.is_some_and(|l| !l["item_id"].is_null()) {
            return Err(Error::validation(format!(
                "line {n} is goods, and the sales order was not mirrored into a purchase order: \
                 goods are invoiced against an order"
            )));
        } else if !charge.is_null() {
            out.insert("charge_id".into(), charge);
        } else {
            out.insert("account_id".into(), on_account(rule, n)?);
        }
        lines.push(Value::Object(out));
    }
    let mut body = json!({
        "vendor_reference": source.doc.number,
        "lines": lines,
    });
    match mirrored_order {
        Some(order) => body["purchase_order_id"] = json!(order),
        None => {
            body["vendor_id"] = json!(rule.vendor_id);
            body["currency"] = source.body["currency"].clone();
        }
    }
    Ok((VENDOR_INVOICE, body, vec![]))
}

/// Hears the seller's commands and mirrors as the rule's user.
struct Mirroring;

#[async_trait]
impl Subscriber for Mirroring {
    fn name(&self) -> &'static str {
        "intercompany"
    }

    fn wants(&self, topic: &str) -> bool {
        topic == "sales_order.approve" || topic == "sales_invoice.post"
    }

    async fn handle(&self, kernel: &Kernel, event: &Event) -> Result<()> {
        let Some(source_id) = document::body::uuid(&event.payload["input"]["id"]) else {
            return Ok(());
        };
        let mut conn = kernel.pool().acquire().await?;
        let Some(doc) = load_doc(&mut conn, source_id).await? else {
            return Ok(());
        };
        // An approval request, a refusal or a reversal since: nothing to do.
        let wanted = if event.topic == "sales_order.approve" {
            doc.status == "open" || doc.status == "closed"
        } else {
            doc.status == "posted"
        };
        if !wanted {
            return Ok(());
        }
        let source = load_source(kernel, &mut conn, source_id).await?;
        let Some(rule) = rule_for(&mut conn, doc.company_id, source.customer_id).await? else {
            return Ok(());
        };
        let wants = if doc.doc_type == SALES_ORDER {
            rule.mirror_orders
        } else {
            rule.mirror_invoices
        };
        if !wants {
            return Ok(());
        }
        let outcome = match load_actor(kernel.pool(), rule.run_as_user_id, None).await {
            Ok(actor) => {
                execute(
                    kernel,
                    &actor,
                    MIRROR,
                    json!({ "source_id": source_id }),
                    Options {
                        idempotency_key: Some(format!("intercompany:{source_id}")),
                        ..Options::default()
                    },
                )
                .await
            }
            Err(error) => Err(error),
        };
        match outcome {
            Ok(_) => Ok(()),
            // A failure of the server is tried again; a refusal is kept,
            // to be seen and run again by hand once it is fixed.
            Err(Error::Internal(error)) => Err(Error::Internal(error)),
            Err(error) => {
                tracing::info!(%source_id, %error, "intercompany mirror refused");
                refused(&mut conn, &doc, &rule, &error.to_string()).await
            }
        }
    }
}

async fn refused(conn: &mut PgConnection, doc: &Document, rule: &Rule, why: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO intercompany_document (source_id, company_id, buyer_company_id, rule_id,
             status, message, updated_at)
         VALUES ($1, $2, $3, $4, 'refused', $5, now())
         ON CONFLICT (source_id) DO UPDATE SET status = 'refused', message = EXCLUDED.message,
             updated_at = now()
         WHERE intercompany_document.mirror_id IS NULL",
    )
    .bind(doc.id)
    .bind(doc.company_id)
    .bind(rule.buyer_company_id)
    .bind(rule.id)
    .bind(why)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ------------------------------------------------------------- balances

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BalancesInput {
    /// Rules where this company sells or buys.
    company_id: Uuid,
    /// Count what was posted by this day. Today when omitted.
    as_of: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct BalanceRow {
    rule_id: Uuid,
    seller_company_id: Uuid,
    seller_code: String,
    buyer_company_id: Uuid,
    buyer_code: String,
    currency: String,
    /// What the seller has open on the buyer's customer, in `currency`.
    seller_receivable: Decimal,
    /// What the buyer has open on the seller's vendor, in `currency`.
    buyer_payable: Decimal,
    /// Receivable less payable: zero when both books agree.
    difference: Decimal,
    /// The receivable at the value the seller carries it, in the seller's
    /// accounting currency.
    seller_value: Decimal,
    seller_currency: String,
    /// The payable at the value the buyer carries it, in the buyer's
    /// accounting currency.
    buyer_value: Decimal,
    buyer_currency: String,
}

/// A document of one side whose mirror is not posted (or not there).
#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct Unmatched {
    source_id: Uuid,
    source_number: Option<String>,
    company_id: Uuid,
    status: String,
    mirror_id: Option<Uuid>,
    mirror_number: Option<String>,
    mirror_status: Option<String>,
    message: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct BalancesOutput {
    company_id: Uuid,
    as_of: NaiveDate,
    rows: Vec<BalanceRow>,
    /// Sales invoices posted by then whose vendor invoice is not posted:
    /// what usually explains a difference.
    unmatched: Vec<Unmatched>,
    /// Rules left out because the caller may not read open items in both
    /// companies.
    omitted: i64,
}

struct Balances;

/// What is open of the items of one company, kind and party, per currency,
/// as of a day: in the item's currency and at the value it is carried at.
const OPEN: &str = "
    SELECT COALESCE(i.currency, c.base_currency) AS currency,
           sum(COALESCE(i.txn_amount, i.amount) - COALESCE((
               SELECT sum(COALESCE(s.txn_amount, s.amount)) FROM settlement s
               JOIN posting sp ON sp.id = s.posting_id
               WHERE s.open_item_id = i.id AND sp.posting_date <= $5), 0)) AS open,
           sum(i.amount - COALESCE((
               SELECT sum(s.amount) FROM settlement s JOIN posting sp ON sp.id = s.posting_id
               WHERE s.open_item_id = i.id AND sp.posting_date <= $5), 0)
               + COALESCE((
               SELECT sum(v.amount) FROM open_item_revaluation v
               JOIN posting vp ON vp.id = v.posting_id
               WHERE v.open_item_id = i.id AND vp.posting_date <= $5), 0)) AS value
    FROM open_item i
    JOIN posting p ON p.id = i.posting_id
    JOIN company c ON c.id = i.company_id
    WHERE i.company_id = $1 AND i.kind = $2 AND i.party_type = $3 AND i.party_id = $4
      AND p.posting_date <= $5
    GROUP BY 1";

#[async_trait]
impl Command for Balances {
    type Input = BalancesInput;
    type Output = BalancesOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "intercompany.balances",
            "What companies of this database owe each other, from both sides, as of a day: for \
             each intercompany rule where the company sells or buys, and each currency, what \
             the seller has open on the customer that stands for the buyer and what the buyer \
             has open on the vendor that stands for the seller, with the difference, and each \
             at the value its own books carry it in their accounting currency. Also the sales \
             invoices whose vendor invoice is not posted yet, which usually explain a \
             difference. A rule is left out unless the caller may read open items in both \
             companies.",
        )
        .report("Intercompany balances")
        .permission("open_item:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: BalancesInput) -> Result<BalancesOutput> {
        ctx.require("open_item:read", input.company_id)?;
        let as_of = input
            .as_of
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        #[derive(sqlx::FromRow)]
        struct RuleRow {
            id: Uuid,
            company_id: Uuid,
            seller_code: String,
            seller_currency: String,
            buyer_company_id: Uuid,
            buyer_code: String,
            buyer_currency: String,
            customer_id: Uuid,
            vendor_id: Uuid,
        }
        let rules: Vec<RuleRow> = sqlx::query_as(
            "SELECT r.id, r.company_id, s.code AS seller_code, s.base_currency AS seller_currency,
                    r.buyer_company_id, b.code AS buyer_code, b.base_currency AS buyer_currency,
                    r.customer_id, r.vendor_id
             FROM intercompany_rule r
             JOIN company s ON s.id = r.company_id JOIN company b ON b.id = r.buyer_company_id
             WHERE $1 IN (r.company_id, r.buyer_company_id)
             ORDER BY s.code, b.code",
        )
        .bind(input.company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut rows = vec![];
        let mut unmatched = vec![];
        let mut omitted = 0;
        for rule in rules {
            if !(ctx.actor.can("open_item:read", rule.company_id)
                && ctx.actor.can("open_item:read", rule.buyer_company_id))
            {
                omitted += 1;
                continue;
            }
            let side = |company: Uuid, kind: &'static str, party: &'static str, id: Uuid| {
                (company, kind, party, id)
            };
            let mut sums: std::collections::BTreeMap<String, [Decimal; 4]> = Default::default();
            for (slot, (company, kind, party, id)) in [
                side(rule.company_id, "receivable", "customer", rule.customer_id),
                side(rule.buyer_company_id, "payable", "vendor", rule.vendor_id),
            ]
            .into_iter()
            .enumerate()
            {
                let open: Vec<(String, Option<Decimal>, Option<Decimal>)> = sqlx::query_as(OPEN)
                    .bind(company)
                    .bind(kind)
                    .bind(party)
                    .bind(id)
                    .bind(as_of)
                    .fetch_all(&mut *ctx.conn)
                    .await?;
                for (currency, open, value) in open {
                    let entry = sums.entry(currency).or_default();
                    entry[slot] += open.unwrap_or_default();
                    entry[slot + 2] += value.unwrap_or_default();
                }
            }
            for (currency, [receivable, payable, seller_value, buyer_value]) in sums {
                if receivable.is_zero() && payable.is_zero() {
                    continue;
                }
                rows.push(BalanceRow {
                    rule_id: rule.id,
                    seller_company_id: rule.company_id,
                    seller_code: rule.seller_code.clone(),
                    buyer_company_id: rule.buyer_company_id,
                    buyer_code: rule.buyer_code.clone(),
                    currency,
                    seller_receivable: receivable,
                    buyer_payable: payable,
                    difference: receivable - payable,
                    seller_value,
                    seller_currency: rule.seller_currency.clone(),
                    buyer_value,
                    buyer_currency: rule.buyer_currency.clone(),
                });
            }
            let mut open: Vec<Unmatched> = sqlx::query_as(
                "SELECT d.id AS source_id, d.number AS source_number, d.company_id,
                        COALESCE(x.status, 'not mirrored') AS status, x.mirror_id,
                        m.number AS mirror_number, m.status AS mirror_status, x.message
                 FROM document d
                 JOIN posting p ON p.document_id = d.id AND p.reverses_id IS NULL
                 LEFT JOIN intercompany_document x ON x.source_id = d.id
                 LEFT JOIN document m ON m.id = x.mirror_id
                 WHERE d.company_id = $1 AND d.doc_type = 'sales_invoice' AND d.status = 'posted'
                   AND p.posting_date <= $3
                   AND EXISTS (SELECT 1 FROM open_item i WHERE i.posting_id = p.id
                               AND i.party_type = 'customer' AND i.party_id = $2)
                   AND (m.id IS NULL OR m.status <> 'posted')
                 ORDER BY d.doc_date, d.number",
            )
            .bind(rule.company_id)
            .bind(rule.customer_id)
            .bind(as_of)
            .fetch_all(&mut *ctx.conn)
            .await?;
            unmatched.append(&mut open);
        }
        Ok(BalancesOutput {
            company_id: input.company_id,
            as_of,
            rows,
            unmatched,
            omitted,
        })
    }
}
