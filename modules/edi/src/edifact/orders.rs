//! ORDERS (directory D.96A), read into the same `PurchaseOrder` the 850
//! is read into, and written from one (to make test files, and for a
//! company that buys over EDIFACT later).
//!
//! What is read, in this module's words:
//! - `BGM`: the order number, and what it is (an original, a
//!   replacement, a cancellation);
//! - `DTM`: the order's date (137) and the dates the buyer asks for
//!   (delivery, shipment, not before, not after);
//! - `RFF` and `CUX`: references, the currency;
//! - `NAD`: the buyer, supplier, delivery point and invoicee, with their
//!   numbers (a GLN when the agency is 9) and addresses;
//! - a line per `LIN` (its number, a GTIN), with `PIA` (the supplier's and
//!   the buyer's numbers), `IMD` (a description), `QTY` 21 (ordered),
//!   `PRI` (`AAA` net, else `AAB` gross), `DTM`;
//! - `UNS`, `CNT` 2 (the lines the buyer counted), `MOA` 86 or 79 (a
//!   total).
//!
//! Anything else is kept as unhandled, with its position, as the 850
//! reader does: a person sees what the proposal leaves out.

use crate::edifact::Message;
use crate::edifact::common::{
    date_of, date_out, dtm, imd, lin_product, moa, nad, pia_products, pri, product_segments, qty,
    read_imd, read_nad, read_num, read_rff, rff, seg,
};
use crate::edifact::contrl::{MISSING, NOT_SUPPORTED, Verdict};
use crate::segment::{Delimiters, Segment};
use crate::x12::po850::{Line, PurchaseOrder};

/// What BGM 1225 says the order is, and the typed message's purpose.
const PURPOSES: &[(&str, &str)] = &[("9", "00"), ("1", "01"), ("5", "05"), ("6", "06")];

pub fn purpose_in(code: &str) -> String {
    PURPOSES
        .iter()
        .find(|(e, _)| *e == code)
        .map_or(code, |(_, t)| t)
        .to_owned()
}

pub fn purpose_out(code: &str) -> String {
    PURPOSES
        .iter()
        .find(|(_, t)| *t == code)
        .map_or(code, |(e, _)| e)
        .to_owned()
}

/// Reads an ORDERS message.
pub fn read(message: &Message, d: &Delimiters) -> (PurchaseOrder, Verdict) {
    let mut order = PurchaseOrder::default();
    let mut verdict = Verdict::default();
    if message.kind != "ORDERS" {
        verdict.codes.push(NOT_SUPPORTED);
        return (order, verdict);
    }
    let mut seen_bgm = false;
    let mut summary = false;
    let mut line: Option<Line> = None;
    let flush = |order: &mut PurchaseOrder, line: &mut Option<Line>| {
        if let Some(l) = line.take() {
            order.lines.push(l);
        }
    };
    let last = message.segments.len().saturating_sub(1);
    for (position, segment) in message.segments.iter().enumerate() {
        if position == 0 || position == last {
            continue;
        }
        let tag = segment.tag.as_str();
        match (tag, line.as_mut()) {
            ("BGM", _) => {
                seen_bgm = true;
                order.number = segment.get(2).to_owned();
                order.purpose = purpose_in(segment.get(3));
                order.order_type = segment.component(1, 1).to_owned();
            }
            ("DTM", None) if !summary => match date_of(segment) {
                Some(date) if date.qualifier == "137" => order.date = Some(date.date),
                Some(date) => order.dates.push(date),
                None => order.unhandled.push((position, tag.into())),
            },
            ("RFF", None) => order.references.push(read_rff(segment)),
            ("CUX", _) => order.currency = Some(segment.component(1, 2).to_owned()),
            ("NAD", None) => order.parties.push(read_nad(segment)),
            ("LIN", _) => {
                flush(&mut order, &mut line);
                let mut l = Line {
                    line_id: segment.get(1).to_owned(),
                    ..Line::default()
                };
                l.product_ids.extend(lin_product(segment));
                line = Some(l);
            }
            ("PIA", Some(l)) => l.product_ids.extend(pia_products(segment)),
            ("IMD", Some(l)) => l.descriptions.push(read_imd(segment)),
            ("QTY", Some(l)) if segment.component(1, 1) == "21" => {
                l.quantity = read_num(segment.component(1, 2), d);
                l.uom = segment.component(1, 3).to_owned();
            }
            ("PRI", Some(l)) => {
                let price = read_num(segment.component(1, 2), d);
                match segment.component(1, 1) {
                    "AAA" => l.unit_price = price,
                    "AAB" if l.unit_price.is_none() => l.unit_price = price,
                    _ => {}
                }
            }
            ("DTM", Some(l)) => match date_of(segment) {
                Some(date) => l.dates.push(date),
                None => order.unhandled.push((position, tag.into())),
            },
            ("RFF", Some(l)) => l.references.push(read_rff(segment)),
            ("NAD", Some(l)) => l.parties.push(read_nad(segment)),
            ("UNS", _) => {
                flush(&mut order, &mut line);
                summary = true;
            }
            ("CNT", _) if segment.component(1, 1) == "2" => {
                order.lines_stated = segment.component(1, 2).parse().ok();
            }
            ("MOA", _) if matches!(segment.component(1, 1), "86" | "79") => {
                order.total = read_num(segment.component(1, 2), d);
            }
            _ => order.unhandled.push((position, tag.into())),
        }
    }
    flush(&mut order, &mut line);
    if !seen_bgm {
        verdict.codes.push(MISSING);
    }
    if let Some(stated) = order.lines_stated
        && stated as usize != order.lines.len()
    {
        order.notes.push(format!(
            "The order counts {stated} lines and has {}.",
            order.lines.len()
        ));
    }
    (order, verdict)
}

/// The segments of an ORDERS for `order`, from the BGM to the last CNT or
/// MOA.
pub fn body(order: &PurchaseOrder, d: &Delimiters) -> Vec<Segment> {
    let mark = d.decimal;
    let mut out = vec![seg(
        "BGM",
        vec![
            comps![if order.order_type.is_empty() {
                "220".to_owned()
            } else {
                order.order_type.clone()
            }],
            comps![order.number.clone()],
            comps![purpose_out(&order.purpose)],
        ],
    )];
    if let Some(date) = order.date {
        out.push(dtm("137", date));
    }
    for date in &order.dates {
        out.push(dtm(&date_out(&date.qualifier), date.date));
    }
    out.extend(order.references.iter().map(rff));
    out.extend(order.parties.iter().map(nad));
    if let Some(currency) = &order.currency {
        out.push(seg("CUX", vec![comps!["2", currency.clone(), "9"]]));
    }
    for line in &order.lines {
        let (lin, pia) = product_segments(&line.product_ids);
        out.push(seg("LIN", vec![comps![line.line_id.clone()], vec![], lin]));
        out.extend(pia);
        out.extend(line.descriptions.iter().map(|t| imd(t)));
        if let Some(q) = line.quantity {
            out.push(qty("21", q, &line.uom, mark));
        }
        for date in &line.dates {
            out.push(dtm(&date_out(&date.qualifier), date.date));
        }
        if let Some(price) = line.unit_price {
            out.push(pri("AAA", price, mark));
        }
        out.extend(line.references.iter().map(rff));
        out.extend(line.parties.iter().map(nad));
    }
    out.push(seg("UNS", vec![comps!["S"]]));
    if let Some(total) = order.total {
        out.push(moa("86", total, mark));
    }
    out.push(seg("CNT", vec![comps!["2", order.lines.len().to_string()]]));
    out
}

/// What of an order an ORDERS carries: the fields this module reads, as
/// it reads them back.
pub fn carried(order: &PurchaseOrder) -> PurchaseOrder {
    let mut out = order.clone();
    out.release.clear();
    out.terms.clear();
    out.freight_terms = None;
    out.hash_total = None;
    out.unhandled.clear();
    out.notes.clear();
    out.lines_stated = Some(order.lines.len() as u32);
    if out.order_type.is_empty() {
        out.order_type = "220".into();
    }
    for line in &mut out.lines {
        line.price_basis.clear();
        line.pack = None;
        line.allocations.clear();
    }
    for party in out
        .parties
        .iter_mut()
        .chain(out.lines.iter_mut().flat_map(|l| l.parties.iter_mut()))
    {
        party.address_lines.truncate(4);
    }
    for reference in out
        .references
        .iter_mut()
        .chain(out.lines.iter_mut().flat_map(|l| l.references.iter_mut()))
    {
        reference.description.clear();
    }
    out
}
