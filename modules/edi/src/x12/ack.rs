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

/// Which acknowledgement a partner expects: the 997, or from version
/// 005010 the 999, which names the version it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckForm {
    Ack997,
    /// The 999 names the version of the group it answers (GS08) in AK1,
    /// and the implementation convention of each set (ST03, else GS08) in
    /// AK2, in the order of the sets.
    Ack999 {
        version: String,
        conventions: Vec<String>,
    },
}

/// At most this many segment errors are reported per set, element errors
/// per segment, and segment errors per acknowledgement: a set's status
/// still says it is rejected, and an acknowledgement stays within what its
/// reader takes.
pub const MAX_SEGMENT_ERRORS: usize = 100;
pub const MAX_ELEMENT_ERRORS: usize = 10;
pub const MAX_ERRORS: usize = 50_000;

/// A value echoed from what a partner sent, as it can be written with
/// separators `d`: separators and control characters left out.
fn clean(text: &str, d: &Delimiters, max: usize) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() && !u8::try_from(*ch).is_ok_and(|b| d.is_separator(b)))
        .take(max)
        .collect()
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
                let mut segments = verdict.segments;
                segments.truncate(MAX_SEGMENT_ERRORS);
                for segment in &mut segments {
                    segment.elements.truncate(MAX_ELEMENT_ERRORS);
                }
                SetAck {
                    id: set.id.clone(),
                    control: set.control.clone(),
                    status: if codes.is_empty() {
                        Status::Accepted
                    } else {
                        Status::Rejected
                    },
                    codes,
                    segments,
                }
            })
            .collect();
        let mut sets = sets;
        let mut budget = MAX_ERRORS;
        for set in &mut sets {
            let take = set.segments.len().min(budget);
            set.segments.truncate(take);
            budget -= take;
        }
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
        self.body_as(d, &AckForm::Ack997)
    }

    /// The acknowledgement as it will be written with separators `d`:
    /// what it echoes of the partner's file without those separators.
    /// What a read-back is compared with.
    pub fn cleaned(&self, d: &Delimiters) -> Self {
        let mut out = self.clone();
        out.functional_id = clean(&out.functional_id, d, 2);
        out.group_control = clean(&out.group_control, d, 9);
        for set in &mut out.sets {
            set.id = clean(&set.id, d, 3);
            set.control = clean(&set.control, d, 9);
            for segment in &mut set.segments {
                segment.tag = clean(&segment.tag, d, 3);
            }
        }
        out
    }

    /// The body in either form. The 999 says the same as the 997 with
    /// other tags for the set's errors (IK3, IK4, IK5), and names the
    /// version of the group it answers in AK1 and AK2. The codes this
    /// crate raises mean the same in both.
    pub fn body_as(&self, d: &Delimiters, form: &AckForm) -> Vec<Segment> {
        let (ak3, ak4, ak5) = match form {
            AckForm::Ack997 => ("AK3", "AK4", "AK5"),
            AckForm::Ack999 { .. } => ("IK3", "IK4", "IK5"),
        };
        let (version, conventions) = match form {
            AckForm::Ack997 => (None, &[][..]),
            AckForm::Ack999 {
                version,
                conventions,
            } => (Some(clean(version, d, 12)), conventions.as_slice()),
        };
        let codes =
            |codes: &[u8]| -> Vec<Element> { codes.iter().map(|c| el(c.to_string())).collect() };
        let mut ak1 = vec![
            clean(&self.functional_id, d, 2),
            clean(&self.group_control, d, 9),
        ];
        ak1.extend(version.clone());
        let mut out = vec![Segment::new("AK1", &ak1)];
        for (n, set) in self.sets.iter().enumerate() {
            let mut ak2 = vec![clean(&set.id, d, 3), clean(&set.control, d, 9)];
            if let Some(version) = &version {
                ak2.push(
                    conventions
                        .get(n)
                        .map(|c| clean(c, d, 35))
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| version.clone()),
                );
            }
            out.push(Segment::new("AK2", &ak2));
            for segment in &set.segments {
                out.push(Segment::new(
                    ak3,
                    &[
                        clean(&segment.tag, d, 3),
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
                        ak4,
                        vec![
                            position,
                            el(""),
                            el(element.code.to_string()),
                            el(value.trim()),
                        ],
                    ));
                }
            }
            let mut ak5_elements = vec![el(set.status.code())];
            ak5_elements.extend(codes(&set.codes));
            out.push(segment_of(ak5, ak5_elements));
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

    /// Reads a 997 or 999 a partner sent: which group it answers, and
    /// what became of each set in it. A 999's context segments (CTX) are
    /// read past: they repeat what the error names.
    pub fn read(set: &TransactionSet) -> Result<Self, EdiError> {
        let nine = match set.id.as_str() {
            "997" => false,
            "999" => true,
            _ => return Err(EdiError::at(0, "the transaction set is not a 997 or a 999")),
        };
        let (ak3, ak4, ak5) = if nine {
            ("IK3", "IK4", "IK5")
        } else {
            ("AK3", "AK4", "AK5")
        };
        let number = |s: &Segment, n: usize| s.get(n).parse::<usize>().ok();
        let code_list = |s: &Segment, from: usize| -> Vec<u8> {
            (from..from + 5)
                .filter_map(|n| s.get(n).parse::<u8>().ok())
                .collect()
        };
        let mut ack: Option<FunctionalAck> = None;
        let mut closed = false;
        for segment in &set.segments {
            let bad = |what: &str| {
                EdiError::at(
                    segment.offset,
                    format!("the {} {what}", if nine { "999" } else { "997" }),
                )
            };
            let tag = segment.tag.as_str();
            let tag = if tag == ak3 {
                "AK3"
            } else if tag == ak4 {
                "AK4"
            } else if tag == ak5 {
                "AK5"
            } else if (tag == "AK3" || tag == "AK4" || tag == "AK5") && nine {
                "unexpected"
            } else {
                tag
            };
            match (tag, ack.as_mut()) {
                ("ST" | "SE", _) => {}
                ("CTX", Some(_)) if nine && !closed => {}
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
            _ => Err(EdiError::at(0, "the acknowledgement has no AK1 or no AK9")),
        }
    }
}

/// What an acknowledgement says about a set, in sentences: for a person
/// reading a rejection, sent or received. Only the codes this crate raises
/// have words; any other is given by its number.
pub fn describe(set: &SetAck) -> Vec<String> {
    let mut out = Vec::new();
    for code in &set.codes {
        let what = match code {
            1 => "is not a kind of transaction set the receiver reads".to_owned(),
            2 => "has no trailer (SE)".to_owned(),
            3 => "has a control number in its trailer that does not match its header".to_owned(),
            4 => "has a segment count in its trailer that is not the number of its segments"
                .to_owned(),
            5 => "has segments in error".to_owned(),
            other => format!("has error code {other}"),
        };
        out.push(format!("Set {} {} {what}.", set.id, set.control));
    }
    for segment in &set.segments {
        let what = match segment.code {
            Some(SEGMENT_UNEXPECTED) => "is where it is not allowed".to_owned(),
            Some(SEGMENT_MISSING) => "is missing".to_owned(),
            Some(SEGMENT_HAS_ELEMENT_ERRORS) | None => "has errors".to_owned(),
            Some(other) => format!("has error code {other}"),
        };
        out.push(format!(
            "Segment {} at position {} {what}.",
            segment.tag, segment.position
        ));
        for element in &segment.elements {
            let what = match element.code {
                ELEMENT_MISSING => "is missing",
                2 => "is missing although the element paired with it is there",
                ELEMENT_INVALID_CHARACTER => "holds a character that does not belong there",
                ELEMENT_INVALID_DATE => "is not a date",
                _ => "is in error",
            };
            let value = if element.value.is_empty() {
                String::new()
            } else {
                format!(" ('{}')", element.value)
            };
            out.push(format!(
                "Element {} of {} {what}{value} (code {}).",
                element.position, segment.tag, element.code
            ));
        }
    }
    out
}
