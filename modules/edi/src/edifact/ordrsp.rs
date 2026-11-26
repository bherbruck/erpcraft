//! ORDRSP (D.96A): the seller's answer to an order, written from the same
//! `OrderAck` the 855 is written from, and read back to check it.
//!
//! In this module's words: `BGM` 231 with the seller's order number and
//! what the answer is (29 accepted as ordered, 4 changed, 27 not
//! accepted); `DTM` 137 the day it was answered; `RFF` `ON` the buyer's
//! order with its date (`DTM` 171); the parties; a `LIN` per line of the
//! order, with what is done with it (5 accepted, 3 quantity changed, 6
//! price changed, 7 not accepted), `QTY` 21 as ordered and 113 as it will
//! be delivered, `PRI` `AAA`; then `UNS` and `CNT` 2.

use crate::edifact::Message;
use crate::edifact::common::{
    date_of, date_out, dtm, lin_product, nad, pia_products, pri, product_segments, qty, read_dtm,
    read_nad, read_num, read_rff, rff, seg,
};
use crate::edifact::contrl::{MISSING, NOT_SUPPORTED, Verdict};
use crate::segment::{Delimiters, Segment};
use crate::x12::po850::Reference;
use crate::x12::poack855::{self, AckLine, LineStatus, OrderAck};

const ANSWERS: &[(&str, &str)] = &[
    ("29", poack855::ACCEPTED),
    ("4", poack855::CHANGED),
    ("27", poack855::REJECTED),
];

const ACTIONS: &[(&str, &str)] = &[
    ("5", poack855::LINE_ACCEPTED),
    ("3", poack855::LINE_QUANTITY_CHANGED),
    ("6", poack855::LINE_PRICE_CHANGED),
    ("7", poack855::LINE_REJECTED),
];

fn typed(table: &[(&str, &str)], code: &str) -> String {
    table
        .iter()
        .find(|(e, _)| *e == code)
        .map_or(code, |(_, t)| t)
        .to_owned()
}

fn edifact(table: &[(&str, &str)], code: &str) -> String {
    table
        .iter()
        .find(|(_, t)| *t == code)
        .map_or(code, |(e, _)| e)
        .to_owned()
}

pub fn body(ack: &OrderAck, d: &Delimiters) -> Vec<Segment> {
    let mark = d.decimal;
    let mut out = vec![seg(
        "BGM",
        vec![
            comps!["231"],
            comps![ack.seller_order.clone()],
            comps![edifact(ANSWERS, &ack.ack_type)],
        ],
    )];
    if let Some(date) = ack.date {
        out.push(dtm("137", date));
    }
    for date in &ack.dates {
        out.push(dtm(&date_out(&date.qualifier), date.date));
    }
    if let Some(currency) = &ack.currency {
        out.push(seg("CUX", vec![comps!["2", currency.clone(), "9"]]));
    }
    out.push(rff(&Reference {
        qualifier: "ON".into(),
        value: ack.order_number.clone(),
        description: String::new(),
    }));
    if let Some(date) = ack.order_date {
        out.push(dtm("171", date));
    }
    out.extend(ack.references.iter().map(rff));
    out.extend(ack.parties.iter().map(nad));
    for line in &ack.lines {
        let status = line.statuses.first();
        let (lin, pia) = product_segments(&line.product_ids);
        out.push(seg(
            "LIN",
            vec![
                comps![line.line_id.clone()],
                comps![
                    status
                        .map(|s| edifact(ACTIONS, &s.code))
                        .unwrap_or_default()
                ],
                lin,
            ],
        ));
        out.extend(pia);
        if let Some(q) = line.quantity {
            out.push(qty("21", q, &line.uom, mark));
        }
        if let Some(status) = status {
            if let Some(q) = status.quantity {
                out.push(qty("113", q, &status.uom, mark));
            }
            if let Some(date) = status.date {
                out.push(dtm(&date_out(&status.date_qualifier), date));
            }
        }
        if let Some(price) = line.unit_price {
            out.push(pri("AAA", price, mark));
        }
    }
    out.push(seg("UNS", vec![comps!["S"]]));
    out.push(seg("CNT", vec![comps!["2", ack.lines.len().to_string()]]));
    out
}

pub fn read(message: &Message, d: &Delimiters) -> (OrderAck, Verdict) {
    let mut ack = OrderAck {
        purpose: "00".into(),
        ..OrderAck::default()
    };
    let mut verdict = Verdict::default();
    if message.kind != "ORDRSP" {
        verdict.codes.push(NOT_SUPPORTED);
        return (ack, verdict);
    }
    let mut seen_bgm = false;
    let mut after_order_ref = false;
    let mut line: Option<AckLine> = None;
    let last = message.segments.len().saturating_sub(1);
    for (position, segment) in message.segments.iter().enumerate() {
        if position == 0 || position == last {
            continue;
        }
        let tag = segment.tag.as_str();
        match (tag, line.as_mut()) {
            ("BGM", _) => {
                seen_bgm = true;
                ack.seller_order = segment.get(2).to_owned();
                ack.ack_type = typed(ANSWERS, segment.get(3));
            }
            ("DTM", None) => match read_dtm(segment) {
                Some((q, date, _)) if q == "137" => ack.date = Some(date),
                Some((q, date, _)) if q == "171" && after_order_ref => ack.order_date = Some(date),
                Some(_) => ack.dates.extend(date_of(segment)),
                None => ack.unhandled.push((position, tag.into())),
            },
            ("CUX", _) => ack.currency = Some(segment.component(1, 2).to_owned()),
            ("RFF", None) => {
                let reference = read_rff(segment);
                if reference.qualifier == "ON" && ack.order_number.is_empty() {
                    ack.order_number = reference.value;
                    after_order_ref = true;
                } else {
                    after_order_ref = false;
                    ack.references.push(reference);
                }
            }
            ("NAD", None) => ack.parties.push(read_nad(segment)),
            ("LIN", _) => {
                if let Some(l) = line.take() {
                    ack.lines.push(l);
                }
                let mut l = AckLine {
                    line_id: segment.get(1).to_owned(),
                    ..AckLine::default()
                };
                l.product_ids.extend(lin_product(segment));
                if !segment.get(2).is_empty() {
                    l.statuses.push(LineStatus {
                        code: typed(ACTIONS, segment.get(2)),
                        ..LineStatus::default()
                    });
                }
                line = Some(l);
            }
            ("PIA", Some(l)) => l.product_ids.extend(pia_products(segment)),
            ("QTY", Some(l)) => {
                let q = read_num(segment.component(1, 2), d);
                let uom = segment.component(1, 3).to_owned();
                match (segment.component(1, 1), l.statuses.first_mut()) {
                    ("21", _) => {
                        l.quantity = q;
                        l.uom = uom;
                    }
                    ("113", Some(status)) => {
                        status.quantity = q;
                        status.uom = uom;
                    }
                    _ => ack.unhandled.push((position, tag.into())),
                }
            }
            ("DTM", Some(l)) => match (date_of(segment), l.statuses.first_mut()) {
                (Some(date), Some(status)) => {
                    status.date_qualifier = date.qualifier;
                    status.date = Some(date.date);
                }
                _ => ack.unhandled.push((position, tag.into())),
            },
            ("PRI", Some(l)) if segment.component(1, 1) == "AAA" => {
                l.unit_price = read_num(segment.component(1, 2), d);
            }
            ("UNS", _) => {
                if let Some(l) = line.take() {
                    ack.lines.push(l);
                }
            }
            ("CNT", _) if segment.component(1, 1) == "2" => {
                ack.lines_stated = segment.component(1, 2).parse().ok();
            }
            _ => ack.unhandled.push((position, tag.into())),
        }
    }
    if let Some(l) = line.take() {
        ack.lines.push(l);
    }
    if !seen_bgm {
        verdict.codes.push(MISSING);
    }
    (ack, verdict)
}

/// What of an acknowledgement an ORDRSP carries, as it reads back.
pub fn carried(ack: &OrderAck) -> OrderAck {
    let mut out = ack.clone();
    out.purpose = "00".into();
    out.release.clear();
    out.unhandled.clear();
    out.notes.clear();
    out.lines_stated = Some(ack.lines.len() as u32);
    for line in &mut out.lines {
        line.statuses.truncate(1);
    }
    for reference in &mut out.references {
        reference.description.clear();
    }
    for party in &mut out.parties {
        party.address_lines.truncate(4);
    }
    out
}
