//! REMADV (D.96A): the remittance advice, read into and written from the
//! same `Remittance` the 820 is.
//!
//! In this module's words: `BGM` 481 (a remittance advice) with the
//! payment's trace number; `DTM` 137 the day it is paid; `MOA` 9 what it
//! pays in all; `NAD` the payer (`PR`) and the payee (`PE`); an `AJT`
//! with its reason and `MOA` 5 its amount for an adjustment of the whole
//! payment; then per document paid a `DOC` (380 an invoice) with its
//! number, `MOA` 12 what is paid of it, 77 what it came to, 52 the
//! discount, and its own `AJT` and `MOA` 5. (These codes are this module's
//! choice and not checked against the directory.)

use crate::edifact::Message;
use crate::edifact::common::{dtm, moa, nad, read_dtm, read_nad, read_num, seg};
use crate::edifact::contrl::{MISSING, NOT_SUPPORTED, Verdict};
use crate::segment::{Delimiters, Segment};
use crate::x12::remit820::{Adjustment, INVOICE, RemitItem, Remittance};

fn adjustment(a: &Adjustment, mark: u8, out: &mut Vec<Segment>) {
    out.push(seg(
        "AJT",
        vec![comps![a.reason.clone()], comps![a.reference.clone()]],
    ));
    if let Some(amount) = a.amount {
        out.push(moa("5", amount, mark));
    }
}

pub fn body(remit: &Remittance, d: &Delimiters) -> Vec<Segment> {
    let mark = d.decimal;
    let mut out = vec![seg(
        "BGM",
        vec![comps!["481"], comps![remit.trace.clone()], comps!["9"]],
    )];
    if let Some(date) = remit.date {
        out.push(dtm("137", date));
    }
    if let Some(amount) = remit.amount {
        out.push(moa("9", amount, mark));
    }
    out.extend(remit.parties.iter().map(nad));
    for a in &remit.adjustments {
        adjustment(a, mark, &mut out);
    }
    for item in &remit.items {
        out.push(seg(
            "DOC",
            vec![
                comps![if item.qualifier == INVOICE { "380" } else { "" }],
                comps![item.reference.clone()],
            ],
        ));
        if let Some(a) = item.paid {
            out.push(moa("12", a, mark));
        }
        if let Some(a) = item.invoiced {
            out.push(moa("77", a, mark));
        }
        if let Some(a) = item.discount {
            out.push(moa("52", a, mark));
        }
        for a in &item.adjustments {
            adjustment(a, mark, &mut out);
        }
    }
    out.push(seg("UNS", vec![comps!["S"]]));
    out
}

pub fn read(message: &Message, d: &Delimiters) -> (Remittance, Verdict) {
    let mut remit = Remittance {
        handling: "I".into(),
        credit_debit: "C".into(),
        ..Remittance::default()
    };
    let mut verdict = Verdict::default();
    if message.kind != "REMADV" {
        verdict.codes.push(NOT_SUPPORTED);
        return (remit, verdict);
    }
    let mut seen_bgm = false;
    // An AJT opened, whose amount the next MOA 5 gives.
    let mut open: Option<Adjustment> = None;
    let close = |remit: &mut Remittance, a: Adjustment| match remit.items.last_mut() {
        Some(item) => item.adjustments.push(a),
        None => remit.adjustments.push(a),
    };
    let last = message.segments.len().saturating_sub(1);
    for (position, segment) in message.segments.iter().enumerate() {
        if position == 0 || position == last {
            continue;
        }
        let tag = segment.tag.as_str();
        let value = || read_num(segment.component(1, 2), d);
        let qualifier = segment.component(1, 1);
        if tag != "MOA"
            && let Some(a) = open.take()
        {
            close(&mut remit, a);
        }
        match tag {
            "BGM" => {
                seen_bgm = true;
                remit.trace = segment.get(2).to_owned();
            }
            "DTM" => match read_dtm(segment) {
                Some((q, date, _)) if q == "137" => remit.date = Some(date),
                _ => remit.unhandled.push((position, tag.into())),
            },
            "NAD" if remit.items.is_empty() => remit.parties.push(read_nad(segment)),
            "AJT" => {
                open = Some(Adjustment {
                    reason: segment.get(1).to_owned(),
                    reference: segment.get(2).to_owned(),
                    ..Adjustment::default()
                })
            }
            "DOC" => remit.items.push(RemitItem {
                qualifier: if segment.get(1) == "380" {
                    INVOICE.into()
                } else {
                    segment.get(1).to_owned()
                },
                reference: segment.get(2).to_owned(),
                ..RemitItem::default()
            }),
            "MOA" if qualifier == "5" && open.is_some() => {
                if let Some(mut a) = open.take() {
                    a.amount = value();
                    close(&mut remit, a);
                }
            }
            "MOA" if qualifier == "9" && remit.items.is_empty() => remit.amount = value(),
            "MOA" if !remit.items.is_empty() && matches!(qualifier, "12" | "77" | "52") => {
                if let Some(item) = remit.items.last_mut() {
                    match qualifier {
                        "12" => item.paid = value(),
                        "77" => item.invoiced = value(),
                        _ => item.discount = value(),
                    }
                }
            }
            "UNS" => {}
            _ => remit.unhandled.push((position, tag.into())),
        }
    }
    if let Some(a) = open.take() {
        close(&mut remit, a);
    }
    if !seen_bgm {
        verdict.codes.push(MISSING);
    }
    if let (Some(stated), Some(paid)) = (remit.amount, remit.items_paid())
        && stated != paid
    {
        remit.notes.push(format!(
            "The payment says it is {stated} (MOA 9) and its documents come to {paid}."
        ));
    }
    (remit, verdict)
}

/// What of a remittance a REMADV carries, as it reads back.
pub fn carried(remit: &Remittance) -> Remittance {
    let mut out = remit.clone();
    out.handling = "I".into();
    out.credit_debit = "C".into();
    out.method.clear();
    out.unhandled.clear();
    out.notes.clear();
    for a in out
        .adjustments
        .iter_mut()
        .chain(out.items.iter_mut().flat_map(|i| i.adjustments.iter_mut()))
    {
        a.reference_qualifier.clear();
    }
    for party in &mut out.parties {
        party.address_lines.truncate(4);
    }
    out
}
