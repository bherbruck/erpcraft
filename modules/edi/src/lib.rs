//! Electronic data interchange: ANSI X12 and UN/EDIFACT, read and written
//! by the ERP itself, with no value-added network and no translator
//! between the company and its trading partner.
//!
//! Two layers:
//!
//! - **The format layer**, free of the database and the kernel, so it can
//!   be fuzzed and tested alone: [`segment`] (the one tokenizer both
//!   syntaxes share, its limits, and the writer), [`x12`] (interchanges,
//!   groups and sets; the 850 as a typed order; the 997 and 999; the
//!   TA1), [`edifact`], [`gs1`] (check digits and the SSCC), and
//!   [`explain`] (a sentence per segment).
//! - **The module**: trading partners and cross-references as entities;
//!   the log of every interchange with its bytes ([`store`]); control
//!   numbers ([`control`]); receiving ([`inbound`]); proposals
//!   ([`proposal`]) and drafts ([`draft`]) of sales orders; a partner's
//!   implementation guide as data ([`guide`]).
//!
//! ARCHITECTURE.md ("Electronic data interchange") says what the module
//! is, PLAN.md in this directory the order it is built in, and README.md
//! the sources and what was verified against them.

use std::sync::Arc;

use erpcraft_kernel::{DynCommand, EntityMeta, Migration, Module, PolicyPoint, command};

pub mod edifact;
pub mod explain;
pub mod gs1;
mod inbound_edifact;
pub mod segment;
pub mod x12;

pub mod change;
pub mod commands;
pub mod commands_out;
pub mod control;
pub mod draft;
pub mod entities;
pub mod guide;
pub mod guide_commands;
pub mod inbound;
pub mod inventory;
pub mod label;
pub mod outbound;
pub mod partner;
pub mod policy;
pub mod proposal;
pub mod shipment;
pub mod standalone;
pub mod store;
pub mod subscriber;

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

pub struct EdiModule;

impl Module for EdiModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("de", include_str!("../i18n/de.json")),
            ("fr", include_str!("../i18n/fr.json")),
        ]
    }

    fn name(&self) -> &'static str {
        "edi"
    }

    fn label(&self) -> String {
        "EDI".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["sales", "stock", "party", "purchasing", "payments"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "edi",
                sql: include_str!("../migrations/0001_edi.sql"),
            },
            Migration {
                version: 2,
                name: "shipment",
                sql: include_str!("../migrations/0002_shipment.sql"),
            },
            Migration {
                version: 3,
                name: "review",
                sql: include_str!("../migrations/0003_review.sql"),
            },
            Migration {
                version: 4,
                name: "company_scope",
                sql: include_str!("../migrations/0004_company_scope.sql"),
            },
            Migration {
                version: 5,
                name: "edifact",
                sql: include_str!("../migrations/0005_edifact.sql"),
            },
            Migration {
                version: 6,
                name: "guide",
                sql: include_str!("../migrations/0006_guide.sql"),
            },
            Migration {
                version: 8,
                name: "phase7",
                sql: include_str!("../migrations/0008_phase7.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        entities::all()
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        policy::points()
    }

    fn subscribers(&self) -> Vec<Arc<dyn erpcraft_kernel::Subscriber>> {
        vec![Arc::new(subscriber::EdiSubscriber)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(commands::Receive),
            command(commands::Propose),
            command(commands::Draft),
            command(commands::Dismiss),
            command(commands::Explain),
            command(commands::ContentOf),
            command(commands::MarkSent),
            command(commands::Purge),
            command(commands_out::Generate),
            command(commands_out::Pack),
            command(commands_out::AutoPack),
            command(commands_out::Ship),
            command(commands_out::LabelOf),
            command(commands_out::Pending),
            command(commands_out::Resend),
            command(guide_commands::Check),
            command(guide_commands::Derive),
            command(guide_commands::Export),
            command(guide_commands::Import),
            command(inventory::Advise),
            command(change::ProposeChange),
            command(change::ApplyChange),
        ]
    }
}
