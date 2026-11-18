//! The 997 functional acknowledgement: built for what was received, and
//! read when a partner answers what was sent.
//!
//! A 997 says whether each transaction set of a functional group could be
//! read as X12. It says nothing about the order itself: a set that is
//! accepted here can still name an item nobody knows. That answer is a
//! business document (an 855), not this one.
//!
//! The codes are the ones of the standard's segments AK3 to AK9; the
//! constants below name those this crate raises.

use crate::segment::{Delimiters, EdiError, Element, Segment};
use crate::x12::{Group, TransactionSet, el};

/// AK304: a segment is where the transaction set does not allow it.
pub const SEGMENT_UNEXPECTED: u8 = 2;
/// AK304: a mandatory segment is missing.
pub const SEGMENT_MISSING: u8 = 3;
/// AK304: the segment has data element errors (listed in AK4).
pub const SEGMENT_HAS_ELEMENT_ERRORS: u8 = 8;
/// AK403: a mandatory data element is missing.
pub const ELEMENT_MISSING: u8 = 1;
/// AK403: an invalid character, such as a letter in a number.
pub const ELEMENT_INVALID_CHARACTER: u8 = 6;
/// AK403: not a date.
pub const ELEMENT_INVALID_DATE: u8 = 8;
/// AK502: the transaction set is not one this receiver reads.
pub const SET_NOT_SUPPORTED: u8 = 1;
/// AK502: one or more segments are in error.
pub const SET_HAS_SEGMENT_ERRORS: u8 = 5;

/// The longest copy of bad data an AK404 carries.
const BAD_VALUE_LEN: usize = 99;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementError {
    /// The element's place in its segment, counted from 1.
    pub position: usize,
    /// The component's place in a composite element, if the error is
    /// about one.
    pub component: Option<usize>,
    /// AK403.
    pub code: u8,
    /// The value that was wrong, for AK404. Written without separators
    /// and cut to 99 characters.
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentError {
    pub tag: String,
    /// The segment's place in its transaction set, the ST being 1. For a
    /// missing segment, where it should have been.
    pub position: usize,
    /// AK304, when the segment itself is wrong.
    pub code: Option<u8>,
    pub elements: Vec<ElementError>,
}

impl SegmentError {
    pub fn segment(tag: &str, position: usize, code: u8) -> Self {
        SegmentError {
            tag: tag.to_owned(),
            position,
            code: Some(code),
            elements: Vec::new(),
        }
    }

    pub fn element(segment: &Segment, position: usize, n: usize, code: u8) -> Self {
        SegmentError {
            tag: segment.tag.clone(),
            position,
            code: Some(SEGMENT_HAS_ELEMENT_ERRORS),
            elements: vec![ElementError {
                position: n,
                component: None,
                code,
                value: segment.get(n).to_owned(),
            }],
        }
    }
}

/// What reading one transaction set found wrong with its syntax.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetVerdict {
    /// AK502 codes beyond those of the envelope.
    pub codes: Vec<u8>,
    pub segments: Vec<SegmentError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Accepted,
    /// Accepted, but errors were noted.
    AcceptedWithErrors,
    /// Of a group only: some of its sets were accepted.
    PartiallyAccepted,
    Rejected,
}

impl Status {
    pub fn code(self) -> &'static str {
        match self {
            Status::Accepted => "A",
            Status::AcceptedWithErrors => "E",
            Status::PartiallyAccepted => "P",
            Status::Rejected => "R",
        }
    }

    fn of(code: &str) -> Option<Self> {
        Some(match code {
            "A" => Status::Accepted,
            "E" => Status::AcceptedWithErrors,
            "P" => Status::PartiallyAccepted,
            // M and W are rejections for security reasons, X one whose
            // content could not be analysed.
            "R" | "M" | "W" | "X" => Status::Rejected,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAck {
    pub id: String,
    pub control: String,
    pub status: Status,
    /// AK502 to AK506.
    pub codes: Vec<u8>,
    pub segments: Vec<SegmentError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionalAck {
    /// AK101: the functional identifier of the group answered (`PO`).
    pub functional_id: String,
    /// AK102: that group's control number.
    pub group_control: String,
    pub sets: Vec<SetAck>,
    pub status: Status,
    /// AK902: how many sets the group said it has.
    pub sets_stated: String,
    /// AK903.
    pub received: usize,
    /// AK904.
    pub accepted: usize,
    /// AK905 to AK909.
    pub codes: Vec<u8>,
}

fn segment_of(tag: &str, elements: Vec<Element>) -> Segment {
    Segment {
        tag: tag.to_owned(),
        elements,
        offset: 0,
    }
}

impl FunctionalAck {
    /// The acknowledgement of a group. `check` reads each transaction set
    /// and says what is wrong with its syntax; the errors of the
    /// envelopes are added here. A set with any error is rejected: whether
    /// a partner's set with errors is still worked on is not something a
    /// parser decides.
    pub fn of(group: &Group, mut check: impl FnMut(&TransactionSet) -> SetVerdict) -> Self {
        let sets: Vec<SetAck> = group
            .sets
            .iter()
            .map(|set| {
                let verdict = check(set);
                let mut codes: Vec<u8> = set.errors.iter().map(|e| e.code()).collect();
                codes.extend(verdict.codes);
                if !verdict.segments.is_empty() {
                    codes.push(SET_HAS_SEGMENT_ERRORS);
                }
                codes.dedup();
                codes.truncate(5);
                SetAck {
                    id: set.id.clone(),
                    control: set.control.clone(),
                    status: if codes.is_empty() {
                        Status::Accepted
                    } else {
                        Status::Rejected
                    },
                    codes,
                    segments: verdict.segments,
                }
            })
            .collect();
        let received = sets.len();
        let accepted = sets.iter().filter(|s| s.status == Status::Accepted).count();
        let codes: Vec<u8> = group.errors.iter().map(|e| e.code()).collect();
        let status = if accepted == 0 {
            Status::Rejected
        } else if accepted < received {
            Status::PartiallyAccepted
        } else if codes.is_empty() {
            Status::Accepted
        } else {
            Status::AcceptedWithErrors
        };
        FunctionalAck {
            functional_id: group.functional_id.clone(),
            group_control: group.control.clone(),
            sets,
            status,
            sets_stated: group
                .sets_stated
                .clone()
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .unwrap_or_else(|| received.to_string()),
            received,
            accepted,
            codes,
        }
    }

    /// The body of the 997, from AK1 to AK9, to hand to `x12::write`. The
    /// separators are those it will be written with: a copy of bad data
    /// must not contain one.
    pub fn body(&self, d: &Delimiters) -> Vec<Segment> {
        let codes =
            |codes: &[u8]| -> Vec<Element> { codes.iter().map(|c| el(c.to_string())).collect() };
        let mut out = vec![Segment::new(
            "AK1",
            &[&self.functional_id, &self.group_control],
        )];
        for set in &self.sets {
            out.push(Segment::new("AK2", &[&set.id, &set.control]));
            for segment in &set.segments {
                out.push(Segment::new(
                    "AK3",
                    &[
                        segment.tag.clone(),
                        segment.position.to_string(),
                        String::new(),
                        segment.code.map(|c| c.to_string()).unwrap_or_default(),
                    ],
                ));
                for element in &segment.elements {
                    let position = match element.component {
                        Some(c) => {
                            Element::composite(&[element.position.to_string(), c.to_string()])
                        }
                        None => el(element.position.to_string()),
                    };
                    let value: String = element
                        .value
                        .chars()
                        .filter(|ch| {
                            !ch.is_control() && !u8::try_from(*ch).is_ok_and(|b| d.is_separator(b))
                        })
                        .take(BAD_VALUE_LEN)
                        .collect();
                    out.push(segment_of(
                        "AK4",
                        vec![
                            position,
                            el(""),
                            el(element.code.to_string()),
                            el(value.trim()),
                        ],
                    ));
                }
            }
            let mut ak5 = vec![el(set.status.code())];
            ak5.extend(codes(&set.codes));
            out.push(segment_of("AK5", ak5));
        }
        let mut ak9 = vec![
            el(self.status.code()),
            el(self.sets_stated.clone()),
            el(self.received.to_string()),
            el(self.accepted.to_string()),
        ];
        ak9.extend(codes(&self.codes));
        out.push(segment_of("AK9", ak9));
        out
    }

    /// Reads a 997 a partner sent: which group it answers, and what
    /// became of each set in it.
    pub fn read(set: &TransactionSet) -> Result<Self, EdiError> {
        if set.id != "997" {
            return Err(EdiError::at(0, "the transaction set is not a 997"));
        }
        let number = |s: &Segment, n: usize| s.get(n).parse::<usize>().ok();
        let code_list = |s: &Segment, from: usize| -> Vec<u8> {
            (from..from + 5)
                .filter_map(|n| s.get(n).parse::<u8>().ok())
                .collect()
        };
        let mut ack: Option<FunctionalAck> = None;
        let mut closed = false;
        for segment in &set.segments {
            let bad = |what: &str| EdiError::at(segment.offset, format!("the 997 {what}"));
            match (segment.tag.as_str(), ack.as_mut()) {
                ("ST" | "SE", _) => {}
                ("AK1", None) => {
                    ack = Some(FunctionalAck {
                        functional_id: segment.get(1).to_owned(),
                        group_control: segment.get(2).to_owned(),
                        sets: Vec::new(),
                        status: Status::Rejected,
                        sets_stated: String::new(),
                        received: 0,
                        accepted: 0,
                        codes: Vec::new(),
                    });
                }
                ("AK2", Some(ack)) if !closed => ack.sets.push(SetAck {
                    id: segment.get(1).to_owned(),
                    control: segment.get(2).to_owned(),
                    // Until its AK5 says otherwise.
                    status: Status::Rejected,
                    codes: Vec::new(),
                    segments: Vec::new(),
                }),
                ("AK3", Some(ack)) if !closed => {
                    let set = ack
                        .sets
                        .last_mut()
                        .ok_or_else(|| bad("has an AK3 before any AK2"))?;
                    set.segments.push(SegmentError {
                        tag: segment.get(1).to_owned(),
                        position: number(segment, 2).unwrap_or(0),
                        code: segment.get(4).parse().ok(),
                        elements: Vec::new(),
                    });
                }
                ("AK4", Some(ack)) if !closed => {
                    let error = ack
                        .sets
                        .last_mut()
                        .and_then(|s| s.segments.last_mut())
                        .ok_or_else(|| bad("has an AK4 before any AK3"))?;
                    error.elements.push(ElementError {
                        position: segment.component(1, 1).parse().unwrap_or(0),
                        component: segment.component(1, 2).parse().ok(),
                        code: segment.get(3).parse().unwrap_or(0),
                        value: segment.get(4).to_owned(),
                    });
                }
                ("AK5", Some(ack)) if !closed => {
                    let set = ack
                        .sets
                        .last_mut()
                        .ok_or_else(|| bad("has an AK5 before any AK2"))?;
                    set.status = Status::of(segment.get(1))
                        .ok_or_else(|| bad("has an AK5 with an unknown status"))?;
                    set.codes = code_list(segment, 2);
                }
                ("AK9", Some(ack)) if !closed => {
                    ack.status = Status::of(segment.get(1))
                        .ok_or_else(|| bad("has an AK9 with an unknown status"))?;
                    ack.sets_stated = segment.get(2).to_owned();
                    ack.received =
                        number(segment, 3).ok_or_else(|| bad("has no count of sets received"))?;
                    ack.accepted =
                        number(segment, 4).ok_or_else(|| bad("has no count of sets accepted"))?;
                    ack.codes = code_list(segment, 5);
                    closed = true;
                }
                _ => {
                    return Err(bad(&format!(
                        "has segment {} where it does not belong",
                        segment.tag
                    )));
                }
            }
        }
        match ack {
            Some(ack) if closed => Ok(ack),
            _ => Err(EdiError::at(0, "the 997 has no AK1 or no AK9")),
        }
    }
}
