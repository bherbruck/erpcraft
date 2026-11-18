//! Reading and writing the values of X12 elements, shared by the typed
//! messages: numbers, whole numbers, dates, amounts in cents. A reader
//! keeps what it finds wrong as the errors a 997 would name.

use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::segment::{Element, Segment};
use crate::x12::ack::{
    ELEMENT_INVALID_CHARACTER, ELEMENT_INVALID_DATE, ELEMENT_MISSING, SegmentError, SetVerdict,
};

/// The longest number read: `numeric(20, 6)` holds anything shorter.
pub const MAX_NUMBER_LEN: usize = 18;

/// What a reader found wrong, as it reads.
#[derive(Default)]
pub struct Fields {
    pub verdict: SetVerdict,
}

impl Fields {
    pub fn error(&mut self, position: usize, segment: &Segment, n: usize, code: u8) {
        self.verdict
            .segments
            .push(SegmentError::element(segment, position, n, code));
    }

    pub fn required(&mut self, position: usize, segment: &Segment, n: usize) -> String {
        let value = segment.get(n);
        if value.is_empty() {
            self.error(position, segment, n, ELEMENT_MISSING);
        }
        value.to_owned()
    }

    fn plain(segment: &Segment, n: usize) -> bool {
        segment
            .element(n)
            .is_none_or(|e| e.repetitions() == 1 && e.components(0).len() == 1)
    }

    /// A number: digits, an optional point, an optional leading minus.
    pub fn number(&mut self, position: usize, segment: &Segment, n: usize) -> Option<Decimal> {
        let text = segment.get(n);
        if text.is_empty() {
            return None;
        }
        let parsed = Self::plain(segment, n)
            .then(|| parse_number(text))
            .flatten();
        if parsed.is_none() {
            self.error(position, segment, n, ELEMENT_INVALID_CHARACTER);
        }
        parsed
    }

    /// An amount written in cents without a point (`TDS01`, `SAC05`).
    pub fn cents(&mut self, position: usize, segment: &Segment, n: usize) -> Option<Decimal> {
        let text = segment.get(n);
        if text.is_empty() {
            return None;
        }
        let digits = text.strip_prefix('-').unwrap_or(text);
        let parsed = (Self::plain(segment, n)
            && text.len() <= MAX_NUMBER_LEN
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse::<i64>().ok().map(|c| Decimal::new(c, 2)))
        .flatten();
        if parsed.is_none() {
            self.error(position, segment, n, ELEMENT_INVALID_CHARACTER);
        }
        parsed
    }

    pub fn whole(&mut self, position: usize, segment: &Segment, n: usize) -> Option<u32> {
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
            self.error(position, segment, n, ELEMENT_INVALID_CHARACTER);
        }
        parsed
    }

    /// A date as `CCYYMMDD`.
    pub fn date(&mut self, position: usize, segment: &Segment, n: usize) -> Option<NaiveDate> {
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
            self.error(position, segment, n, ELEMENT_INVALID_DATE);
        }
        parsed
    }
}

pub fn parse_number(text: &str) -> Option<Decimal> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let well_formed = text.len() <= MAX_NUMBER_LEN
        && digits.bytes().any(|b| b.is_ascii_digit())
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && digits.bytes().filter(|b| *b == b'.').count() <= 1;
    well_formed.then(|| Decimal::from_str(text).ok()).flatten()
}

/// A number as X12 writes it: no exponent, no trailing zeros, no point
/// for a whole number.
pub fn number(value: Decimal) -> String {
    value.normalize().to_string()
}

pub fn opt_number(value: Option<Decimal>) -> String {
    value.map(number).unwrap_or_default()
}

/// An amount in cents without a point. `None` when it has more than two
/// places: an amount is never rounded to be written.
pub fn cents(value: Decimal) -> Option<String> {
    let scaled = value.checked_mul(Decimal::ONE_HUNDRED)?;
    (scaled.fract().is_zero()).then(|| scaled.trunc().normalize().to_string())
}

pub fn date(value: Option<NaiveDate>) -> String {
    value
        .map(|d| d.format("%Y%m%d").to_string())
        .unwrap_or_default()
}

/// A segment from its elements, for the writers.
pub fn segment(tag: &str, elements: Vec<String>) -> Segment {
    Segment {
        tag: tag.to_owned(),
        elements: elements.into_iter().map(Element::simple).collect(),
        offset: 0,
    }
}

use crate::x12::po850::{DateOf, Party, ProductId, Reference};

pub fn reference_segment(r: &Reference) -> Segment {
    segment(
        "REF",
        vec![r.qualifier.clone(), r.value.clone(), r.description.clone()],
    )
}

pub fn date_segment(d: &DateOf) -> Segment {
    segment("DTM", vec![d.qualifier.clone(), date(Some(d.date))])
}

/// The N1 loop of a party: N1, N3 with two address lines each, N4.
pub fn party_segments(p: &Party) -> Vec<Segment> {
    let mut out = vec![segment(
        "N1",
        vec![
            p.role.clone(),
            p.name.clone(),
            p.id_qualifier.clone(),
            p.id.clone(),
        ],
    )];
    for pair in p.address_lines.chunks(2) {
        out.push(segment("N3", pair.to_vec()));
    }
    if !(p.city.is_empty()
        && p.region.is_empty()
        && p.postal_code.is_empty()
        && p.country.is_empty())
    {
        out.push(segment(
            "N4",
            vec![
                p.city.clone(),
                p.region.clone(),
                p.postal_code.clone(),
                p.country.clone(),
            ],
        ));
    }
    out
}

/// Product ids as qualifier and value pairs, from element `from` on.
pub fn product_elements(ids: &[ProductId]) -> Vec<String> {
    ids.iter()
        .flat_map(|p| [p.qualifier.clone(), p.value.clone()])
        .collect()
}

/// Reads qualifier and value pairs of product ids from element `from` to
/// `to`. A qualifier without its number or the reverse is an error.
pub fn read_product_ids(
    fields: &mut Fields,
    position: usize,
    segment: &Segment,
    from: usize,
    to: usize,
) -> Vec<ProductId> {
    let mut ids = Vec::new();
    for n in (from..=to).step_by(2) {
        let (qualifier, value) = (segment.get(n), segment.get(n + 1));
        if !qualifier.is_empty() && !value.is_empty() {
            ids.push(ProductId {
                qualifier: qualifier.to_owned(),
                value: value.to_owned(),
            });
        } else if qualifier.is_empty() != value.is_empty() {
            let missing = if qualifier.is_empty() { n } else { n + 1 };
            fields.error(position, segment, missing, 2);
        }
    }
    ids
}

/// Applies an N1, N2, N3 or N4 segment to a list of parties. Returns
/// false when an N2 to N4 has no N1 before it.
pub fn read_party(parties: &mut Vec<Party>, in_party: bool, segment: &Segment) -> bool {
    match segment.tag.as_str() {
        "N1" => {
            parties.push(Party {
                role: segment.get(1).to_owned(),
                name: segment.get(2).to_owned(),
                id_qualifier: segment.get(3).to_owned(),
                id: segment.get(4).to_owned(),
                ..Party::default()
            });
            true
        }
        tag => {
            let Some(party) = parties.last_mut().filter(|_| in_party) else {
                return false;
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
            true
        }
    }
}
