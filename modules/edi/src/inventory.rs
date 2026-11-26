//! The 846 inventory advice (INVRPT for an EDIFACT partner): what is
//! available of each item the partner has a number for, as
//! `stock.availability` says it, in the partner's unit and rounded down to
//! whole units of it.

use std::collections::BTreeMap;

use chrono::Utc;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::outbound::{self, Generated};
use crate::partner::{self, Partner};
use crate::proposal::Finding;
use crate::standalone::{self, Standalone};
use crate::x12::inventory846::{AVAILABLE, InventoryAdvice, InventoryItem};
use crate::x12::po850::ProductId;

/// BIA02 as this module writes it: the seller's report of its inventory.
/// (Not checked against the standard; a partner's guide may want another.)
pub const REPORT_TYPE: &str = "SI";

/// The product id qualifier a cross-reference is written with.
fn qualifier(kind: &str, value: &str, edifact: bool) -> &'static str {
    match (kind, edifact) {
        ("gtin", true) => "EN",
        ("gtin", false) => match value.len() {
            12 => "UP",
            14 => "UK",
            _ => "EN",
        },
        ("seller_item", _) => "VN",
        _ => "IN",
    }
}

#[derive(sqlx::FromRow)]
struct Xref {
    item_id: Uuid,
    kind: String,
    value: String,
    partner_uom: Option<String>,
    units_per: Decimal,
    own: bool,
    item_code: String,
    item_uom: String,
}

/// What a partner's items are, and how much of each can be promised.
pub async fn advice(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
) -> Result<(InventoryAdvice, Vec<Finding>)> {
    let mut findings = vec![];
    let rows: Vec<Xref> = sqlx::query_as(
        "SELECT x.item_id, x.kind, x.value, x.partner_uom, x.units_per,
                x.partner_id IS NOT NULL AS own, i.code AS item_code, i.uom AS item_uom
         FROM edi_item_xref x JOIN item i ON i.id = x.item_id
         WHERE x.company_id = $1 AND (x.partner_id = $2 OR x.partner_id IS NULL) AND i.active
         ORDER BY i.code, x.partner_id IS NULL, x.kind, x.value",
    )
    .bind(partner.company_id)
    .bind(partner.id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    // Availability as the command shows it, with the caller's rights.
    let availability = ctx
        .kernel
        .registry()
        .command("stock.availability")?
        .handler
        .clone();
    let mut input = json!({ "company_id": partner.company_id });
    if let Some(warehouse) = partner.warehouse_id {
        input["warehouse_id"] = json!(warehouse);
    }
    let available = availability.run_json(ctx, input).await?;
    let mut by_item: BTreeMap<Uuid, Decimal> = BTreeMap::new();
    for row in available["rows"].as_array().into_iter().flatten() {
        let (Some(item), Some(qty)) = (
            row["item_id"].as_str().and_then(|s| s.parse::<Uuid>().ok()),
            erpcraft_kernel::document::body::decimal(&row["available"]),
        ) else {
            continue;
        };
        *by_item.entry(item).or_default() += qty;
    }

    let edifact = partner.is_edifact();
    let mut items: Vec<InventoryItem> = vec![];
    let mut seen: Vec<Uuid> = vec![];
    for row in &rows {
        if seen.contains(&row.item_id) {
            continue;
        }
        seen.push(row.item_id);
        let numbers: Vec<&Xref> = rows.iter().filter(|r| r.item_id == row.item_id).collect();
        let mut product_ids: Vec<ProductId> = vec![];
        for n in &numbers {
            let id = ProductId {
                qualifier: qualifier(&n.kind, &n.value, edifact).into(),
                value: n.value.clone(),
            };
            if !product_ids.contains(&id) {
                product_ids.push(id);
            }
        }
        // The partner's unit: what its own number says, else what a unit
        // cross-reference calls the item's unit.
        let unit = numbers
            .iter()
            .filter(|n| n.partner_uom.is_some())
            .min_by_key(|n| !n.own)
            .map(|n| (n.partner_uom.clone().unwrap_or_default(), n.units_per));
        let unit = match unit {
            Some(unit) => Some(unit),
            None => sqlx::query_scalar::<_, String>(
                "SELECT partner_uom FROM edi_uom_xref
                 WHERE company_id = $1 AND (partner_id = $2 OR partner_id IS NULL) AND uom = $3
                 ORDER BY partner_id NULLS LAST, partner_uom LIMIT 1",
            )
            .bind(partner.company_id)
            .bind(partner.id)
            .bind(&row.item_uom)
            .fetch_optional(&mut *ctx.conn)
            .await?
            .map(|code| (code, Decimal::ONE)),
        };
        let Some((uom, units_per)) = unit.filter(|(_, per)| *per > Decimal::ZERO) else {
            findings.push(Finding::blocking(
                "unit_unknown",
                None,
                format!(
                    "Item {} is in {}, which no unit cross-reference names for the partner: \
                     add one (edi_uom_xref) or give the item's number a partner's unit.",
                    row.item_code, row.item_uom
                ),
            ));
            continue;
        };
        let on_hand = by_item
            .get(&row.item_id)
            .copied()
            .unwrap_or_default()
            .max(Decimal::ZERO);
        // Whole units of the partner's: a part of a case cannot be promised.
        let quantity = (on_hand / units_per).floor();
        items.push(InventoryItem {
            product_ids,
            qualifier: AVAILABLE.into(),
            quantity: Some(quantity.normalize()),
            uom,
        });
    }
    if items.is_empty() && findings.is_empty() {
        findings.push(Finding::blocking(
            "no_items",
            None,
            "The partner has no item numbers: there is nothing to advise.",
        ));
    }
    let now = Utc::now();
    Ok((
        InventoryAdvice {
            purpose: "00".into(),
            report_type: REPORT_TYPE.into(),
            report_id: now.format("%Y%m%d%H%M%S").to_string(),
            date: Some(now.date_naive()),
            items,
            ..InventoryAdvice::default()
        },
        findings,
    ))
}

/// Makes, checks and (unless a preview) stores a partner's inventory
/// advice.
pub async fn generate(ctx: &mut Ctx<'_>, partner_id: Uuid, preview: bool) -> Result<Generated> {
    let partner = partner::load(&mut *ctx.conn, partner_id).await?;
    ctx.require(outbound::PERMISSION, partner.company_id)?;
    if !partner.active {
        return Err(Error::state("the partner is not active"));
    }
    let (advice, findings) = advice(ctx, &partner).await?;
    let edifact = partner.is_edifact();
    let d = if edifact {
        partner
            .edifact_envelope(Utc::now().naive_utc(), 0)?
            .delimiters
    } else {
        crate::Delimiters::X12
    };
    let body = if edifact {
        crate::edifact::invrpt::body(&advice, &d)
    } else {
        advice.body()
    };
    let meant = advice.clone();
    let reads_back = move |bytes: &[u8]| -> bool {
        if edifact {
            let Some(interchange) = crate::edifact::read(bytes).ok().and_then(|mut i| i.pop())
            else {
                return false;
            };
            let Some(message) = interchange.messages.first() else {
                return false;
            };
            let (read, verdict) = crate::edifact::invrpt::read(message, &interchange.delimiters);
            interchange.errors.is_empty()
                && message.errors.is_empty()
                && verdict.is_clean()
                && read == crate::edifact::invrpt::carried(&meant)
        } else {
            let Some(set) = crate::x12::read(bytes)
                .ok()
                .and_then(|mut i| i.pop())
                .filter(|i| i.errors.is_empty())
                .and_then(|i| i.groups.into_iter().next())
                .and_then(|g| g.sets.into_iter().next())
                .filter(|s| s.errors.is_empty())
            else {
                return false;
            };
            let (read, verdict) = crate::x12::inventory846::read(&set);
            let mut meant = meant.clone();
            meant.items_stated = Some(meant.items.len() as u32);
            verdict == Default::default() && read == meant
        }
    };
    standalone::send(
        ctx,
        Standalone {
            code: if edifact { "INVRPT" } else { "846" }.into(),
            functional_id: "IB",
            body: Some(body),
            findings,
            business_key: Some(advice.report_id.clone()),
            doc_type: None,
            document_id: None,
            total: Decimal::ZERO,
            partner,
        },
        preview,
        reads_back,
    )
    .await
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdviceInput {
    /// The partner to advise.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "edi_partner", "key": "id" }))]
    partner_id: Uuid,
    /// Show the advice and what is wrong with it; store nothing.
    #[serde(default)]
    preview: bool,
}

pub struct Advise;

#[async_trait]
impl Command for Advise {
    type Input = AdviceInput;
    type Output = Generated;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi.inventory_advice",
            "Make a partner's inventory advice (an 846, or INVRPT for an EDIFACT partner): for \
             each item the partner has a number for, what stock.availability says is available \
             (from the partner's warehouse if it names one), in the partner's unit and rounded \
             down to whole units of it, nothing below zero. It is read back before it is kept, \
             and stored ready to send. Takes the rights stock.availability takes.",
        )
        .permission(outbound::PERMISSION)
        .label("Inventory advice")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: AdviceInput) -> Result<Generated> {
        generate(ctx, input.partner_id, input.preview).await
    }
}
