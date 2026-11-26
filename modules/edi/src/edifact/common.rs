//! What the EDIFACT messages share: building segments of composites,
//! numbers in the interchange's decimal mark, dates, and the few codes the
//! messages act on, with the code the typed messages carry for each.
//!
//! The typed messages (`PurchaseOrder`, `OrderAck`, `ShipNotice`,
//! `Invoice`) were made for X12 first, so their codes are X12's: a party's
//! role is `BY` or `ST`, a date's qualifier `002`. An EDIFACT reader turns
//! the codes it acts on into those, and a writer turns them back; a code it
//! does not know is carried as it was sent. The tables below are this
//! module's own mapping, written in its own words.

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::edifact::number;
use crate::segment::{Delimiters, Element, Segment};
use crate::x12::po850::{DateOf, Party, ProductId, Reference};

/// A segment of elements, each a list of components.
pub fn seg(tag: &str, elements: Vec<Vec<String>>) -> Segment {
    Segment {
        tag: tag.to_owned(),
        elements: elements.iter().map(|c| Element::composite(c)).collect(),
        offset: 0,
    }
}

/// A number written with the interchange's decimal mark, without
/// trailing zeros.
pub fn num(value: Decimal, mark: u8) -> String {
    let text = value.normalize().to_string();
    if mark == b'.' {
        text
    } else {
        text.replace('.', &(mark as char).to_string())
    }
}

/// A number as it was written, in the interchange's decimal mark.
pub fn read_num(text: &str, d: &Delimiters) -> Option<Decimal> {
    if text.is_empty() {
        return None;
    }
    number(text, d)
}

/// `DTM` with a date as `CCYYMMDD` (format 102).
pub fn dtm(qualifier: &str, date: NaiveDate) -> Segment {
    seg(
        "DTM",
        vec![comps![qualifier, date.format("%Y%m%d").to_string(), "102"]],
    )
}

/// The date of a `DTM`, in format 102 or 203 (with a time); `None` for
/// any other format or a date that is no date.
pub fn read_dtm(segment: &Segment) -> Option<(String, NaiveDate, String)> {
    let qualifier = segment.component(1, 1).to_owned();
    let value = segment.component(1, 2);
    let format = segment.component(1, 3);
    let (day, time) = match (format, value.len()) {
        ("102" | "", 8) => (value, ""),
        ("203", 12) => (&value[..8], &value[8..]),
        _ => return None,
    };
    NaiveDate::parse_from_str(day, "%Y%m%d")
        .ok()
        .map(|d| (qualifier, d, time.to_owned()))
}

/// A party's role: EDIFACT's code (3035) and the typed message's.
const ROLES: &[(&str, &str)] = &[
    ("BY", "BY"),
    ("SU", "VN"),
    ("DP", "ST"),
    ("IV", "BT"),
    ("SF", "SF"),
    ("UC", "Z7"),
];

/// A date's qualifier: EDIFACT's (2005) and the typed message's.
const DATES: &[(&str, &str)] = &[
    ("2", "002"),
    ("10", "010"),
    ("11", "011"),
    ("17", "017"),
    ("35", "035"),
    ("63", "038"),
    ("64", "037"),
    ("61", "001"),
];

/// Who assigned a party's or item's number: EDIFACT's code list agency
/// (3055) and the typed message's id qualifier.
const AGENCIES: &[(&str, &str)] = &[("9", "UL"), ("91", "91"), ("92", "92")];

/// What a product number is: EDIFACT's item type (7143) and the typed
/// message's qualifier.
const PRODUCTS: &[(&str, &str)] = &[("EN", "EN"), ("SRV", "UK"), ("SA", "VN"), ("IN", "IN")];

fn to_typed(table: &[(&str, &str)], code: &str) -> String {
    table
        .iter()
        .find(|(e, _)| *e == code)
        .map_or(code, |(_, t)| t)
        .to_owned()
}

fn to_edifact(table: &[(&str, &str)], code: &str) -> String {
    table
        .iter()
        .find(|(_, t)| *t == code)
        .map_or(code, |(e, _)| e)
        .to_owned()
}

pub fn role_in(code: &str) -> String {
    to_typed(ROLES, code)
}
pub fn role_out(code: &str) -> String {
    to_edifact(ROLES, code)
}
pub fn date_in(code: &str) -> String {
    to_typed(DATES, code)
}
pub fn date_out(code: &str) -> String {
    to_edifact(DATES, code)
}
pub fn product_in(code: &str) -> String {
    to_typed(PRODUCTS, code)
}
pub fn product_out(code: &str) -> String {
    to_edifact(PRODUCTS, code)
}

/// `NAD`: role, id and its agency, name, street, city, region, postal
/// code, country.
pub fn nad(party: &Party) -> Segment {
    let agency = to_edifact(AGENCIES, &party.id_qualifier);
    let id = if party.id.is_empty() {
        vec![]
    } else {
        comps![party.id.clone(), "", agency]
    };
    let lines: Vec<String> = party.address_lines.iter().take(4).cloned().collect();
    seg(
        "NAD",
        vec![
            comps![role_out(&party.role)],
            id,
            vec![],
            if party.name.is_empty() {
                vec![]
            } else {
                comps![party.name.clone()]
            },
            lines,
            comps![party.city.clone()],
            comps![party.region.clone()],
            comps![party.postal_code.clone()],
            comps![party.country.clone()],
        ],
    )
}

pub fn read_nad(segment: &Segment) -> Party {
    let lines: Vec<String> = segment
        .element(5)
        .map(|e| {
            e.components(0)
                .into_iter()
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Party {
        role: role_in(segment.get(1)),
        id: segment.component(2, 1).to_owned(),
        id_qualifier: if segment.component(2, 1).is_empty() {
            String::new()
        } else {
            to_typed(AGENCIES, segment.component(2, 3))
        },
        name: segment.component(4, 1).to_owned(),
        address_lines: lines,
        city: segment.get(6).to_owned(),
        region: segment.get(7).to_owned(),
        postal_code: segment.get(8).to_owned(),
        country: segment.get(9).to_owned(),
    }
}

/// `RFF`: qualifier and value.
pub fn rff(reference: &Reference) -> Segment {
    seg(
        "RFF",
        vec![comps![reference.qualifier.clone(), reference.value.clone()]],
    )
}

pub fn read_rff(segment: &Segment) -> Reference {
    Reference {
        qualifier: segment.component(1, 1).to_owned(),
        value: segment.component(1, 2).to_owned(),
        description: String::new(),
    }
}

pub fn date_of(segment: &Segment) -> Option<DateOf> {
    read_dtm(segment).map(|(q, date, _)| DateOf {
        qualifier: date_in(&q),
        date,
    })
}

/// A line's product numbers: the first GTIN in `LIN`, the rest in `PIA`.
/// Returns the `LIN` item number element and the `PIA` segments.
pub fn product_segments(ids: &[ProductId]) -> (Vec<String>, Vec<Segment>) {
    let mut lin = vec![];
    let mut pia = vec![];
    for id in ids {
        if lin.is_empty() && id.qualifier == "EN" {
            lin = comps![id.value.clone(), "EN"];
        } else {
            pia.push(seg(
                "PIA",
                vec![
                    comps!["1"],
                    comps![id.value.clone(), product_out(&id.qualifier)],
                ],
            ));
        }
    }
    (lin, pia)
}

/// The product number of a `LIN`, if it names one.
pub fn lin_product(segment: &Segment) -> Option<ProductId> {
    let value = segment.component(3, 1);
    (!value.is_empty()).then(|| ProductId {
        qualifier: product_in(segment.component(3, 2)),
        value: value.to_owned(),
    })
}

/// The product numbers of a `PIA`: each of its item number elements.
pub fn pia_products(segment: &Segment) -> Vec<ProductId> {
    (2..=6)
        .filter_map(|n| {
            let value = segment.component(n, 1);
            (!value.is_empty()).then(|| ProductId {
                qualifier: product_in(segment.component(n, 2)),
                value: value.to_owned(),
            })
        })
        .collect()
}

/// `QTY`: qualifier, quantity, unit.
pub fn qty(qualifier: &str, quantity: Decimal, uom: &str, mark: u8) -> Segment {
    seg("QTY", vec![comps![qualifier, num(quantity, mark), uom]])
}

/// `MOA`: qualifier and amount.
pub fn moa(qualifier: &str, amount: Decimal, mark: u8) -> Segment {
    seg("MOA", vec![comps![qualifier, num(amount, mark)]])
}

/// `PRI`: qualifier and price.
pub fn pri(qualifier: &str, price: Decimal, mark: u8) -> Segment {
    seg("PRI", vec![comps![qualifier, num(price, mark)]])
}

/// `IMD` with a free text description.
pub fn imd(text: &str) -> Segment {
    seg("IMD", vec![comps!["F"], vec![], comps!["", "", "", text]])
}

/// The description of an `IMD`.
pub fn read_imd(segment: &Segment) -> String {
    segment.component(3, 4).to_owned()
}
