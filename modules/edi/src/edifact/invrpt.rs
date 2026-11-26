//! INVRPT (D.96A): the inventory report, written from the same
//! `InventoryAdvice` the 846 is written from, and read back to check it.
//!
//! In this module's words: `BGM` 35 (an inventory report) with its number;
//! `DTM` 137 its date; a `LIN` per item with its GTIN, `PIA` its other
//! numbers, and `QTY` 145 the quantity in stock that can be promised (the
//! typed message's `33`) in the partner's unit; then `UNS` and `CNT` 2.

use crate::edifact::Message;
use crate::edifact::common::{
    dtm, lin_product, pia_products, product_segments, qty, read_dtm, read_num, seg,
};
use crate::edifact::contrl::{MISSING, NOT_SUPPORTED, Verdict};
use crate::segment::{Delimiters, Segment};
use crate::x12::inventory846::{AVAILABLE, InventoryAdvice, InventoryItem};

/// QTY 6063: EDIFACT's code for what the typed message calls available.
const QUANTITIES: &[(&str, &str)] = &[("145", AVAILABLE)];

fn typed(code: &str) -> String {
    QUANTITIES
        .iter()
        .find(|(e, _)| *e == code)
        .map_or(code, |(_, t)| t)
        .to_owned()
}

fn edifact(code: &str) -> String {
    QUANTITIES
        .iter()
        .find(|(_, t)| *t == code)
        .map_or(code, |(e, _)| e)
        .to_owned()
}

pub fn body(advice: &InventoryAdvice, d: &Delimiters) -> Vec<Segment> {
    let mark = d.decimal;
    let mut out = vec![seg(
        "BGM",
        vec![comps!["35"], comps![advice.report_id.clone()], comps!["9"]],
    )];
    if let Some(date) = advice.date {
        out.push(dtm("137", date));
    }
    for (n, item) in advice.items.iter().enumerate() {
        let (lin, pia) = product_segments(&item.product_ids);
        out.push(seg("LIN", vec![comps![(n + 1).to_string()], vec![], lin]));
        out.extend(pia);
        if let Some(q) = item.quantity {
            out.push(qty(&edifact(&item.qualifier), q, &item.uom, mark));
        }
    }
    out.push(seg("UNS", vec![comps!["S"]]));
    out.push(seg(
        "CNT",
        vec![comps!["2", advice.items.len().to_string()]],
    ));
    out
}

pub fn read(message: &Message, d: &Delimiters) -> (InventoryAdvice, Verdict) {
    let mut advice = InventoryAdvice {
        purpose: "00".into(),
        ..InventoryAdvice::default()
    };
    let mut verdict = Verdict::default();
    if message.kind != "INVRPT" {
        verdict.codes.push(NOT_SUPPORTED);
        return (advice, verdict);
    }
    let mut seen_bgm = false;
    let last = message.segments.len().saturating_sub(1);
    for (position, segment) in message.segments.iter().enumerate() {
        if position == 0 || position == last {
            continue;
        }
        let tag = segment.tag.as_str();
        match tag {
            "BGM" => {
                seen_bgm = true;
                advice.report_id = segment.get(2).to_owned();
            }
            "DTM" if advice.items.is_empty() => match read_dtm(segment) {
                Some((q, date, _)) if q == "137" => advice.date = Some(date),
                _ => advice.unhandled.push((position, tag.into())),
            },
            "LIN" => {
                let mut item = InventoryItem::default();
                item.product_ids.extend(lin_product(segment));
                advice.items.push(item);
            }
            "PIA" if !advice.items.is_empty() => {
                if let Some(item) = advice.items.last_mut() {
                    item.product_ids.extend(pia_products(segment));
                }
            }
            "QTY" if !advice.items.is_empty() => {
                if let Some(item) = advice.items.last_mut() {
                    item.qualifier = typed(segment.component(1, 1));
                    item.quantity = read_num(segment.component(1, 2), d);
                    item.uom = segment.component(1, 3).to_owned();
                }
            }
            "UNS" => {}
            "CNT" if segment.component(1, 1) == "2" => {
                advice.items_stated = segment.component(1, 2).parse().ok();
            }
            _ => advice.unhandled.push((position, tag.into())),
        }
    }
    if !seen_bgm {
        verdict.codes.push(MISSING);
    }
    (advice, verdict)
}

/// What of an advice an INVRPT carries, as it reads back.
pub fn carried(advice: &InventoryAdvice) -> InventoryAdvice {
    let mut out = advice.clone();
    out.purpose = "00".into();
    out.report_type.clear();
    out.unhandled.clear();
    out.notes.clear();
    out.items_stated = Some(advice.items.len() as u32);
    out
}
