//! Receiving: store, read, route, acknowledge.
//!
//! 1. The bytes are read on the blocking pool, behind `catch_unwind`: a
//!    panic is a refusal, kept as `unreadable`, never a crash.
//! 2. They are stored in the same transaction, once it is known whose
//!    they are ([`store`]). The same bytes again are a repeat: the first
//!    result is returned and nothing is done twice.
//! 3. The interchange is routed to the partner its envelope names. None:
//!    `unroutable`, and nothing else happens.
//! 4. Each functional group is acknowledged by syntax alone, at once.
//!    Acknowledgements are never acknowledged.
//! 5. Each set becomes a message: `proposed` for an order that can be
//!    drafted, `test` for test traffic, `rejected` for a set with syntax
//!    errors or an interchange whose control number was used before.

use std::panic::AssertUnwindSafe;

use chrono::Utc;
use erpcraft_kernel::{Ctx, Error, PgConnection, Result, new_id};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::control::{self, Kind};
use crate::partner::{self, Partner};
use crate::proposal::Finding;
use crate::segment::EdiError;
use crate::store;
use crate::x12::ack::{AckForm, FunctionalAck, SetVerdict, Status};
use crate::x12::po850::{self, PurchaseOrder};
use crate::x12::ta1::{NOTE_DUPLICATE, Ta1, Ta1Status};
use crate::x12::{self, Interchange};

pub const PERMISSION: &str = "edi:receive";

/// What reading found in one transaction set.
pub enum SetRead {
    Order(Box<PurchaseOrder>, SetVerdict),
    /// A 997 or 999 the partner sent.
    Ack(std::result::Result<FunctionalAck, String>),
    /// A set this module does not read: refused in the acknowledgement.
    Other(SetVerdict),
}

pub struct ReadInterchange {
    pub interchange: Interchange,
    /// Per group, per set.
    pub sets: Vec<Vec<SetRead>>,
}

/// Reads a file: its interchanges, and every set in them as what it is.
/// Pure and bounded; run it with [`blocking`].
pub fn read_file(bytes: &[u8]) -> std::result::Result<Vec<ReadInterchange>, EdiError> {
    let interchanges = x12::read(bytes)?;
    Ok(interchanges
        .into_iter()
        .map(|interchange| {
            let sets = interchange
                .groups
                .iter()
                .map(|group| {
                    group
                        .sets
                        .iter()
                        .map(|set| match set.id.as_str() {
                            "850" => {
                                let (order, verdict) = po850::read(set);
                                SetRead::Order(Box::new(order), verdict)
                            }
                            "997" | "999" => {
                                SetRead::Ack(FunctionalAck::read(set).map_err(|e| e.message))
                            }
                            _ => SetRead::Other(SetVerdict {
                                codes: vec![crate::x12::ack::SET_NOT_SUPPORTED],
                                segments: vec![],
                            }),
                        })
                        .collect()
                })
                .collect();
            ReadInterchange { interchange, sets }
        })
        .collect())
}

/// Runs a reader on the blocking pool. A panic inside it is caught and
/// returned as an error, so no file can stop the server.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> std::result::Result<T, String> {
    match tokio::task::spawn_blocking(move || std::panic::catch_unwind(AssertUnwindSafe(f))).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) | Err(_) => {
            tracing::error!("an EDI reader panicked; the file is kept as unreadable");
            Err("the file could not be read: the reader failed on it".to_owned())
        }
    }
}

// ------------------------------------------------------------------ output

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub struct InterchangeOut {
    pub id: Uuid,
    pub direction: String,
    pub state: String,
    pub company_id: Option<Uuid>,
    pub partner_id: Option<Uuid>,
    pub control: Option<String>,
    pub usage: Option<String>,
    pub size: i32,
    pub sha256: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub struct MessageOut {
    pub id: Uuid,
    /// None for a message to send that failed its checks.
    pub interchange_id: Option<Uuid>,
    pub direction: String,
    pub message: String,
    pub group_control: Option<String>,
    pub control: Option<String>,
    pub position: Option<i32>,
    pub state: String,
    /// The partner's order number.
    pub business_key: Option<String>,
    pub doc_type: Option<String>,
    pub document_id: Option<Uuid>,
    /// `[{severity, code, line, message}]`.
    pub findings: Value,
    pub ack_message_id: Option<Uuid>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReceiveOutput {
    pub interchange: InterchangeOut,
    /// The sets of the interchange, one message each.
    pub messages: Vec<MessageOut>,
    /// What was made to answer it: a 997 or 999 per group, a TA1.
    /// Fetch their bytes with `edi_interchange.content`.
    pub acknowledgements: Vec<MessageOut>,
    /// These bytes were received before; this is what became of them then.
    pub repeat: bool,
}

const INTERCHANGE_COLUMNS: &str =
    "id, direction, state, company_id, partner_id, control, usage, size, sha256, error";
pub const MESSAGE_COLUMNS: &str = "id, interchange_id, direction, message, group_control, \
    control, position, state, business_key, doc_type, document_id, findings, ack_message_id";

pub async fn interchange_out(conn: &mut PgConnection, id: Uuid) -> Result<InterchangeOut> {
    Ok(sqlx::query_as(&format!(
        "SELECT {INTERCHANGE_COLUMNS} FROM edi_interchange WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&mut *conn)
    .await?)
}

pub(crate) async fn output(
    conn: &mut PgConnection,
    id: Uuid,
    repeat: bool,
) -> Result<ReceiveOutput> {
    let interchange = interchange_out(conn, id).await?;
    let messages = sqlx::query_as(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM edi_message WHERE interchange_id = $1 ORDER BY position"
    ))
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let acknowledgements = sqlx::query_as(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM edi_message WHERE answers_interchange_id = $1
         ORDER BY created_at, id"
    ))
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(ReceiveOutput {
        interchange,
        messages,
        acknowledgements,
        repeat,
    })
}

// ----------------------------------------------------------------- receive

pub async fn receive(
    ctx: &mut Ctx<'_>,
    bytes: Vec<u8>,
    company_id: Option<Uuid>,
) -> Result<ReceiveOutput> {
    if crate::syntax_of(&bytes) == Some(crate::Syntax::Edifact) {
        return crate::inbound_edifact::receive(ctx, bytes, company_id).await;
    }
    if let Some(company) = company_id {
        ctx.require(PERMISSION, company)?;
    }
    // Read before anything is stored, but stored in this same transaction:
    // what is kept is decided by whose it is, and a preview, a refusal or a
    // failure keeps nothing. A panic in the reader is caught.
    let hash = store::sha256(&bytes);
    let owned = bytes.clone();
    let read = blocking(move || read_file(&owned).map_err(|e| e.to_string())).await;
    let mut read = match read.and_then(|r| r) {
        Ok(read) if read.len() == 1 => read,
        Ok(read) => {
            let why = format!(
                "the file holds {} interchanges; each is received on its own",
                read.len()
            );
            return unrouted(ctx, &bytes, &hash, "unreadable", None, &why).await;
        }
        Err(why) => return unrouted(ctx, &bytes, &hash, "unreadable", None, &why).await,
    };
    let ReadInterchange { interchange, sets } = read.remove(0);
    let isa = &interchange.isa;

    let partner = match partner::route(&mut *ctx.conn, isa, company_id).await? {
        Ok(partner) => partner,
        Err(why) => return unrouted(ctx, &bytes, &hash, "unroutable", Some(isa), &why).await,
    };
    // Refused here, a caller learns nothing about what was received before.
    ctx.require(PERMISSION, partner.company_id)?;
    // One receive per partner at a time: the check for a control number
    // used before and the numbers of the answers are taken in turn.
    sqlx::query("SELECT 1 FROM edi_partner WHERE id = $1 FOR NO KEY UPDATE")
        .bind(partner.id)
        .execute(&mut *ctx.conn)
        .await?;
    let Some(id) =
        store::insert_routed(&mut *ctx.conn, &bytes, &hash, &partner, ctx.actor.user_id).await?
    else {
        let id = store::routed_repeat(&mut *ctx.conn, partner.company_id, &hash).await?;
        ctx.no_change();
        return output(ctx.conn, id, true).await;
    };
    ctx.touch("edi_interchange", id);

    // The same control number from this partner with other bytes: kept as
    // its duplicate, answered with a TA1 that says so, and not worked on.
    let first: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM edi_interchange
         WHERE partner_id = $1 AND direction = 'in' AND control = $2 AND id <> $3
           AND duplicate_of IS NULL",
    )
    .bind(partner.id)
    .bind(&isa.control)
    .bind(id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let duplicate = first.is_some();
    let test = isa.usage != 'P' || partner.is_test();
    // An envelope whose trailer does not match its header is rejected as
    // a whole, as its TA1 says; nothing in it is worked on.
    let envelope_ta1 = Ta1::of(&interchange);
    let envelope_rejected = envelope_ta1.status == Ta1Status::Rejected;

    sqlx::query(
        "UPDATE edi_interchange SET syntax = 'x12', control = $2, usage = $3, state = 'read',
             error = $4, duplicate_of = $5
         WHERE id = $1",
    )
    .bind(id)
    .bind(&isa.control)
    .bind(isa.usage.to_string())
    .bind(if duplicate {
        Some("this control number came from this partner before")
    } else if envelope_rejected {
        Some("the interchange's trailer does not match its header")
    } else {
        None
    })
    .bind(first)
    .execute(&mut *ctx.conn)
    .await?;

    let mut position = 0i32;
    for (group, reads) in interchange.groups.iter().zip(sets) {
        let mut verdicts = Vec::new();
        let mut kinds = Vec::new();
        let mut answers = Vec::new();
        for read in reads {
            match read {
                SetRead::Order(order, verdict) => {
                    verdicts.push(verdict);
                    kinds.push(Some(order));
                    answers.push(None);
                }
                SetRead::Other(verdict) => {
                    verdicts.push(verdict);
                    kinds.push(None);
                    answers.push(None);
                }
                SetRead::Ack(ack) => {
                    // An acknowledgement is never acknowledged: it is
                    // matched to what it answers.
                    verdicts.push(SetVerdict::default());
                    kinds.push(None);
                    answers.push(Some(ack));
                }
            }
        }
        let is_ack_group = group.functional_id == "FA";
        let mut next = verdicts.iter();
        let ack = FunctionalAck::of(group, |_| next.next().cloned().unwrap_or_default());
        let mut ids = Vec::new();
        for (((set, set_ack), order), answer) in
            group.sets.iter().zip(&ack.sets).zip(kinds).zip(answers)
        {
            position += 1;
            let mut findings: Vec<Finding> = Vec::new();
            let state = if is_ack_group {
                "received"
            } else if duplicate {
                findings.push(Finding::blocking(
                    "duplicate_interchange",
                    None,
                    format!(
                        "The interchange's control number {} came from this partner before, \
                         with other content; it was answered with a TA1 and not worked on.",
                        isa.control
                    ),
                ));
                "rejected"
            } else if envelope_rejected {
                findings.push(Finding::blocking(
                    "interchange_rejected",
                    None,
                    "The interchange's control number in its trailer (IEA02) is not the one in \
                     its header (ISA13): it was rejected with a TA1 and not worked on.",
                ));
                "rejected"
            } else if set_ack.status == Status::Rejected {
                for sentence in crate::x12::ack::describe(set_ack) {
                    findings.push(Finding::blocking("syntax", None, sentence));
                }
                "rejected"
            } else if test {
                "test"
            } else if order.is_some() {
                "proposed"
            } else {
                "received"
            };
            if let Some(order) = &order {
                for note in &order.notes {
                    findings.push(Finding::note("count", None, note.clone()));
                }
            }
            let message_id = new_id();
            sqlx::query(
                "INSERT INTO edi_message (id, interchange_id, company_id, partner_id, direction,
                     message, group_control, control, position, state, business_key, purpose,
                     findings)
                 VALUES ($1, $2, $3, $4, 'in', $5, $6, $7, $8, $9, $10, $11, $12)",
            )
            .bind(message_id)
            .bind(id)
            .bind(partner.company_id)
            .bind(partner.id)
            .bind(message_code(&set.id))
            .bind(&group.control)
            .bind(&set.control)
            .bind(position)
            .bind(state)
            .bind(
                order
                    .as_ref()
                    .map(|o| o.number.clone())
                    .filter(|n| !n.is_empty()),
            )
            .bind(
                order
                    .as_ref()
                    .map(|o| o.purpose.clone())
                    .filter(|p| !p.is_empty()),
            )
            .bind(json!(findings))
            .execute(&mut *ctx.conn)
            .await?;
            ctx.touch("edi_message", message_id);
            ids.push(message_id);
            if let Some(answer) = answer
                && !duplicate
                && !envelope_rejected
            {
                match_ack(ctx, &partner, message_id, answer).await?;
            }
        }
        if !is_ack_group
            && !duplicate
            && !envelope_rejected
            && matches!(partner.ack_form.as_str(), "997" | "999")
        {
            let ack_id = acknowledge(ctx, &partner, &interchange, group, &ack, id).await?;
            sqlx::query("UPDATE edi_message SET ack_message_id = $2 WHERE id = ANY($1)")
                .bind(&ids)
                .bind(ack_id)
                .execute(&mut *ctx.conn)
                .await?;
        }
    }

    let ta1 = if duplicate {
        Some(Ta1::answering(isa, Ta1Status::Rejected, NOTE_DUPLICATE))
    } else if isa.ack_requested || !interchange.errors.is_empty() {
        Some(envelope_ta1)
    } else {
        None
    };
    if let Some(ta1) = ta1
        && partner.ack_form != "none"
    {
        send_ta1(ctx, &partner, &interchange, &ta1, id).await?;
    }
    output(ctx.conn, id, false).await
}

/// Marks the messages a partner's 997 or 999 answers: acknowledged,
/// acknowledged with errors, or rejected with the place in words.
async fn match_ack(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
    ack_message: Uuid,
    answer: std::result::Result<FunctionalAck, String>,
) -> Result<()> {
    let ack = match answer {
        Ok(ack) => ack,
        Err(why) => {
            let finding = Finding::note(
                "unreadable_ack",
                None,
                format!("This acknowledgement could not be read: {why}"),
            );
            sqlx::query("UPDATE edi_message SET findings = $2 WHERE id = $1")
                .bind(ack_message)
                .bind(json!([finding]))
                .execute(&mut *ctx.conn)
                .await?;
            return Ok(());
        }
    };
    let state_of = |status: Status| match status {
        Status::Accepted => ("A", "acknowledged"),
        Status::AcceptedWithErrors | Status::PartiallyAccepted => ("E", "acknowledged_with_errors"),
        Status::Rejected => ("R", "rejected"),
    };
    let mut answered = Vec::new();
    for set in &ack.sets {
        let (code, state) = state_of(set.status);
        let findings: Vec<Finding> = if set.status == Status::Rejected {
            crate::x12::ack::describe(set)
                .into_iter()
                .map(|s| Finding::blocking("partner_rejected", None, s))
                .collect()
        } else {
            Vec::new()
        };
        sqlx::query(
            "UPDATE edi_message SET ack_status = $5, state = $6, ack_message_id = $7,
                 findings = findings || $8
             WHERE partner_id = $1 AND direction = 'out' AND group_control = $2
               AND control = $3 AND message = $4",
        )
        .bind(partner.id)
        .bind(&ack.group_control)
        .bind(&set.control)
        .bind(&set.id)
        .bind(code)
        .bind(state)
        .bind(ack_message)
        .bind(json!(findings))
        .execute(&mut *ctx.conn)
        .await?;
        answered.push(set.control.clone());
    }
    // A group answered as a whole (no AK2): every set in it.
    if ack.sets.is_empty() {
        let (code, state) = state_of(ack.status);
        sqlx::query(
            "UPDATE edi_message SET ack_status = $3, state = $4, ack_message_id = $5
             WHERE partner_id = $1 AND direction = 'out' AND group_control = $2
               AND message NOT IN ('997', '999', 'TA1')",
        )
        .bind(partner.id)
        .bind(&ack.group_control)
        .bind(code)
        .bind(state)
        .bind(ack_message)
        .execute(&mut *ctx.conn)
        .await?;
    }
    Ok(())
}

/// The usage an answer is written with: the interchange's own, when it is
/// one there is.
fn usage_of(isa: &crate::x12::Isa) -> char {
    match isa.usage {
        'T' => 'T',
        'I' => 'I',
        _ => 'P',
    }
}

/// What a message is called, as the database keeps it: a set whose type
/// is no type is named by nothing.
fn message_code(id: &str) -> &str {
    if id.len() <= 6
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        id
    } else {
        ""
    }
}

/// Keeps bytes nobody could be found for, for the caller who sent them:
/// unreadable, or unroutable with the envelope read. The same bytes from
/// the same caller again are a repeat.
async fn unrouted(
    ctx: &mut Ctx<'_>,
    bytes: &[u8],
    hash: &str,
    state: &str,
    isa: Option<&crate::x12::Isa>,
    why: &str,
) -> Result<ReceiveOutput> {
    unrouted_with(
        ctx,
        bytes,
        hash,
        state,
        isa.map(|i| i.usage.to_string()),
        why,
    )
    .await
}

/// [`unrouted`] with the usage as the envelope gave it, of either syntax.
pub(crate) async fn unrouted_with(
    ctx: &mut Ctx<'_>,
    bytes: &[u8],
    hash: &str,
    state: &str,
    usage: Option<String>,
    why: &str,
) -> Result<ReceiveOutput> {
    let (id, repeat) = store::insert_unrouted(
        &mut *ctx.conn,
        bytes,
        hash,
        ctx.actor.user_id,
        state,
        usage,
        why,
    )
    .await?;
    ctx.touch("edi_interchange", id);
    if repeat {
        ctx.no_change();
    }
    output(ctx.conn, id, repeat).await
}

/// Whether two acknowledgements say the same: what was read back from the
/// bytes written against what was meant. Copies of bad data are left out:
/// they are cut and cleaned of separators when written.
fn same_meaning(a: &FunctionalAck, b: &FunctionalAck) -> bool {
    let shape = |ack: &FunctionalAck| {
        (
            ack.functional_id.clone(),
            ack.group_control.clone(),
            ack.status,
            ack.received,
            ack.accepted,
            ack.codes.clone(),
            ack.sets
                .iter()
                .map(|s| {
                    (
                        s.id.clone(),
                        s.control.clone(),
                        s.status,
                        s.codes.clone(),
                        s.segments
                            .iter()
                            .map(|e| {
                                (
                                    e.tag.clone(),
                                    e.position,
                                    e.code,
                                    e.elements
                                        .iter()
                                        .map(|el| (el.position, el.component, el.code))
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    shape(a) == shape(b)
}

/// Writes, checks and stores the acknowledgement of one group, ready to
/// be sent. Returns its message.
async fn acknowledge(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
    interchange: &Interchange,
    group: &crate::x12::Group,
    ack: &FunctionalAck,
    answers: Uuid,
) -> Result<Uuid> {
    let (form, set_id) = match partner.ack_form.as_str() {
        "999" => (
            AckForm::Ack999 {
                version: group.version.clone(),
                conventions: group
                    .sets
                    .iter()
                    .map(|set| set.segments[0].get(3).to_owned())
                    .collect(),
            },
            "999",
        ),
        _ => (AckForm::Ack997, "997"),
    };
    let d = partner.delimiters()?;
    let ack = &ack.cleaned(&d);
    let body = ack.body_as(&d, &form);
    let at = Utc::now().naive_utc();
    // The last step before the bytes are fixed.
    let interchange_control = control::next(&mut *ctx.conn, partner.id, Kind::Interchange).await?;
    let group_control = control::next(&mut *ctx.conn, partner.id, Kind::Group).await?;
    let envelope = partner.envelope(
        "FA",
        at,
        interchange_control,
        group_control,
        usage_of(&interchange.isa),
    )?;
    let text = x12::write(&envelope, set_id, &[body])
        .map_err(|e| Error::internal(format!("the acknowledgement could not be written: {e}")))?;
    // Read back as the partner will read it.
    let back = x12::read_with(text.as_bytes(), crate::segment::Limits::for_answers())
        .ok()
        .and_then(|mut all| all.pop())
        .and_then(|i| i.groups.into_iter().next())
        .and_then(|g| g.sets.into_iter().next())
        .and_then(|s| FunctionalAck::read(&s).ok());
    if !back.is_some_and(|back| same_meaning(&back, ack)) {
        return Err(Error::internal(
            "the acknowledgement written does not read back as what was meant",
        ));
    }
    let control_text = format!("{interchange_control:09}");
    let bytes = partner.finish(text);
    let out = store::store_outgoing(
        &mut *ctx.conn,
        store::Outgoing {
            company_id: partner.company_id,
            partner_id: partner.id,
            control: &control_text,
            usage: usage_of(&interchange.isa),
            bytes: &bytes,
        },
    )
    .await?;
    ctx.touch("edi_interchange", out);
    let message_id = new_id();
    sqlx::query(
        "INSERT INTO edi_message (id, interchange_id, company_id, partner_id, direction, message,
             group_control, control, position, state, answers_interchange_id)
         VALUES ($1, $2, $3, $4, 'out', $5, $6, '0001', 1, 'ready', $7)",
    )
    .bind(message_id)
    .bind(out)
    .bind(partner.company_id)
    .bind(partner.id)
    .bind(set_id)
    .bind(group_control.to_string())
    .bind(answers)
    .execute(&mut *ctx.conn)
    .await?;
    ctx.touch("edi_message", message_id);
    Ok(message_id)
}

async fn send_ta1(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
    interchange: &Interchange,
    ta1: &Ta1,
    answers: Uuid,
) -> Result<Uuid> {
    let at = Utc::now().naive_utc();
    let interchange_control = control::next(&mut *ctx.conn, partner.id, Kind::Interchange).await?;
    let envelope =
        partner.envelope("TA", at, interchange_control, 0, usage_of(&interchange.isa))?;
    let text = x12::write_ta1s(&envelope, &[ta1.segment()])
        .map_err(|e| Error::internal(format!("the TA1 could not be written: {e}")))?;
    let back = x12::read(text.as_bytes())
        .ok()
        .and_then(|mut all| all.pop())
        .and_then(|i| i.ta1.into_iter().next())
        .and_then(|s| Ta1::read(&s).ok());
    if back.as_ref() != Some(ta1) {
        return Err(Error::internal(
            "the TA1 written does not read back as what was meant",
        ));
    }
    let control_text = format!("{interchange_control:09}");
    let bytes = partner.finish(text);
    let out = store::store_outgoing(
        &mut *ctx.conn,
        store::Outgoing {
            company_id: partner.company_id,
            partner_id: partner.id,
            control: &control_text,
            usage: usage_of(&interchange.isa),
            bytes: &bytes,
        },
    )
    .await?;
    ctx.touch("edi_interchange", out);
    let message_id = new_id();
    sqlx::query(
        "INSERT INTO edi_message (id, interchange_id, company_id, partner_id, direction, message,
             position, state, answers_interchange_id)
         VALUES ($1, $2, $3, $4, 'out', 'TA1', 1, 'ready', $5)",
    )
    .bind(message_id)
    .bind(out)
    .bind(partner.company_id)
    .bind(partner.id)
    .bind(answers)
    .execute(&mut *ctx.conn)
    .await?;
    ctx.touch("edi_message", message_id);
    Ok(message_id)
}
