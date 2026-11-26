//! UN/EDIFACT envelopes: the interchange (UNB/UNZ) and its messages
//! (UNH/UNT), read with the tokenizer X12 uses.
//!
//! A file may begin with a UNA, nine bytes that state its separators in
//! this order: component, data element, decimal mark, release character,
//! a fifth that is reserved (a space) up to syntax version 3 and the
//! repetition separator from version 4, and the segment terminator.
//! Without a UNA the defaults apply (`:` `+` `.` `?` `'`, and `*` for
//! repetitions in version 4).
//!
//! As with X12, a file that cannot be followed is an [`EdiError`], and one
//! whose counts or references do not agree is read with its errors, as
//! the codes a CONTRL message reports them with.

use std::str::FromStr;

use rust_decimal::Decimal;

/// Shorthand for the components of an element, for the message modules.
macro_rules! comps {
    ($($c:expr),* $(,)?) => { vec![$(String::from($c)),*] };
}

pub mod common;
pub mod contrl;
pub mod desadv;
pub mod invoic;
pub mod invrpt;
pub mod ordchg;
pub mod orders;
pub mod ordrsp;
pub mod remadv;
mod write;

pub use write::{Envelope, Outgoing, SyntaxId, write};

use crate::segment::{
    Budget, Charset, Delimiters, EdiError, MAX_BYTES, Segment, Tokenizer, skip_filler,
};

/// Interchanges in one file.
pub const MAX_INTERCHANGES: usize = 100;

/// CONTRL's syntax error code (data element 0085) for references that do
/// not match: UNH and UNT, or UNB and UNZ.
pub const REFERENCES_DO_NOT_MATCH: u8 = 28;
/// And for a control count that is not the number received.
pub const COUNT_DOES_NOT_MATCH: u8 = 29;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// UNH 0062: the message reference number.
    pub reference: String,
    /// Such as `ORDERS`.
    pub kind: String,
    /// Such as `D`.
    pub version: String,
    /// Such as `96A`.
    pub release: String,
    /// Such as `UN`.
    pub agency: String,
    /// The association's subset, such as `EAN008` for EANCOM.
    pub association: String,
    /// Every segment from the UNH to the UNT, both included.
    pub segments: Vec<Segment>,
    /// Codes of data element 0085.
    pub errors: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interchange {
    /// Whether the file stated its separators with a UNA.
    pub una: bool,
    pub delimiters: Delimiters,
    /// Such as `UNOA`, `UNOC` (ISO 8859-1) or `UNOY` (UTF-8).
    pub syntax_id: String,
    pub syntax_version: String,
    pub sender: String,
    pub sender_qualifier: String,
    pub receiver: String,
    pub receiver_qualifier: String,
    /// `YYMMDD` (or `CCYYMMDD` in version 4), as sent.
    pub date: String,
    pub time: String,
    /// UNB 0020: the interchange control reference.
    pub control: String,
    /// UNB 0035 is `1`: a test.
    pub test: bool,
    pub messages: Vec<Message>,
    /// Codes of data element 0085.
    pub errors: Vec<u8>,
}

/// Reads every interchange of a file.
pub fn read(bytes: &[u8]) -> Result<Vec<Interchange>, EdiError> {
    if bytes.len() > MAX_BYTES {
        return Err(EdiError::at(
            0,
            format!("the file is larger than {} MiB", MAX_BYTES / 1024 / 1024),
        ));
    }
    let charset = Charset::of(bytes);
    let mut budget = Budget::default();
    let mut interchanges = Vec::new();
    let mut pos = skip_filler(bytes, 0, &Delimiters::EDIFACT);
    if pos >= bytes.len() {
        return Err(EdiError::at(0, "the file is empty"));
    }
    while pos < bytes.len() {
        if interchanges.len() == MAX_INTERCHANGES {
            return Err(EdiError::at(
                pos,
                format!("the file has more than {MAX_INTERCHANGES} interchanges"),
            ));
        }
        let (interchange, end) = read_interchange(bytes, pos, charset, &mut budget)?;
        interchanges.push(interchange);
        pos = skip_filler(bytes, end, &Delimiters::EDIFACT);
    }
    Ok(interchanges)
}

/// The separators a UNA at `pos` states, if there is one.
fn read_una(bytes: &[u8], pos: usize) -> Result<Option<Delimiters>, EdiError> {
    if bytes.get(pos..pos + 3) != Some(b"UNA") {
        return Ok(None);
    }
    let Some(una) = bytes.get(pos + 3..pos + 9) else {
        return Err(EdiError::at(pos, "the UNA segment is cut off"));
    };
    let delimiters = Delimiters {
        component: una[0],
        element: una[1],
        decimal: una[2],
        release: (una[3] != b' ').then_some(una[3]),
        repetition: (una[4] != b' ').then_some(una[4]),
        terminator: una[5],
    };
    if !matches!(delimiters.decimal, b'.' | b',') {
        return Err(EdiError::at(
            pos,
            "the UNA segment states a decimal mark that is neither a point nor a comma",
        ));
    }
    delimiters.check(pos)?;
    Ok(Some(delimiters))
}

fn read_interchange(
    bytes: &[u8],
    pos: usize,
    charset: Charset,
    budget: &mut Budget,
) -> Result<(Interchange, usize), EdiError> {
    let una = read_una(bytes, pos)?;
    let mut delimiters = una.unwrap_or(Delimiters::EDIFACT);
    let start = if una.is_some() { pos + 9 } else { pos };
    let mut tokens = Tokenizer::new(bytes, start, delimiters, charset);
    let unb = match tokens.next(budget)? {
        Some(segment) if segment.tag == "UNB" => segment,
        _ => {
            return Err(EdiError::at(
                start,
                "an interchange must begin with a UNB segment, after its UNA if it has one",
            ));
        }
    };
    let syntax_version = unb.component(1, 2).to_owned();
    if una.is_none() && syntax_version.as_str() >= "4" {
        // Version 4 has a repetition separator even without a UNA. The
        // UNB has no repeated element, so it was read rightly without.
        delimiters.repetition = Some(b'*');
        tokens = Tokenizer::new(bytes, tokens.position(), delimiters, charset);
    }
    let mut interchange = Interchange {
        una: una.is_some(),
        delimiters,
        syntax_id: unb.component(1, 1).to_owned(),
        syntax_version,
        sender: unb.component(2, 1).to_owned(),
        sender_qualifier: unb.component(2, 2).to_owned(),
        receiver: unb.component(3, 1).to_owned(),
        receiver_qualifier: unb.component(3, 2).to_owned(),
        date: unb.component(4, 1).to_owned(),
        time: unb.component(4, 2).to_owned(),
        control: unb.get(5).to_owned(),
        test: unb.get(11) == "1",
        messages: Vec::new(),
        errors: Vec::new(),
    };
    let mut message: Option<Message> = None;
    loop {
        let at = tokens.position();
        let Some(segment) = tokens.next(budget)? else {
            return Err(EdiError::at(
                at,
                "the file ends before the interchange trailer (UNZ): it was cut off",
            ));
        };
        match (segment.tag.as_str(), message.as_mut()) {
            ("UNB" | "UNA", _) => {
                return Err(EdiError::at(
                    segment.offset,
                    "an interchange begins before the one before it ended with a UNZ",
                ));
            }
            ("UNG" | "UNE", _) => {
                return Err(EdiError::at(
                    segment.offset,
                    "the interchange uses functional groups (UNG), which are not read",
                ));
            }
            ("UNH", None) => {
                message = Some(Message {
                    reference: segment.get(1).to_owned(),
                    kind: segment.component(2, 1).to_owned(),
                    version: segment.component(2, 2).to_owned(),
                    release: segment.component(2, 3).to_owned(),
                    agency: segment.component(2, 4).to_owned(),
                    association: segment.component(2, 5).to_owned(),
                    segments: vec![segment],
                    errors: Vec::new(),
                });
            }
            ("UNT", Some(_)) => {
                let Some(mut open) = message.take() else {
                    continue;
                };
                let stated = segment.get(1).parse::<usize>().ok();
                let matches = segment.get(2) == open.reference;
                open.segments.push(segment);
                if stated != Some(open.segments.len()) {
                    open.errors.push(COUNT_DOES_NOT_MATCH);
                }
                if !matches {
                    open.errors.push(REFERENCES_DO_NOT_MATCH);
                }
                interchange.messages.push(open);
            }
            ("UNZ", None) => {
                if segment.get(1).parse::<usize>().ok() != Some(interchange.messages.len()) {
                    interchange.errors.push(COUNT_DOES_NOT_MATCH);
                }
                if segment.get(2) != interchange.control {
                    interchange.errors.push(REFERENCES_DO_NOT_MATCH);
                }
                return Ok((interchange, tokens.position()));
            }
            ("UNH" | "UNZ", Some(_)) => {
                return Err(EdiError::at(
                    segment.offset,
                    "a message has no UNT segment to end it",
                ));
            }
            (_, Some(open)) => open.segments.push(segment),
            (tag, None) => {
                return Err(EdiError::at(
                    segment.offset,
                    format!("segment {tag} is outside a message"),
                ));
            }
        }
    }
}

/// A number as EDIFACT writes it: digits, at most one decimal mark (the
/// one the interchange states), an optional leading minus. Nothing else
/// is a number: no exponent, no thousands separator, no plus sign.
pub fn number(text: &str, delimiters: &Delimiters) -> Option<Decimal> {
    let mark = delimiters.decimal;
    let digits = text.strip_prefix('-').unwrap_or(text);
    let well_formed = text.len() <= 18
        && digits.bytes().any(|b| b.is_ascii_digit())
        && digits.bytes().all(|b| b.is_ascii_digit() || b == mark)
        && digits.bytes().filter(|b| *b == mark).count() <= 1;
    if !well_formed {
        return None;
    }
    let normal: String = text
        .chars()
        .map(|c| if c == mark as char { '.' } else { c })
        .collect();
    Decimal::from_str(&normal).ok()
}
