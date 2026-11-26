//! ANSI X12 envelopes: the interchange (ISA/IEA), its functional groups
//! (GS/GE) and their transaction sets (ST/SE).
//!
//! The ISA is the only fixed-width segment of X12: 106 bytes, and it is
//! where a file says which separators it uses. The element separator is
//! its fourth byte, the component separator its 105th (ISA16) and the
//! segment terminator its 106th. From version 00402 on, ISA11 is the
//! repetition separator; before that it is a letter, `U`. Nothing here
//! assumes `*`, `>` or `~`.
//!
//! Two kinds of trouble are kept apart. A file that cannot be followed at
//! all (no ISA, a segment outside its envelope, cut off before the IEA) is
//! an [`EdiError`]: there is nobody to acknowledge to. A file that can be
//! followed but whose counts or control numbers do not agree is read, and
//! each envelope carries its errors as the codes a 997 or TA1 reports
//! them with, so that the partner is told instead of ignored.

use chrono::NaiveDateTime;

use crate::segment::{
    Budget, Charset, Delimiters, EdiError, Element, Limits, Segment, Tokenizer, skip_filler,
    write_segment,
};

pub mod ack;
pub mod asn856;
pub mod change860;
pub mod fields;
pub mod inventory846;
pub mod invoice810;
pub mod po850;
pub mod poack855;
pub mod remit820;
pub mod ta1;

/// Interchanges in one file. A mailbox pickup may hold several.
pub const MAX_INTERCHANGES: usize = 100;

const ISA_LEN: usize = 106;
/// Where the element separator stands in an ISA, after each of its
/// sixteen fixed-width elements' predecessors.
const ISA_SEPARATORS: [usize; 16] = [
    3, 6, 17, 20, 31, 34, 50, 53, 69, 76, 81, 83, 89, 99, 101, 103,
];

/// The interchange header. Identifiers are as sent, without their padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Isa {
    pub sender_qualifier: String,
    pub sender_id: String,
    pub receiver_qualifier: String,
    pub receiver_id: String,
    /// `YYMMDD`, as sent.
    pub date: String,
    /// `HHMM`, as sent.
    pub time: String,
    /// ISA12, such as `00401` or `00501`.
    pub version: String,
    /// ISA13, nine digits, as sent.
    pub control: String,
    /// ISA14: the sender asks for a TA1.
    pub ack_requested: bool,
    /// ISA15: `P` production, `T` test, `I` information.
    pub usage: char,
    pub delimiters: Delimiters,
}

/// An error of an interchange envelope, by its TA105 note code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterchangeError {
    /// 001: ISA13 and IEA02 do not match.
    ControlNumberMismatch,
    /// 021: IEA01 is not the number of groups.
    GroupCountMismatch,
}

/// An error of a functional group, by its AK905 code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupError {
    /// 3: the group has no GE.
    TrailerMissing,
    /// 4: GS06 and GE02 do not agree.
    ControlNumberMismatch,
    /// 5: GE01 is not the number of transaction sets.
    SetCountMismatch,
}

impl GroupError {
    pub fn code(self) -> u8 {
        match self {
            GroupError::TrailerMissing => 3,
            GroupError::ControlNumberMismatch => 4,
            GroupError::SetCountMismatch => 5,
        }
    }
}

/// An error of a transaction set's envelope, by its AK502 code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetError {
    /// 2: the set has no SE.
    TrailerMissing,
    /// 3: ST02 and SE02 do not match.
    ControlNumberMismatch,
    /// 4: SE01 is not the number of segments.
    SegmentCountMismatch,
}

impl SetError {
    pub fn code(self) -> u8 {
        match self {
            SetError::TrailerMissing => 2,
            SetError::ControlNumberMismatch => 3,
            SetError::SegmentCountMismatch => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionSet {
    /// ST01, such as `850`.
    pub id: String,
    /// ST02, as sent.
    pub control: String,
    /// Every segment from the ST to the SE, both included, so that a
    /// segment's place in this list plus one is the position a 997 names.
    pub segments: Vec<Segment>,
    pub errors: Vec<SetError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// GS01, such as `PO` for 850s or `FA` for 997s.
    pub functional_id: String,
    pub sender: String,
    pub receiver: String,
    /// GS06, as sent.
    pub control: String,
    /// GS08, such as `004010` or `005010`, sometimes with an industry
    /// suffix (`004010VICS`).
    pub version: String,
    /// GE01 as the sender stated it, when the group has a GE.
    pub sets_stated: Option<String>,
    pub sets: Vec<TransactionSet>,
    pub errors: Vec<GroupError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interchange {
    pub isa: Isa,
    /// TA1 segments, which stand between the ISA and the first group.
    pub ta1: Vec<Segment>,
    pub groups: Vec<Group>,
    pub errors: Vec<InterchangeError>,
}

/// Reads every interchange of a file.
pub fn read(bytes: &[u8]) -> Result<Vec<Interchange>, EdiError> {
    read_with(bytes, Limits::default())
}

/// Reads every interchange of a file within other limits.
pub fn read_with(bytes: &[u8], limits: Limits) -> Result<Vec<Interchange>, EdiError> {
    if bytes.len() > limits.bytes {
        return Err(EdiError::at(
            0,
            format!("the file is larger than {} MiB", limits.bytes / 1024 / 1024),
        ));
    }
    let charset = Charset::of(bytes);
    let mut budget = Budget::of(limits);
    let mut interchanges = Vec::new();
    let mut pos = skip_filler(bytes, 0, &Delimiters::X12);
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
        let isa = read_isa(bytes, pos, charset)?;
        let mut tokens = Tokenizer::new(bytes, pos + ISA_LEN, isa.delimiters, charset);
        interchanges.push(read_interchange(isa, &mut tokens, &mut budget)?);
        pos = skip_filler(bytes, tokens.position(), &Delimiters::X12);
    }
    Ok(interchanges)
}

fn read_isa(bytes: &[u8], pos: usize, charset: Charset) -> Result<Isa, EdiError> {
    let Some(isa) = bytes.get(pos..pos + ISA_LEN) else {
        return Err(EdiError::at(
            pos,
            "an interchange must begin with an ISA segment of 106 characters",
        ));
    };
    if &isa[..3] != b"ISA" {
        return Err(EdiError::at(
            pos,
            "an interchange must begin with an ISA segment",
        ));
    }
    let element = isa[3];
    if ISA_SEPARATORS.iter().any(|at| isa[*at] != element) {
        return Err(EdiError::at(
            pos,
            "the ISA segment does not have its sixteen elements at their fixed widths",
        ));
    }
    let field = |n: usize| -> String {
        let from = ISA_SEPARATORS[n - 1] + 1;
        let to = ISA_SEPARATORS.get(n).copied().unwrap_or(ISA_LEN - 1);
        let raw: String = match charset {
            Charset::Utf8 => String::from_utf8_lossy(&isa[from..to]).into_owned(),
            Charset::Latin1 => isa[from..to].iter().map(|b| *b as char).collect(),
        };
        raw.trim_end().to_owned()
    };
    let version = field(12);
    let isa11 = isa[82];
    let repetition =
        (version.as_str() >= "00402" && !isa11.is_ascii_alphanumeric() && isa11 != b' ')
            .then_some(isa11);
    let delimiters = Delimiters {
        element,
        component: isa[104],
        repetition,
        terminator: isa[105],
        release: None,
        decimal: b'.',
    };
    delimiters.check(pos)?;
    let control = field(13);
    if control.len() != 9 || !control.bytes().all(|b| b.is_ascii_digit()) {
        return Err(EdiError::at(
            pos,
            "the interchange control number (ISA13) is not nine digits",
        ));
    }
    Ok(Isa {
        sender_qualifier: field(5),
        sender_id: field(6),
        receiver_qualifier: field(7),
        receiver_id: field(8),
        date: field(9),
        time: field(10),
        version,
        control,
        ack_requested: field(14) == "1",
        usage: field(15).chars().next().unwrap_or(' '),
        delimiters,
    })
}

fn read_interchange(
    isa: Isa,
    tokens: &mut Tokenizer<'_>,
    budget: &mut Budget,
) -> Result<Interchange, EdiError> {
    let mut interchange = Interchange {
        isa,
        ta1: Vec::new(),
        groups: Vec::new(),
        errors: Vec::new(),
    };
    let mut group: Option<Group> = None;
    let mut set: Option<TransactionSet> = None;

    // A set or group that the next envelope segment cuts short is kept,
    // with the error a 997 reports for it.
    fn close_set(group: &mut Option<Group>, set: &mut Option<TransactionSet>) {
        if let (Some(group), Some(mut open)) = (group.as_mut(), set.take()) {
            open.errors.push(SetError::TrailerMissing);
            group.sets.push(open);
        }
    }
    fn close_group(interchange: &mut Interchange, group: &mut Option<Group>) {
        if let Some(mut open) = group.take() {
            open.errors.push(GroupError::TrailerMissing);
            interchange.groups.push(open);
        }
    }

    loop {
        let at = tokens.position();
        let Some(segment) = tokens.next(budget)? else {
            return Err(EdiError::at(
                at,
                "the file ends before the interchange trailer (IEA): it was cut off",
            ));
        };
        match segment.tag.as_str() {
            "ISA" => {
                return Err(EdiError::at(
                    segment.offset,
                    "an interchange begins before the one before it ended with an IEA",
                ));
            }
            "IEA" => {
                close_set(&mut group, &mut set);
                close_group(&mut interchange, &mut group);
                if segment.get(1).parse::<usize>().ok() != Some(interchange.groups.len()) {
                    interchange
                        .errors
                        .push(InterchangeError::GroupCountMismatch);
                }
                if segment.get(2) != interchange.isa.control {
                    interchange
                        .errors
                        .push(InterchangeError::ControlNumberMismatch);
                }
                return Ok(interchange);
            }
            "TA1" if group.is_none() => interchange.ta1.push(segment),
            "GS" => {
                close_set(&mut group, &mut set);
                close_group(&mut interchange, &mut group);
                group = Some(Group {
                    functional_id: segment.get(1).to_owned(),
                    sender: segment.get(2).to_owned(),
                    receiver: segment.get(3).to_owned(),
                    control: segment.get(6).to_owned(),
                    version: segment.get(8).to_owned(),
                    sets_stated: None,
                    sets: Vec::new(),
                    errors: Vec::new(),
                });
            }
            "GE" => {
                close_set(&mut group, &mut set);
                let Some(mut open) = group.take() else {
                    return Err(EdiError::at(
                        segment.offset,
                        "a GE segment has no functional group to end",
                    ));
                };
                open.sets_stated = Some(segment.get(1).to_owned());
                if segment.get(1).parse::<usize>().ok() != Some(open.sets.len()) {
                    open.errors.push(GroupError::SetCountMismatch);
                }
                if segment.get(2) != open.control {
                    open.errors.push(GroupError::ControlNumberMismatch);
                }
                interchange.groups.push(open);
            }
            "ST" => {
                if group.is_none() {
                    return Err(EdiError::at(
                        segment.offset,
                        "a transaction set begins outside a functional group",
                    ));
                }
                close_set(&mut group, &mut set);
                set = Some(TransactionSet {
                    id: segment.get(1).to_owned(),
                    control: segment.get(2).to_owned(),
                    segments: vec![segment],
                    errors: Vec::new(),
                });
            }
            "SE" => {
                let (Some(group), Some(mut open)) = (group.as_mut(), set.take()) else {
                    return Err(EdiError::at(
                        segment.offset,
                        "an SE segment has no transaction set to end",
                    ));
                };
                let stated = segment.get(1).parse::<usize>().ok();
                let control_matches = segment.get(2) == open.control;
                open.segments.push(segment);
                if stated != Some(open.segments.len()) {
                    open.errors.push(SetError::SegmentCountMismatch);
                }
                if !control_matches {
                    open.errors.push(SetError::ControlNumberMismatch);
                }
                group.sets.push(open);
            }
            _ => match set.as_mut() {
                Some(open) => open.segments.push(segment),
                None => {
                    return Err(EdiError::at(
                        segment.offset,
                        format!("segment {} is outside a transaction set", segment.tag),
                    ));
                }
            },
        }
    }
}

/// What an interchange to send is addressed with. The control numbers
/// come from the caller: they are a sequence per partner, kept in the
/// database, and this crate keeps no state.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub sender_qualifier: String,
    pub sender_id: String,
    pub receiver_qualifier: String,
    pub receiver_id: String,
    /// GS02 and GS03. Often the ISA identifiers again; a partner's guide
    /// says.
    pub group_sender: String,
    pub group_receiver: String,
    pub at: NaiveDateTime,
    /// ISA12, such as `00401`.
    pub version: String,
    /// GS08, such as `004010`.
    pub group_version: String,
    /// GS01, such as `FA`.
    pub functional_id: String,
    /// At most 999,999,999; written as nine digits in the ISA.
    pub interchange_control: u32,
    pub group_control: u32,
    pub usage: char,
    pub ack_requested: bool,
    pub delimiters: Delimiters,
}

impl Envelope {
    /// The envelope of an answer to an interchange: its sender and
    /// receiver swapped, its version, usage and separators kept.
    pub fn reply_to(
        interchange: &Interchange,
        group: &Group,
        functional_id: &str,
        at: NaiveDateTime,
        interchange_control: u32,
        group_control: u32,
    ) -> Self {
        let isa = &interchange.isa;
        Envelope {
            sender_qualifier: isa.receiver_qualifier.clone(),
            sender_id: isa.receiver_id.clone(),
            receiver_qualifier: isa.sender_qualifier.clone(),
            receiver_id: isa.sender_id.clone(),
            group_sender: group.receiver.clone(),
            group_receiver: group.sender.clone(),
            at,
            version: isa.version.clone(),
            group_version: group.version.clone(),
            functional_id: functional_id.to_owned(),
            interchange_control,
            group_control,
            usage: isa.usage,
            ack_requested: false,
            delimiters: isa.delimiters,
        }
    }
}

/// Writes one interchange of one functional group. Each set is its body
/// segments, without ST and SE: those are written here, numbered from
/// `0001`, with the segment count worked out.
pub fn write(
    envelope: &Envelope,
    set_id: &str,
    bodies: &[Vec<Segment>],
) -> Result<String, EdiError> {
    let d = envelope.delimiters;
    let mut out = isa(envelope)?;
    if envelope.group_control > 999_999_999 {
        return Err(EdiError::at(
            0,
            "a control number has more than nine digits",
        ));
    }
    write_segment(
        &mut out,
        &Segment::new(
            "GS",
            &[
                envelope.functional_id.as_str(),
                &envelope.group_sender,
                &envelope.group_receiver,
                &envelope.at.format("%Y%m%d").to_string(),
                &envelope.at.format("%H%M").to_string(),
                &envelope.group_control.to_string(),
                "X",
                &envelope.group_version,
            ],
        ),
        &d,
    )?;
    for (i, body) in bodies.iter().enumerate() {
        let control = format!("{:04}", i + 1);
        write_segment(&mut out, &Segment::new("ST", &[set_id, &control]), &d)?;
        for segment in body {
            write_segment(&mut out, segment, &d)?;
        }
        let count = (body.len() + 2).to_string();
        write_segment(&mut out, &Segment::new("SE", &[&count, &control]), &d)?;
    }
    write_segment(
        &mut out,
        &Segment::new(
            "GE",
            &[bodies.len().to_string(), envelope.group_control.to_string()],
        ),
        &d,
    )?;
    write_segment(
        &mut out,
        &Segment::new(
            "IEA",
            &[
                "1".to_owned(),
                format!("{:09}", envelope.interchange_control),
            ],
        ),
        &d,
    )?;
    Ok(out)
}

/// Writes an interchange that holds TA1 segments and no group: the
/// acknowledgement of an interchange. The group fields of the envelope
/// are not used.
pub fn write_ta1s(envelope: &Envelope, ta1s: &[Segment]) -> Result<String, EdiError> {
    let d = envelope.delimiters;
    let mut out = isa(envelope)?;
    for segment in ta1s {
        write_segment(&mut out, segment, &d)?;
    }
    write_segment(
        &mut out,
        &Segment::new(
            "IEA",
            &[
                "0".to_owned(),
                format!("{:09}", envelope.interchange_control),
            ],
        ),
        &d,
    )?;
    Ok(out)
}

/// The ISA of an envelope, 106 characters, terminator included.
fn isa(envelope: &Envelope) -> Result<String, EdiError> {
    let d = envelope.delimiters;
    d.check(0)?;
    if envelope.interchange_control > 999_999_999 {
        return Err(EdiError::at(
            0,
            "a control number has more than nine digits",
        ));
    }
    let fixed = |text: &str, width: usize, name: &str| -> Result<String, EdiError> {
        let plain = text
            .bytes()
            .all(|b| (0x20..0x7F).contains(&b) && !d.is_separator(b));
        if text.len() > width || !plain {
            return Err(EdiError::at(
                0,
                format!(
                    "{name} does not fit the ISA: at most {width} characters, none of them \
                     a separator"
                ),
            ));
        }
        Ok(format!("{text:<width$}"))
    };
    let mut out = String::new();
    let e = d.element as char;
    let isa = [
        "00".to_owned(),
        fixed("", 10, "ISA02")?,
        "00".to_owned(),
        fixed("", 10, "ISA04")?,
        fixed(&envelope.sender_qualifier, 2, "the sender qualifier")?,
        fixed(&envelope.sender_id, 15, "the sender identifier")?,
        fixed(&envelope.receiver_qualifier, 2, "the receiver qualifier")?,
        fixed(&envelope.receiver_id, 15, "the receiver identifier")?,
        envelope.at.format("%y%m%d").to_string(),
        envelope.at.format("%H%M").to_string(),
        match d.repetition {
            Some(r) if envelope.version.as_str() >= "00402" => (r as char).to_string(),
            _ => "U".to_owned(),
        },
        fixed(&envelope.version, 5, "the version")?,
        format!("{:09}", envelope.interchange_control),
        if envelope.ack_requested { "1" } else { "0" }.to_owned(),
        fixed(&envelope.usage.to_string(), 1, "the usage indicator")?,
        (d.component as char).to_string(),
    ];
    out.push_str("ISA");
    for field in &isa {
        out.push(e);
        out.push_str(field);
    }
    out.push(d.terminator as char);
    if out.len() != ISA_LEN {
        return Err(EdiError::at(0, "the ISA did not come to 106 characters"));
    }
    Ok(out)
}

/// Writes segments back as they were read: what a message log shows, and
/// what the round-trip test compares.
pub fn write_segments(segments: &[Segment], d: &Delimiters) -> Result<String, EdiError> {
    let mut out = String::new();
    for segment in segments {
        write_segment(&mut out, segment, d)?;
    }
    Ok(out)
}

/// A simple element, for the writers of this module.
pub(crate) fn el(value: impl Into<String>) -> Element {
    Element::simple(value)
}
