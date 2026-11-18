//! The 810 invoice: what the seller charges for an order, line by line,
//! with tax, charges and allowances, and the total in cents. Written from
//! a posted sales invoice or credit note; read back to check it.
//!
//! Amounts in TDS01 and SAC05 are written in cents without a point, as
//! the set has them. BIG07 says whether it is an invoice (`DI`) or a
//! credit (`CR`); the amounts of a credit are written as positive figures.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::segment::Segment;
use crate::x12::TransactionSet;
use crate::x12::ack::{SEGMENT_MISSING, SET_NOT_SUPPORTED, SegmentError, SetVerdict};
use crate::x12::fields::{
    self, Fields, date, date_segment, opt_number, party_segments, product_elements, read_party,
    read_product_ids, reference_segment, segment,
};
use crate::x12::po850::{DateOf, Party, ProductId, Reference, Terms};

/// BIG07.
pub const DEBIT: &str = "DI";
pub const CREDIT: &str = "CR";

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Invoice {
    /// BIG01 and BIG02.
    pub date: Option<NaiveDate>,
    pub number: String,
    /// BIG03 and BIG04: the buyer's order, as received.
    pub order_date: Option<NaiveDate>,
    pub order_number: String,
    /// BIG05.
    pub release: String,
    /// BIG07: `DI` or `CR`.
    pub kind: String,
    pub currency: Option<String>,
    pub references: Vec<Reference>,
    pub parties: Vec<Party>,
    pub terms: Vec<Terms>,
    pub dates: Vec<DateOf>,
    pub lines: Vec<InvoiceLine>,
    /// TDS01: what the invoice comes to, tax and charges included.
    pub total: Option<Decimal>,
    pub taxes: Vec<Tax>,
    /// Charges and allowances of the whole invoice.
    pub charges: Vec<Charge>,
    /// CTT01.
    pub lines_stated: Option<u32>,
    pub unhandled: Vec<(usize, String)>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InvoiceLine {
    /// IT101: the order's line id, echoed.
    pub line_id: String,
    pub quantity: Option<Decimal>,
    /// IT103: the partner's unit.
    pub uom: String,
    pub unit_price: Option<Decimal>,
    /// IT106 on: the order's product ids, echoed.
    pub product_ids: Vec<ProductId>,
    pub description: String,
    /// Allowances and charges of this line (a discount).
    pub charges: Vec<Charge>,
}

/// TXI: one tax.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Tax {
    /// TXI01: the kind of tax.
    pub kind: String,
    /// TXI02.
    pub amount: Decimal,
    /// TXI03.
    pub percent: Option<Decimal>,
}

/// SAC: an allowance (`A`) or a charge (`C`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Charge {
    pub indicator: String,
    /// SAC02: what it is for.
    pub code: String,
    /// SAC05, in cents when written.
    pub amount: Decimal,
    /// SAC15.
    pub description: String,
}

fn charge_segment(c: &Charge) -> Option<Segment> {
    let mut elements = vec![
        c.indicator.clone(),
        c.code.clone(),
        String::new(),
        String::new(),
        fields::cents(c.amount)?,
    ];
    if !c.description.is_empty() {
        elements.extend(std::iter::repeat_n(String::new(), 9));
        elements.push(c.description.clone());
    }
    Some(segment("SAC", elements))
}

impl Invoice {
    /// The segments from BIG to CTT, without ST and SE. `None` when an
    /// amount has more than two places, which a total in cents cannot
    /// carry: an amount is never rounded to be written.
    pub fn body(&self) -> Option<Vec<Segment>> {
        let mut out = vec![segment(
            "BIG",
            vec![
                date(self.date),
                self.number.clone(),
                date(self.order_date),
                self.order_number.clone(),
                self.release.clone(),
                String::new(),
                self.kind.clone(),
            ],
        )];
        if let Some(currency) = &self.currency {
            out.push(segment("CUR", vec!["SE".into(), currency.clone()]));
        }
        out.extend(self.references.iter().map(reference_segment));
        for party in &self.parties {
            out.extend(party_segments(party));
        }
        for t in &self.terms {
            out.push(segment(
                "ITD",
                vec![
                    t.type_code.clone(),
                    t.basis.clone(),
                    opt_number(t.discount_percent),
                    String::new(),
                    t.discount_days.map(|d| d.to_string()).unwrap_or_default(),
                    String::new(),
                    t.net_days.map(|d| d.to_string()).unwrap_or_default(),
                ],
            ));
        }
        out.extend(self.dates.iter().map(date_segment));
        for line in &self.lines {
            let mut it1 = vec![
                line.line_id.clone(),
                opt_number(line.quantity),
                line.uom.clone(),
                opt_number(line.unit_price),
                String::new(),
            ];
            it1.extend(product_elements(&line.product_ids));
            out.push(segment("IT1", it1));
            if !line.description.is_empty() {
                out.push(segment(
                    "PID",
                    vec![
                        "F".into(),
                        String::new(),
                        String::new(),
                        String::new(),
                        line.description.clone(),
                    ],
                ));
            }
            for c in &line.charges {
                out.push(charge_segment(c)?);
            }
        }
        out.push(segment(
            "TDS",
            vec![fields::cents(self.total.unwrap_or_default())?],
        ));
        for t in &self.taxes {
            out.push(segment(
                "TXI",
                vec![
                    t.kind.clone(),
                    fields::number(t.amount),
                    opt_number(t.percent),
                ],
            ));
        }
        for c in &self.charges {
            out.push(charge_segment(c)?);
        }
        out.push(segment("CTT", vec![self.lines.len().to_string()]));
        Some(out)
    }
}

/// Reads a transaction set as an 810.
pub fn read(set: &TransactionSet) -> (Invoice, SetVerdict) {
    let mut fields = Fields::default();
    let mut invoice = Invoice::default();
    if set.id != "810" {
        fields.verdict.codes.push(SET_NOT_SUPPORTED);
        return (invoice, fields.verdict);
    }
    let mut seen_big = false;
    let mut in_party = false;
    let mut summary = false;
    for (index, segment) in set.segments.iter().enumerate() {
        let position = index + 1;
        let tag = segment.tag.as_str();
        if !matches!(tag, "N1" | "N2" | "N3" | "N4") {
            in_party = false;
        }
        let in_lines = !invoice.lines.is_empty();
        match tag {
            "ST" | "SE" => {}
            "BIG" if !seen_big && position == 2 => {
                seen_big = true;
                invoice.date = fields.date(position, segment, 1);
                invoice.number = fields.required(position, segment, 2);
                invoice.order_date = fields.date(position, segment, 3);
                invoice.order_number = segment.get(4).to_owned();
                invoice.release = segment.get(5).to_owned();
                invoice.kind = segment.get(7).to_owned();
            }
            "CUR" if !in_lines => invoice.currency = Some(fields.required(position, segment, 2)),
            "REF" if !in_lines => invoice.references.push(Reference {
                qualifier: fields.required(position, segment, 1),
                value: segment.get(2).to_owned(),
                description: segment.get(3).to_owned(),
            }),
            "N1" | "N2" | "N3" | "N4" if !in_lines => {
                if read_party(&mut invoice.parties, in_party, segment) {
                    in_party = true;
                } else {
                    invoice.unhandled.push((position, tag.to_owned()));
                }
            }
            "ITD" if !in_lines => invoice.terms.push(Terms {
                type_code: segment.get(1).to_owned(),
                basis: segment.get(2).to_owned(),
                discount_percent: fields.number(position, segment, 3),
                discount_days: fields.whole(position, segment, 5),
                net_days: fields.whole(position, segment, 7),
                description: segment.get(12).to_owned(),
            }),
            "DTM" if !in_lines => {
                let qualifier = fields.required(position, segment, 1);
                if let Some(date) = fields.date(position, segment, 2) {
                    invoice.dates.push(DateOf { qualifier, date });
                }
            }
            "IT1" if !summary => invoice.lines.push(InvoiceLine {
                line_id: segment.get(1).to_owned(),
                quantity: fields.number(position, segment, 2),
                uom: segment.get(3).to_owned(),
                unit_price: fields.number(position, segment, 4),
                product_ids: read_product_ids(&mut fields, position, segment, 6, 24),
                description: String::new(),
                charges: Vec::new(),
            }),
            "PID" if in_lines && !summary => {
                if let Some(line) = invoice.lines.last_mut() {
                    line.description = segment.get(5).to_owned();
                }
            }
            "TDS" => {
                summary = true;
                invoice.total = fields.cents(position, segment, 1);
            }
            "TXI" if summary => {
                let amount = fields.number(position, segment, 2);
                invoice.taxes.push(Tax {
                    kind: fields.required(position, segment, 1),
                    amount: amount.unwrap_or_default(),
                    percent: fields.number(position, segment, 3),
                });
            }
            "SAC" => {
                let charge = Charge {
                    indicator: fields.required(position, segment, 1),
                    code: segment.get(2).to_owned(),
                    amount: fields.cents(position, segment, 5).unwrap_or_default(),
                    description: segment.get(15).to_owned(),
                };
                match (summary, invoice.lines.last_mut()) {
                    (false, Some(line)) => line.charges.push(charge),
                    _ => invoice.charges.push(charge),
                }
            }
            "CTT" => invoice.lines_stated = fields.whole(position, segment, 1),
            _ => invoice.unhandled.push((position, tag.to_owned())),
        }
    }
    if !seen_big {
        fields
            .verdict
            .segments
            .push(SegmentError::segment("BIG", 2, SEGMENT_MISSING));
    }
    if invoice.total.is_none() {
        fields.verdict.segments.push(SegmentError::segment(
            "TDS",
            set.segments.len(),
            SEGMENT_MISSING,
        ));
    }
    if let Some(stated) = invoice.lines_stated
        && stated as usize != invoice.lines.len()
    {
        invoice.notes.push(format!(
            "The invoice says it has {stated} lines (CTT01) and has {}.",
            invoice.lines.len()
        ));
    }
    (invoice, fields.verdict)
}

/// What the lines, charges, allowances and taxes of an invoice add up
/// to, as its total should: each line's quantity times price, rounded to
/// the cent. `None` on an overflow or a missing figure.
pub fn computed_total(invoice: &Invoice) -> Option<Decimal> {
    let mut total = Decimal::ZERO;
    let signed = |c: &Charge| -> Decimal {
        if c.indicator == "A" {
            -c.amount
        } else {
            c.amount
        }
    };
    for line in &invoice.lines {
        let amount = line
            .quantity?
            .checked_mul(line.unit_price?)?
            .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::MidpointAwayFromZero);
        total = total.checked_add(amount)?;
        for c in &line.charges {
            total = total.checked_add(signed(c))?;
        }
    }
    for c in &invoice.charges {
        total = total.checked_add(signed(c))?;
    }
    for t in &invoice.taxes {
        total = total.checked_add(t.amount)?;
    }
    Some(total)
}
