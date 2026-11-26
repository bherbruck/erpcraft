//! The 860 order change: what a buyer changes in an order it sent. Read
//! into a proposal of changes against the sales order its 850 became;
//! written for test files and read back.
//!
//! In this module's words: `BCH` with its purpose (`01` cancel the whole
//! order, `04` change), the order type, the buyer's order number, the
//! release, the change's own number and the order's date; references,
//! dates and parties as the 850 has them; then a `POC` per line changed,
//! with the line id, what is done with it, the quantity and unit it has
//! after the change, its price and its product ids. Line changes this
//! module acts on: `AI` add the line, `DI` delete it, `QI` and `QD` its
//! quantity raised or lowered, `PC` its price, `CA` several of these.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::segment::Segment;
use crate::x12::TransactionSet;
use crate::x12::ack::{SEGMENT_MISSING, SET_NOT_SUPPORTED, SegmentError, SetVerdict};
use crate::x12::fields::{
    Fields, date, date_segment, opt_number, party_segments, product_elements, read_party,
    read_product_ids, reference_segment, segment,
};
use crate::x12::po850::{DateOf, Party, ProductId, Reference};

/// BCH01.
pub const CANCEL: &str = "01";
pub const CHANGE: &str = "04";
/// POC02.
pub const ADD: &str = "AI";
pub const DELETE: &str = "DI";
pub const QUANTITY_UP: &str = "QI";
pub const QUANTITY_DOWN: &str = "QD";
pub const PRICE: &str = "PC";
pub const CHANGES: &str = "CA";

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OrderChange {
    /// BCH01: `01` cancel, `04` change.
    pub purpose: String,
    /// BCH02.
    pub order_type: String,
    /// BCH03: the buyer's order number the change is to.
    pub order_number: String,
    /// BCH04.
    pub release: String,
    /// BCH05: the change's own number, counting the changes to the order.
    pub change_number: String,
    /// BCH06: the order's date.
    pub order_date: Option<NaiveDate>,
    pub references: Vec<Reference>,
    pub dates: Vec<DateOf>,
    pub parties: Vec<Party>,
    pub lines: Vec<ChangeLine>,
    /// CTT01.
    pub lines_stated: Option<u32>,
    pub unhandled: Vec<(usize, String)>,
    pub notes: Vec<String>,
    /// Not in the file: the change is an 850 that replaces the order, so
    /// a line of the order it leaves out is deleted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub replaces_all: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChangeLine {
    /// POC01: the line id of the order.
    pub line_id: String,
    /// POC02: what is done with the line.
    pub change: String,
    /// POC03 and POC05: the line's quantity after the change, and its unit.
    pub quantity: Option<Decimal>,
    pub uom: String,
    /// POC06: its price after the change.
    pub unit_price: Option<Decimal>,
    pub product_ids: Vec<ProductId>,
    pub dates: Vec<DateOf>,
}

impl OrderChange {
    /// The segments of the set from BCH to CTT, without ST and SE.
    pub fn body(&self) -> Vec<Segment> {
        let mut out = vec![segment(
            "BCH",
            vec![
                self.purpose.clone(),
                self.order_type.clone(),
                self.order_number.clone(),
                self.release.clone(),
                self.change_number.clone(),
                date(self.order_date),
            ],
        )];
        out.extend(self.references.iter().map(reference_segment));
        out.extend(self.dates.iter().map(date_segment));
        for party in &self.parties {
            out.extend(party_segments(party));
        }
        for line in &self.lines {
            let mut poc = vec![
                line.line_id.clone(),
                line.change.clone(),
                opt_number(line.quantity),
                String::new(),
                line.uom.clone(),
                opt_number(line.unit_price),
                String::new(),
            ];
            poc.extend(product_elements(&line.product_ids));
            out.push(segment("POC", poc));
            out.extend(line.dates.iter().map(date_segment));
        }
        out.push(segment("CTT", vec![self.lines.len().to_string()]));
        out
    }
}

/// Reads a transaction set as an 860.
pub fn read(set: &TransactionSet) -> (OrderChange, SetVerdict) {
    let mut fields = Fields::default();
    let mut change = OrderChange::default();
    if set.id != "860" {
        fields.verdict.codes.push(SET_NOT_SUPPORTED);
        return (change, fields.verdict);
    }
    let mut seen_bch = false;
    let mut in_party = false;
    for (index, segment) in set.segments.iter().enumerate() {
        let position = index + 1;
        let tag = segment.tag.as_str();
        if !matches!(tag, "N1" | "N2" | "N3" | "N4") {
            in_party = false;
        }
        match tag {
            "ST" | "SE" => {}
            "BCH" if !seen_bch && position == 2 => {
                seen_bch = true;
                change.purpose = fields.required(position, segment, 1);
                change.order_type = segment.get(2).to_owned();
                change.order_number = fields.required(position, segment, 3);
                change.release = segment.get(4).to_owned();
                change.change_number = segment.get(5).to_owned();
                change.order_date = fields.date(position, segment, 6);
            }
            "REF" if change.lines.is_empty() => change.references.push(Reference {
                qualifier: fields.required(position, segment, 1),
                value: segment.get(2).to_owned(),
                description: segment.get(3).to_owned(),
            }),
            "DTM" if change.lines_stated.is_none() => {
                let qualifier = fields.required(position, segment, 1);
                if let Some(date) = fields.date(position, segment, 2) {
                    let of = DateOf { qualifier, date };
                    match change.lines.last_mut() {
                        Some(line) => line.dates.push(of),
                        None => change.dates.push(of),
                    }
                }
            }
            "N1" | "N2" | "N3" | "N4" if change.lines.is_empty() => {
                if read_party(&mut change.parties, in_party, segment) {
                    in_party = true;
                } else {
                    change.unhandled.push((position, tag.to_owned()));
                }
            }
            "POC" if change.lines_stated.is_none() => change.lines.push(ChangeLine {
                line_id: fields.required(position, segment, 1),
                change: fields.required(position, segment, 2),
                quantity: fields.number(position, segment, 3),
                uom: segment.get(5).to_owned(),
                unit_price: fields.number(position, segment, 6),
                product_ids: read_product_ids(&mut fields, position, segment, 8, 30),
                dates: vec![],
            }),
            "CTT" => change.lines_stated = fields.whole(position, segment, 1),
            _ => change.unhandled.push((position, tag.to_owned())),
        }
    }
    if !seen_bch {
        fields
            .verdict
            .segments
            .push(SegmentError::segment("BCH", 2, SEGMENT_MISSING));
    }
    if let Some(stated) = change.lines_stated
        && stated as usize != change.lines.len()
    {
        change.notes.push(format!(
            "The change says it has {stated} lines (CTT01) and has {}.",
            change.lines.len()
        ));
    }
    (change, fields.verdict)
}

/// An 850 that cancels (`01`) or replaces (`05`) an order, as a change:
/// a cancellation changes nothing line by line (the whole order goes); a
/// replacement states every line as it is to be, so each is a change of
/// all it says (`CA`), and lines it leaves out are deleted by the caller.
pub fn of_order(order: &crate::x12::po850::PurchaseOrder) -> OrderChange {
    OrderChange {
        purpose: if order.purpose == "01" {
            CANCEL.into()
        } else {
            CHANGE.into()
        },
        order_type: order.order_type.clone(),
        order_number: order.number.clone(),
        release: order.release.clone(),
        change_number: String::new(),
        order_date: order.date,
        references: order.references.clone(),
        dates: order.dates.clone(),
        parties: order.parties.clone(),
        lines: if order.purpose == "01" {
            vec![]
        } else {
            order
                .lines
                .iter()
                .map(|l| ChangeLine {
                    line_id: l.line_id.clone(),
                    change: CHANGES.into(),
                    quantity: l.quantity,
                    uom: l.uom.clone(),
                    unit_price: l.unit_price,
                    product_ids: l.product_ids.clone(),
                    dates: vec![],
                })
                .collect()
        },
        lines_stated: None,
        unhandled: vec![],
        notes: vec![],
        replaces_all: order.purpose == "05",
    }
}
