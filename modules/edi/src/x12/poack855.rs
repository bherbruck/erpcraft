//! The 855 order acknowledgement: what the seller does with each line of
//! a buyer's order. Written from an approved sales order and what the
//! partner's 850 said; read back to check it, and readable from a vendor.
//!
//! The acknowledgement type in BAK02 and the line codes in ACK01 are the
//! few this module acts on: `AD` accepted with detail and no change, `AC`
//! accepted with detail and changes, `RD` rejected with detail; per line
//! `IA` accepted, `IQ` quantity changed, `IP` price changed, `IR`
//! rejected, `IB` backordered.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::segment::Segment;
use crate::x12::TransactionSet;
use crate::x12::ack::{SEGMENT_MISSING, SET_NOT_SUPPORTED, SegmentError, SetVerdict};
use crate::x12::fields::{
    self, Fields, date, date_segment, number, opt_number, party_segments, product_elements,
    read_party, read_product_ids, reference_segment, segment,
};
use crate::x12::po850::{DateOf, Party, ProductId, Reference};

/// BAK02.
pub const ACCEPTED: &str = "AD";
pub const CHANGED: &str = "AC";
pub const REJECTED: &str = "RD";
/// ACK01.
pub const LINE_ACCEPTED: &str = "IA";
pub const LINE_QUANTITY_CHANGED: &str = "IQ";
pub const LINE_PRICE_CHANGED: &str = "IP";
pub const LINE_REJECTED: &str = "IR";
pub const LINE_BACKORDERED: &str = "IB";

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OrderAck {
    /// BAK01: `00` original.
    pub purpose: String,
    /// BAK02: `AD`, `AC` or `RD`.
    pub ack_type: String,
    /// BAK03: the buyer's order number, as received.
    pub order_number: String,
    /// BAK04: the buyer's order date, as received.
    pub order_date: Option<NaiveDate>,
    /// BAK05: the release number, as received.
    pub release: String,
    /// BAK08: the seller's own order number.
    pub seller_order: String,
    /// BAK09: when the order was acknowledged.
    pub date: Option<NaiveDate>,
    pub currency: Option<String>,
    pub references: Vec<Reference>,
    pub dates: Vec<DateOf>,
    pub parties: Vec<Party>,
    pub lines: Vec<AckLine>,
    /// CTT01.
    pub lines_stated: Option<u32>,
    pub unhandled: Vec<(usize, String)>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AckLine {
    /// PO101 of the order, echoed.
    pub line_id: String,
    /// As ordered, in the partner's unit.
    pub quantity: Option<Decimal>,
    pub uom: String,
    pub unit_price: Option<Decimal>,
    /// As ordered, echoed.
    pub product_ids: Vec<ProductId>,
    pub statuses: Vec<LineStatus>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LineStatus {
    /// ACK01.
    pub code: String,
    /// ACK02 and ACK03: the quantity this status is about.
    pub quantity: Option<Decimal>,
    pub uom: String,
    /// ACK04 and ACK05: a date, such as when the line ships.
    pub date_qualifier: String,
    pub date: Option<NaiveDate>,
}

impl OrderAck {
    /// The segments of the set from BAK to CTT, without ST and SE.
    pub fn body(&self) -> Vec<Segment> {
        let mut out = vec![segment(
            "BAK",
            vec![
                self.purpose.clone(),
                self.ack_type.clone(),
                self.order_number.clone(),
                date(self.order_date),
                self.release.clone(),
                String::new(),
                String::new(),
                self.seller_order.clone(),
                date(self.date),
            ],
        )];
        if let Some(currency) = &self.currency {
            out.push(segment("CUR", vec!["SE".into(), currency.clone()]));
        }
        out.extend(self.references.iter().map(reference_segment));
        out.extend(self.dates.iter().map(date_segment));
        for party in &self.parties {
            out.extend(party_segments(party));
        }
        for line in &self.lines {
            let mut po1 = vec![
                line.line_id.clone(),
                opt_number(line.quantity),
                line.uom.clone(),
                opt_number(line.unit_price),
                String::new(),
            ];
            po1.extend(product_elements(&line.product_ids));
            out.push(segment("PO1", po1));
            for status in &line.statuses {
                out.push(segment(
                    "ACK",
                    vec![
                        status.code.clone(),
                        opt_number(status.quantity),
                        status.uom.clone(),
                        status.date_qualifier.clone(),
                        date(status.date),
                    ],
                ));
            }
        }
        out.push(segment("CTT", vec![self.lines.len().to_string()]));
        out
    }
}

/// Reads a transaction set as an 855.
pub fn read(set: &TransactionSet) -> (OrderAck, SetVerdict) {
    let mut fields = Fields::default();
    let mut ack = OrderAck::default();
    if set.id != "855" {
        fields.verdict.codes.push(SET_NOT_SUPPORTED);
        return (ack, fields.verdict);
    }
    let mut seen_bak = false;
    let mut in_party = false;
    for (index, segment) in set.segments.iter().enumerate() {
        let position = index + 1;
        let tag = segment.tag.as_str();
        if !matches!(tag, "N1" | "N2" | "N3" | "N4") {
            in_party = false;
        }
        match tag {
            "ST" | "SE" => {}
            "BAK" if !seen_bak && position == 2 => {
                seen_bak = true;
                ack.purpose = fields.required(position, segment, 1);
                ack.ack_type = fields.required(position, segment, 2);
                ack.order_number = fields.required(position, segment, 3);
                ack.order_date = fields.date(position, segment, 4);
                ack.release = segment.get(5).to_owned();
                ack.seller_order = segment.get(8).to_owned();
                ack.date = fields.date(position, segment, 9);
            }
            "CUR" if ack.lines.is_empty() => {
                ack.currency = Some(fields.required(position, segment, 2));
            }
            "REF" if ack.lines.is_empty() => ack.references.push(Reference {
                qualifier: fields.required(position, segment, 1),
                value: segment.get(2).to_owned(),
                description: segment.get(3).to_owned(),
            }),
            "DTM" if ack.lines.is_empty() => {
                let qualifier = fields.required(position, segment, 1);
                if let Some(date) = fields.date(position, segment, 2) {
                    ack.dates.push(DateOf { qualifier, date });
                }
            }
            "N1" | "N2" | "N3" | "N4" if ack.lines.is_empty() => {
                if read_party(&mut ack.parties, in_party, segment) {
                    in_party = true;
                } else {
                    ack.unhandled.push((position, tag.to_owned()));
                }
            }
            "PO1" if ack.lines_stated.is_none() => {
                let line = AckLine {
                    line_id: segment.get(1).to_owned(),
                    quantity: fields.number(position, segment, 2),
                    uom: segment.get(3).to_owned(),
                    unit_price: fields.number(position, segment, 4),
                    product_ids: read_product_ids(&mut fields, position, segment, 6, 24),
                    statuses: Vec::new(),
                };
                ack.lines.push(line);
            }
            "ACK" if !ack.lines.is_empty() && ack.lines_stated.is_none() => {
                let status = LineStatus {
                    code: fields.required(position, segment, 1),
                    quantity: fields.number(position, segment, 2),
                    uom: segment.get(3).to_owned(),
                    date_qualifier: segment.get(4).to_owned(),
                    date: fields.date(position, segment, 5),
                };
                if let Some(line) = ack.lines.last_mut() {
                    line.statuses.push(status);
                }
            }
            "CTT" => ack.lines_stated = fields.whole(position, segment, 1),
            _ => ack.unhandled.push((position, tag.to_owned())),
        }
    }
    if !seen_bak {
        fields
            .verdict
            .segments
            .push(SegmentError::segment("BAK", 2, SEGMENT_MISSING));
    }
    if let Some(stated) = ack.lines_stated
        && stated as usize != ack.lines.len()
    {
        ack.notes.push(format!(
            "The acknowledgement says it has {stated} lines (CTT01) and has {}.",
            ack.lines.len()
        ));
    }
    let _ = number;
    let _ = fields::MAX_NUMBER_LEN;
    (ack, fields.verdict)
}
