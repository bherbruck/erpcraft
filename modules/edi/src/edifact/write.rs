//! Writing an EDIFACT interchange: a UNA always (so the reader never has to
//! guess the separators or the decimal mark), the UNB, each message from
//! its UNH to its UNT with the segments counted, and the UNZ with the
//! messages counted. Separators in data are released; text is checked
//! against the character set the UNB names, and encoded in it.

use chrono::NaiveDateTime;

use crate::segment::{Delimiters, EdiError, Segment, write_segment};

/// The character set of the interchange (UNB 0001), and what may be
/// written in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxId {
    /// Capital letters, digits, a space and a few marks.
    Unoa,
    /// UNOA with small letters.
    Unob,
    /// ISO 8859-1: Western European letters.
    Unoc,
    /// UTF-8: any character.
    Unoy,
}

impl SyntaxId {
    pub fn parse(code: &str) -> Option<SyntaxId> {
        match code {
            "UNOA" => Some(SyntaxId::Unoa),
            "UNOB" => Some(SyntaxId::Unob),
            "UNOC" => Some(SyntaxId::Unoc),
            "UNOY" => Some(SyntaxId::Unoy),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            SyntaxId::Unoa => "UNOA",
            SyntaxId::Unob => "UNOB",
            SyntaxId::Unoc => "UNOC",
            SyntaxId::Unoy => "UNOY",
        }
    }

    /// Whether `c` can be written in this set.
    pub fn allows(self, c: char) -> bool {
        const MARKS: &str = " .,-()/='+:?!\"%&*;<>";
        match self {
            SyntaxId::Unoa => c.is_ascii_uppercase() || c.is_ascii_digit() || MARKS.contains(c),
            SyntaxId::Unob => c.is_ascii_alphanumeric() || MARKS.contains(c),
            SyntaxId::Unoc => (c as u32) <= 0xFF && !c.is_control(),
            SyntaxId::Unoy => !c.is_control(),
        }
    }

    /// The bytes of text written in this set.
    pub fn encode(self, text: &str) -> Vec<u8> {
        match self {
            SyntaxId::Unoc => text.chars().map(|c| c as u32 as u8).collect(),
            _ => text.as_bytes().to_vec(),
        }
    }
}

/// What the envelope of an interchange to send says.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub delimiters: Delimiters,
    pub syntax: SyntaxId,
    /// `3` or `4`.
    pub syntax_version: String,
    pub sender: String,
    pub sender_qualifier: String,
    pub receiver: String,
    pub receiver_qualifier: String,
    pub at: NaiveDateTime,
    /// UNB 0020, the interchange control reference.
    pub control: String,
    pub test: bool,
}

/// One message to write: its type and version as UNH names them, its
/// reference, and its segments from after the UNH to before the UNT.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub kind: String,
    pub version: String,
    pub release: String,
    pub agency: String,
    /// The association's subset code (such as EANCOM's), or empty.
    pub association: String,
    pub reference: String,
    pub body: Vec<Segment>,
}

/// The interchange as bytes in its character set.
pub fn write(envelope: &Envelope, messages: &[Outgoing]) -> Result<Vec<u8>, EdiError> {
    let d = envelope.delimiters;
    d.check(0)?;
    let mut out = String::new();
    out.push_str("UNA");
    for b in [
        d.component,
        d.element,
        d.decimal,
        d.release.unwrap_or(b' '),
        d.repetition.unwrap_or(b' '),
        d.terminator,
    ] {
        out.push(b as char);
    }
    let unb = Segment {
        tag: "UNB".into(),
        elements: vec![
            crate::segment::Element::composite(&[
                envelope.syntax.code(),
                envelope.syntax_version.as_str(),
            ]),
            crate::segment::Element::composite(&[
                envelope.sender.as_str(),
                envelope.sender_qualifier.as_str(),
            ]),
            crate::segment::Element::composite(&[
                envelope.receiver.as_str(),
                envelope.receiver_qualifier.as_str(),
            ]),
            crate::segment::Element::composite(&[
                envelope.at.format("%y%m%d").to_string(),
                envelope.at.format("%H%M").to_string(),
            ]),
            crate::segment::Element::simple(envelope.control.clone()),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            crate::segment::Element::simple(if envelope.test { "1" } else { "" }),
        ],
        offset: 0,
    };
    let mut segments = vec![unb];
    for message in messages {
        let mut head = vec![
            message.kind.as_str(),
            message.version.as_str(),
            message.release.as_str(),
            message.agency.as_str(),
        ];
        if !message.association.is_empty() {
            head.push(message.association.as_str());
        }
        segments.push(Segment {
            tag: "UNH".into(),
            elements: vec![
                crate::segment::Element::simple(message.reference.clone()),
                crate::segment::Element::composite(&head),
            ],
            offset: 0,
        });
        segments.extend(message.body.iter().cloned());
        segments.push(Segment::new(
            "UNT",
            &[
                (message.body.len() + 2).to_string(),
                message.reference.clone(),
            ],
        ));
    }
    segments.push(Segment::new(
        "UNZ",
        &[messages.len().to_string(), envelope.control.clone()],
    ));
    for segment in &segments {
        for element in &segment.elements {
            for r in 0..element.repetitions() {
                for text in element.components(r) {
                    if let Some(c) = text.chars().find(|c| !envelope.syntax.allows(*c)) {
                        return Err(EdiError::at(
                            0,
                            format!(
                                "a value of segment {} has the character '{c}', which {} \
                                 cannot carry",
                                segment.tag,
                                envelope.syntax.code()
                            ),
                        ));
                    }
                }
            }
        }
        write_segment(&mut out, segment, &d)?;
    }
    Ok(envelope.syntax.encode(&out))
}
