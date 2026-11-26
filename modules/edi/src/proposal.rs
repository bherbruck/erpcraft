//! What a partner's order would become: the order as typed, what each part
//! of it resolves to here and how, and what stands in the way.
//!
//! Read-only, and read with the caller's rights, as `einvoice.read` is: a
//! customer or item the caller may not read is left out, and says so.
//! Every judgement a person must make is a finding, never a change made
//! quietly: an item nobody knows, a unit nobody converts, a price that
//! does not divide into the item's unit, an order number drafted before.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use erpcraft_kernel::{Ctx, Decimal, Error, Result, entity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::guide;
use crate::inbound::{ReadInterchange, SetRead, blocking, read_file};
use crate::partner::{self, Partner};
use crate::policy::{DUPLICATE_ORDER, PRICE_ZERO};
use crate::x12::po850::{Line, PurchaseOrder};

/// The places a price may have after it is divided into the item's unit:
/// the scale of a sales order's unit price.
const PRICE_SCALE: u32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Stops a draft until a person decides.
    Blocking,
    /// For a person to read; a draft takes `accept_notes`.
    Note,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub severity: Severity,
    /// What kind of finding, for a program: `item_unknown`, `unit_unknown`,
    /// `price_does_not_divide`, `ship_to_unknown`, `duplicate_order`, ...
    pub code: String,
    /// The partner's line id, when it is about a line.
    pub line: Option<String>,
    pub message: String,
}

impl Finding {
    pub fn blocking(code: &str, line: Option<&str>, message: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Blocking,
            code: code.to_owned(),
            line: line.map(str::to_owned),
            message: message.into(),
        }
    }

    pub fn note(code: &str, line: Option<&str>, message: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Note,
            code: code.to_owned(),
            line: line.map(str::to_owned),
            message: message.into(),
        }
    }
}

/// A person's choices for what blocked, by the partner's line id.
#[derive(Debug, Clone, Default)]
pub struct Choices {
    pub items: BTreeMap<String, Uuid>,
    pub prices: BTreeMap<String, Decimal>,
    pub shipping_address_id: Option<Uuid>,
    pub warehouse_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PartnerRef {
    pub id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CustomerRef {
    pub id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ShipTo {
    /// The ship-to as the order names it: role, name, qualifier and code.
    pub sent: Option<Value>,
    /// One of the customer's addresses; none for the customer's default.
    pub address_id: Option<Uuid>,
    /// `location` (by a location cross-reference), `chosen`, `default`
    /// (the order names no place), or `unknown`.
    pub how: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProposedLine {
    /// PO101: the partner's line id, echoed in every answer.
    pub line_id: String,
    /// The item numbers as sent: `[{qualifier, value}]`.
    pub product_ids: Value,
    pub description: Option<String>,
    pub partner_qty: Option<Decimal>,
    pub partner_uom: String,
    pub partner_price: Option<Decimal>,
    pub item_id: Option<Uuid>,
    pub item_code: Option<String>,
    pub item_name: Option<String>,
    /// How the item was found: `gtin 040000100010`, `seller_item CH-1001`,
    /// `chosen`.
    pub how_matched: Option<String>,
    /// How many of the item's own unit one of the partner's is.
    pub units_per: Option<Decimal>,
    /// The item's own unit.
    pub uom: Option<String>,
    /// In the item's own unit, as the sales order will carry them.
    pub qty: Option<Decimal>,
    pub unit_price: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Proposal {
    pub message_id: Uuid,
    pub state: String,
    pub company_id: Uuid,
    pub partner: PartnerRef,
    /// The order as it was sent, every value as read.
    #[schemars(with = "Value")]
    pub order: PurchaseOrder,
    /// Left out for a caller who may not read the customer.
    pub customer: Option<CustomerRef>,
    /// The order's currency; the company's when it names none.
    pub currency: Option<String>,
    pub ship_to: ShipTo,
    pub warehouse_id: Option<Uuid>,
    /// The date the draft takes: the order's own.
    pub doc_date: Option<NaiveDate>,
    pub lines: Vec<ProposedLine>,
    /// The lines at the partner's prices, without tax.
    pub total_net: Option<Decimal>,
    pub findings: Vec<Finding>,
    /// No blocking finding stands.
    pub would_draft: bool,
}

/// The message's row, as a proposal needs it.
#[derive(sqlx::FromRow)]
pub struct MessageRow {
    pub id: Uuid,
    pub interchange_id: Option<Uuid>,
    pub company_id: Uuid,
    pub partner_id: Uuid,
    pub direction: String,
    pub message: String,
    pub position: Option<i32>,
    pub state: String,
    pub findings: Value,
}

pub async fn message_row(ctx: &mut Ctx<'_>, id: Uuid, lock: bool) -> Result<MessageRow> {
    let row: MessageRow = sqlx::query_as(&format!(
        "SELECT id, interchange_id, company_id, partner_id, direction, message, position, state,
                findings
         FROM edi_message WHERE id = $1 {}",
        if lock { "FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .fetch_optional(&mut *ctx.conn)
    .await?
    .ok_or_else(|| Error::not_found("edi_message"))?;
    Ok(row)
}

/// The segments of a message received, as they came, for its guide's
/// constraints.
async fn segments_of(ctx: &mut Ctx<'_>, row: &MessageRow) -> Result<Vec<crate::Segment>> {
    let (Some(interchange), Some(position)) = (row.interchange_id, row.position) else {
        return Ok(vec![]);
    };
    let payload: Vec<u8> = sqlx::query_scalar("SELECT payload FROM edi_interchange WHERE id = $1")
        .bind(interchange)
        .fetch_one(&mut *ctx.conn)
        .await?;
    let index = (position as usize).saturating_sub(1);
    let edifact = row.message == "ORDERS";
    blocking(move || -> Vec<crate::Segment> {
        if edifact {
            crate::edifact::read(&payload)
                .ok()
                .and_then(|mut all| all.pop())
                .and_then(|i| i.messages.into_iter().nth(index))
                .map(|m| m.segments)
                .unwrap_or_default()
        } else {
            crate::x12::read(&payload)
                .ok()
                .and_then(|mut all| all.pop())
                .and_then(|i| i.groups.into_iter().flat_map(|g| g.sets).nth(index))
                .map(|s| s.segments)
                .unwrap_or_default()
        }
    })
    .await
    .map_err(Error::internal)
}

/// The typed order of a message received, read again from its bytes.
pub async fn order_of(ctx: &mut Ctx<'_>, row: &MessageRow) -> Result<PurchaseOrder> {
    if row.direction != "in" || !matches!(row.message.as_str(), "850" | "ORDERS") {
        return Err(Error::validation(
            "this message is not an order received: only an 850 or an ORDERS is proposed",
        ));
    }
    let (Some(interchange), Some(position)) = (row.interchange_id, row.position) else {
        return Err(Error::internal("an order received has no interchange"));
    };
    let payload: Vec<u8> = sqlx::query_scalar("SELECT payload FROM edi_interchange WHERE id = $1")
        .bind(interchange)
        .fetch_one(&mut *ctx.conn)
        .await?;
    let position = position as usize;
    if row.message == "ORDERS" {
        let read = blocking(move || crate::inbound_edifact::order_at(&payload, position))
            .await
            .and_then(|r| r)
            .map_err(Error::internal)?;
        return read.ok_or_else(|| Error::internal("the message is no longer where it was read"));
    }
    let read = blocking(
        move || -> std::result::Result<Option<PurchaseOrder>, String> {
            let mut all: Vec<ReadInterchange> = read_file(&payload).map_err(|e| e.to_string())?;
            let Some(first) = all.pop() else {
                return Ok(None);
            };
            Ok(first
                .sets
                .into_iter()
                .flatten()
                .nth(position.saturating_sub(1))
                .and_then(|set| match set {
                    SetRead::Order(order, _) => Some(*order),
                    _ => None,
                }))
        },
    )
    .await
    .and_then(|r| r)
    .map_err(Error::internal)?;
    read.ok_or_else(|| Error::internal("the message's set is no longer where it was read"))
}

/// Which kind of cross-reference a product id qualifier is looked up as.
/// These are the qualifiers this module acts on; a guide may name others
/// (phase 4).
pub fn kind_of(qualifier: &str) -> Option<&'static str> {
    match qualifier {
        "UP" | "EN" | "UK" => Some("gtin"),
        "VN" | "VP" => Some("seller_item"),
        "IN" | "BP" => Some("buyer_item"),
        _ => None,
    }
}

const KIND_ORDER: [&str; 3] = ["gtin", "seller_item", "buyer_item"];

#[derive(sqlx::FromRow)]
struct XrefRow {
    item_id: Uuid,
    partner_uom: Option<String>,
    units_per: Decimal,
}

async fn xref(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
    kind: &str,
    value: &str,
) -> Result<Option<XrefRow>> {
    Ok(sqlx::query_as(
        "SELECT item_id, partner_uom, units_per FROM edi_item_xref
         WHERE company_id = $4 AND (partner_id = $1 OR partner_id IS NULL) AND kind = $2
           AND CASE WHEN kind = 'gtin' THEN lpad(value, 14, '0') = lpad($3, 14, '0')
                    ELSE value = $3 END
         ORDER BY partner_id NULLS LAST LIMIT 1",
    )
    .bind(partner.id)
    .bind(kind)
    .bind(value)
    .bind(partner.company_id)
    .fetch_optional(&mut *ctx.conn)
    .await?)
}

/// The item a line names, by its cross-references in the order gtin,
/// seller's number, buyer's number (or as the partner's guide says); and
/// how it was found.
async fn match_item(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
    line: &Line,
    guide: &guide::Options,
) -> Result<Option<(XrefRow, String)>> {
    let order: Vec<(&str, Vec<&str>)> = match &guide.item_match {
        Some(matches) => matches
            .iter()
            .map(|m| {
                (
                    m.kind.code(),
                    m.qualifiers.iter().map(String::as_str).collect(),
                )
            })
            .collect(),
        None => KIND_ORDER.iter().map(|k| (*k, vec![])).collect(),
    };
    for (kind, qualifiers) in order {
        for id in line.product_ids.iter().filter(|p| {
            if qualifiers.is_empty() {
                kind_of(&p.qualifier) == Some(kind)
            } else {
                qualifiers.contains(&p.qualifier.as_str())
            }
        }) {
            if let Some(row) = xref(ctx, partner, kind, &id.value).await? {
                return Ok(Some((row, format!("{kind} {}", id.value))));
            }
        }
    }
    Ok(None)
}

/// What the item is, if the caller may read it.
async fn item(ctx: &mut Ctx<'_>, id: Uuid) -> Result<Option<(String, String, String)>> {
    match entity::get(ctx, "item", id).await {
        Ok(record) => Ok(Some((
            record["code"].as_str().unwrap_or_default().to_owned(),
            record["name"].as_str().unwrap_or_default().to_owned(),
            record["uom"].as_str().unwrap_or_default().to_owned(),
        ))),
        Err(Error::NotFound(_) | Error::Forbidden(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Our unit for a partner's unit code, by the partner's cross-reference
/// or the one for every partner.
async fn our_uom(ctx: &mut Ctx<'_>, partner: &Partner, code: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT uom FROM edi_uom_xref
         WHERE company_id = $3 AND (partner_id = $1 OR partner_id IS NULL) AND partner_uom = $2
         ORDER BY partner_id NULLS LAST LIMIT 1",
    )
    .bind(partner.id)
    .bind(code)
    .bind(partner.company_id)
    .fetch_optional(&mut *ctx.conn)
    .await?)
}

/// How many stock units one of `uom` is, for an item sold in that unit
/// (`item_uom`).
async fn sales_factor(ctx: &mut Ctx<'_>, item_id: Uuid, uom: &str) -> Result<Option<Decimal>> {
    Ok(
        sqlx::query_scalar("SELECT factor FROM item_uom WHERE item_id = $1 AND uom = $2 AND sales")
            .bind(item_id)
            .bind(uom)
            .fetch_optional(&mut *ctx.conn)
            .await?,
    )
}

/// Each line of an order as the sales order line it would become: its
/// item, quantity and price in the item's own unit, with what is wrong
/// with it in `findings`; and what the lines come to at the partner's
/// prices, if every one has a price.
pub(crate) async fn propose_lines(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
    spec: &guide::Spec,
    order_lines: &[Line],
    choices: &Choices,
    findings: &mut Vec<Finding>,
) -> Result<(Vec<ProposedLine>, Option<Decimal>)> {
    let mut lines = Vec::new();
    let mut total = Some(Decimal::ZERO);
    for line in order_lines {
        let id = line.line_id.as_str();
        let mut proposed = ProposedLine {
            line_id: line.line_id.clone(),
            product_ids: json!(line.product_ids),
            description: line.descriptions.first().cloned(),
            partner_qty: line.quantity,
            partner_uom: line.uom.clone(),
            partner_price: line.unit_price,
            item_id: None,
            item_code: None,
            item_name: None,
            how_matched: None,
            units_per: None,
            uom: None,
            qty: None,
            unit_price: None,
        };
        if id.is_empty() {
            findings.push(Finding::blocking(
                "line_id",
                None,
                "A line has no line id (PO101), which every answer must echo.",
            ));
        }
        if !line.allocations.is_empty() {
            findings.push(Finding::note(
                "allocation",
                Some(id),
                format!(
                    "Line {id} splits its quantity over {} locations (SDQ); the draft ships \
                     it all to one address.",
                    line.allocations.len()
                ),
            ));
        }
        if !line.parties.is_empty() {
            findings.push(Finding::note(
                "line_party",
                Some(id),
                format!("Line {id} names a party of its own, which the draft does not keep."),
            ));
        }
        if !matches!(line.price_basis.as_str(), "" | "PE") {
            findings.push(Finding::blocking(
                "price_basis",
                Some(id),
                format!(
                    "Line {id} states its price per {} (PO105), not per unit: give its price \
                     per the partner's unit (prices).",
                    line.price_basis
                ),
            ));
        }

        // The item: chosen, or by cross-reference.
        let matched = match choices.items.get(id) {
            Some(item) => Some((
                XrefRow {
                    item_id: *item,
                    partner_uom: None,
                    units_per: Decimal::ONE,
                },
                "chosen".to_owned(),
            )),
            None => match_item(ctx, partner, line, &spec.options).await?,
        };
        let Some((xref, how)) = matched else {
            findings.push(Finding::blocking(
                "item_unknown",
                Some(id),
                format!(
                    "Line {id} names no item known here ({}): choose one (items), and \
                     remember it for the next order.",
                    line.product_ids
                        .iter()
                        .map(|p| format!("{} {}", p.qualifier, p.value))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
            total = None;
            lines.push(proposed);
            continue;
        };
        proposed.how_matched = Some(how);
        let Some((code, name, uom)) = item(ctx, xref.item_id).await? else {
            findings.push(Finding::blocking(
                "item_unreadable",
                Some(id),
                format!("Line {id} is for an item you may not read."),
            ));
            total = None;
            lines.push(proposed);
            continue;
        };
        proposed.item_id = Some(xref.item_id);
        proposed.item_code = Some(code.clone());
        proposed.item_name = Some(name);
        proposed.uom = Some(uom.clone());

        // How many of the item's unit one of the partner's is.
        let units_per = if xref.partner_uom.as_deref() == Some(line.uom.as_str()) {
            Some(xref.units_per)
        } else {
            let ours = our_uom(ctx, partner, &line.uom).await?;
            if ours.as_deref() == Some(uom.as_str()) || line.uom.eq_ignore_ascii_case(&uom) {
                Some(Decimal::ONE)
            } else {
                // Another unit of the item that it is sold in: its factor.
                match ours {
                    Some(ours) => sales_factor(ctx, xref.item_id, &ours).await?,
                    None => None,
                }
            }
        };
        let Some(units_per) = units_per else {
            findings.push(Finding::blocking(
                "unit_unknown",
                Some(id),
                format!(
                    "Line {id} is in {}, and no cross-reference says how many {uom} of item \
                     {code} that is (an item number's partner unit and units per, or a unit \
                     cross-reference).",
                    line.uom
                ),
            ));
            total = None;
            lines.push(proposed);
            continue;
        };
        proposed.units_per = Some(units_per);

        match line.quantity {
            Some(q) if q > Decimal::ZERO => match q.checked_mul(units_per) {
                Some(qty) if fits(qty) => proposed.qty = Some(qty),
                Some(qty) if qty.abs() < Decimal::from(100_000_000_000_000i64) => {
                    findings.push(Finding::blocking(
                        "quantity_precision",
                        Some(id),
                        format!(
                            "Line {id} comes to {} {uom}, more decimal places than a sales \
                             order line holds (at most 6): correct the quantity or the \
                             cross-reference's units per.",
                            qty.normalize()
                        ),
                    ))
                }
                _ => findings.push(Finding::blocking(
                    "quantity_too_large",
                    Some(id),
                    format!(
                        "Line {id} orders more than a sales order line holds (at most 14 \
                         digits before the point)."
                    ),
                )),
            },
            _ => findings.push(Finding::blocking(
                "quantity",
                Some(id),
                format!("Line {id} orders no quantity above zero."),
            )),
        }
        let partner_price = choices.prices.get(id).copied().or(line.unit_price);
        match partner_price {
            None => findings.push(Finding::blocking(
                "price_missing",
                Some(id),
                format!("Line {id} states no price: give one (prices)."),
            )),
            Some(p) if p < Decimal::ZERO => findings.push(Finding::blocking(
                "price",
                Some(id),
                format!("Line {id} has a price below zero."),
            )),
            Some(p) if p.is_zero() && !choices.prices.contains_key(id) => {
                let decision = ctx
                    .policy(PRICE_ZERO, partner.company_id)
                    .await?
                    .evaluate(price_zero_facts(partner, id))?;
                findings.push(match decision {
                    erpcraft_kernel::Decision::Allowed => {
                        proposed.unit_price = Some(Decimal::ZERO);
                        Finding::note(
                            "price_zero",
                            Some(id),
                            format!(
                                "Line {id} is priced at nothing, and the policy {PRICE_ZERO} \
                                 lets it be supplied free."
                            ),
                        )
                    }
                    erpcraft_kernel::Decision::Refused(_) => Finding::blocking(
                        "price_zero",
                        Some(id),
                        format!(
                            "Line {id} is priced at nothing: give the price meant (prices), or \
                             0 there to supply it free; the policy {PRICE_ZERO} may allow it."
                        ),
                    ),
                })
            }
            Some(p) if !fits(p) => findings.push(Finding::blocking(
                "price_too_large",
                Some(id),
                format!("Line {id} has a price larger than a sales order line holds."),
            )),
            Some(p) => match divide_price(p, units_per) {
                Some(unit) => proposed.unit_price = Some(unit),
                None => findings.push(Finding::blocking(
                    "price_does_not_divide",
                    Some(id),
                    format!(
                        "Line {id} costs {p} per {}, which does not divide into {units_per} \
                         {uom} without rounding: give the price per {} that is meant \
                         (prices), or correct the cross-reference.",
                        line.uom, line.uom
                    ),
                )),
            },
        }
        total = match (total, line.quantity, partner_price) {
            (Some(t), Some(q), Some(p)) => q
                .checked_mul(p)
                .map(|a| a.round_dp(2))
                .and_then(|a| t.checked_add(a)),
            _ => None,
        };
        lines.push(proposed);
    }
    Ok((lines, total))
}

/// Whether a quantity or price fits a sales order line, `numeric(20, 6)`.
/// The facts of [`PRICE_ZERO`] for one line.
pub fn price_zero_facts(partner: &Partner, line: &str) -> Value {
    json!({ "partner_code": partner.code, "line": line })
}

pub(crate) fn fits(value: Decimal) -> bool {
    value.abs() < Decimal::from(100_000_000_000_000i64) && value.normalize().scale() <= 6
}

/// A unit price divided into a smaller unit, only when it divides exactly
/// within the places a price has. Never rounded.
pub fn divide_price(price: Decimal, units_per: Decimal) -> Option<Decimal> {
    if units_per <= Decimal::ZERO {
        return None;
    }
    let unit = price.checked_div(units_per)?;
    let rounded = unit.round_dp(PRICE_SCALE);
    (rounded.checked_mul(units_per)? == price).then(|| rounded.normalize())
}

pub async fn propose(ctx: &mut Ctx<'_>, message_id: Uuid, choices: &Choices) -> Result<Proposal> {
    let row = message_row(ctx, message_id, false).await?;
    ctx.require("edi_message:read", row.company_id)?;
    let order = order_of(ctx, &row).await?;
    let partner = partner::load(&mut *ctx.conn, row.partner_id).await?;
    build(ctx, &row, &partner, order, choices).await
}

pub async fn build(
    ctx: &mut Ctx<'_>,
    row: &MessageRow,
    partner: &Partner,
    order: PurchaseOrder,
    choices: &Choices,
) -> Result<Proposal> {
    let mut findings: Vec<Finding> = Vec::new();
    let spec = guide::spec_for(&mut *ctx.conn, partner, &row.message, "in").await?;

    // What reading found stays: syntax errors and counts.
    if let Ok(stored) = serde_json::from_value::<Vec<Finding>>(row.findings.clone()) {
        findings.extend(stored);
    }
    match row.state.as_str() {
        "proposed" => {}
        "test" => findings.push(Finding::blocking(
            "test",
            None,
            "This order is test traffic (ISA15, or a partner still in test): it is \
             acknowledged and never becomes a document.",
        )),
        "drafted" => findings.push(Finding::blocking(
            "drafted",
            None,
            "This order was drafted already.",
        )),
        "dismissed" => findings.push(Finding::blocking(
            "dismissed",
            None,
            "This order was dismissed.",
        )),
        "rejected" => {
            if !findings.iter().any(|f| f.severity == Severity::Blocking) {
                findings.push(Finding::blocking(
                    "rejected",
                    None,
                    "This order was rejected when it was read.",
                ));
            }
        }
        other => findings.push(Finding::blocking(
            "state",
            None,
            format!("A message in the state {other} is not drafted."),
        )),
    }
    if order.purpose != "00" {
        findings.push(Finding::blocking(
            "purpose",
            None,
            format!(
                "The order's purpose is {} (BEG01), not an original order: a change, \
                 cancellation or replacement is not drafted as a new order. See what it \
                 changes with edi.change_proposal.",
                order.purpose
            ),
        ));
    }
    if !spec.constraints.is_empty() {
        let segments = segments_of(ctx, row).await?;
        for why in guide::violations(&spec.constraints, &segments) {
            findings.push(Finding::note(
                "guide",
                None,
                format!("The order breaks the partner's guide: {why}."),
            ));
        }
    }
    for (position, tag) in &order.unhandled {
        findings.push(Finding::note(
            "unhandled",
            None,
            format!(
                "Segment {tag} at position {position} has no field here and is not carried \
                 into the sales order."
            ),
        ));
    }

    // The customer, as the caller may read it.
    let customer_id = partner.customer()?;
    let customer = erpcraft_party::owner(ctx, "customer", customer_id)
        .await?
        .map(|record| CustomerRef {
            id: customer_id,
            code: record["code"].as_str().unwrap_or_default().to_owned(),
            name: record["name"].as_str().unwrap_or_default().to_owned(),
        });

    // Where the goods go, and the warehouse orders to that place ship
    // from, if the place names one.
    let mut location_warehouse: Option<Uuid> = None;
    let (role, qualifier) = match &spec.options.ship_to {
        Some(ship_to) => (ship_to.role.as_str(), ship_to.qualifier.as_deref()),
        None => ("ST", None),
    };
    let sent = order
        .parties
        .iter()
        .find(|p| p.role == role && qualifier.is_none_or(|q| p.id_qualifier == q));
    let ship_to = if let Some(address) = choices.shipping_address_id {
        erpcraft_party::check_address(
            &mut *ctx.conn,
            "customer",
            customer_id,
            address,
            "shipping_address_id",
        )
        .await?;
        ShipTo {
            sent: sent.map(|p| json!(p)),
            address_id: Some(address),
            how: "chosen".into(),
        }
    } else if let Some(party) = sent.filter(|p| !p.id.is_empty()) {
        let known: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT address_id, warehouse_id FROM edi_location_xref
             WHERE partner_id = $1 AND qualifier = $2 AND code = $3",
        )
        .bind(partner.id)
        .bind(&party.id_qualifier)
        .bind(&party.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        location_warehouse = known.and_then(|f| f.1);
        let found = known.and_then(|f| f.0);
        if found.is_none() {
            let why = if known.is_some() {
                "whose location cross-reference has lost its address (removed from the \
                 customer, or the partner names another customer now)"
            } else {
                "which no location cross-reference names"
            };
            findings.push(Finding::blocking(
                "ship_to_unknown",
                None,
                format!(
                    "The order ships to {} ({} {}), {why}: choose one of the customer's \
                     addresses (shipping_address_id), and remember it for the next order.",
                    party.name, party.id_qualifier, party.id
                ),
            ));
        }
        ShipTo {
            sent: Some(json!(party)),
            address_id: found,
            how: if found.is_some() {
                "location"
            } else {
                "unknown"
            }
            .into(),
        }
    } else {
        ShipTo {
            sent: sent.map(|p| json!(p)),
            address_id: None,
            how: "default".into(),
        }
    };

    let warehouse_id = choices
        .warehouse_id
        .or(location_warehouse)
        .or(partner.warehouse_id);
    if warehouse_id.is_none() {
        findings.push(Finding::blocking(
            "warehouse",
            None,
            "Neither the partner nor the draft names the warehouse the goods ship from \
             (warehouse_id).",
        ));
    }

    let (lines, total) =
        propose_lines(ctx, partner, &spec, &order.lines, choices, &mut findings).await?;
    if order.lines.is_empty() {
        findings.push(Finding::blocking(
            "no_lines",
            None,
            "The order has no lines.",
        ));
    }

    // An order number of this partner drafted before.
    if !order.number.is_empty() {
        let drafted_before: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM edi_document WHERE partner_id = $1 AND order_number = $2",
        )
        .bind(partner.id)
        .bind(&order.number)
        .fetch_one(&mut *ctx.conn)
        .await?;
        if drafted_before > 0 {
            let decision = ctx
                .policy(DUPLICATE_ORDER, partner.company_id)
                .await?
                .evaluate(json!({
                    "partner_code": partner.code,
                    "order_number": order.number,
                    "drafted_before": drafted_before,
                }))?;
            let message = format!(
                "Order {} of this partner was drafted {drafted_before} time(s) before.",
                order.number
            );
            findings.push(match decision {
                erpcraft_kernel::Decision::Allowed => {
                    Finding::note("duplicate_order", None, message)
                }
                erpcraft_kernel::Decision::Refused(_) => Finding::blocking(
                    "duplicate_order",
                    None,
                    format!("{message} The policy {DUPLICATE_ORDER} refuses it."),
                ),
            });
        }
    }

    let would_draft = !findings.iter().any(|f| f.severity == Severity::Blocking);
    let currency = order.currency.clone();
    Ok(Proposal {
        message_id: row.id,
        state: row.state.clone(),
        company_id: row.company_id,
        partner: PartnerRef {
            id: partner.id,
            code: partner.code.clone(),
            name: partner.name.clone(),
        },
        doc_date: order.date,
        order,
        customer,
        currency,
        ship_to,
        warehouse_id,
        lines,
        total_net: total.filter(|t| !t.is_zero() || would_draft),
        findings,
        would_draft,
    })
}
