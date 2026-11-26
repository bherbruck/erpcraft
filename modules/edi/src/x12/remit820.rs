//! The 820 remittance advice: what a payment pays. Read from a customer
//! into a proposal of the open items it settles; written to a vendor from
//! a posted payment run; each checked by reading it back.
//!
//! In this module's words: `BPR` the payment (how it is handled, its
//! amount, credit or debit, how it is paid, and in BPR16 the day it is
//! paid); `TRN` its trace number (the check or transfer's); `N1` the payer
//! (`PR`) and the payee (`PE`); then an `RMR` per document paid (its
//! qualifier, `IV` for an invoice, and number; what is paid, what the
//! document came to, the discount taken), each with the `ADX` adjustments
//! taken off it (an amount, below zero for a deduction, and a reason
//! code). An `ADX` before any `RMR` is an adjustment of the whole payment.
//! Amounts are written with a point, as numbers.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::segment::Segment;
use crate::x12::TransactionSet;
use crate::x12::ack::{SEGMENT_MISSING, SET_NOT_SUPPORTED, SegmentError, SetVerdict};
use crate::x12::fields::{Fields, date, opt_number, party_segments, read_party, segment};
use crate::x12::po850::Party;

/// RMR01: the document paid is an invoice.
pub const INVOICE: &str = "IV";

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Remittance {
    /// BPR01: `C` the payment and its advice together, `I` the advice only.
    pub handling: String,
    /// BPR02.
    pub amount: Option<Decimal>,
    /// BPR03: `C` credit (to the payee), `D` debit.
    pub credit_debit: String,
    /// BPR04: how it is paid, such as `ACH` or `CHK`.
    pub method: String,
    /// BPR16: the day the money is paid.
    pub date: Option<NaiveDate>,
    /// TRN02: the trace number.
    pub trace: String,
    pub parties: Vec<Party>,
    /// Adjustments of the whole payment.
    pub adjustments: Vec<Adjustment>,
    pub items: Vec<RemitItem>,
    pub unhandled: Vec<(usize, String)>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RemitItem {
    /// RMR01, such as `IV`.
    pub qualifier: String,
    /// RMR02: the document's number, as the payee issued it.
    pub reference: String,
    /// RMR04: what is paid of it.
    pub paid: Option<Decimal>,
    /// RMR05: what it came to.
    pub invoiced: Option<Decimal>,
    /// RMR06: the discount taken.
    pub discount: Option<Decimal>,
    pub adjustments: Vec<Adjustment>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Adjustment {
    /// ADX01: below zero for what is deducted.
    pub amount: Option<Decimal>,
    /// ADX02: the payer's reason code.
    pub reason: String,
    /// ADX03 and ADX04: a reference, such as the payer's claim number.
    pub reference_qualifier: String,
    pub reference: String,
}

fn adjustment(a: &Adjustment) -> Segment {
    segment(
        "ADX",
        vec![
            opt_number(a.amount),
            a.reason.clone(),
            a.reference_qualifier.clone(),
            a.reference.clone(),
        ],
    )
}

impl Remittance {
    /// The segments of the set from BPR to the last RMR or ADX, without ST
    /// and SE.
    pub fn body(&self) -> Vec<Segment> {
        let mut bpr = vec![
            self.handling.clone(),
            opt_number(self.amount),
            self.credit_debit.clone(),
            self.method.clone(),
        ];
        bpr.resize(15, String::new());
        bpr.push(date(self.date));
        let mut out = vec![
            segment("BPR", bpr),
            segment("TRN", vec!["1".into(), self.trace.clone()]),
        ];
        for party in &self.parties {
            out.extend(party_segments(party));
        }
        out.extend(self.adjustments.iter().map(adjustment));
        for item in &self.items {
            out.push(segment(
                "RMR",
                vec![
                    item.qualifier.clone(),
                    item.reference.clone(),
                    String::new(),
                    opt_number(item.paid),
                    opt_number(item.invoiced),
                    opt_number(item.discount),
                ],
            ));
            out.extend(item.adjustments.iter().map(adjustment));
        }
        out
    }

    /// What the items say is paid.
    pub fn items_paid(&self) -> Option<Decimal> {
        self.items
            .iter()
            .try_fold(Decimal::ZERO, |t, i| t.checked_add(i.paid?))
    }
}

/// Reads a transaction set as an 820.
pub fn read(set: &TransactionSet) -> (Remittance, SetVerdict) {
    let mut fields = Fields::default();
    let mut remit = Remittance::default();
    if set.id != "820" {
        fields.verdict.codes.push(SET_NOT_SUPPORTED);
        return (remit, fields.verdict);
    }
    let mut seen_bpr = false;
    let mut in_party = false;
    for (index, segment) in set.segments.iter().enumerate() {
        let position = index + 1;
        let tag = segment.tag.as_str();
        if !matches!(tag, "N1" | "N2" | "N3" | "N4") {
            in_party = false;
        }
        match tag {
            "ST" | "SE" => {}
            "BPR" if !seen_bpr && position == 2 => {
                seen_bpr = true;
                remit.handling = fields.required(position, segment, 1);
                remit.amount = fields.number(position, segment, 2);
                remit.credit_debit = segment.get(3).to_owned();
                remit.method = segment.get(4).to_owned();
                remit.date = fields.date(position, segment, 16);
            }
            "TRN" if remit.trace.is_empty() => remit.trace = fields.required(position, segment, 2),
            "N1" | "N2" | "N3" | "N4" if remit.items.is_empty() => {
                if read_party(&mut remit.parties, in_party, segment) {
                    in_party = true;
                } else {
                    remit.unhandled.push((position, tag.to_owned()));
                }
            }
            "RMR" => remit.items.push(RemitItem {
                qualifier: fields.required(position, segment, 1),
                reference: fields.required(position, segment, 2),
                paid: fields.number(position, segment, 4),
                invoiced: fields.number(position, segment, 5),
                discount: fields.number(position, segment, 6),
                adjustments: vec![],
            }),
            "ADX" => {
                let a = Adjustment {
                    amount: fields.number(position, segment, 1),
                    reason: segment.get(2).to_owned(),
                    reference_qualifier: segment.get(3).to_owned(),
                    reference: segment.get(4).to_owned(),
                };
                match remit.items.last_mut() {
                    Some(item) => item.adjustments.push(a),
                    None => remit.adjustments.push(a),
                }
            }
            _ => remit.unhandled.push((position, tag.to_owned())),
        }
    }
    if !seen_bpr {
        fields
            .verdict
            .segments
            .push(SegmentError::segment("BPR", 2, SEGMENT_MISSING));
    }
    if let (Some(stated), Some(paid)) = (remit.amount, remit.items_paid())
        && stated != paid
    {
        remit.notes.push(format!(
            "The payment says it is {stated} (BPR02) and its documents come to {paid}."
        ));
    }
    (remit, fields.verdict)
}
