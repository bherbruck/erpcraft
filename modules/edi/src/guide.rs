//! A partner's implementation guide as data (PLAN.md, phase 4).
//!
//! What differs between partners is not code: it is a guide, a record of
//! the company's with one spec per message and direction. A spec has
//! three closed parts:
//!
//! - `options`: choices the standard leaves open, by name. Received:
//!   `item_match`, `ship_to`, `dates`, `references`. Sent: `hierarchy`,
//!   `pack_qualifier`, `ack_detail`, `free_text`, `uom`.
//! - `constraints`: per segment, and per element (and component), whether
//!   it is `required` or `not_used`, its `min` and `max` length and the
//!   `codes` it may hold. What is sent and breaks one is not sent: the
//!   message is `invalid`. What is received and breaks one is a finding on
//!   the proposal; the acknowledgement answers the standard, not the guide.
//! - `extra`: segments added to a message sent, after or before a segment
//!   the writer writes, each element a constant or an expression over a
//!   closed set of facts (`partner.*`, `order.*` as received,
//!   `document.*`, and `line.*` for a place inside a line), with an
//!   optional condition. An expression that cannot be worked out makes the
//!   message `invalid`, never a segment with an empty value.
//!
//! A guide may build on one other guide of the company, and every guide
//! builds on the module's own base for its syntax and version
//! (`guides/*.json`): options of the guide replace its base's, a
//! constraint replaces the base's for the same place, and extra segments
//! are added to the base's.

use std::collections::{BTreeMap, BTreeSet};

use erpcraft_kernel::expression::Expression;
use erpcraft_kernel::{Error, PgConnection, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::outbound::Typed;
use crate::partner::Partner;
use crate::proposal::Finding;
use crate::segment::{Element, Segment};
use crate::x12::asn856::{Item, Node, Pack, PackKind, ShipNotice};

/// The names an expression may read, and the fields of each.
pub const FACTS: &[(&str, &[&str])] = &[
    (
        "partner",
        &[
            "code",
            "name",
            "our_id",
            "our_qualifier",
            "their_id",
            "their_qualifier",
        ],
    ),
    (
        "order",
        &[
            "number",
            "date",
            "release",
            "purpose",
            "currency",
            "reference",
            "party",
        ],
    ),
    (
        "document",
        &[
            "doc_type",
            "number",
            "date",
            "currency",
            "total_net",
            "total_tax",
            "total_gross",
        ],
    ),
    (
        "line",
        &["line_id", "quantity", "uom", "unit_price", "product"],
    ),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    #[serde(default, skip_serializing_if = "Options::is_empty")]
    pub options: Options,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<Extra>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// Received: the kinds of cross-reference an item is looked up by, in
    /// order, each with the product id qualifiers that carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_match: Option<Vec<ItemMatch>>,
    /// Received: the party that is the place to ship to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_to: Option<ShipTo>,
    /// Received: which date qualifiers say when to ship.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dates: Option<Dates>,
    /// Received: the reference qualifiers kept, to be echoed in what is
    /// sent back. All are kept when this is left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<String>>,
    /// Sent (ship notice): the levels of the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy: Option<Hierarchy>,
    /// Sent (ship notice): the code for what the shipment is packed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_qualifier: Option<String>,
    /// Sent (order response): every line, or the header alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_detail: Option<AckDetail>,
    /// Sent: what replaces a separator in a description. Identifiers are
    /// never altered. Left out, the separator is dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text: Option<String>,
    /// Sent: the code written for a unit, by the code the order named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uom: Option<BTreeMap<String, String>>,
}

impl Options {
    fn is_empty(&self) -> bool {
        *self == Options::default()
    }

    fn inbound_set(&self) -> Vec<&'static str> {
        let mut set = vec![];
        if self.item_match.is_some() {
            set.push("item_match");
        }
        if self.ship_to.is_some() {
            set.push("ship_to");
        }
        if self.dates.is_some() {
            set.push("dates");
        }
        if self.references.is_some() {
            set.push("references");
        }
        set
    }

    fn outbound_set(&self) -> Vec<&'static str> {
        let mut set = vec![];
        if self.hierarchy.is_some() {
            set.push("hierarchy");
        }
        if self.pack_qualifier.is_some() {
            set.push("pack_qualifier");
        }
        if self.ack_detail.is_some() {
            set.push("ack_detail");
        }
        if self.free_text.is_some() {
            set.push("free_text");
        }
        if self.uom.is_some() {
            set.push("uom");
        }
        set
    }

    /// `self` over `base`, option by option.
    fn over(&self, base: &Options) -> Options {
        Options {
            item_match: self.item_match.clone().or_else(|| base.item_match.clone()),
            ship_to: self.ship_to.clone().or_else(|| base.ship_to.clone()),
            dates: self.dates.clone().or_else(|| base.dates.clone()),
            references: self.references.clone().or_else(|| base.references.clone()),
            hierarchy: self.hierarchy.or(base.hierarchy),
            pack_qualifier: self
                .pack_qualifier
                .clone()
                .or_else(|| base.pack_qualifier.clone()),
            ack_detail: self.ack_detail.or(base.ack_detail),
            free_text: self.free_text.clone().or_else(|| base.free_text.clone()),
            uom: self.uom.clone().or_else(|| base.uom.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemMatch {
    pub kind: ItemKind,
    pub qualifiers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Gtin,
    SellerItem,
    BuyerItem,
}

impl ItemKind {
    pub fn code(self) -> &'static str {
        match self {
            ItemKind::Gtin => "gtin",
            ItemKind::SellerItem => "seller_item",
            ItemKind::BuyerItem => "buyer_item",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShipTo {
    /// The party's role as the typed order has it, such as `ST`.
    pub role: String,
    /// The id qualifier its place is looked up by; any when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dates {
    /// The qualifiers of the date to ship by, the first found wins.
    pub ship: Vec<String>,
    /// The qualifier of the date after which the order is cancelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<String>,
}

/// The levels of a ship notice: shipment, order, then tare (pallet), pack
/// (carton) and item as named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Hierarchy {
    /// Shipment, order, pack, item: pallets are left out.
    #[serde(rename = "SOPI")]
    Sopi,
    /// Shipment, order, pallet, pack, item, as packed.
    #[serde(rename = "SOTPI")]
    Sotpi,
    /// Shipment, order, item, pack: each item with the cartons it is in.
    #[serde(rename = "SOIP")]
    Soip,
    /// Shipment, order, item: no packs.
    #[serde(rename = "SOI")]
    Soi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AckDetail {
    Line,
    Header,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    pub segment: String,
    /// Counted from 1; the segment as a whole when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element: Option<usize>,
    /// Counted from 1, within the element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_used: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codes: Option<Vec<String>>,
}

impl Constraint {
    fn place(&self) -> (String, Option<usize>, Option<usize>) {
        (self.segment.clone(), self.element, self.component)
    }

    fn named(&self) -> String {
        match (self.element, self.component) {
            (None, _) => format!("segment {}", self.segment),
            (Some(e), None) => format!("{}{e:02}", self.segment),
            (Some(e), Some(c)) => format!("{}{e:02}-{c}", self.segment),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Extra {
    pub segment: String,
    /// Written after this segment (after each, inside a line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Written before this segment (before each, inside a line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    pub elements: Vec<ExtraElement>,
    /// Written only where this is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// One element of an extra segment: a constant, an expression, or a
/// composite of these.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtraElement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<ExtraElement>>,
}

impl Extra {
    fn place(&self) -> &str {
        self.after
            .as_deref()
            .or(self.before.as_deref())
            .unwrap_or("")
    }
}

impl Spec {
    /// `self` over `base`.
    pub fn over(&self, base: &Spec) -> Spec {
        let own: BTreeSet<_> = self.constraints.iter().map(Constraint::place).collect();
        let mut constraints: Vec<Constraint> = base
            .constraints
            .iter()
            .filter(|c| !own.contains(&c.place()))
            .cloned()
            .collect();
        constraints.extend(self.constraints.iter().cloned());
        let mut extra = base.extra.clone();
        extra.extend(self.extra.iter().cloned());
        Spec {
            options: self.options.over(&base.options),
            constraints,
            extra,
        }
    }
}

/// The segments a message's writer writes, as far as a guide may name
/// them: the header's, a line's (the first is where a line starts) and
/// the summary's.
pub struct Layout {
    pub header: &'static [&'static str],
    pub line: &'static [&'static str],
    pub summary: &'static [&'static str],
}

impl Layout {
    fn has(&self, tag: &str) -> bool {
        self.header.contains(&tag) || self.line.contains(&tag) || self.summary.contains(&tag)
    }

    fn in_line(&self, tag: &str) -> bool {
        self.line.contains(&tag)
    }
}

/// The messages a guide describes, with their direction.
pub const MESSAGES: &[(&str, &str, &str)] = &[
    ("x12", "850", "in"),
    ("x12", "855", "out"),
    ("x12", "856", "out"),
    ("x12", "810", "out"),
    ("edifact", "ORDERS", "in"),
    ("edifact", "ORDRSP", "out"),
    ("edifact", "DESADV", "out"),
    ("edifact", "INVOIC", "out"),
];

pub fn layout(message: &str) -> Option<Layout> {
    let (header, line, summary): (&[&str], &[&str], &[&str]) = match message {
        "855" => (
            &["BAK", "CUR", "REF", "DTM", "N1", "N2", "N3", "N4"],
            &["PO1", "ACK"],
            &["CTT"],
        ),
        "856" => (
            &[
                "BSN", "HL", "TD1", "TD5", "TD3", "REF", "DTM", "N1", "N2", "N3", "N4", "PRF",
                "MAN",
            ],
            &["LIN", "SN1", "PID"],
            &["CTT"],
        ),
        "810" => (
            &["BIG", "CUR", "REF", "N1", "N2", "N3", "N4", "ITD", "DTM"],
            &["IT1", "PID"],
            &["TDS", "TXI", "SAC", "CTT"],
        ),
        "ORDRSP" => (
            &["BGM", "DTM", "CUX", "RFF", "NAD"],
            &["LIN", "PIA", "QTY", "PRI"],
            &["UNS", "CNT"],
        ),
        "DESADV" => (
            &[
                "BGM", "DTM", "RFF", "NAD", "TDT", "EQD", "MEA", "CPS", "PAC", "PCI", "GIN",
            ],
            &["LIN", "PIA", "QTY", "IMD"],
            &["CNT"],
        ),
        "INVOIC" => (
            &["BGM", "DTM", "RFF", "NAD", "CUX", "ALC", "MOA"],
            &["LIN", "PIA", "IMD", "QTY", "PRI"],
            &["UNS", "CNT", "TAX"],
        ),
        _ => return None,
    };
    Some(Layout {
        header,
        line,
        summary,
    })
}

fn is_tag(tag: &str) -> bool {
    (2..=3).contains(&tag.len())
        && tag
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && tag.as_bytes()[0].is_ascii_uppercase()
}

/// Whether `message` travels in `direction` in a guide of `syntax`.
pub fn describes(syntax: &str, message: &str, direction: &str) -> bool {
    MESSAGES.contains(&(syntax, message, direction))
}

fn names() -> Vec<&'static str> {
    FACTS.iter().map(|(n, _)| *n).collect()
}

/// An expression as a guide may hold it: it parses, and reads only the
/// facts declared, and `line.*` only inside a line.
fn expression(source: &str, in_line: bool) -> std::result::Result<Expression, String> {
    let parsed = Expression::parse_stored(source, &names()).map_err(|e| e.to_string())?;
    for (name, fields) in FACTS {
        for field in parsed.fields_of(name) {
            if !fields.contains(&field.as_str()) {
                return Err(format!(
                    "expression `{source}` reads {name}.{field}, which is not a fact a guide \
                     may read"
                ));
            }
        }
    }
    if !in_line && !parsed.fields_of("line").is_empty() {
        return Err(format!(
            "expression `{source}` reads line.*, but its segment is not inside a line"
        ));
    }
    Ok(parsed)
}

fn check_element(e: &ExtraElement, in_line: bool, nested: bool) -> std::result::Result<(), String> {
    let given = [e.value.is_some(), e.expr.is_some(), e.components.is_some()]
        .iter()
        .filter(|g| **g)
        .count();
    if given != 1 {
        return Err("an element of an extra segment is one of value, expr or components".into());
    }
    if let Some(source) = &e.expr {
        expression(source, in_line)?;
    }
    if let Some(components) = &e.components {
        if nested {
            return Err("a component cannot have components of its own".into());
        }
        if components.is_empty() {
            return Err("a composite element has at least one component".into());
        }
        for c in components {
            check_element(c, in_line, true)?;
        }
    }
    Ok(())
}

/// Checks a spec for a message of a guide: what it names exists, and its
/// expressions parse and read only declared facts. The reason it cannot
/// be used otherwise.
pub fn check(
    syntax: &str,
    message: &str,
    direction: &str,
    spec: &Spec,
) -> std::result::Result<(), String> {
    if !describes(syntax, message, direction) {
        return Err(format!(
            "message {message} ({direction}) is not one a guide of this syntax describes"
        ));
    }
    let outbound = direction == "out";
    let wrong = if outbound {
        spec.options.inbound_set()
    } else {
        spec.options.outbound_set()
    };
    if let Some(option) = wrong.first() {
        return Err(format!(
            "option {option} is not one for a message {}",
            if outbound { "sent" } else { "received" }
        ));
    }
    let notice = matches!(message, "856" | "DESADV");
    if !notice && spec.options.hierarchy.is_some() {
        return Err("hierarchy is an option of a ship notice".into());
    }
    if message != "856" && spec.options.pack_qualifier.is_some() {
        return Err("pack_qualifier is an option of an 856".into());
    }
    if !matches!(message, "855" | "ORDRSP") && spec.options.ack_detail.is_some() {
        return Err("ack_detail is an option of an order response".into());
    }
    if let Some(free) = &spec.options.free_text
        && (free.chars().count() > 1 || free.chars().any(|c| "*~>^:+'?".contains(c)))
    {
        return Err("free_text is one character that is not a separator, or nothing".into());
    }
    if let Some(item_match) = &spec.options.item_match {
        if item_match.is_empty() {
            return Err("item_match names at least one kind".into());
        }
        for m in item_match {
            if m.qualifiers.is_empty() || m.qualifiers.iter().any(|q| !is_code(q)) {
                return Err(format!(
                    "item_match {} needs qualifiers of one to three letters or digits",
                    m.kind.code()
                ));
            }
        }
    }
    if let Some(ship_to) = &spec.options.ship_to
        && (!is_code(&ship_to.role) || ship_to.qualifier.as_deref().is_some_and(|q| !is_code(q)))
    {
        return Err(
            "ship_to names a role, and a qualifier, of one to three letters or digits".into(),
        );
    }
    if let Some(dates) = &spec.options.dates
        && (dates.ship.is_empty() || dates.ship.iter().chain(&dates.cancel).any(|q| !is_code(q)))
    {
        return Err("dates names the qualifiers of the date to ship by".into());
    }
    if let Some(references) = &spec.options.references
        && references.iter().any(|q| !is_code(q))
    {
        return Err("references are qualifiers of one to three letters or digits".into());
    }
    if let Some(code) = &spec.options.pack_qualifier
        && (code.is_empty() || code.len() > 8 || !code.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return Err("pack_qualifier is a code of up to 8 letters or digits".into());
    }
    if let Some(uom) = &spec.options.uom
        && uom.iter().any(|(k, v)| !is_code(k) || !is_code(v))
    {
        return Err("uom maps unit codes of one to three letters or digits".into());
    }
    let layout = layout(message);
    for c in &spec.constraints {
        if !is_tag(&c.segment) {
            return Err(format!("{} is not a segment tag", c.segment));
        }
        if let Some(layout) = &layout
            && !layout.has(&c.segment)
        {
            return Err(format!(
                "the {message} this module writes has no segment {}",
                c.segment
            ));
        }
        if c.element.is_some_and(|e| e == 0 || e > 99)
            || c.component.is_some_and(|e| e == 0 || e > 99)
        {
            return Err(format!(
                "{}: elements and components count from 1",
                c.named()
            ));
        }
        if c.component.is_some() && c.element.is_none() {
            return Err(format!("{}: a component needs its element", c.named()));
        }
        if c.required && c.not_used {
            return Err(format!(
                "{} cannot be both required and not used",
                c.named()
            ));
        }
        if let (Some(min), Some(max)) = (c.min, c.max)
            && min > max
        {
            return Err(format!("{}: min is more than max", c.named()));
        }
        if c.element.is_none() && (c.min.is_some() || c.max.is_some() || c.codes.is_some()) {
            return Err(format!(
                "{}: min, max and codes are said of an element",
                c.named()
            ));
        }
    }
    if !outbound && !spec.extra.is_empty() {
        return Err("extra segments are added to a message sent, not one received".into());
    }
    for extra in &spec.extra {
        if !is_tag(&extra.segment) {
            return Err(format!("{} is not a segment tag", extra.segment));
        }
        if extra.after.is_some() == extra.before.is_some() {
            return Err(format!(
                "extra segment {} names one place: after or before",
                extra.segment
            ));
        }
        let place = extra.place();
        let Some(layout) = &layout else {
            return Err(format!("message {message} takes no extra segments"));
        };
        if !layout.has(place) {
            return Err(format!(
                "extra segment {}: the {message} this module writes has no segment {place}",
                extra.segment
            ));
        }
        if extra.elements.is_empty() {
            return Err(format!("extra segment {} has no elements", extra.segment));
        }
        let in_line = layout.in_line(place);
        for e in &extra.elements {
            check_element(e, in_line, false)
                .map_err(|why| format!("extra segment {}: {why}", extra.segment))?;
        }
        if let Some(condition) = &extra.condition {
            expression(condition, in_line)
                .map_err(|why| format!("extra segment {}: {why}", extra.segment))?;
        }
    }
    Ok(())
}

fn is_code(code: &str) -> bool {
    (1..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A spec as stored, read.
pub fn parse(spec: &Value) -> std::result::Result<Spec, String> {
    serde_json::from_value(spec.clone()).map_err(|e| format!("the spec cannot be read: {e}"))
}

/// The module's own base for a syntax and version.
pub fn builtin(syntax: &str, version: &str) -> BTreeMap<(String, String), Spec> {
    let text = match (syntax, version) {
        ("x12", "005010") => include_str!("../guides/x12_005010.json"),
        ("edifact", _) => include_str!("../guides/edifact_d96a.json"),
        _ => include_str!("../guides/x12_004010.json"),
    };
    messages_of(text).unwrap_or_default()
}

/// The messages of a guide file: `{ "messages": [{ message, direction,
/// spec }] }`.
fn messages_of(text: &str) -> std::result::Result<BTreeMap<(String, String), Spec>, String> {
    #[derive(Deserialize)]
    struct File {
        messages: Vec<FileMessage>,
    }
    let file: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(file
        .messages
        .into_iter()
        .map(|m| ((m.message, m.direction), m.spec))
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileMessage {
    pub message: String,
    pub direction: String,
    pub spec: Spec,
}

/// The spec a partner's message is read or written under: the module's
/// base, the guide's base and the guide, in that order.
pub async fn spec_for(
    conn: &mut PgConnection,
    partner: &Partner,
    message: &str,
    direction: &str,
) -> Result<Spec> {
    spec_of(
        conn,
        &partner.syntax,
        &partner.version,
        partner.guide_id,
        message,
        direction,
    )
    .await
}

/// The spec of a message under a guide (or none: the module's base).
pub async fn spec_of(
    conn: &mut PgConnection,
    syntax: &str,
    version: &str,
    guide: Option<Uuid>,
    message: &str,
    direction: &str,
) -> Result<Spec> {
    let key = (message.to_owned(), direction.to_owned());
    let mut spec = builtin(syntax, version).remove(&key).unwrap_or_default();
    let Some(guide) = guide else {
        return Ok(spec);
    };
    // The base's first, then the guide's own.
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT m.spec FROM edi_guide g
         JOIN edi_guide_message m ON m.guide_id = g.id OR m.guide_id = g.base_guide_id
         WHERE g.id = $1 AND m.message = $2 AND m.direction = $3
         ORDER BY m.guide_id = g.id",
    )
    .bind(guide)
    .bind(message)
    .bind(direction)
    .fetch_all(&mut *conn)
    .await?;
    for (stored,) in rows {
        let own = parse(&stored).map_err(Error::internal)?;
        spec = own.over(&spec);
    }
    Ok(spec)
}

// ------------------------------------------------------------- constraints

/// What breaks the constraints in `segments` (a message's, envelope
/// included or not), in words.
pub fn violations(constraints: &[Constraint], segments: &[Segment]) -> Vec<String> {
    let mut out = vec![];
    for c in constraints {
        let found: Vec<&Segment> = segments.iter().filter(|s| s.tag == c.segment).collect();
        let Some(element) = c.element else {
            if c.required && found.is_empty() {
                out.push(format!("segment {} is required by the guide", c.segment));
            }
            if c.not_used && !found.is_empty() {
                out.push(format!("segment {} is not used in the guide", c.segment));
            }
            continue;
        };
        for s in found {
            let value = match c.component {
                Some(component) => s.component(element, component),
                None => s.get(element),
            };
            let name = c.named();
            if value.is_empty() {
                if c.required {
                    out.push(format!("{name} is required by the guide"));
                }
                continue;
            }
            if c.not_used {
                out.push(format!(
                    "{name} is not used in the guide, and holds {value}"
                ));
            }
            let length = value.chars().count();
            if c.min.is_some_and(|min| length < min) || c.max.is_some_and(|max| length > max) {
                out.push(format!(
                    "{name} is {length} characters long ({value}); the guide says {} to {}",
                    c.min.unwrap_or(1),
                    c.max.map_or("any".to_owned(), |m| m.to_string())
                ));
            }
            if let Some(codes) = &c.codes
                && !codes.iter().any(|code| code == value)
            {
                out.push(format!(
                    "{name} holds {value}, which is not one of the guide's codes ({})",
                    codes.join(", ")
                ));
            }
        }
    }
    out
}

// ------------------------------------------------------------- options out

fn clean(text: &str, free: Option<&str>) -> String {
    text.chars()
        .map(|c| {
            if "*~>^:+'?".contains(c) {
                free.unwrap_or("").to_owned()
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn unit(uom: &mut String, map: Option<&BTreeMap<String, String>>) {
    if let Some(code) = map.and_then(|m| m.get(uom.as_str())) {
        *uom = code.clone();
    }
}

/// Applies a spec's options to a message made, before it is written.
pub fn apply_options(options: &Options, typed: &mut Typed) {
    let free = options.free_text.as_deref();
    let map = options.uom.as_ref();
    match typed {
        Typed::Ack(ack) => {
            if options.ack_detail == Some(AckDetail::Header) {
                ack.lines.clear();
            }
            for line in &mut ack.lines {
                unit(&mut line.uom, map);
                for status in &mut line.statuses {
                    unit(&mut status.uom, map);
                }
            }
        }
        Typed::Invoice(invoice) => {
            for line in &mut invoice.lines {
                unit(&mut line.uom, map);
                line.description = clean(&line.description, free);
                for c in &mut line.charges {
                    c.description = clean(&c.description, free);
                }
            }
            for c in &mut invoice.charges {
                c.description = clean(&c.description, free);
            }
        }
        Typed::Notice(notice) => {
            if let Some(code) = &options.pack_qualifier
                && let Some(lading) = notice.lading.as_mut()
            {
                lading.packaging = code.clone();
            }
            if let Some(hierarchy) = options.hierarchy {
                arrange(notice, hierarchy);
            }
            for order in &mut notice.orders {
                walk_items(&mut order.contents, &mut |item| {
                    unit(&mut item.uom, map);
                    unit(&mut item.ordered_uom, map);
                    item.description = clean(&item.description, free);
                });
            }
        }
    }
}

fn walk_items(nodes: &mut [Node], f: &mut impl FnMut(&mut Item)) {
    for node in nodes {
        match node {
            Node::Item(item) => {
                f(item);
                for pack in &mut item.packs {
                    walk_items(&mut pack.contents, f);
                }
            }
            Node::Pack(pack) => walk_items(&mut pack.contents, f),
        }
    }
}

/// The cartons of a tree, wherever they are, each with what it holds.
fn cartons(nodes: Vec<Node>, out: &mut Vec<Pack>, loose: &mut Vec<Item>) {
    for node in nodes {
        match node {
            Node::Pack(pack) if pack.kind == PackKind::Pallet => cartons(pack.contents, out, loose),
            Node::Pack(pack) => out.push(pack),
            Node::Item(item) => loose.push(item),
        }
    }
}

/// Items of the same line made one, their quantities added.
fn merged(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Item> = vec![];
    for item in items {
        match out.iter_mut().find(|i| i.line_id == item.line_id) {
            Some(existing) => {
                existing.shipped = match (existing.shipped, item.shipped) {
                    (Some(a), Some(b)) => a.checked_add(b),
                    (a, b) => a.or(b),
                };
                existing.packs.extend(item.packs);
            }
            None => out.push(item),
        }
    }
    out
}

/// Rearranges a notice's tree to the levels a guide wants. The notice
/// was made shipment, order, (pallet,) carton, item.
fn arrange(notice: &mut ShipNotice, hierarchy: Hierarchy) {
    if hierarchy == Hierarchy::Sotpi {
        return;
    }
    notice.structure = match hierarchy {
        Hierarchy::Soip => "0002",
        Hierarchy::Soi => "0004",
        _ => "0001",
    }
    .into();
    for order in &mut notice.orders {
        let mut packs = vec![];
        let mut loose = vec![];
        cartons(std::mem::take(&mut order.contents), &mut packs, &mut loose);
        order.contents = match hierarchy {
            Hierarchy::Sopi => packs
                .into_iter()
                .map(Node::Pack)
                .chain(loose.into_iter().map(Node::Item))
                .collect(),
            Hierarchy::Soip | Hierarchy::Soi => {
                let mut items = loose;
                for pack in packs {
                    let sscc = pack.sscc.clone();
                    for node in pack.contents {
                        if let Node::Item(mut item) = node {
                            if hierarchy == Hierarchy::Soip {
                                item.packs.push(Pack {
                                    kind: PackKind::Carton,
                                    sscc: sscc.clone(),
                                    contents: vec![],
                                });
                            }
                            items.push(item);
                        }
                    }
                }
                merged(items).into_iter().map(Node::Item).collect()
            }
            Hierarchy::Sotpi => unreachable!(),
        };
    }
}

// ------------------------------------------------------------- extra out

/// What an expression of a message sent may read, but its lines.
pub fn facts(partner: &Partner, order: &Value, document: &Value) -> Value {
    json!({
        "partner": {
            "code": partner.code,
            "name": partner.name,
            "our_id": partner.our_id,
            "our_qualifier": partner.our_qualifier,
            "their_id": partner.their_id,
            "their_qualifier": partner.their_qualifier,
        },
        "order": order,
        "document": document,
    })
}

/// The facts of each line of a message, in the order its lines are
/// written.
pub fn line_facts(typed: &Typed) -> Vec<Value> {
    let product = |ids: &[crate::x12::po850::ProductId]| -> Value {
        let mut map = serde_json::Map::new();
        for id in ids {
            map.entry(id.qualifier.clone())
                .or_insert_with(|| json!(id.value));
        }
        Value::Object(map)
    };
    let number = |d: Option<rust_decimal::Decimal>| d.map(|d| d.normalize().to_string());
    match typed {
        Typed::Ack(ack) => ack
            .lines
            .iter()
            .map(|l| {
                json!({ "line_id": l.line_id, "quantity": number(l.quantity), "uom": l.uom,
                        "unit_price": number(l.unit_price), "product": product(&l.product_ids) })
            })
            .collect(),
        Typed::Invoice(invoice) => invoice
            .lines
            .iter()
            .map(|l| {
                json!({ "line_id": l.line_id, "quantity": number(l.quantity), "uom": l.uom,
                        "unit_price": number(l.unit_price), "product": product(&l.product_ids) })
            })
            .collect(),
        Typed::Notice(notice) => {
            let mut out = vec![];
            let mut nodes: Vec<Node> = notice
                .orders
                .iter()
                .flat_map(|o| o.contents.clone())
                .collect();
            walk_items(&mut nodes, &mut |item| {
                out.push(
                    json!({ "line_id": item.line_id, "quantity": number(item.shipped),
                                 "uom": item.uom, "unit_price": null,
                                 "product": product(&item.product_ids) }),
                );
            });
            out
        }
    }
}

fn text_of(value: Value, source: &str) -> std::result::Result<String, String> {
    let text = match value {
        Value::String(s) => s,
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        other => {
            return Err(format!(
                "expression `{source}` gives {other}, which is not text or a number"
            ));
        }
    };
    if text.is_empty() {
        return Err(format!("expression `{source}` gives nothing"));
    }
    Ok(text)
}

fn value_of(e: &ExtraElement, facts: &Value) -> std::result::Result<String, String> {
    if let Some(v) = &e.value {
        return Ok(v.clone());
    }
    let source = e.expr.as_deref().unwrap_or("");
    let parsed = expression(source, true)?;
    let value = parsed.evaluate(facts).map_err(|e| e.to_string())?;
    text_of(value, source)
}

fn built(extra: &Extra, facts: &Value) -> std::result::Result<Option<Segment>, String> {
    if let Some(condition) = &extra.condition {
        let parsed = expression(condition, true)?;
        if !parsed.is_true(facts).map_err(|e| e.to_string())? {
            return Ok(None);
        }
    }
    let mut elements = vec![];
    for e in &extra.elements {
        elements.push(match &e.components {
            Some(components) => {
                let parts = components
                    .iter()
                    .map(|c| value_of(c, facts))
                    .collect::<std::result::Result<Vec<String>, String>>()?;
                Element::composite(&parts)
            }
            None => Element::simple(value_of(e, facts)?),
        });
    }
    Ok(Some(Segment {
        tag: extra.segment.clone(),
        elements,
        offset: 0,
    }))
}

/// Adds a spec's extra segments to a message's body, and checks its
/// constraints on the result. What fails is a blocking finding.
pub fn apply_out(
    spec: &Spec,
    message: &str,
    facts: &Value,
    lines: &[Value],
    body: &mut Vec<Segment>,
) -> Vec<Finding> {
    let mut findings = vec![];
    if let Some(layout) = layout(message) {
        for extra in &spec.extra {
            let place = extra.place();
            let in_line = layout.in_line(place);
            let starts = layout.line.first().copied().unwrap_or("");
            // Where each goes: (index to insert at, line index).
            let mut at: Vec<(usize, Option<usize>)> = vec![];
            let mut line: Option<usize> = None;
            for (i, s) in body.iter().enumerate() {
                if s.tag == starts {
                    line = Some(line.map_or(0, |l| l + 1));
                }
                if s.tag == place {
                    let index = if extra.after.is_some() { i + 1 } else { i };
                    if in_line {
                        at.push((index, line));
                    } else if extra.after.is_some() {
                        at = vec![(index, None)];
                    } else if at.is_empty() {
                        at.push((index, None));
                    }
                }
            }
            if at.is_empty() {
                if !in_line {
                    findings.push(Finding::blocking(
                        "guide_extra",
                        None,
                        format!(
                            "The guide adds segment {} at {place}, which this message does not have.",
                            extra.segment
                        ),
                    ));
                }
                continue;
            }
            // From the end, so earlier places keep their index.
            for (index, line) in at.into_iter().rev() {
                let mut facts = facts.clone();
                facts["line"] = line
                    .and_then(|l| lines.get(l).cloned())
                    .unwrap_or_else(|| json!({}));
                match built(extra, &facts) {
                    Ok(Some(segment)) => body.insert(index, segment),
                    Ok(None) => {}
                    Err(why) => findings.push(Finding::blocking(
                        "guide_extra",
                        None,
                        format!(
                            "The guide's segment {} cannot be written: {why}.",
                            extra.segment
                        ),
                    )),
                }
            }
        }
    }
    for why in violations(&spec.constraints, body) {
        findings.push(Finding::blocking(
            "guide",
            None,
            format!("The message breaks the partner's guide: {why}."),
        ));
    }
    findings
}

// --------------------------------------------------------- import, export

/// A guide as one file, to share between companies.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuideFile {
    /// Always `erpcraft-edi-guide`.
    pub format: String,
    pub code: String,
    pub name: String,
    pub syntax: String,
    pub version: String,
    /// The code of the guide it builds on, of the company it is imported
    /// into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub messages: Vec<FileMessage>,
}

pub const FORMAT: &str = "erpcraft-edi-guide";

/// A guide's code, name, syntax, version, base's code and notes.
type ExportRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

/// A guide and its messages, as a file.
pub async fn export(conn: &mut PgConnection, guide_id: Uuid) -> Result<GuideFile> {
    let row: Option<ExportRow> = sqlx::query_as(
        "SELECT g.code, g.name, g.syntax, g.version, b.code, g.notes FROM edi_guide g
             LEFT JOIN edi_guide b ON b.id = g.base_guide_id WHERE g.id = $1",
    )
    .bind(guide_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((code, name, syntax, version, base, notes)) = row else {
        return Err(Error::not_found("edi_guide"));
    };
    let rows: Vec<(String, String, Value)> = sqlx::query_as(
        "SELECT message, direction, spec FROM edi_guide_message WHERE guide_id = $1
         ORDER BY message, direction",
    )
    .bind(guide_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut messages = vec![];
    for (message, direction, spec) in rows {
        messages.push(FileMessage {
            message,
            direction,
            spec: parse(&spec).map_err(Error::internal)?,
        });
    }
    Ok(GuideFile {
        format: FORMAT.into(),
        code,
        name,
        syntax,
        version,
        base,
        notes,
        messages,
    })
}

/// Checks a guide file through, message by message.
pub fn check_file(file: &GuideFile) -> std::result::Result<(), String> {
    if file.format != FORMAT {
        return Err(format!("the file is not an {FORMAT} file"));
    }
    let mut seen = BTreeSet::new();
    for m in &file.messages {
        if !seen.insert((m.message.clone(), m.direction.clone())) {
            return Err(format!(
                "message {} ({}) is in the file twice",
                m.message, m.direction
            ));
        }
        check(&file.syntax, &m.message, &m.direction, &m.spec)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(v: Value) -> Spec {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn the_builtin_bases_are_valid() {
        for (syntax, version) in [("x12", "004010"), ("x12", "005010"), ("edifact", "D96A")] {
            let messages = builtin(syntax, version);
            assert!(!messages.is_empty(), "{syntax} {version}");
            for ((message, direction), spec) in messages {
                check(syntax, &message, &direction, &spec)
                    .unwrap_or_else(|why| panic!("{syntax} {message}: {why}"));
            }
        }
    }

    #[test]
    fn a_spec_is_closed() {
        let bad: std::result::Result<Spec, _> =
            serde_json::from_value(json!({ "options": { "colour": "red" } }));
        assert!(bad.is_err());
        let bad: std::result::Result<Spec, _> = serde_json::from_value(json!({ "mapping": [] }));
        assert!(bad.is_err());
    }

    #[test]
    fn what_a_spec_names_must_exist() {
        let s = spec(json!({ "constraints": [{ "segment": "ZZZ", "required": true }] }));
        assert!(check("x12", "810", "out", &s).unwrap_err().contains("ZZZ"));
        let s = spec(json!({ "extra": [{ "segment": "REF", "after": "XYZ",
                                         "elements": [{ "value": "IA" }] }] }));
        assert!(check("x12", "810", "out", &s).unwrap_err().contains("XYZ"));
        let s = spec(json!({ "extra": [{ "segment": "REF", "after": "BIG",
                                         "elements": [{ "expr": "partner.secret" }] }] }));
        assert!(
            check("x12", "810", "out", &s)
                .unwrap_err()
                .contains("partner.secret")
        );
        let s = spec(json!({ "extra": [{ "segment": "REF", "after": "BIG",
                                         "elements": [{ "expr": "line.line_id" }] }] }));
        assert!(
            check("x12", "810", "out", &s)
                .unwrap_err()
                .contains("inside a line")
        );
        let s = spec(json!({ "options": { "hierarchy": "SOTPI" } }));
        assert!(check("x12", "810", "out", &s).is_err());
        assert!(check("x12", "856", "out", &s).is_ok());
        let s = spec(
            json!({ "options": { "item_match": [{ "kind": "gtin", "qualifiers": ["UP"] }] } }),
        );
        assert!(check("x12", "856", "out", &s).is_err());
        assert!(check("x12", "850", "in", &s).is_ok());
        assert!(check("edifact", "850", "in", &s).is_err());
    }

    #[test]
    fn a_guide_over_its_base() {
        let base = spec(json!({
            "options": { "hierarchy": "SOTPI", "pack_qualifier": "CTN25" },
            "constraints": [{ "segment": "REF", "element": 1, "codes": ["BM"] }],
            "extra": [{ "segment": "REF", "after": "BSN", "elements": [{ "value": "A" }] }],
        }));
        let own = spec(json!({
            "options": { "hierarchy": "SOPI" },
            "constraints": [{ "segment": "REF", "element": 1, "codes": ["BM", "CN"] }],
            "extra": [{ "segment": "REF", "after": "BSN", "elements": [{ "value": "B" }] }],
        }));
        let both = own.over(&base);
        assert_eq!(both.options.hierarchy, Some(Hierarchy::Sopi));
        assert_eq!(both.options.pack_qualifier.as_deref(), Some("CTN25"));
        assert_eq!(both.constraints.len(), 1);
        assert_eq!(both.constraints[0].codes.as_ref().unwrap().len(), 2);
        assert_eq!(both.extra.len(), 2);
    }

    #[test]
    fn constraints_are_checked_on_segments() {
        let s = spec(json!({ "constraints": [
            { "segment": "REF", "required": true },
            { "segment": "BEG", "element": 2, "codes": ["SA"] },
            { "segment": "BEG", "element": 3, "max": 4 },
            { "segment": "PO1", "element": 4, "not_used": true },
        ] }));
        let segments = vec![
            Segment::new("BEG", &["00", "DS", "123456"]),
            Segment::new("PO1", &["1", "2", "EA", "3.5"]),
        ];
        let found = violations(&s.constraints, &segments);
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(found[0].contains("segment REF is required"));
        assert!(found[1].contains("BEG02 holds DS"));
        assert!(found[2].contains("BEG03 is 6 characters"));
        assert!(found[3].contains("PO104 is not used"));
    }

    #[test]
    fn an_extra_segment_goes_where_named_or_the_message_is_invalid() {
        let s = spec(json!({ "extra": [
            { "segment": "REF", "after": "BIG",
              "elements": [{ "value": "IA" }, { "expr": "partner.our_id" }] },
            { "segment": "REF", "after": "IT1",
              "elements": [{ "value": "LI" }, { "expr": "line.line_id + '-' + document.number" }],
              "condition": "line.uom == 'CA'" },
        ] }));
        let mut body = vec![
            Segment::new("BIG", &["20261120", "INV-1"]),
            Segment::new("IT1", &["1", "4", "EA", "2"]),
            Segment::new("IT1", &["2", "4", "CA", "2"]),
            Segment::new("TDS", &["1600"]),
        ];
        let facts = json!({ "partner": { "our_id": "V100" }, "order": {},
                            "document": { "number": "INV-1" } });
        let lines = vec![
            json!({ "line_id": "1", "uom": "EA" }),
            json!({ "line_id": "2", "uom": "CA" }),
        ];
        let findings = apply_out(&s, "810", &facts, &lines, &mut body);
        assert!(findings.is_empty(), "{findings:?}");
        let tags: Vec<String> = body
            .iter()
            .map(|s| format!("{}*{}*{}", s.tag, s.get(1), s.get(2)))
            .collect();
        assert_eq!(
            tags,
            [
                "BIG*20261120*INV-1",
                "REF*IA*V100",
                "IT1*1*4",
                "IT1*2*4",
                "REF*LI*2-INV-1",
                "TDS*1600*"
            ]
        );

        // A fact that is not there: invalid, not an empty value.
        let facts = json!({ "partner": {}, "order": {}, "document": { "number": "INV-1" } });
        let mut body = vec![Segment::new("BIG", &["20261120", "INV-1"])];
        let findings = apply_out(&s, "810", &facts, &[], &mut body);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].code, "guide_extra");
        assert_eq!(body.len(), 1);
    }
}
