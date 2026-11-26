//! The 846 inventory advice: how much of each of a partner's items the
//! seller can still promise. Written from stock availability for the items
//! the partner has numbers for; read back to check it.
//!
//! In this module's words: `BIA` with its purpose (`00` original), the
//! report's kind, its number and date; then per item a `LIN` with the
//! partner's product ids and a `QTY` with the quantity (`33`, what is
//! available) in the partner's unit; `CTT` the number of items.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::segment::Segment;
use crate::x12::TransactionSet;
use crate::x12::ack::{SEGMENT_MISSING, SET_NOT_SUPPORTED, SegmentError, SetVerdict};
use crate::x12::fields::{Fields, date, opt_number, product_elements, read_product_ids, segment};
use crate::x12::po850::ProductId;

/// QTY01: the quantity is what is available to promise.
pub const AVAILABLE: &str = "33";

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InventoryAdvice {
    /// BIA01: `00` original.
    pub purpose: String,
    /// BIA02: what kind of report (the partner's guide says which code).
    pub report_type: String,
    /// BIA03: the report's number.
    pub report_id: String,
    /// BIA04.
    pub date: Option<NaiveDate>,
    pub items: Vec<InventoryItem>,
    /// CTT01.
    pub items_stated: Option<u32>,
    pub unhandled: Vec<(usize, String)>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InventoryItem {
    pub product_ids: Vec<ProductId>,
    /// QTY01, such as `33`.
    pub qualifier: String,
    pub quantity: Option<Decimal>,
    pub uom: String,
}

impl InventoryAdvice {
    /// The segments of the set from BIA to CTT, without ST and SE.
    pub fn body(&self) -> Vec<Segment> {
        let mut out = vec![segment(
            "BIA",
            vec![
                self.purpose.clone(),
                self.report_type.clone(),
                self.report_id.clone(),
                date(self.date),
            ],
        )];
        for item in &self.items {
            let mut lin = vec![String::new()];
            lin.extend(product_elements(&item.product_ids));
            out.push(segment("LIN", lin));
            out.push(segment(
                "QTY",
                vec![
                    item.qualifier.clone(),
                    opt_number(item.quantity),
                    item.uom.clone(),
                ],
            ));
        }
        out.push(segment("CTT", vec![self.items.len().to_string()]));
        out
    }
}

/// Reads a transaction set as an 846.
pub fn read(set: &TransactionSet) -> (InventoryAdvice, SetVerdict) {
    let mut fields = Fields::default();
    let mut advice = InventoryAdvice::default();
    if set.id != "846" {
        fields.verdict.codes.push(SET_NOT_SUPPORTED);
        return (advice, fields.verdict);
    }
    let mut seen_bia = false;
    for (index, segment) in set.segments.iter().enumerate() {
        let position = index + 1;
        let tag = segment.tag.as_str();
        match tag {
            "ST" | "SE" => {}
            "BIA" if !seen_bia && position == 2 => {
                seen_bia = true;
                advice.purpose = fields.required(position, segment, 1);
                advice.report_type = segment.get(2).to_owned();
                advice.report_id = fields.required(position, segment, 3);
                advice.date = fields.date(position, segment, 4);
            }
            "LIN" if advice.items_stated.is_none() => advice.items.push(InventoryItem {
                product_ids: read_product_ids(&mut fields, position, segment, 2, 30),
                ..InventoryItem::default()
            }),
            "QTY" if advice.items_stated.is_none() && !advice.items.is_empty() => {
                let qualifier = fields.required(position, segment, 1);
                let quantity = fields.number(position, segment, 2);
                if let Some(item) = advice.items.last_mut() {
                    item.qualifier = qualifier;
                    item.quantity = quantity;
                    item.uom = segment.get(3).to_owned();
                }
            }
            "CTT" => advice.items_stated = fields.whole(position, segment, 1),
            _ => advice.unhandled.push((position, tag.to_owned())),
        }
    }
    if !seen_bia {
        fields
            .verdict
            .segments
            .push(SegmentError::segment("BIA", 2, SEGMENT_MISSING));
    }
    if let Some(stated) = advice.items_stated
        && stated as usize != advice.items.len()
    {
        advice.notes.push(format!(
            "The advice says it has {stated} items (CTT01) and has {}.",
            advice.items.len()
        ));
    }
    (advice, fields.verdict)
}
