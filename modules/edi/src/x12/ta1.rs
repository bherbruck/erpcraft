//! The TA1: the acknowledgement of an interchange envelope, as opposed to
//! the 997, which answers the functional groups inside it.
//!
//! A TA1 is sent back when the sender asked for one (ISA14 is `1`) or when
//! the envelope itself is at fault. It names the interchange by its
//! control number, date and time, says whether it was accepted, and gives
//! one note code. The note codes below are the ones this crate raises.

use crate::segment::{EdiError, Segment};
use crate::x12::{Interchange, InterchangeError, Isa};

/// TA105: nothing wrong.
pub const NOTE_NONE: &str = "000";
/// TA105: ISA13 and IEA02 do not match.
pub const NOTE_CONTROL_MISMATCH: &str = "001";
/// TA105: IEA01 is not the number of groups.
pub const NOTE_GROUP_COUNT: &str = "021";
/// TA105: this control number came from this sender before, with other
/// content.
pub const NOTE_DUPLICATE: &str = "025";

/// TA104: what became of the interchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ta1Status {
    Accepted,
    /// Accepted, with errors noted.
    AcceptedWithErrors,
    Rejected,
}

impl Ta1Status {
    pub fn code(self) -> &'static str {
        match self {
            Ta1Status::Accepted => "A",
            Ta1Status::AcceptedWithErrors => "E",
            Ta1Status::Rejected => "R",
        }
    }

    fn of(code: &str) -> Option<Self> {
        Some(match code {
            "A" => Ta1Status::Accepted,
            "E" => Ta1Status::AcceptedWithErrors,
            "R" => Ta1Status::Rejected,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ta1 {
    /// TA101: the control number of the interchange answered, nine digits.
    pub control: String,
    /// TA102 and TA103, as that interchange's ISA stated them.
    pub date: String,
    pub time: String,
    pub status: Ta1Status,
    /// TA105, three digits.
    pub note: String,
}

impl Ta1 {
    /// The TA1 for an interchange that was read. Its envelope's errors
    /// decide: a control number that does not match its trailer rejects
    /// it; a wrong group count is noted and the groups are still answered.
    pub fn of(interchange: &Interchange) -> Self {
        let (status, note) = match interchange.errors.first() {
            None => (Ta1Status::Accepted, NOTE_NONE),
            Some(InterchangeError::ControlNumberMismatch) => {
                (Ta1Status::Rejected, NOTE_CONTROL_MISMATCH)
            }
            Some(InterchangeError::GroupCountMismatch) => {
                (Ta1Status::AcceptedWithErrors, NOTE_GROUP_COUNT)
            }
        };
        Ta1::answering(&interchange.isa, status, note)
    }

    /// A TA1 for an interchange's header with a status and note chosen by
    /// the caller, such as a duplicate.
    pub fn answering(isa: &Isa, status: Ta1Status, note: &str) -> Self {
        // Echoed from the partner's header, which may hold anything: the
        // date and time only as digits.
        let digits = |text: &str| {
            text.chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
        };
        Ta1 {
            control: isa.control.clone(),
            date: digits(&isa.date),
            time: digits(&isa.time),
            status,
            note: note.to_owned(),
        }
    }

    pub fn segment(&self) -> Segment {
        Segment::new(
            "TA1",
            &[
                self.control.as_str(),
                &self.date,
                &self.time,
                self.status.code(),
                &self.note,
            ],
        )
    }

    /// Reads a TA1 segment a partner sent.
    pub fn read(segment: &Segment) -> Result<Self, EdiError> {
        let bad = |what: &str| EdiError::at(segment.offset, format!("the TA1 {what}"));
        if segment.tag != "TA1" {
            return Err(bad("is not a TA1 segment"));
        }
        let control = segment.get(1);
        if control.len() != 9 || !control.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("does not name a control number of nine digits"));
        }
        let note = segment.get(5);
        if note.len() != 3 || !note.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("has no note code of three digits"));
        }
        Ok(Ta1 {
            control: control.to_owned(),
            date: segment.get(2).to_owned(),
            time: segment.get(3).to_owned(),
            status: Ta1Status::of(segment.get(4)).ok_or_else(|| bad("has an unknown status"))?,
            note: note.to_owned(),
        })
    }
}
