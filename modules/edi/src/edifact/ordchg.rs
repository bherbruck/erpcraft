//! ORDCHG (D.96A): the buyer's change to an order, read into the same
//! `OrderChange` the 860 is read into, and written for test files.
//!
//! In this module's words: `BGM` 230 with the change's number and what it
//! is (`1` cancel the order, `4` change it); `DTM` 137 its date; `RFF`
//! `ON` the order changed, with its date (`DTM` 171); the parties; a `LIN`
//! per line changed with what is done with it (`1` added, `2` deleted, `3`
//! changed), `PIA` its other numbers, `QTY` 21 its quantity after the
//! change, `PRI` `AAA` its price, a `DTM` its dates; `UNS` and `CNT` 2.

use crate::edifact::Message;
use crate::edifact::common::{
    date_of, date_out, dtm, lin_product, nad, pia_products, pri, product_segments, qty, read_dtm,
    read_nad, read_num, read_rff, rff, seg,
};
use crate::edifact::contrl::{MISSING, NOT_SUPPORTED, Verdict};
use crate::segment::{Delimiters, Segment};
use crate::x12::change860::{self, ChangeLine, OrderChange};
use crate::x12::po850::Reference;

const PURPOSES: &[(&str, &str)] = &[("1", change860::CANCEL), ("4", change860::CHANGE)];
const ACTIONS: &[(&str, &str)] = &[
    ("1", change860::ADD),
    ("2", change860::DELETE),
    ("3", change860::CHANGES),
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

pub fn body(change: &OrderChange, d: &Delimiters) -> Vec<Segment> {
    let mark = d.decimal;
    let mut out = vec![seg(
        "BGM",
        vec![
            comps!["230"],
            comps![change.change_number.clone()],
            comps![edifact(PURPOSES, &change.purpose)],
        ],
    )];
    for date in &change.dates {
        out.push(dtm(&date_out(&date.qualifier), date.date));
    }
    out.push(rff(&Reference {
        qualifier: "ON".into(),
        value: change.order_number.clone(),
        description: String::new(),
    }));
    if let Some(date) = change.order_date {
        out.push(dtm("171", date));
    }
    out.extend(change.references.iter().map(rff));
    out.extend(change.parties.iter().map(nad));
    for line in &change.lines {
        let (lin, pia) = product_segments(&line.product_ids);
        out.push(seg(
            "LIN",
            vec![
                comps![line.line_id.clone()],
                comps![edifact(ACTIONS, &line.change)],
                lin,
            ],
        ));
        out.extend(pia);
        if let Some(q) = line.quantity {
            out.push(qty("21", q, &line.uom, mark));
        }
        if let Some(p) = line.unit_price {
            out.push(pri("AAA", p, mark));
        }
        for date in &line.dates {
            out.push(dtm(&date_out(&date.qualifier), date.date));
        }
    }
    out.push(seg("UNS", vec![comps!["S"]]));
    out.push(seg(
        "CNT",
        vec![comps!["2", change.lines.len().to_string()]],
    ));
    out
}

pub fn read(message: &Message, d: &Delimiters) -> (OrderChange, Verdict) {
    let mut change = OrderChange::default();
    let mut verdict = Verdict::default();
    if message.kind != "ORDCHG" {
        verdict.codes.push(NOT_SUPPORTED);
        return (change, verdict);
    }
    let mut seen_bgm = false;
    let mut after_order_ref = false;
    let mut line: Option<ChangeLine> = None;
    let last = message.segments.len().saturating_sub(1);
    for (position, segment) in message.segments.iter().enumerate() {
        if position == 0 || position == last {
            continue;
        }
        let tag = segment.tag.as_str();
        match (tag, line.as_mut()) {
            ("BGM", _) => {
                seen_bgm = true;
                change.change_number = segment.get(2).to_owned();
                change.purpose = typed(PURPOSES, segment.get(3));
            }
            ("DTM", None) => match read_dtm(segment) {
                Some((q, date, _)) if q == "171" && after_order_ref => {
                    change.order_date = Some(date)
                }
                Some(_) => change.dates.extend(date_of(segment)),
                None => change.unhandled.push((position, tag.into())),
            },
            ("RFF", None) => {
                let reference = read_rff(segment);
                if reference.qualifier == "ON" && change.order_number.is_empty() {
                    change.order_number = reference.value;
                    after_order_ref = true;
                } else {
                    after_order_ref = false;
                    change.references.push(reference);
                }
            }
            ("NAD", None) => change.parties.push(read_nad(segment)),
            ("LIN", _) => {
                if let Some(l) = line.take() {
                    change.lines.push(l);
                }
                let mut l = ChangeLine {
                    line_id: segment.get(1).to_owned(),
                    change: typed(ACTIONS, segment.get(2)),
                    ..ChangeLine::default()
                };
                l.product_ids.extend(lin_product(segment));
                line = Some(l);
            }
            ("PIA", Some(l)) => l.product_ids.extend(pia_products(segment)),
            ("QTY", Some(l)) if segment.component(1, 1) == "21" => {
                l.quantity = read_num(segment.component(1, 2), d);
                l.uom = segment.component(1, 3).to_owned();
            }
            ("PRI", Some(l)) if segment.component(1, 1) == "AAA" => {
                l.unit_price = read_num(segment.component(1, 2), d);
            }
            ("DTM", Some(l)) => match date_of(segment) {
                Some(date) => l.dates.push(date),
                None => change.unhandled.push((position, tag.into())),
            },
            ("UNS", _) => {
                if let Some(l) = line.take() {
                    change.lines.push(l);
                }
            }
            ("CNT", _) if segment.component(1, 1) == "2" => {
                change.lines_stated = segment.component(1, 2).parse().ok();
            }
            _ => change.unhandled.push((position, tag.into())),
        }
    }
    if let Some(l) = line.take() {
        change.lines.push(l);
    }
    if !seen_bgm {
        verdict.codes.push(MISSING);
    }
    if let Some(stated) = change.lines_stated
        && stated as usize != change.lines.len()
    {
        change.notes.push(format!(
            "The change says it has {stated} lines (CNT) and has {}.",
            change.lines.len()
        ));
    }
    (change, verdict)
}
