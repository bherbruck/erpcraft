//! The 850 purchase order, read into a typed structure.
//!
//! This reads what the standard says an 850 is, for versions 004010 and
//! 005010, whose 850s differ in nothing read here. It does not know any
//! partner: which qualifier carries the buyer's item number, whether the
//! ship-to is a store or a distribution centre, and which segments must
//! be there are a partner's implementation guide, which is data applied
//! to this structure afterwards.
//!
//! Three things come back, and they are kept apart:
//! - the order, with every value as it was sent;
//! - the syntax errors, in the terms of a 997, which acknowledges syntax
//!   and nothing else;
//! - `unhandled` and `notes` on the order: segments this reader has no
//!   field for, and counts that do not agree. Nothing a partner sent is
//!   dropped without saying so, because a person decides on a proposal
//!   and must see what it leaves out.

use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::segment::Segment;
use crate::x12::TransactionSet;
use crate::x12::ack::{
    ELEMENT_INVALID_CHARACTER, ELEMENT_INVALID_DATE, ELEMENT_MISSING, SEGMENT_MISSING,
    SEGMENT_UNEXPECTED, SET_NOT_SUPPORTED, SegmentError, SetVerdict,
};

/// The longest number read: `numeric(20, 6)` holds anything shorter.
const MAX_NUMBER_LEN: usize = 18;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PurchaseOrder {
    /// BEG01: `00` original, `01` cancellation, `05` replacement,
    /// `06` confirmation.
    pub purpose: String,
    /// BEG02: `SA` stand-alone, `NE` new, `RL` release, `DS` drop ship.
    pub order_type: String,
    /// BEG03: the buyer's order number.
    pub number: String,
    /// BEG04: the release number of a blanket order.
    pub release: String,
    /// BEG05.
    pub date: Option<NaiveDate>,
    /// CUR02, when the order names a currency.
    pub currency: Option<String>,
    pub references: Vec<Reference>,
    pub dates: Vec<DateOf>,
    pub terms: Vec<Terms>,
    /// FOB01: who pays the freight (`CC` collect, `PP` prepaid).
    pub freight_terms: Option<String>,
    pub parties: Vec<Party>,
    pub lines: Vec<Line>,
    /// CTT01: the number of lines the sender counted.
    pub lines_stated: Option<u32>,
    /// CTT02: the sum of the quantities, as the sender added them.
    pub hash_total: Option<Decimal>,
    /// AMT02 of `AMT*TT`: the order's total.
    pub total: Option<Decimal>,
    /// Segments this reader has no field for: position in the set and tag.
    pub unhandled: Vec<(usize, String)>,
    /// What does not agree, in sentences.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reference {
    /// REF01, such as `DP` department, `IA` the vendor number the buyer
    /// assigned, `PD` promotion.
    pub qualifier: String,
    pub value: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DateOf {
    /// DTM01, such as `002` delivery requested, `010` requested ship,
    /// `037` ship not before, `038` ship no later, `001` cancel after.
    pub qualifier: String,
    pub date: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Terms {
    /// ITD01.
    pub type_code: String,
    /// ITD02: what the days count from (`3` invoice date).
    pub basis: String,
    pub discount_percent: Option<Decimal>,
    pub discount_days: Option<u32>,
    pub net_days: Option<u32>,
    /// ITD12.
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Party {
    /// N101: `BY` buyer, `ST` ship to, `BT` bill to, `VN` vendor,
    /// `SF` ship from, `Z7` mark for.
    pub role: String,
    pub name: String,
    /// N103: `92` assigned by the buyer, `91` by the seller, `UL` a GLN,
    /// `1` a D-U-N-S number.
    pub id_qualifier: String,
    pub id: String,
    /// N2 and N3, in order.
    pub address_lines: Vec<String>,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Line {
    /// PO101: the buyer's line number, to be echoed in every answer.
    pub line_id: String,
    pub quantity: Option<Decimal>,
    /// PO103: the unit as the partner codes it (`EA`, `CA`).
    pub uom: String,
    pub unit_price: Option<Decimal>,
    /// PO105: what the price is per (`PE` each, `PD` dozen).
    pub price_basis: String,
    /// PO106 onwards, in pairs.
    pub product_ids: Vec<ProductId>,
    /// PID05.
    pub descriptions: Vec<String>,
    pub pack: Option<Pack>,
    pub dates: Vec<DateOf>,
    pub references: Vec<Reference>,
    /// SDQ: how the line's quantity is split over stores or warehouses.
    pub allocations: Vec<Allocation>,
    /// A party of this line alone, such as a ship-to per line.
    pub parties: Vec<Party>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductId {
    /// Such as `UP` a UPC of 12 digits, `EN` an EAN of 13, `UK` a GTIN of
    /// 14, `VN` or `VP` the seller's number, `IN` or `BP` the buyer's.
    pub qualifier: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pack {
    /// PO401: consumer units in a case.
    pub per_case: Option<Decimal>,
    /// PO402 and PO403: the size of one unit.
    pub size: Option<Decimal>,
    pub size_uom: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Allocation {
    /// SDQ02: what kind of number `location` is.
    pub id_qualifier: String,
    pub location: String,
    pub quantity: Decimal,
    pub uom: String,
}

/// Reads a transaction set as an 850. The order holds whatever could be
/// read; the verdict holds the syntax errors, for the 997. An order with
/// errors in its verdict must not be drafted.
pub fn read(set: &TransactionSet) -> (PurchaseOrder, SetVerdict) {
    let mut reader = Reader::default();
    if set.id != "850" {
        reader.verdict.codes.push(SET_NOT_SUPPORTED);
        return (reader.order, reader.verdict);
    }
    for (index, segment) in set.segments.iter().enumerate() {
        reader.segment(index + 1, segment);
    }
    reader.finish(set.segments.len());
    (reader.order, reader.verdict)
}

#[derive(Default)]
struct Reader {
    order: PurchaseOrder,
    verdict: SetVerdict,
    seen_beg: bool,
    summary: bool,
    /// The N1 loop the next N2, N3 or N4 belongs to is the last party of
    /// the order, or of the last line once a line began.
    in_party: bool,
}

impl Reader {
    fn element_error(&mut self, position: usize, segment: &Segment, n: usize, code: u8) {
        self.verdict
            .segments
            .push(SegmentError::element(segment, position, n, code));
    }

    fn required(&mut self, position: usize, segment: &Segment, n: usize) -> String {
        let value = segment.get(n);
        if value.is_empty() {
            self.element_error(position, segment, n, ELEMENT_MISSING);
        }
        value.to_owned()
    }

    /// Whether element `n` is one plain value, as every number and date
    /// is: neither a composite nor repeated.
    fn plain(segment: &Segment, n: usize) -> bool {
        segment
            .element(n)
            .is_none_or(|e| e.repetitions() == 1 && e.components(0).len() == 1)
    }

    /// A number in X12's form: digits, an optional point, an optional
    /// leading minus. No exponent and no thousands separator.
    fn number(&mut self, position: usize, segment: &Segment, n: usize) -> Option<Decimal> {
        let text = segment.get(n);
        if text.is_empty() {
            return None;
        }
        let digits = text.strip_prefix('-').unwrap_or(text);
        let well_formed = Self::plain(segment, n)
            && text.len() <= MAX_NUMBER_LEN
            && digits.bytes().any(|b| b.is_ascii_digit())
            && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && digits.bytes().filter(|b| *b == b'.').count() <= 1;
        let parsed = well_formed.then(|| Decimal::from_str(text).ok()).flatten();
        if parsed.is_none() {
            self.element_error(position, segment, n, ELEMENT_INVALID_CHARACTER);
        }
        parsed
    }

    fn whole(&mut self, position: usize, segment: &Segment, n: usize) -> Option<u32> {
        let text = segment.get(n);
        if text.is_empty() {
            return None;
        }
        let parsed = (Self::plain(segment, n)
            && text.len() <= 9
            && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten();
        if parsed.is_none() {
            self.element_error(position, segment, n, ELEMENT_INVALID_CHARACTER);
        }
        parsed
    }

    /// A date as `CCYYMMDD`.
    fn date(&mut self, position: usize, segment: &Segment, n: usize) -> Option<NaiveDate> {
        let text = segment.get(n);
        if text.is_empty() {
            return None;
        }
        let parsed = (Self::plain(segment, n)
            && text.len() == 8
            && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| NaiveDate::parse_from_str(text, "%Y%m%d").ok())
        .flatten();
        if parsed.is_none() {
            self.element_error(position, segment, n, ELEMENT_INVALID_DATE);
        }
        parsed
    }

    fn party(&mut self) -> Option<&mut Party> {
        if !self.in_party {
            return None;
        }
        match self.order.lines.last_mut() {
            Some(line) => line.parties.last_mut(),
            None => self.order.parties.last_mut(),
        }
    }

    fn segment(&mut self, position: usize, segment: &Segment) {
        let tag = segment.tag.as_str();
        if !matches!(tag, "N1" | "N2" | "N3" | "N4") {
            self.in_party = false;
        }
        if position == 2 && tag != "BEG" {
            self.verdict
                .segments
                .push(SegmentError::segment("BEG", position, SEGMENT_MISSING));
        }
        match tag {
            "ST" | "SE" => {}
            "BEG" if !self.seen_beg && position == 2 => {
                self.seen_beg = true;
                self.order.purpose = self.required(position, segment, 1);
                self.order.order_type = self.required(position, segment, 2);
                self.order.number = self.required(position, segment, 3);
                self.order.release = segment.get(4).to_owned();
                self.required(position, segment, 5);
                self.order.date = self.date(position, segment, 5);
            }
            "CUR" if self.order.lines.is_empty() => {
                self.order.currency = Some(self.required(position, segment, 2));
            }
            "REF" => {
                let reference = Reference {
                    qualifier: self.required(position, segment, 1),
                    value: segment.get(2).to_owned(),
                    description: segment.get(3).to_owned(),
                };
                match self.order.lines.last_mut() {
                    Some(line) => line.references.push(reference),
                    None => self.order.references.push(reference),
                }
            }
            "DTM" => {
                let qualifier = self.required(position, segment, 1);
                if let Some(date) = self.date(position, segment, 2) {
                    let date = DateOf { qualifier, date };
                    match self.order.lines.last_mut() {
                        Some(line) => line.dates.push(date),
                        None => self.order.dates.push(date),
                    }
                }
            }
            "ITD" if self.order.lines.is_empty() => {
                let terms = Terms {
                    type_code: segment.get(1).to_owned(),
                    basis: segment.get(2).to_owned(),
                    discount_percent: self.number(position, segment, 3),
                    discount_days: self.whole(position, segment, 5),
                    net_days: self.whole(position, segment, 7),
                    description: segment.get(12).to_owned(),
                };
                self.order.terms.push(terms);
            }
            "FOB" if self.order.lines.is_empty() => {
                self.order.freight_terms = Some(self.required(position, segment, 1));
            }
            "N1" if !self.summary => {
                let party = Party {
                    role: self.required(position, segment, 1),
                    name: segment.get(2).to_owned(),
                    id_qualifier: segment.get(3).to_owned(),
                    id: segment.get(4).to_owned(),
                    ..Party::default()
                };
                match self.order.lines.last_mut() {
                    Some(line) => line.parties.push(party),
                    None => self.order.parties.push(party),
                }
                self.in_party = true;
            }
            "N2" | "N3" | "N4" => {
                let Some(party) = self.party() else {
                    self.verdict.segments.push(SegmentError::segment(
                        tag,
                        position,
                        SEGMENT_UNEXPECTED,
                    ));
                    return;
                };
                if tag == "N4" {
                    party.city = segment.get(1).to_owned();
                    party.region = segment.get(2).to_owned();
                    party.postal_code = segment.get(3).to_owned();
                    party.country = segment.get(4).to_owned();
                } else {
                    for n in 1..=2 {
                        if !segment.get(n).is_empty() {
                            party.address_lines.push(segment.get(n).to_owned());
                        }
                    }
                }
            }
            "PO1" if !self.summary => {
                let mut line = Line {
                    line_id: segment.get(1).to_owned(),
                    quantity: self.number(position, segment, 2),
                    uom: segment.get(3).to_owned(),
                    unit_price: self.number(position, segment, 4),
                    price_basis: segment.get(5).to_owned(),
                    ..Line::default()
                };
                for n in (6..=24).step_by(2) {
                    let (qualifier, value) = (segment.get(n), segment.get(n + 1));
                    if !qualifier.is_empty() && !value.is_empty() {
                        line.product_ids.push(ProductId {
                            qualifier: qualifier.to_owned(),
                            value: value.to_owned(),
                        });
                    } else if qualifier.is_empty() != value.is_empty() {
                        // A qualifier without its number or the reverse:
                        // X12's "conditional required" pair.
                        let missing = if qualifier.is_empty() { n } else { n + 1 };
                        self.element_error(position, segment, missing, 2);
                    }
                }
                self.order.lines.push(line);
            }
            "PID" if !self.order.lines.is_empty() && !self.summary => {
                let text = segment.get(5).to_owned();
                if let (Some(line), false) = (self.order.lines.last_mut(), text.is_empty()) {
                    line.descriptions.push(text);
                }
            }
            "PO4" if !self.order.lines.is_empty() && !self.summary => {
                let pack = Pack {
                    per_case: self.number(position, segment, 1),
                    size: self.number(position, segment, 2),
                    size_uom: segment.get(3).to_owned(),
                };
                if let Some(line) = self.order.lines.last_mut() {
                    line.pack = Some(pack);
                }
            }
            "SDQ" if !self.order.lines.is_empty() && !self.summary => {
                let uom = self.required(position, segment, 1);
                let id_qualifier = segment.get(2).to_owned();
                for n in (3..=21).step_by(2) {
                    let location = segment.get(n).to_owned();
                    if location.is_empty() {
                        continue;
                    }
                    let Some(quantity) = self.number(position, segment, n + 1) else {
                        continue;
                    };
                    if let Some(line) = self.order.lines.last_mut() {
                        line.allocations.push(Allocation {
                            id_qualifier: id_qualifier.clone(),
                            location,
                            quantity,
                            uom: uom.clone(),
                        });
                    }
                }
            }
            "CTT" => {
                self.summary = true;
                self.order.lines_stated = self.whole(position, segment, 1);
                self.order.hash_total = self.number(position, segment, 2);
            }
            "AMT" if self.summary && segment.get(1) == "TT" => {
                self.order.total = self.number(position, segment, 2);
            }
            _ => self.order.unhandled.push((position, tag.to_owned())),
        }
    }

    fn finish(&mut self, segments: usize) {
        if self.order.lines.is_empty() {
            self.verdict
                .segments
                .push(SegmentError::segment("PO1", segments, SEGMENT_MISSING));
        }
        let lines = self.order.lines.len();
        if let Some(stated) = self.order.lines_stated
            && stated as usize != lines
        {
            self.order.notes.push(format!(
                "The order says it has {stated} lines (CTT01) and has {lines}."
            ));
        }
        // Added with a check: a sum of numbers a partner chose must not be
        // able to overflow and stop the server.
        let sum = |numbers: &mut dyn Iterator<Item = Decimal>| -> Option<Decimal> {
            let mut total = Decimal::ZERO;
            for n in numbers {
                total = total.checked_add(n)?;
            }
            Some(total)
        };
        let quantities = sum(&mut self.order.lines.iter().filter_map(|l| l.quantity));
        if let (Some(stated), Some(quantities)) = (self.order.hash_total, quantities)
            && stated != quantities
        {
            self.order.notes.push(format!(
                "The order's hash total (CTT02) is {stated} and its quantities add up to \
                 {quantities}."
            ));
        }
        for line in &self.order.lines {
            let allocated = sum(&mut line.allocations.iter().map(|a| a.quantity));
            if let (false, Some(quantity), Some(allocated)) =
                (line.allocations.is_empty(), line.quantity, allocated)
                && allocated != quantity
            {
                self.order.notes.push(format!(
                    "Line {} orders {quantity} and allocates {allocated} to locations (SDQ).",
                    line.line_id
                ));
            }
        }
    }
}
