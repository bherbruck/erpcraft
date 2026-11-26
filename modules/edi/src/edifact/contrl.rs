//! CONTRL: the syntax answer of an EDIFACT interchange, as the 997 is for
//! X12. `UCI` answers the interchange (its control reference, sender and
//! recipient, and an action), `UCM` each message (its reference and type,
//! and an action), each with the syntax error code (0085) that rejected
//! it. The actions this module writes and reads are `7` (acknowledged:
//! the interchange or message was received and its syntax is right) and
//! `4` (rejected). An acknowledgement is never acknowledged.

use crate::edifact::common::seg;
use crate::edifact::{Interchange, Message};
use crate::segment::Segment;

/// Action (0083): acknowledged, or rejected.
pub const ACKNOWLEDGED: &str = "7";
pub const REJECTED: &str = "4";

/// Syntax error codes (0085) this module raises.
pub const MISSING: u8 = 13;
pub const NOT_SUPPORTED: u8 = 14;

/// What reading a message found wrong with its syntax: codes of 0085.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    pub codes: Vec<u8>,
}

impl Verdict {
    pub fn is_clean(&self) -> bool {
        self.codes.is_empty()
    }
}

/// One message as a CONTRL answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageAnswer {
    pub reference: String,
    pub kind: String,
    pub version: String,
    pub release: String,
    pub agency: String,
    pub action: String,
    pub code: Option<u8>,
}

/// A CONTRL, built or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contrl {
    /// The control reference of the interchange answered (UNB 0020).
    pub interchange: String,
    pub sender: String,
    pub sender_qualifier: String,
    pub receiver: String,
    pub receiver_qualifier: String,
    pub action: String,
    pub code: Option<u8>,
    pub messages: Vec<MessageAnswer>,
}

impl Contrl {
    /// The answer to a received interchange: each message acknowledged or
    /// rejected by its own codes and those of `verdicts` (by position),
    /// the interchange rejected if its envelope is wrong. CONTRLs received
    /// are not answered: leave them out of `verdicts`.
    pub fn of(interchange: &Interchange, verdicts: &[Verdict]) -> Contrl {
        let messages: Vec<MessageAnswer> = interchange
            .messages
            .iter()
            .zip(verdicts)
            .map(|(m, verdict)| {
                let code = m.errors.first().or(verdict.codes.first()).copied();
                MessageAnswer {
                    reference: m.reference.clone(),
                    kind: m.kind.clone(),
                    version: m.version.clone(),
                    release: m.release.clone(),
                    agency: m.agency.clone(),
                    action: if code.is_some() {
                        REJECTED
                    } else {
                        ACKNOWLEDGED
                    }
                    .into(),
                    code,
                }
            })
            .collect();
        let code = interchange.errors.first().copied();
        Contrl {
            interchange: interchange.control.clone(),
            // The answer names the interchange as it was sent.
            sender: interchange.sender.clone(),
            sender_qualifier: interchange.sender_qualifier.clone(),
            receiver: interchange.receiver.clone(),
            receiver_qualifier: interchange.receiver_qualifier.clone(),
            action: if code.is_some() {
                REJECTED
            } else {
                ACKNOWLEDGED
            }
            .into(),
            code,
            messages,
        }
    }

    pub fn body(&self) -> Vec<Segment> {
        let code = |c: Option<u8>| c.map(|c| comps![c.to_string()]).unwrap_or_default();
        let mut out = vec![seg(
            "UCI",
            vec![
                comps![self.interchange.clone()],
                comps![self.sender.clone(), self.sender_qualifier.clone()],
                comps![self.receiver.clone(), self.receiver_qualifier.clone()],
                comps![self.action.clone()],
                code(self.code),
            ],
        )];
        for m in &self.messages {
            out.push(seg(
                "UCM",
                vec![
                    comps![m.reference.clone()],
                    comps![
                        m.kind.clone(),
                        m.version.clone(),
                        m.release.clone(),
                        m.agency.clone()
                    ],
                    comps![m.action.clone()],
                    code(m.code),
                ],
            ));
        }
        out
    }

    /// A CONTRL message read; `None` when it has no UCI.
    pub fn read(message: &Message) -> Option<Contrl> {
        let mut contrl: Option<Contrl> = None;
        let number = |s: &str| s.parse::<u8>().ok();
        for segment in &message.segments {
            match segment.tag.as_str() {
                "UCI" => {
                    contrl = Some(Contrl {
                        interchange: segment.get(1).to_owned(),
                        sender: segment.component(2, 1).to_owned(),
                        sender_qualifier: segment.component(2, 2).to_owned(),
                        receiver: segment.component(3, 1).to_owned(),
                        receiver_qualifier: segment.component(3, 2).to_owned(),
                        action: segment.get(4).to_owned(),
                        code: number(segment.get(5)),
                        messages: vec![],
                    })
                }
                "UCM" => {
                    if let Some(c) = contrl.as_mut() {
                        c.messages.push(MessageAnswer {
                            reference: segment.get(1).to_owned(),
                            kind: segment.component(2, 1).to_owned(),
                            version: segment.component(2, 2).to_owned(),
                            release: segment.component(2, 3).to_owned(),
                            agency: segment.component(2, 4).to_owned(),
                            action: segment.get(3).to_owned(),
                            code: number(segment.get(4)),
                        });
                    }
                }
                _ => {}
            }
        }
        contrl
    }
}
