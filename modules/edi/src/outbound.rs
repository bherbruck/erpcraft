//! Sending: an 855, 856 or 810 made from a document in a locked state.
//!
//! 1. **Generate** the typed message from the document, from what
//!    `edi_document` kept of the partner's order (its numbers, line ids,
//!    item numbers and units as sent), and the packing for a ship notice.
//! 2. **Validate**: the bytes are read back with the reader an incoming
//!    message meets and must say what was meant, and the figures must be
//!    the books': the quantities of an 856 are the posted delivery's, the
//!    total of an 810 is the invoice's to the cent. A message that fails is
//!    kept as `invalid` with what is wrong, and no bytes are fixed for it.
//! 3. **Number and fix**: the control numbers are taken as the last step
//!    and the bytes stored, ready to send. They never change after.

use std::collections::{BTreeMap, HashMap};

use chrono::{NaiveDate, Utc};
use erpcraft_kernel::document::{self, DocumentView};
use erpcraft_kernel::{Ctx, Decimal, Error, Result, entity, new_id};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::commands::ExplainedSegment;
use crate::control::{self, Kind};
use crate::explain::explain;
use crate::inbound::{MESSAGE_COLUMNS, MessageOut};
use crate::partner::{self, Partner};
use crate::policy::{BEFORE_ASN, SEND};
use crate::proposal::{Finding, Severity};
use crate::store;
use crate::x12::asn856::{
    self, Carrier, Item, ItemLot, Lading, Node, Pack, PackKind, ShipNotice, ShipOrder,
};
use crate::x12::invoice810::{self, Charge, Invoice, InvoiceLine, Tax};
use crate::x12::po850::{DateOf, Party, ProductId, Reference};
use crate::x12::poack855::{self, AckLine, LineStatus, OrderAck};
use crate::x12::{self, TransactionSet};

pub const PERMISSION: &str = "edi:send";

/// The message to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Ack855,
    Notice856,
    Invoice810,
}

impl Message {
    pub fn parse(text: &str) -> Result<Message> {
        match text {
            "855" | "ORDRSP" => Ok(Message::Ack855),
            "856" | "DESADV" => Ok(Message::Notice856),
            "810" | "INVOIC" => Ok(Message::Invoice810),
            _ => Err(Error::field(
                "message",
                "must be 855, 856 or 810 (ORDRSP, DESADV or INVOIC for an EDIFACT partner)",
            )),
        }
    }

    /// What the message is called in a partner's syntax.
    pub fn code_for(self, partner: &Partner) -> &'static str {
        match (self, partner.is_edifact()) {
            (Message::Ack855, true) => "ORDRSP",
            (Message::Notice856, true) => "DESADV",
            (Message::Invoice810, true) => "INVOIC",
            (message, false) => message.code(),
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Message::Ack855 => "855",
            Message::Notice856 => "856",
            Message::Invoice810 => "810",
        }
    }

    /// GS01 of the group it travels in.
    fn functional_id(self) -> &'static str {
        match self {
            Message::Ack855 => "PR",
            Message::Notice856 => "SH",
            Message::Invoice810 => "IN",
        }
    }

    /// The document types it is made from.
    fn made_from(self, doc_type: &str) -> bool {
        matches!(
            (self, doc_type),
            (Message::Ack855, "sales_order")
                | (Message::Notice856, "sales_delivery")
                | (Message::Invoice810, "sales_invoice" | "sales_credit_note")
        )
    }
}

/// One line of the partner's order, as it was sent.
#[derive(Debug, Clone)]
pub struct SentLine {
    pub partner_line_id: String,
    pub product_ids: Vec<ProductId>,
    pub partner_uom: String,
    pub partner_qty: Decimal,
    pub partner_price: Option<Decimal>,
    pub units_per: Decimal,
}

/// What `edi_document` kept of the partner's order.
#[derive(Debug, Clone)]
pub struct SentOrder {
    pub order_id: Uuid,
    pub partner_id: Uuid,
    pub message_id: Uuid,
    pub order_number: String,
    pub release: String,
    pub order_date: Option<NaiveDate>,
    pub header: Value,
    /// By the sales order line.
    pub lines: BTreeMap<Uuid, SentLine>,
}

impl SentOrder {
    fn references(&self) -> Vec<Reference> {
        serde_json::from_value(self.header["references"].clone()).unwrap_or_default()
    }

    fn ship_to(&self) -> Option<Party> {
        let parties: Vec<Party> =
            serde_json::from_value(self.header["parties"].clone()).unwrap_or_default();
        parties.into_iter().find(|p| p.role == "ST")
    }
}

/// The head of an `edi_document` row.
type HeadRow = (Uuid, Uuid, String, Option<String>, Option<NaiveDate>, Value);
/// A row of `edi_document_line`.
type LineRow = (
    Uuid,
    String,
    Value,
    String,
    Decimal,
    Option<Decimal>,
    Decimal,
);

/// The partner's order a sales order was drafted from, if one was.
pub async fn sent_order(ctx: &mut Ctx<'_>, order_id: Uuid) -> Result<Option<SentOrder>> {
    let head: Option<HeadRow> = sqlx::query_as(
        "SELECT partner_id, message_id, order_number, release, order_date, header
             FROM edi_document WHERE doc_type = 'sales_order' AND document_id = $1",
    )
    .bind(order_id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let Some((partner_id, message_id, order_number, release, order_date, header)) = head else {
        return Ok(None);
    };
    let rows: Vec<LineRow> = sqlx::query_as(
        "SELECT line_id, partner_line_id, product_ids, partner_uom, partner_qty,
                    partner_price, units_per
             FROM edi_document_line WHERE doc_type = 'sales_order' AND document_id = $1",
    )
    .bind(order_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let lines = rows
        .into_iter()
        .map(|(line, id, products, uom, qty, price, units_per)| {
            (
                line,
                SentLine {
                    partner_line_id: id,
                    product_ids: serde_json::from_value(products).unwrap_or_default(),
                    partner_uom: uom,
                    partner_qty: qty,
                    partner_price: price,
                    units_per,
                },
            )
        })
        .collect();
    Ok(Some(SentOrder {
        order_id,
        partner_id,
        message_id,
        order_number,
        release: release.unwrap_or_default(),
        order_date,
        header,
        lines,
    }))
}

/// The sales order a document follows from.
pub async fn order_id_of(ctx: &mut Ctx<'_>, view: &DocumentView) -> Result<Option<Uuid>> {
    let body = &view.body;
    Ok(match view.header.doc_type.as_str() {
        "sales_order" => Some(view.header.id),
        "sales_delivery" | "sales_invoice" => document::body::uuid(&body["sales_order_id"]),
        "sales_credit_note" => match document::body::uuid(&body["sales_invoice_id"]) {
            Some(invoice) => {
                let invoice = document::get(ctx, "sales_invoice", invoice).await?;
                document::body::uuid(&invoice.body["sales_order_id"])
            }
            None => None,
        },
        _ => None,
    })
}

/// A quantity in the item's unit as the partner's: exact, or nothing.
pub fn to_partner_qty(qty: Decimal, units_per: Decimal) -> Option<Decimal> {
    let q = qty.checked_div(units_per)?;
    (q.checked_mul(units_per)? == qty).then(|| q.normalize())
}

fn to_partner_price(price: Decimal, units_per: Decimal) -> Option<Decimal> {
    price.checked_mul(units_per).map(|p| p.normalize())
}

fn decimal(value: &Value) -> Option<Decimal> {
    document::body::decimal(value)
}

/// A message to send, typed.
pub enum Typed {
    Ack(OrderAck),
    Notice(ShipNotice),
    Invoice(Invoice),
}

impl Typed {
    fn body(&self) -> Option<Vec<crate::segment::Segment>> {
        match self {
            Typed::Ack(m) => Some(m.body()),
            Typed::Notice(m) => Some(m.body()),
            Typed::Invoice(m) => m.body(),
        }
    }

    /// The EDIFACT segments of the message, from after the UNH to before
    /// the UNT; `None` when it cannot be written (an amount with more than
    /// two places, a notice of more than one order).
    fn edifact_body(&self, d: &crate::segment::Delimiters) -> Option<Vec<crate::segment::Segment>> {
        match self {
            Typed::Ack(m) => Some(crate::edifact::ordrsp::body(m, d)),
            Typed::Notice(m) => crate::edifact::desadv::body(m, d),
            Typed::Invoice(m) => crate::edifact::invoic::body(m, d),
        }
    }

    /// Whether the EDIFACT message read back says what this one meant, as
    /// far as EDIFACT carries it.
    fn edifact_reads_back(&self, bytes: &[u8]) -> bool {
        let Some(interchange) = crate::edifact::read(bytes).ok().and_then(|mut i| i.pop()) else {
            return false;
        };
        let d = interchange.delimiters;
        let Some(message) = interchange.messages.first() else {
            return false;
        };
        if !interchange.errors.is_empty() || !message.errors.is_empty() {
            return false;
        }
        match self {
            Typed::Ack(meant) => {
                let (read, verdict) = crate::edifact::ordrsp::read(message, &d);
                verdict.is_clean() && read == crate::edifact::ordrsp::carried(meant)
            }
            Typed::Notice(meant) => {
                let (read, verdict) = crate::edifact::desadv::read(message, &d);
                verdict.is_clean() && read == crate::edifact::desadv::carried(meant)
            }
            Typed::Invoice(meant) => {
                let (read, verdict) = crate::edifact::invoic::read(message, &d);
                verdict.is_clean() && read == crate::edifact::invoic::carried(meant)
            }
        }
    }

    /// Whether the set read back says what this message meant.
    fn reads_back(&self, set: &TransactionSet) -> bool {
        if !set.errors.is_empty() {
            return false;
        }
        match self {
            Typed::Ack(meant) => {
                let (read, verdict) = poack855::read(set);
                let mut meant = meant.clone();
                meant.lines_stated = Some(meant.lines.len() as u32);
                verdict == Default::default() && read == meant
            }
            Typed::Notice(meant) => {
                let (read, verdict) = asn856::read(set);
                let mut meant = meant.clone();
                meant.levels_stated = Some(meant.levels() as u32);
                verdict == Default::default() && read == meant
            }
            Typed::Invoice(meant) => {
                let (read, verdict) = invoice810::read(set);
                let mut meant = meant.clone();
                meant.lines_stated = Some(meant.lines.len() as u32);
                verdict == Default::default() && read == meant
            }
        }
    }
}

/// A message made, before it is checked and fixed.
pub struct Made {
    pub message: Message,
    pub partner: Partner,
    pub sent: SentOrder,
    pub view: DocumentView,
    pub typed: Option<Typed>,
    pub findings: Vec<Finding>,
    /// For the policy `edi.send`.
    pub total: Decimal,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Generated {
    /// The message on record; none for a preview.
    pub message: Option<MessageOut>,
    /// `ready`, or `invalid` with the findings that say why.
    pub state: String,
    pub findings: Vec<Finding>,
    /// The message's segments, each with what it says.
    pub segments: Vec<ExplainedSegment>,
    /// The message this one replaces.
    pub supersedes: Option<Uuid>,
}

fn blocking(findings: &mut Vec<Finding>, code: &str, line: Option<&str>, message: String) {
    findings.push(Finding::blocking(code, line, message));
}

/// Makes the message from a document the caller may read.
pub async fn make(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid, message: Message) -> Result<Made> {
    if !message.made_from(doc_type) {
        return Err(Error::field(
            "doc_type",
            format!(
                "an {} is not made from a {doc_type}: an 855 from a sales order, an 856 from a \
                 sales delivery, an 810 from a sales invoice or credit note",
                message.code()
            ),
        ));
    }
    let view = document::get(ctx, doc_type, id).await?;
    let locked = match doc_type {
        "sales_order" => matches!(view.header.status.as_str(), "open" | "closed" | "cancelled"),
        _ => view.header.status == "posted",
    };
    if !locked {
        return Err(Error::state(format!(
            "this {doc_type} is {}: a message is made from an approved order or a posted \
             document, never from a draft",
            view.header.status
        )));
    }
    let order_id = order_id_of(ctx, &view)
        .await?
        .ok_or_else(|| Error::validation("this document follows from no sales order"))?;
    let sent = sent_order(ctx, order_id).await?.ok_or_else(|| {
        Error::validation("this document's sales order was not drafted from a partner's order")
    })?;
    let partner = partner::load(&mut *ctx.conn, sent.partner_id).await?;
    ctx.require(PERMISSION, partner.company_id)?;
    let mut made = Made {
        message,
        partner,
        sent,
        view,
        typed: None,
        findings: Vec::new(),
        total: Decimal::ZERO,
    };
    match message {
        Message::Ack855 => ack(&mut made),
        Message::Notice856 => notice(ctx, &mut made).await?,
        Message::Invoice810 => {
            // A credit note's lines name the invoice's lines, which name
            // the order's.
            let mut order_line_of: HashMap<Uuid, Uuid> = HashMap::new();
            if doc_type == "sales_credit_note"
                && let Some(invoice) = document::body::uuid(&made.view.body["sales_invoice_id"])
            {
                let invoice = document::get(ctx, "sales_invoice", invoice).await?;
                for line in document::body::lines(&invoice.body, "lines") {
                    if let (Some(id), Some(order_line)) = (
                        document::body::uuid(&line["id"]),
                        document::body::uuid(&line["order_line_id"]),
                    ) {
                        order_line_of.insert(id, order_line);
                    }
                }
            }
            invoice(&mut made, &order_line_of)
        }
    }
    Ok(made)
}

fn ack(made: &mut Made) {
    let view = &made.view;
    let sent = &made.sent;
    let cancelled = view.header.status == "cancelled";
    let lines: HashMap<Uuid, &Value> = document::body::lines(&view.body, "lines")
        .iter()
        .filter_map(|l| document::body::uuid(&l["id"]).map(|id| (id, l)))
        .collect();
    let mut acked = Vec::new();
    let mut ordered: Vec<(&Uuid, &SentLine)> = sent.lines.iter().collect();
    ordered.sort_by_key(|a| natural(&a.1.partner_line_id));
    for (line_id, line) in ordered {
        let id = line.partner_line_id.as_str();
        let order_line = lines.get(line_id).filter(|_| !cancelled);
        let (code, quantity, price) = match order_line {
            None => (
                poack855::LINE_REJECTED,
                Some(Decimal::ZERO),
                line.partner_price,
            ),
            Some(l) => {
                let (Some(qty), Some(price)) = (decimal(&l["qty"]), decimal(&l["unit_price"]))
                else {
                    blocking(
                        &mut made.findings,
                        "line",
                        Some(id),
                        format!("Order line for partner line {id} has no quantity or price."),
                    );
                    continue;
                };
                let Some(pq) = to_partner_qty(qty, line.units_per) else {
                    blocking(
                        &mut made.findings,
                        "quantity_not_whole",
                        Some(id),
                        format!(
                            "Line {id}: {qty} is no number of {} of {} each.",
                            line.partner_uom, line.units_per
                        ),
                    );
                    continue;
                };
                let pp = to_partner_price(price, line.units_per);
                let code = if pq != line.partner_qty {
                    poack855::LINE_QUANTITY_CHANGED
                } else if line.partner_price.is_some() && pp != line.partner_price {
                    poack855::LINE_PRICE_CHANGED
                } else {
                    poack855::LINE_ACCEPTED
                };
                (code, Some(pq), pp)
            }
        };
        acked.push(AckLine {
            line_id: line.partner_line_id.clone(),
            quantity: Some(line.partner_qty.normalize()),
            uom: line.partner_uom.clone(),
            unit_price: price.map(|p| p.normalize()),
            product_ids: line.product_ids.clone(),
            statuses: vec![LineStatus {
                code: code.into(),
                quantity,
                uom: line.partner_uom.clone(),
                date_qualifier: String::new(),
                date: None,
            }],
        });
    }
    let codes: Vec<&str> = acked
        .iter()
        .flat_map(|l| l.statuses.iter().map(|s| s.code.as_str()))
        .collect();
    let ack_type = if codes.iter().all(|c| *c == poack855::LINE_ACCEPTED) {
        poack855::ACCEPTED
    } else if codes.iter().all(|c| *c == poack855::LINE_REJECTED) {
        poack855::REJECTED
    } else {
        poack855::CHANGED
    };
    made.typed = Some(Typed::Ack(OrderAck {
        purpose: "00".into(),
        ack_type: ack_type.into(),
        order_number: sent.order_number.clone(),
        order_date: sent.order_date,
        release: sent.release.clone(),
        seller_order: view.header.number.clone().unwrap_or_default(),
        date: Some(Utc::now().date_naive()),
        lines: acked,
        ..OrderAck::default()
    }));
}

/// Sorts partner line ids as numbers where they are numbers.
fn natural(id: &str) -> (u64, String) {
    (id.parse().unwrap_or(u64::MAX), id.to_owned())
}

#[derive(sqlx::FromRow)]
struct ShipmentRow {
    carrier_scac: Option<String>,
    carrier_name: Option<String>,
    service: Option<String>,
    bill_of_lading: Option<String>,
    tracking: Option<String>,
    shipped_at: Option<chrono::DateTime<Utc>>,
    estimated_delivery: Option<NaiveDate>,
    gross_weight: Option<Decimal>,
    weight_uom: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PackRow {
    id: Uuid,
    parent_id: Option<Uuid>,
    kind: String,
    sscc: String,
}

async fn notice(ctx: &mut Ctx<'_>, made: &mut Made) -> Result<()> {
    let delivery_id = made.view.header.id;
    let shipment: Option<ShipmentRow> = sqlx::query_as(
        "SELECT carrier_scac, carrier_name, service, bill_of_lading, tracking, shipped_at,
                estimated_delivery, gross_weight, weight_uom
         FROM edi_shipment WHERE delivery_id = $1",
    )
    .bind(delivery_id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let packs: Vec<PackRow> = sqlx::query_as(
        "SELECT id, parent_id, kind, sscc FROM edi_pack
         WHERE delivery_id = $1 AND NOT voided ORDER BY position",
    )
    .bind(delivery_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let contents: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
        "SELECT pl.pack_id, pl.delivery_line_id, pl.qty FROM edi_pack_line pl
         JOIN edi_pack p ON p.id = pl.pack_id
         WHERE p.delivery_id = $1 AND NOT p.voided ORDER BY p.position, pl.delivery_line_id",
    )
    .bind(delivery_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let Some(shipment) = shipment.filter(|_| !packs.is_empty()) else {
        blocking(
            &mut made.findings,
            "not_packed",
            None,
            "The delivery is not packed: record its cartons with edi_shipment.pack or \
             edi_shipment.auto_pack first."
                .into(),
        );
        return Ok(());
    };

    // Each delivery line, with the partner's line it ships.
    let mut delivery_lines: HashMap<Uuid, (Decimal, &SentLine)> = HashMap::new();
    for line in document::body::lines(&made.view.body, "lines") {
        let (Some(id), Some(order_line), Some(qty)) = (
            document::body::uuid(&line["id"]),
            document::body::uuid(&line["order_line_id"]),
            decimal(&line["qty"]),
        ) else {
            continue;
        };
        match made.sent.lines.get(&order_line) {
            Some(sent) => {
                delivery_lines.insert(id, (qty, sent));
            }
            None => blocking(
                &mut made.findings,
                "line_not_ordered",
                None,
                format!(
                    "Delivery line {} ships an order line the partner did not order.",
                    line["line_no"]
                ),
            ),
        }
    }

    // The lots of each delivery line, with their expiry dates.
    let lot_rows: Vec<(Uuid, String, Option<NaiveDate>, bool)> = sqlx::query_as(
        "SELECT sl.line_id, sl.lot, COALESCE(l.expiry_date, sl.expiry_date),
                it.tracking = 'serial'
         FROM stock_line_lot sl
         JOIN sales_delivery_line dl ON dl.id = sl.line_id
         JOIN sales_order_line ol ON ol.id = dl.order_line_id
         JOIN item it ON it.id = ol.item_id
         LEFT JOIN stock_lot l
             ON l.company_id = $2 AND l.item_id = ol.item_id AND l.code = sl.lot
         WHERE sl.document_id = $1 ORDER BY sl.line_id, sl.seq",
    )
    .bind(delivery_id)
    .bind(made.view.header.company_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let mut lots_of: HashMap<Uuid, Vec<ItemLot>> = HashMap::new();
    // A lot code goes into an element as it is: one that holds a
    // separator of the partner's would break the message, and the LIN's
    // product id is at most 48 characters.
    let separators: Vec<&str> = [
        Some(made.partner.element_separator.as_str()),
        Some(made.partner.component_separator.as_str()),
        made.partner.repetition_separator.as_deref(),
        Some(made.partner.segment_terminator.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect();
    for (line, lot, expiry, serial) in lot_rows {
        if lot.chars().count() > 48 || separators.iter().any(|s| lot.contains(s)) {
            blocking(
                &mut made.findings,
                "lot_not_writable",
                None,
                format!(
                    "Lot {lot} cannot be written in an X12 element for this partner: it is longer \
                     than 48 characters or holds one of its separators."
                ),
            );
            continue;
        }
        lots_of.entry(line).or_default().push(ItemLot {
            lot,
            expiry,
            serial,
        });
    }

    // What the cartons hold, in the partner's units, checked against the
    // delivery line by line.
    let mut packed: HashMap<Uuid, Decimal> = HashMap::new();
    let mut items_of: HashMap<Uuid, Vec<Node>> = HashMap::new();
    for (pack_id, line_id, qty) in &contents {
        let Some((_, sent)) = delivery_lines.get(line_id) else {
            continue;
        };
        let id = sent.partner_line_id.as_str();
        let entry = packed.entry(*line_id).or_default();
        *entry = entry.checked_add(*qty).unwrap_or(Decimal::MAX);
        let Some(shipped) = to_partner_qty(*qty, sent.units_per) else {
            blocking(
                &mut made.findings,
                "quantity_not_whole",
                Some(id),
                format!(
                    "A carton holds {qty} of line {id}, which is no number of {} of {} each.",
                    sent.partner_uom, sent.units_per
                ),
            );
            continue;
        };
        items_of.entry(*pack_id).or_default().push(Node::Item(Item {
            line_id: sent.partner_line_id.clone(),
            product_ids: sent.product_ids.clone(),
            shipped: Some(shipped),
            uom: sent.partner_uom.clone(),
            ordered: Some(sent.partner_qty.normalize()),
            ordered_uom: sent.partner_uom.clone(),
            description: String::new(),
            packs: Vec::new(),
            lots: lots_of.get(line_id).cloned().unwrap_or_default(),
        }));
    }
    for (line_id, (qty, sent)) in &delivery_lines {
        let in_cartons = packed.get(line_id).copied().unwrap_or_default();
        if in_cartons != *qty {
            blocking(
                &mut made.findings,
                "packing",
                Some(&sent.partner_line_id),
                format!(
                    "Line {} delivers {qty} and its cartons hold {in_cartons}.",
                    sent.partner_line_id
                ),
            );
        }
    }

    // Pallets, and the cartons on them or on their own.
    let mut cartons_of: HashMap<Uuid, Vec<Node>> = HashMap::new();
    let mut top: Vec<(Uuid, Node)> = Vec::new();
    for p in &packs {
        if p.kind == "carton" {
            let node = Node::Pack(Pack {
                kind: PackKind::Carton,
                sscc: Some(p.sscc.clone()),
                contents: items_of.remove(&p.id).unwrap_or_default(),
            });
            match p.parent_id {
                Some(pallet) => cartons_of.entry(pallet).or_default().push(node),
                None => top.push((p.id, node)),
            }
        } else {
            top.push((
                p.id,
                Node::Pack(Pack {
                    kind: PackKind::Pallet,
                    sscc: Some(p.sscc.clone()),
                    contents: Vec::new(),
                }),
            ));
        }
    }
    let contents: Vec<Node> = top
        .into_iter()
        .map(|(id, node)| match node {
            Node::Pack(mut pack) if pack.kind == PackKind::Pallet => {
                pack.contents = cartons_of.remove(&id).unwrap_or_default();
                Node::Pack(pack)
            }
            other => other,
        })
        .collect();
    let cartons = packs.iter().filter(|p| p.kind == "carton").count();

    let mut references = Vec::new();
    if let Some(bol) = &shipment.bill_of_lading {
        references.push(Reference {
            qualifier: "BM".into(),
            value: bol.clone(),
            description: String::new(),
        });
    }
    if let Some(pro) = &shipment.tracking {
        references.push(Reference {
            qualifier: "CN".into(),
            value: pro.clone(),
            description: String::new(),
        });
    }
    let shipped_on = shipment
        .shipped_at
        .map(|t| t.date_naive())
        .unwrap_or(made.view.header.doc_date);
    let mut dates = vec![DateOf {
        qualifier: "011".into(),
        date: shipped_on,
    }];
    if let Some(eta) = shipment.estimated_delivery {
        dates.push(DateOf {
            qualifier: "017".into(),
            date: eta,
        });
    }
    let mut parties = Vec::new();
    if let Some(ship_to) = made.sent.ship_to() {
        parties.push(ship_to);
    }
    if let Ok(company) = entity::get(ctx, "company", made.partner.company_id).await {
        parties.push(Party {
            role: "SF".into(),
            name: company["name"].as_str().unwrap_or_default().to_owned(),
            ..Party::default()
        });
    }
    made.typed = Some(Typed::Notice(ShipNotice {
        purpose: "00".into(),
        shipment_id: made.view.header.number.clone().unwrap_or_default(),
        date: Some(made.view.header.doc_date),
        time: shipment
            .shipped_at
            .map(|t| t.format("%H%M").to_string())
            .unwrap_or_else(|| "0000".into()),
        // Shipment, order, (pallet,) carton, item: pick and pack.
        structure: "0001".into(),
        lading: Some(Lading {
            packaging: "CTN25".into(),
            quantity: Some(Decimal::from(cartons)),
            weight_qualifier: if shipment.gross_weight.is_some() {
                "G".into()
            } else {
                String::new()
            },
            weight: shipment.gross_weight.map(|w| w.normalize()),
            weight_uom: shipment.weight_uom.clone().unwrap_or_default(),
        }),
        carrier: (shipment.carrier_scac.is_some() || shipment.carrier_name.is_some()).then(|| {
            Carrier {
                id_qualifier: if shipment.carrier_scac.is_some() {
                    "2".into()
                } else {
                    String::new()
                },
                id: shipment.carrier_scac.clone().unwrap_or_default(),
                method: "M".into(),
                routing: shipment.carrier_name.clone().unwrap_or_default(),
                service: shipment.service.clone().unwrap_or_default(),
            }
        }),
        equipment: None,
        references,
        dates,
        parties,
        orders: vec![ShipOrder {
            order_number: made.sent.order_number.clone(),
            release: made.sent.release.clone(),
            order_date: made.sent.order_date,
            references: Vec::new(),
            parties: Vec::new(),
            contents,
        }],
        ..ShipNotice::default()
    }));
    Ok(())
}

fn invoice(made: &mut Made, order_line_of: &HashMap<Uuid, Uuid>) {
    let view = &made.view;
    let credit = view.header.doc_type == "sales_credit_note";
    let summary = &view.header.summary;
    let Some(gross) = decimal(&summary["total_gross"]) else {
        blocking(
            &mut made.findings,
            "total",
            None,
            "The document has no total yet.".into(),
        );
        return;
    };
    made.total = gross.abs();
    // A credit note's lines name invoice lines; the invoice's name order
    // lines. The credit note view carries what each credits.
    let mut lines = Vec::new();
    let mut charges = Vec::new();
    for line in document::body::lines(&view.body, "lines") {
        let qty = decimal(&line["qty"]).unwrap_or_default();
        let price = decimal(&line["unit_price"]).unwrap_or_default();
        let net = decimal(&line["net_amount"]).unwrap_or_else(|| {
            qty.checked_mul(price)
                .map(|a| a.round_dp(2))
                .unwrap_or_default()
        });
        let order_line = document::body::uuid(&line["order_line_id"]).or_else(|| {
            document::body::uuid(&line["invoice_line_id"])
                .and_then(|l| order_line_of.get(&l).copied())
        });
        match order_line.and_then(|l| made.sent.lines.get(&l)) {
            Some(sent) => {
                let id = sent.partner_line_id.as_str();
                let Some(pq) = to_partner_qty(qty, sent.units_per) else {
                    blocking(
                        &mut made.findings,
                        "quantity_not_whole",
                        Some(id),
                        format!(
                            "Line {id}: {qty} is no number of {} of {} each.",
                            sent.partner_uom, sent.units_per
                        ),
                    );
                    continue;
                };
                let pp = to_partner_price(price, sent.units_per);
                let gross_line = qty.checked_mul(price).map(|a| a.round_dp(2));
                let mut line_charges = Vec::new();
                if let Some(off) = gross_line.and_then(|g| g.checked_sub(net))
                    && off > Decimal::ZERO
                {
                    line_charges.push(Charge {
                        indicator: "A".into(),
                        code: "ZZZZ".into(),
                        amount: off.normalize(),
                        description: "Discount".into(),
                    });
                }
                lines.push(InvoiceLine {
                    line_id: sent.partner_line_id.clone(),
                    quantity: Some(pq),
                    uom: sent.partner_uom.clone(),
                    unit_price: pp,
                    product_ids: sent.product_ids.clone(),
                    description: String::new(),
                    charges: line_charges,
                });
            }
            None if order_line.is_some() => blocking(
                &mut made.findings,
                "line_not_ordered",
                None,
                format!(
                    "Line {} invoices an order line the partner did not order.",
                    line["line_no"]
                ),
            ),
            None => charges.push(Charge {
                indicator: "C".into(),
                code: "ZZZZ".into(),
                amount: net.abs().normalize(),
                description: line["description"]
                    .as_str()
                    .unwrap_or("Charge")
                    .chars()
                    .filter(|c| !"*~>^:+'?".contains(*c))
                    .take(80)
                    .collect(),
            }),
        }
    }
    // One TXI per tax code the document recorded, as the tax ledger has
    // it: a group's components each on their own.
    let mut taxes: Vec<Tax> = Vec::new();
    // In the order the ledger recorded them: a group's components in
    // their order.
    let mut by_code: Vec<(String, Decimal, Option<Decimal>)> = Vec::new();
    for posting in &view.postings {
        for entry in posting.entries.get("tax").into_iter().flatten() {
            let code = entry["code"].as_str().unwrap_or_default().to_owned();
            let amount = decimal(&entry["amount"]).unwrap_or_default();
            let rate = decimal(&entry["rate"]);
            match by_code.iter_mut().find(|(c, _, _)| *c == code) {
                Some(slot) => slot.1 = slot.1.checked_add(amount).unwrap_or(Decimal::MAX),
                None => by_code.push((code, amount, rate)),
            }
        }
    }
    for (_, amount, rate) in by_code {
        if !amount.is_zero() {
            taxes.push(Tax {
                kind: "ST".into(),
                amount: amount.abs().normalize(),
                percent: rate.map(|r| r.normalize()),
            });
        }
    }
    if let Some(tax) = decimal(&summary["total_tax"]) {
        let recorded = taxes
            .iter()
            .try_fold(Decimal::ZERO, |t, x| t.checked_add(x.amount));
        if recorded != Some(tax.abs()) {
            blocking(
                &mut made.findings,
                "tax",
                None,
                format!(
                    "The tax recorded ({}) is not the tax of the document ({tax}): the tax \
                     entries may be in another currency or not readable to you.",
                    recorded.unwrap_or_default()
                ),
            );
        }
    }
    let typed = Invoice {
        date: Some(view.header.doc_date),
        number: view.header.number.clone().unwrap_or_default(),
        order_date: made.sent.order_date,
        order_number: made.sent.order_number.clone(),
        release: made.sent.release.clone(),
        kind: if credit {
            invoice810::CREDIT
        } else {
            invoice810::DEBIT
        }
        .into(),
        references: made.sent.references(),
        parties: made.sent.ship_to().into_iter().collect(),
        lines,
        total: Some(gross.abs().normalize()),
        taxes,
        charges,
        ..Invoice::default()
    };
    if invoice810::computed_total(&typed) != Some(gross.abs()) {
        blocking(
            &mut made.findings,
            "total",
            None,
            format!(
                "The lines, charges and taxes come to {} and the document to {gross}: the \
                 partner would not read the total the books hold.",
                invoice810::computed_total(&typed).unwrap_or_default()
            ),
        );
    }
    made.typed = Some(Typed::Invoice(typed));
}

/// Explains a body's segments, numbered as in the set (the ST is 1).
fn explained(body: &[crate::segment::Segment], set_id: &str) -> Vec<ExplainedSegment> {
    body.iter()
        .enumerate()
        .map(|(i, s)| {
            let mut text = String::new();
            let _ = crate::segment::write_segment(&mut text, s, &crate::Delimiters::X12);
            ExplainedSegment {
                position: i + 2,
                tag: s.tag.clone(),
                text,
                says: explain(s, set_id),
            }
        })
        .collect()
}

/// Checks, numbers and stores a message made. With `preview`, shows it
/// and stores nothing.
pub async fn finish(ctx: &mut Ctx<'_>, mut made: Made, preview: bool) -> Result<Generated> {
    let message = made.message;
    let partner = made.partner.clone();
    let doc_type = made.view.header.doc_type.clone();
    let doc_id = made.view.header.id;

    // What this one would replace.
    let previous: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, state FROM edi_message
         WHERE direction = 'out' AND doc_type = $1 AND document_id = $2 AND message = $3
           AND superseded_by IS NULL
         ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
    )
    .bind(&doc_type)
    .bind(doc_id)
    .bind(message.code_for(&partner))
    .fetch_optional(&mut *ctx.conn)
    .await?;
    if let Some((_, state)) = &previous
        && !matches!(state.as_str(), "invalid" | "rejected" | "failed")
    {
        return Err(Error::state(format!(
            "an {} was made for this document already and is {state}: send the same bytes \
             again with edi_message.resend",
            message.code_for(&partner)
        )));
    }
    let code = message.code_for(&partner);

    if partner.is_edifact() {
        return finish_edifact(ctx, made, preview, previous).await;
    }
    let body = made.typed.as_ref().and_then(Typed::body);
    if made.typed.is_some() && body.is_none() {
        made.findings.push(Finding::blocking(
            "amount",
            None,
            "An amount has more than two places, which the message cannot carry without \
             rounding.",
        ));
    }
    let segments = body
        .as_deref()
        .map(|b| explained(b, message.code()))
        .unwrap_or_default();

    let usage = if partner.is_test() { 'T' } else { 'P' };
    let at = Utc::now().naive_utc();
    let mut bytes: Option<Fixed> = None;
    if made
        .findings
        .iter()
        .all(|f| f.severity != Severity::Blocking)
        && let (Some(typed), Some(body)) = (&made.typed, body)
    {
        // The figures are the books'; now the bytes must say so too. A
        // partner reads them with a reader like this one.
        let probe = partner.envelope(message.functional_id(), at, 1, 1, usage)?;
        let reads_back = |text: &str| {
            x12::read(text.as_bytes())
                .ok()
                .and_then(|mut i| i.pop())
                .and_then(|i| i.groups.into_iter().next())
                .and_then(|g| g.sets.into_iter().next())
                .is_some_and(|set| typed.reads_back(&set))
        };
        match x12::write(&probe, message.code(), std::slice::from_ref(&body)) {
            Err(e) => made.findings.push(Finding::blocking(
                "write",
                None,
                format!("The message cannot be written: {}.", e.message),
            )),
            Ok(text) if !reads_back(&text) => made.findings.push(Finding::blocking(
                "read_back",
                None,
                "The message written does not read back as what was meant.",
            )),
            Ok(_) if preview => {}
            Ok(_) => {
                // Policies, then the numbers: the last step before the
                // bytes are fixed.
                decide_send(ctx, &partner, code, &made).await?;
                let interchange =
                    control::next(&mut *ctx.conn, partner.id, Kind::Interchange).await?;
                let group = control::next(&mut *ctx.conn, partner.id, Kind::Group).await?;
                let envelope =
                    partner.envelope(message.functional_id(), at, interchange, group, usage)?;
                let text = x12::write(&envelope, message.code(), std::slice::from_ref(&body))
                    .map_err(|e| Error::internal(e.message))?;
                if !reads_back(&text) {
                    return Err(Error::internal(
                        "the message with its control numbers does not read back",
                    ));
                }
                bytes = Some(Fixed {
                    bytes: partner.finish(text),
                    interchange: format!("{interchange:09}"),
                    group: group.to_string(),
                    set: "0001".into(),
                });
            }
        }
    }
    store_made(ctx, made, preview, previous, segments, bytes).await
}

/// The bytes of a message to send, fixed, with the references they carry.
struct Fixed {
    bytes: Vec<u8>,
    /// ISA13, or UNB 0020.
    interchange: String,
    /// GS06, or the UNB's reference again (EDIFACT has no group here).
    group: String,
    /// ST02, or UNH 0062.
    set: String,
}

/// The policies asked before the bytes of a message are fixed.
async fn decide_send(ctx: &mut Ctx<'_>, partner: &Partner, code: &str, made: &Made) -> Result<()> {
    ctx.decide(
        SEND,
        partner.company_id,
        json!({
            "partner_code": partner.code,
            "message": code,
            "total": made.total,
            "test": partner.is_test(),
        }),
    )
    .await?;
    if made.message == Message::Invoice810 {
        before_asn(ctx, partner, &made.sent).await?;
    }
    Ok(())
}

/// Writes an EDIFACT message to an EDIFACT partner: ORDRSP, DESADV or
/// INVOIC, read back as the partner will read it, then numbered and fixed.
async fn finish_edifact(
    ctx: &mut Ctx<'_>,
    mut made: Made,
    preview: bool,
    previous: Option<(Uuid, String)>,
) -> Result<Generated> {
    let message = made.message;
    let partner = made.partner.clone();
    let code = message.code_for(&partner);
    if let Some(Typed::Invoice(invoice)) = made.typed.as_mut() {
        // What an EDIFACT invoice says that the 810 leaves to the order:
        // its currency, the buyer and seller as the order named them, and
        // the tax as VAT.
        invoice.currency = made.view.header.summary["currency"]
            .as_str()
            .map(str::to_owned);
        let named: Vec<Party> =
            serde_json::from_value(made.sent.header["parties"].clone()).unwrap_or_default();
        invoice.parties = ["BY", "VN", "BT", "ST"]
            .iter()
            .filter_map(|role| named.iter().find(|p| p.role == *role).cloned())
            .collect();
        for tax in &mut invoice.taxes {
            tax.kind = "VA".into();
        }
    }
    let at = Utc::now().naive_utc();
    let probe = partner.edifact_envelope(at, 0)?;
    let body = made
        .typed
        .as_ref()
        .and_then(|t| t.edifact_body(&probe.delimiters));
    if made.typed.is_some() && body.is_none() {
        made.findings.push(Finding::blocking(
            "amount",
            None,
            "An amount has more than two places, or the notice holds more than one order, \
             which the message cannot carry.",
        ));
    }
    let segments = body
        .as_deref()
        .map(|b| explained(b, code))
        .unwrap_or_default();
    let outgoing = |body: Vec<crate::segment::Segment>| crate::edifact::Outgoing {
        kind: code.into(),
        version: "D".into(),
        release: "96A".into(),
        agency: "UN".into(),
        association: String::new(),
        reference: "1".into(),
        body,
    };
    let mut bytes: Option<Fixed> = None;
    if made
        .findings
        .iter()
        .all(|f| f.severity != Severity::Blocking)
        && let (Some(typed), Some(body)) = (&made.typed, body)
    {
        match crate::edifact::write(&probe, &[outgoing(body.clone())]) {
            Err(e) => made.findings.push(Finding::blocking(
                "write",
                None,
                format!("The message cannot be written: {}.", e.message),
            )),
            Ok(text) if !typed.edifact_reads_back(&text) => made.findings.push(Finding::blocking(
                "read_back",
                None,
                "The message written does not read back as what was meant.",
            )),
            Ok(_) if preview => {}
            Ok(_) => {
                decide_send(ctx, &partner, code, &made).await?;
                let interchange =
                    control::next(&mut *ctx.conn, partner.id, Kind::Interchange).await?;
                let envelope = partner.edifact_envelope(at, interchange)?;
                let written = crate::edifact::write(&envelope, &[outgoing(body)])
                    .map_err(|e| Error::internal(e.message))?;
                if !typed.edifact_reads_back(&written) {
                    return Err(Error::internal(
                        "the message with its control reference does not read back",
                    ));
                }
                bytes = Some(Fixed {
                    bytes: written,
                    interchange: envelope.control.clone(),
                    group: envelope.control,
                    set: "1".into(),
                });
            }
        }
    }
    store_made(ctx, made, preview, previous, segments, bytes).await
}

/// Keeps a message made: `ready` with its bytes stored, or `invalid` with
/// what is wrong; a preview keeps nothing.
async fn store_made(
    ctx: &mut Ctx<'_>,
    made: Made,
    preview: bool,
    previous: Option<(Uuid, String)>,
    mut segments: Vec<ExplainedSegment>,
    bytes: Option<Fixed>,
) -> Result<Generated> {
    let message = made.message;
    let partner = made.partner.clone();
    let doc_type = made.view.header.doc_type.clone();
    let doc_id = made.view.header.id;
    let valid = made
        .findings
        .iter()
        .all(|f| f.severity != Severity::Blocking);
    if preview {
        return Ok(Generated {
            message: None,
            state: if valid { "ready" } else { "invalid" }.into(),
            findings: made.findings,
            segments,
            supersedes: previous.map(|p| p.0),
        });
    }

    let usage = if partner.is_test() { 'T' } else { 'P' };
    let message_id = new_id();
    let set_control = bytes.as_ref().map(|f| f.set.clone());
    let (interchange_id, group_control, state) = match bytes {
        Some(fixed) => {
            let id = store::store_outgoing(
                &mut *ctx.conn,
                store::Outgoing {
                    company_id: partner.company_id,
                    partner_id: partner.id,
                    control: &fixed.interchange,
                    usage,
                    bytes: &fixed.bytes,
                },
            )
            .await?;
            ctx.touch("edi_interchange", id);
            (Some(id), Some(fixed.group), "ready")
        }
        None => {
            segments.clear();
            (None, None, "invalid")
        }
    };
    sqlx::query(
        "INSERT INTO edi_message (id, interchange_id, company_id, partner_id, direction, message,
             group_control, control, position, state, business_key, doc_type, document_id,
             answers_id, findings)
         VALUES ($1, $2, $3, $4, 'out', $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(message_id)
    .bind(interchange_id)
    .bind(partner.company_id)
    .bind(partner.id)
    .bind(message.code_for(&partner))
    .bind(&group_control)
    .bind(&set_control)
    .bind(interchange_id.map(|_| 1i32))
    .bind(state)
    .bind(&made.sent.order_number)
    .bind(&doc_type)
    .bind(doc_id)
    .bind(made.sent.message_id)
    .bind(json!(made.findings))
    .execute(&mut *ctx.conn)
    .await?;
    ctx.touch("edi_message", message_id);
    if let Some((old, _)) = &previous {
        sqlx::query("UPDATE edi_message SET superseded_by = $2 WHERE id = $1")
            .bind(old)
            .bind(message_id)
            .execute(&mut *ctx.conn)
            .await?;
    }
    let out: MessageOut = sqlx::query_as(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM edi_message WHERE id = $1"
    ))
    .bind(message_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    Ok(Generated {
        message: Some(out),
        state: state.into(),
        findings: made.findings,
        segments,
        supersedes: previous.map(|p| p.0),
    })
}

/// An invoice whose order's ship notice the partner has not acknowledged:
/// the policy `edi_invoice.before_asn` decides.
async fn before_asn(ctx: &mut Ctx<'_>, partner: &Partner, sent: &SentOrder) -> Result<()> {
    let notice: Option<String> = sqlx::query_scalar(
        "SELECT state FROM edi_message
         WHERE partner_id = $1 AND direction = 'out' AND message IN ('856', 'DESADV')
           AND business_key = $2
           AND superseded_by IS NULL
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(partner.id)
    .bind(&sent.order_number)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let notice = notice.unwrap_or_else(|| "none".into());
    if notice == "acknowledged" {
        return Ok(());
    }
    ctx.decide(
        BEFORE_ASN,
        partner.company_id,
        json!({
            "partner_code": partner.code,
            "order_number": sent.order_number,
            "notice": notice,
        }),
    )
    .await
}

/// Makes, checks and stores a message, as `edi.generate` does.
pub async fn generate(
    ctx: &mut Ctx<'_>,
    doc_type: &str,
    id: Uuid,
    message: Message,
    preview: bool,
) -> Result<Generated> {
    let made = make(ctx, doc_type, id, message).await?;
    finish(ctx, made, preview).await
}
