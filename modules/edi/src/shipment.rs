//! Packing a delivery to an EDI partner, and shipping it.
//!
//! Each pallet and carton gets an SSCC from the company's GS1 prefix and
//! the next serial of `edi_settings`, taken in a locked row, so no serial
//! is issued twice. Repacking voids the packs before; their SSCCs are
//! never issued again. `ship` checks the packing is complete, posts the
//! delivery through `sales_delivery.post` and makes the 856, in one
//! transaction: the notice exists the moment the goods are posted as gone.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use erpcraft_kernel::document::{self, DocumentView};
use erpcraft_kernel::{Ctx, Decimal, Error, Result, new_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::gs1;
use crate::outbound::{self, Generated, Message, SentOrder};

pub const PERMISSION: &str = "edi:ship";

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Carrier {
    /// The carrier's SCAC, two to four capitals.
    pub carrier_scac: Option<String>,
    /// The carrier's name.
    pub carrier_name: Option<String>,
    /// The service level.
    pub service: Option<String>,
    /// The bill of lading number.
    pub bill_of_lading: Option<String>,
    /// The carrier's PRO or tracking number.
    pub tracking: Option<String>,
    /// When the goods leave.
    pub shipped_at: Option<DateTime<Utc>>,
    /// When the carrier expects to deliver.
    pub estimated_delivery: Option<NaiveDate>,
    /// The shipment's gross weight.
    pub gross_weight: Option<Decimal>,
    /// The weight's unit, such as LB or KG.
    pub weight_uom: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackKind {
    Pallet,
    Carton,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackLineInput {
    /// A line of the delivery.
    pub delivery_line_id: Uuid,
    /// How much of it is in this carton, in the item's own unit.
    pub qty: Decimal,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackInput {
    /// `pallet` or `carton`.
    pub kind: PackKind,
    /// For a carton on a pallet: the pallet's place in `packs`, from 1.
    pub parent: Option<usize>,
    /// The pack's weight.
    pub weight: Option<Decimal>,
    /// What a carton holds; a pallet holds cartons and has none.
    #[serde(default)]
    pub lines: Vec<PackLineInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PackOut {
    pub id: Uuid,
    pub kind: String,
    pub sscc: String,
    pub parent_id: Option<Uuid>,
    /// `[{delivery_line_id, qty}]`.
    pub lines: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ShipmentOut {
    pub delivery_id: Uuid,
    /// `packed` when the cartons hold exactly what the delivery delivers.
    pub state: String,
    pub packs: Vec<PackOut>,
    /// What keeps the shipment from being packed, in sentences.
    pub missing: Vec<String>,
}

/// The delivery, the partner order it follows, and a check that the
/// caller may pack it.
async fn delivery(ctx: &mut Ctx<'_>, delivery_id: Uuid) -> Result<(DocumentView, SentOrder)> {
    let view = document::get(ctx, "sales_delivery", delivery_id).await?;
    let order = outbound::order_id_of(ctx, &view)
        .await?
        .ok_or_else(|| Error::validation("this delivery follows from no sales order"))?;
    let sent = outbound::sent_order(ctx, order).await?.ok_or_else(|| {
        Error::validation("this delivery's order was not drafted from a partner's order")
    })?;
    ctx.require(PERMISSION, view.header.company_id)?;
    ctx.touch("sales_delivery", delivery_id);
    Ok((view, sent))
}

/// Takes the next SSCC of the company. The settings row stays locked
/// until the command ends.
async fn next_sscc(ctx: &mut Ctx<'_>, company_id: Uuid) -> Result<String> {
    let settings: Option<(Option<String>, i32, i64)> = sqlx::query_as(
        "SELECT gs1_company_prefix, sscc_extension, next_sscc_serial FROM edi_settings
         WHERE company_id = $1 FOR UPDATE",
    )
    .bind(company_id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let Some((Some(prefix), extension, serial)) = settings else {
        return Err(Error::validation(
            "the company has no GS1 company prefix to make SSCCs from: set it in edi_settings",
        ));
    };
    let sscc = u8::try_from(extension)
        .ok()
        .zip(u64::try_from(serial).ok())
        .and_then(|(e, s)| gs1::sscc(e, &prefix, s))
        .ok_or_else(|| {
            Error::validation(
                "the company's SSCC serials are used up for its GS1 prefix: every serial the \
                 prefix leaves room for was issued",
            )
        })?;
    sqlx::query(
        "UPDATE edi_settings SET next_sscc_serial = next_sscc_serial + 1 WHERE company_id = $1",
    )
    .bind(company_id)
    .execute(&mut *ctx.conn)
    .await?;
    Ok(sscc)
}

/// What keeps a delivery's packing from matching the delivery.
async fn missing(ctx: &mut Ctx<'_>, view: &DocumentView) -> Result<Vec<String>> {
    let packed: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "SELECT pl.delivery_line_id, sum(pl.qty) FROM edi_pack_line pl
         JOIN edi_pack p ON p.id = pl.pack_id
         WHERE p.delivery_id = $1 AND NOT p.voided GROUP BY pl.delivery_line_id",
    )
    .bind(view.header.id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let packed: HashMap<Uuid, Decimal> = packed.into_iter().collect();
    let mut out = Vec::new();
    let lines = document::body::lines(&view.body, "lines");
    if lines.is_empty() {
        out.push("The delivery has no lines.".to_owned());
    }
    for line in lines {
        let (Some(id), Some(qty)) = (
            document::body::uuid(&line["id"]),
            document::body::decimal(&line["qty"]),
        ) else {
            continue;
        };
        let in_cartons = packed.get(&id).copied().unwrap_or_default();
        if in_cartons != qty {
            out.push(format!(
                "Delivery line {} delivers {} and its cartons hold {}.",
                line["line_no"],
                qty.normalize(),
                in_cartons.normalize()
            ));
        }
    }
    let empty: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM edi_pack p WHERE p.delivery_id = $1 AND NOT p.voided
           AND p.kind = 'carton'
           AND NOT EXISTS (SELECT 1 FROM edi_pack_line pl WHERE pl.pack_id = p.id)",
    )
    .bind(view.header.id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    if empty > 0 {
        out.push(format!("{empty} carton(s) hold nothing."));
    }
    Ok(out)
}

async fn shipment_out(ctx: &mut Ctx<'_>, view: &DocumentView) -> Result<ShipmentOut> {
    let rows: Vec<(Uuid, String, String, Option<Uuid>, Value)> = sqlx::query_as(
        "SELECT p.id, p.kind, p.sscc, p.parent_id,
                COALESCE((SELECT jsonb_agg(jsonb_build_object(
                              'delivery_line_id', pl.delivery_line_id, 'qty', pl.qty::text)
                              ORDER BY pl.delivery_line_id)
                          FROM edi_pack_line pl WHERE pl.pack_id = p.id), '[]')
         FROM edi_pack p WHERE p.delivery_id = $1 AND NOT p.voided ORDER BY p.position",
    )
    .bind(view.header.id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let state: String = sqlx::query_scalar("SELECT state FROM edi_shipment WHERE delivery_id = $1")
        .bind(view.header.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
    let missing = missing(ctx, view).await?;
    Ok(ShipmentOut {
        delivery_id: view.header.id,
        state,
        packs: rows
            .into_iter()
            .map(|(id, kind, sscc, parent_id, lines)| PackOut {
                id,
                kind,
                sscc,
                parent_id,
                lines,
            })
            .collect(),
        missing,
    })
}

/// Records how a delivery is packed, replacing any packing before.
pub async fn pack(
    ctx: &mut Ctx<'_>,
    delivery_id: Uuid,
    carrier: Carrier,
    packs: Vec<PackInput>,
) -> Result<ShipmentOut> {
    let (view, sent) = delivery(ctx, delivery_id).await?;
    if !matches!(view.header.status.as_str(), "draft" | "posted") {
        return Err(Error::state(format!(
            "this delivery is {}: only a delivery to post or posted is packed",
            view.header.status
        )));
    }
    let noticed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM edi_message
                        WHERE doc_type = 'sales_delivery' AND document_id = $1
                          AND message = '856' AND superseded_by IS NULL
                          AND state NOT IN ('invalid', 'rejected', 'failed'))",
    )
    .bind(delivery_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    if noticed {
        return Err(Error::state(
            "the partner was sent a ship notice for this delivery: its packing stays as noticed",
        ));
    }
    if packs.is_empty() {
        return Err(Error::field("packs", "give at least one carton"));
    }
    sqlx::query(
        "INSERT INTO edi_shipment (delivery_id, company_id, partner_id, carrier_scac,
             carrier_name, service, bill_of_lading, tracking, shipped_at, estimated_delivery,
             gross_weight, weight_uom, state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'packing')
         ON CONFLICT (delivery_id) DO UPDATE SET carrier_scac = EXCLUDED.carrier_scac,
             carrier_name = EXCLUDED.carrier_name, service = EXCLUDED.service,
             bill_of_lading = EXCLUDED.bill_of_lading, tracking = EXCLUDED.tracking,
             shipped_at = EXCLUDED.shipped_at, estimated_delivery = EXCLUDED.estimated_delivery,
             gross_weight = EXCLUDED.gross_weight, weight_uom = EXCLUDED.weight_uom,
             state = 'packing'",
    )
    .bind(delivery_id)
    .bind(view.header.company_id)
    .bind(sent.partner_id)
    .bind(&carrier.carrier_scac)
    .bind(&carrier.carrier_name)
    .bind(&carrier.service)
    .bind(&carrier.bill_of_lading)
    .bind(&carrier.tracking)
    .bind(carrier.shipped_at)
    .bind(carrier.estimated_delivery)
    .bind(carrier.gross_weight)
    .bind(&carrier.weight_uom)
    .execute(&mut *ctx.conn)
    .await?;
    sqlx::query("UPDATE edi_pack SET voided = true WHERE delivery_id = $1 AND NOT voided")
        .bind(delivery_id)
        .execute(&mut *ctx.conn)
        .await?;

    let mut ids: Vec<(Uuid, bool)> = Vec::new();
    for (index, input) in packs.iter().enumerate() {
        let n = index + 1;
        let pallet = matches!(input.kind, PackKind::Pallet);
        let parent = match input.parent {
            None => None,
            Some(_) if pallet => {
                return Err(Error::line(
                    "packs",
                    n,
                    Some("parent"),
                    "a pallet stands on nothing",
                ));
            }
            Some(p) => match p.checked_sub(1).and_then(|i| ids.get(i)) {
                Some((id, true)) => Some(*id),
                _ => {
                    return Err(Error::line(
                        "packs",
                        n,
                        Some("parent"),
                        "names no pallet given before it",
                    ));
                }
            },
        };
        if pallet && !input.lines.is_empty() {
            return Err(Error::line(
                "packs",
                n,
                Some("lines"),
                "a pallet holds cartons, not goods: put the goods in cartons on it",
            ));
        }
        let sscc = next_sscc(ctx, view.header.company_id).await?;
        let id = new_id();
        sqlx::query(
            "INSERT INTO edi_pack (id, delivery_id, parent_id, kind, sscc, weight, position)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(delivery_id)
        .bind(parent)
        .bind(if pallet { "pallet" } else { "carton" })
        .bind(&sscc)
        .bind(input.weight)
        .bind(n as i32)
        .execute(&mut *ctx.conn)
        .await?;
        for line in &input.lines {
            if line.qty <= Decimal::ZERO {
                return Err(Error::line(
                    "packs",
                    n,
                    Some("lines"),
                    "a quantity is above zero",
                ));
            }
            sqlx::query(
                "INSERT INTO edi_pack_line (pack_id, delivery_line_id, qty) VALUES ($1, $2, $3)
                 ON CONFLICT (pack_id, delivery_line_id) DO UPDATE
                     SET qty = edi_pack_line.qty + EXCLUDED.qty",
            )
            .bind(id)
            .bind(line.delivery_line_id)
            .bind(line.qty)
            .execute(&mut *ctx.conn)
            .await
            .map_err(|e| match Error::from(e) {
                Error::Validation(m) => Error::line("packs", n, Some("lines"), m.text),
                other => other,
            })?;
        }
        ids.push((id, pallet));
    }
    if missing(ctx, &view).await?.is_empty() {
        sqlx::query("UPDATE edi_shipment SET state = 'packed' WHERE delivery_id = $1")
            .bind(delivery_id)
            .execute(&mut *ctx.conn)
            .await?;
    }
    shipment_out(ctx, &view).await
}

/// Packs each line in cartons of the partner's unit: one carton per case,
/// for goods that ship in their own cases.
pub async fn auto_pack(ctx: &mut Ctx<'_>, delivery_id: Uuid) -> Result<ShipmentOut> {
    let (view, sent) = delivery(ctx, delivery_id).await?;
    let mut packs = Vec::new();
    for line in document::body::lines(&view.body, "lines") {
        let (Some(id), Some(order_line), Some(qty)) = (
            document::body::uuid(&line["id"]),
            document::body::uuid(&line["order_line_id"]),
            document::body::decimal(&line["qty"]),
        ) else {
            continue;
        };
        let Some(sent_line) = sent.lines.get(&order_line) else {
            return Err(Error::validation(format!(
                "delivery line {} ships an order line the partner did not order",
                line["line_no"]
            )));
        };
        let per = sent_line.units_per;
        let count = qty
            .checked_div(per)
            .filter(|c| c.fract().is_zero() && *c > Decimal::ZERO)
            .and_then(|c| c.trunc().normalize().to_string().parse::<usize>().ok());
        let Some(count) = count else {
            return Err(Error::validation(format!(
                "delivery line {} delivers {}, which is no whole number of cases of {}: pack it \
                 with edi_shipment.pack",
                line["line_no"],
                qty.normalize(),
                per.normalize()
            )));
        };
        if packs.len().saturating_add(count) > 10_000 {
            return Err(Error::validation(
                "this would make more than 10,000 cartons: pack it with edi_shipment.pack",
            ));
        }
        for _ in 0..count {
            packs.push(PackInput {
                kind: PackKind::Carton,
                parent: None,
                weight: None,
                lines: vec![PackLineInput {
                    delivery_line_id: id,
                    qty: per,
                }],
            });
        }
    }
    let carrier: Option<Carrier> = sqlx::query_as::<
        _,
        (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<DateTime<Utc>>,
            Option<NaiveDate>,
            Option<Decimal>,
            Option<String>,
        ),
    >(
        "SELECT carrier_scac, carrier_name, service, bill_of_lading, tracking, shipped_at,
                estimated_delivery, gross_weight, weight_uom
         FROM edi_shipment WHERE delivery_id = $1",
    )
    .bind(delivery_id)
    .fetch_optional(&mut *ctx.conn)
    .await?
    .map(|r| Carrier {
        carrier_scac: r.0,
        carrier_name: r.1,
        service: r.2,
        bill_of_lading: r.3,
        tracking: r.4,
        shipped_at: r.5,
        estimated_delivery: r.6,
        gross_weight: r.7,
        weight_uom: r.8,
    });
    pack(ctx, delivery_id, carrier.unwrap_or_default(), packs).await
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Shipped {
    /// The delivery as `sales_delivery.post` returned it.
    pub delivery: Value,
    /// The ship notice made for it.
    pub notice: Generated,
}

/// Checks the packing, posts the delivery and makes the 856, all or
/// nothing.
pub async fn ship(ctx: &mut Ctx<'_>, delivery_id: Uuid) -> Result<Shipped> {
    let (view, _) = delivery(ctx, delivery_id).await?;
    if view.header.status != "draft" {
        return Err(Error::state(format!(
            "this delivery is {}: only a delivery to post is shipped",
            view.header.status
        )));
    }
    let state: Option<String> =
        sqlx::query_scalar("SELECT state FROM edi_shipment WHERE delivery_id = $1 FOR UPDATE")
            .bind(delivery_id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    let missing = missing(ctx, &view).await?;
    if state.is_none() || !missing.is_empty() {
        return Err(Error::validation(format!(
            "the delivery is not packed as it delivers: {}",
            if state.is_none() {
                "it has no packing (edi_shipment.pack or edi_shipment.auto_pack)".to_owned()
            } else {
                missing.join(" ")
            }
        )));
    }
    sqlx::query(
        "UPDATE edi_shipment SET state = 'packed', shipped_at = COALESCE(shipped_at, now())
         WHERE delivery_id = $1",
    )
    .bind(delivery_id)
    .execute(&mut *ctx.conn)
    .await?;
    // The normal post, with every check of a delivery: this module posts
    // nothing itself.
    let post = ctx
        .kernel
        .registry()
        .command("sales_delivery.post")?
        .handler
        .clone();
    let delivery = post.run_json(ctx, json!({ "id": delivery_id })).await?;
    let notice = outbound::generate(
        ctx,
        "sales_delivery",
        delivery_id,
        Message::Notice856,
        false,
    )
    .await?;
    if notice.state != "ready" {
        return Err(Error::validation(format!(
            "the ship notice cannot be made, so nothing is posted: {}",
            notice
                .findings
                .iter()
                .map(|f| f.message.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )));
    }
    Ok(Shipped { delivery, notice })
}
