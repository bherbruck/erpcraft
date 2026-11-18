//! Electronic data interchange: ANSI X12 and UN/EDIFACT, read and written
//! by the ERP itself, with no value-added network and no translator
//! between the company and its trading partner.
//!
//! This crate is the first step: the format layer. It reads and writes
//! the two syntaxes and knows nothing of the database, the kernel or the
//! network.
//!
//! - [`segment`]: the one tokenizer both syntaxes share, its limits, and
//!   the writer.
//! - [`x12`]: X12 interchanges, groups and transaction sets; the 850
//!   purchase order as a typed structure ([`x12::po850`]); the 997
//!   functional acknowledgement, built and read ([`x12::ack`]).
//! - [`edifact`]: EDIFACT interchanges and messages.
//! - [`gs1`]: check digits and the SSCC of a carton.
//!
//! ARCHITECTURE.md ("Electronic data interchange") says what the module
//! becomes, and PLAN.md in this directory the order it is built in.
//! README.md names the sources and what was verified against them.

pub mod edifact;
pub mod gs1;
pub mod segment;
pub mod x12;

pub use segment::{Delimiters, EdiError, Element, Segment};

/// Which syntax a file is in, by how it begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    X12,
    Edifact,
}

/// Tells X12 from EDIFACT by the first segment: `ISA`, or `UNA` or `UNB`.
/// Anything else is neither.
pub fn syntax_of(bytes: &[u8]) -> Option<Syntax> {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    match bytes.get(start..start + 3)? {
        b"ISA" => Some(Syntax::X12),
        b"UNA" | b"UNB" => Some(Syntax::Edifact),
        _ => None,
    }
}
