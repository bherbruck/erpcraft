//! INVOIC (D.96A): the invoice, written from the same `Invoice` the 810 is
//! written from, and read back to check it.
//!
//! In this module's words: `BGM` 380 an invoice or 381 a credit note,
//! with its number; `DTM` 137 its date; `RFF` `ON` the buyer's order with
//! its date (`DTM` 171); `NAD` the parties; `CUX` the currency; `ALC` and
//! `MOA` the charges (`C`, amount 23) and allowances (`A`, amount 204) of
//! the whole invoice; a `LIN` per line, with `PIA`, `IMD`, `QTY` 47
//! invoiced, `MOA` 203 the line's amount, `PRI` `AAA` and the line's own
//! charges and allowances; then `UNS`, `CNT` 2, `MOA` 77 what the invoice
//! comes to and `MOA` 79 what its lines come to, and a `TAX` 7 per tax
//! with its rate and `MOA` 124 its amount. The amounts of a credit note
//! are written as positive figures, as the 810 writes them.

use rust_decimal::Decimal;

use crate::edifact::Message;
use crate::edifact::common::{
    date_of, date_out, dtm, imd, lin_product, moa, nad, num, pia_products, pri, product_segments,
    qty, read_dtm, read_imd, read_nad, read_num, read_rff, rff, seg,
};
use crate::edifact::contrl::{MISSING, NOT_SUPPORTED, Verdict};
use crate::segment::{Delimiters, Segment};
use crate::x12::invoice810::{CREDIT, Charge, DEBIT, Invoice, InvoiceLine, Tax};
use crate::x12::po850::Reference;

const TAXES: &[(&str, &str)] = &[("VAT", "VA"), ("GST", "GS")];

fn tax_in(code: &str) -> String {
    TAXES
        .iter()
        .find(|(e, _)| *e == code)
        .map_or(code, |(_, t)| t)
        .to_owned()
}

fn tax_out(code: &str) -> String {
    TAXES
        .iter()
        .find(|(_, t)| *t == code)
        .map_or(code, |(e, _)| e)
        .to_owned()
}

fn charge(c: &Charge, mark: u8) -> Vec<Segment> {
    vec![
        seg(
            "ALC",
            vec![
                comps![c.indicator.clone()],
                vec![],
                vec![],
                vec![],
                comps![c.code.clone(), "", "", c.description.clone()],
            ],
        ),
        moa(
            if c.indicator == "A" { "204" } else { "23" },
            c.amount,
            mark,
        ),
    ]
}

/// The segments of an INVOIC. `None` when an amount has more than two
/// places, as the 810 refuses it.
pub fn body(invoice: &Invoice, d: &Delimiters) -> Option<Vec<Segment>> {
    let mark = d.decimal;
    let two = |a: Decimal| a.normalize().scale() <= 2;
    let amounts = invoice
        .total
        .iter()
        .chain(invoice.taxes.iter().map(|t| &t.amount))
        .chain(invoice.charges.iter().map(|c| &c.amount))
        .chain(
            invoice
                .lines
                .iter()
                .flat_map(|l| l.charges.iter().map(|c| &c.amount)),
        );
    if !amounts.into_iter().all(|a| two(*a)) {
        return None;
    }
    let mut out = vec![seg(
        "BGM",
        vec![
            comps![if invoice.kind == CREDIT { "381" } else { "380" }],
            comps![invoice.number.clone()],
            comps!["9"],
        ],
    )];
    if let Some(date) = invoice.date {
        out.push(dtm("137", date));
    }
    for date in &invoice.dates {
        out.push(dtm(&date_out(&date.qualifier), date.date));
    }
    out.push(rff(&Reference {
        qualifier: "ON".into(),
        value: invoice.order_number.clone(),
        description: String::new(),
    }));
    if let Some(date) = invoice.order_date {
        out.push(dtm("171", date));
    }
    out.extend(invoice.references.iter().map(rff));
    out.extend(invoice.parties.iter().map(nad));
    if let Some(currency) = &invoice.currency {
        out.push(seg("CUX", vec![comps!["2", currency.clone(), "4"]]));
    }
    for c in &invoice.charges {
        out.extend(charge(c, mark));
    }
    let mut lines_total = Decimal::ZERO;
    for line in &invoice.lines {
        let (lin, pia) = product_segments(&line.product_ids);
        out.push(seg("LIN", vec![comps![line.line_id.clone()], vec![], lin]));
        out.extend(pia);
        if !line.description.is_empty() {
            out.push(imd(&line.description));
        }
        if let Some(q) = line.quantity {
            out.push(qty("47", q, &line.uom, mark));
        }
        if let (Some(q), Some(p)) = (line.quantity, line.unit_price) {
            let amount = (q * p).round_dp(2);
            lines_total += amount;
            out.push(moa("203", amount, mark));
        }
        if let Some(price) = line.unit_price {
            out.push(pri("AAA", price, mark));
        }
        for c in &line.charges {
            out.extend(charge(c, mark));
        }
    }
    out.push(seg("UNS", vec![comps!["S"]]));
    out.push(seg(
        "CNT",
        vec![comps!["2", invoice.lines.len().to_string()]],
    ));
    if let Some(total) = invoice.total {
        out.push(moa("77", total, mark));
    }
    out.push(moa("79", lines_total, mark));
    for tax in &invoice.taxes {
        out.push(seg(
            "TAX",
            vec![
                comps!["7"],
                comps![tax_out(&tax.kind)],
                vec![],
                vec![],
                comps![
                    "",
                    "",
                    "",
                    tax.percent.map(|p| num(p, mark)).unwrap_or_default()
                ],
            ],
        ));
        out.push(moa("124", tax.amount, mark));
    }
    Some(out)
}

pub fn read(message: &Message, d: &Delimiters) -> (Invoice, Verdict) {
    let mut invoice = Invoice::default();
    let mut verdict = Verdict::default();
    if message.kind != "INVOIC" {
        verdict.codes.push(NOT_SUPPORTED);
        return (invoice, verdict);
    }
    let mut seen_bgm = false;
    let mut summary = false;
    let mut after_order_ref = false;
    let mut line: Option<InvoiceLine> = None;
    // The charge an ALC opened, whose amount the next MOA gives.
    let mut open_charge: Option<(bool, Charge)> = None;
    let last = message.segments.len().saturating_sub(1);
    let close = |invoice: &mut Invoice, line: &mut Option<InvoiceLine>, c: (bool, Charge)| match (
        c.0,
        line.as_mut(),
    ) {
        (true, Some(l)) => l.charges.push(c.1),
        _ => invoice.charges.push(c.1),
    };
    for (position, segment) in message.segments.iter().enumerate() {
        if position == 0 || position == last {
            continue;
        }
        let tag = segment.tag.as_str();
        let value = || read_num(segment.component(1, 2), d);
        match (tag, summary) {
            ("BGM", _) => {
                seen_bgm = true;
                invoice.number = segment.get(2).to_owned();
                invoice.kind = if segment.get(1) == "381" {
                    CREDIT
                } else {
                    DEBIT
                }
                .into();
            }
            ("DTM", false) if line.is_none() => match read_dtm(segment) {
                Some((q, date, _)) if q == "137" => invoice.date = Some(date),
                Some((q, date, _)) if q == "171" && after_order_ref => {
                    invoice.order_date = Some(date)
                }
                Some(_) => invoice.dates.extend(date_of(segment)),
                None => invoice.unhandled.push((position, tag.into())),
            },
            ("RFF", false) if line.is_none() => {
                let reference = read_rff(segment);
                if reference.qualifier == "ON" && invoice.order_number.is_empty() {
                    invoice.order_number = reference.value;
                    after_order_ref = true;
                } else {
                    after_order_ref = false;
                    invoice.references.push(reference);
                }
            }
            ("NAD", false) if line.is_none() => invoice.parties.push(read_nad(segment)),
            ("CUX", _) => invoice.currency = Some(segment.component(1, 2).to_owned()),
            ("ALC", false) => {
                if let Some(c) = open_charge.take() {
                    close(&mut invoice, &mut line, c);
                }
                open_charge = Some((
                    line.is_some(),
                    Charge {
                        indicator: segment.get(1).to_owned(),
                        code: segment.component(5, 1).to_owned(),
                        description: segment.component(5, 4).to_owned(),
                        amount: Decimal::ZERO,
                    },
                ));
            }
            ("MOA", false)
                if open_charge.is_some() && matches!(segment.component(1, 1), "204" | "23") =>
            {
                if let Some(mut c) = open_charge.take() {
                    c.1.amount = value().unwrap_or_default();
                    close(&mut invoice, &mut line, c);
                }
            }
            ("LIN", false) => {
                if let Some(c) = open_charge.take() {
                    close(&mut invoice, &mut line, c);
                }
                if let Some(l) = line.take() {
                    invoice.lines.push(l);
                }
                let mut l = InvoiceLine {
                    line_id: segment.get(1).to_owned(),
                    ..InvoiceLine::default()
                };
                l.product_ids.extend(lin_product(segment));
                line = Some(l);
            }
            ("PIA", false) if line.is_some() => {
                if let Some(l) = line.as_mut() {
                    l.product_ids.extend(pia_products(segment));
                }
            }
            ("IMD", false) if line.is_some() => {
                if let Some(l) = line.as_mut() {
                    l.description = read_imd(segment);
                }
            }
            ("QTY", false) if segment.component(1, 1) == "47" && line.is_some() => {
                if let Some(l) = line.as_mut() {
                    l.quantity = value();
                    l.uom = segment.component(1, 3).to_owned();
                }
            }
            ("MOA", false) if segment.component(1, 1) == "203" && line.is_some() => {}
            ("PRI", false) if segment.component(1, 1) == "AAA" && line.is_some() => {
                if let Some(l) = line.as_mut() {
                    l.unit_price = value();
                }
            }
            ("UNS", _) => {
                if let Some(c) = open_charge.take() {
                    close(&mut invoice, &mut line, c);
                }
                if let Some(l) = line.take() {
                    invoice.lines.push(l);
                }
                summary = true;
            }
            ("CNT", true) if segment.component(1, 1) == "2" => {
                invoice.lines_stated = segment.component(1, 2).parse().ok();
            }
            ("MOA", true) if segment.component(1, 1) == "77" => invoice.total = value(),
            ("MOA", true) if segment.component(1, 1) == "79" => {}
            ("TAX", true) => invoice.taxes.push(Tax {
                kind: tax_in(segment.get(2)),
                percent: read_num(segment.component(5, 4), d),
                amount: Decimal::ZERO,
            }),
            ("MOA", true) if segment.component(1, 1) == "124" => match invoice.taxes.last_mut() {
                Some(tax) => tax.amount = value().unwrap_or_default(),
                None => invoice.unhandled.push((position, tag.into())),
            },
            _ => invoice.unhandled.push((position, tag.into())),
        }
    }
    if let Some(c) = open_charge.take() {
        close(&mut invoice, &mut line, c);
    }
    if let Some(l) = line.take() {
        invoice.lines.push(l);
    }
    if !seen_bgm {
        verdict.codes.push(MISSING);
    }
    (invoice, verdict)
}

/// What of an invoice an INVOIC carries, as it reads back.
pub fn carried(invoice: &Invoice) -> Invoice {
    let mut out = invoice.clone();
    out.release.clear();
    out.terms.clear();
    out.unhandled.clear();
    out.notes.clear();
    out.lines_stated = Some(invoice.lines.len() as u32);
    if out.kind != CREDIT {
        out.kind = DEBIT.into();
    }
    for reference in &mut out.references {
        reference.description.clear();
    }
    for party in &mut out.parties {
        party.address_lines.truncate(4);
    }
    for amount in out
        .taxes
        .iter_mut()
        .map(|t| &mut t.amount)
        .chain(out.charges.iter_mut().map(|c| &mut c.amount))
    {
        *amount = amount.normalize();
    }
    out
}
