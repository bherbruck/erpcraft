//! Receiving: store, read, route, acknowledge.
//!
//! 1. The bytes are stored first, in a transaction of their own
//!    ([`store::store_incoming`]). The same bytes again are a repeat: the
//!    first result is returned and nothing is done twice.
//! 2. They are read on the blocking pool, behind `catch_unwind`: a panic
//!    is a refusal, kept as `unreadable`, never a crash.
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

async fn output(conn: &mut PgConnection, id: Uuid, repeat: bool) -> Result<ReceiveOutput> {
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
    if let Some(company) = company_id {
        ctx.require(PERMISSION, company)?;
    }
    let id = store::store_incoming(ctx.kernel.pool(), &bytes, "manual").await?;
    ctx.touch("edi_interchange", id);
    // Locked until this command ends: the same bytes at once wait here,
    // and then find them read.
    let (state, routed_to): (String, Option<Uuid>) =
        sqlx::query_as("SELECT state, company_id FROM edi_interchange WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *ctx.conn)
            .await?;
    if state != "stored" {
        if let Some(company) = routed_to {
            ctx.require(PERMISSION, company)?;
            if company_id.is_some_and(|c| c != company) {
                return Err(Error::validation(
                    "these bytes were received before for another company",
                ));
            }
        }
        ctx.no_change();
        return output(ctx.conn, id, true).await;
    }

    let read = blocking(move || read_file(&bytes).map_err(|e| e.to_string())).await;
    let mut read = match read.and_then(|r| r) {
        Ok(read) => read,
        Err(why) => return unreadable(ctx, id, &why).await,
    };
    if read.len() != 1 {
        let why = format!(
            "the file holds {} interchanges; each is received on its own",
            read.len()
        );
        return unreadable(ctx, id, &why).await;
    }
    let ReadInterchange { interchange, sets } = read.remove(0);
    let isa = &interchange.isa;

    let Some(partner) = partner::route(&mut *ctx.conn, isa, company_id).await? else {
        sqlx::query(
            "UPDATE edi_interchange SET state = 'unroutable', syntax = 'x12', usage = $2,
                 error = $3
             WHERE id = $1",
        )
        .bind(id)
        .bind(isa.usage.to_string())
        .bind(format!(
            "no active partner is from {}/{} to {}/{}",
            isa.sender_qualifier, isa.sender_id, isa.receiver_qualifier, isa.receiver_id
        ))
        .execute(&mut *ctx.conn)
        .await?;
        return output(ctx.conn, id, false).await;
    };
    ctx.require(PERMISSION, partner.company_id)?;

    // The same control number from this partner with other bytes: kept,
    // answered with a TA1 that says so, and not worked on.
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM edi_interchange
                        WHERE partner_id = $1 AND direction = 'in' AND control = $2 AND id <> $3)",
    )
    .bind(partner.id)
    .bind(&isa.control)
    .bind(id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    let test = isa.usage != 'P' || partner.is_test();

    sqlx::query(
        "UPDATE edi_interchange SET company_id = $2, partner_id = $3, syntax = 'x12',
             control = $4, usage = $5, state = 'read', error = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(partner.company_id)
    .bind(partner.id)
    .bind(&isa.control)
    .bind(isa.usage.to_string())
    .bind(duplicate.then_some("this control number came from this partner before"))
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
            .bind(&set.id)
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
            {
                match_ack(ctx, &partner, message_id, answer).await?;
            }
        }
        if !is_ack_group && !duplicate && matches!(partner.ack_form.as_str(), "997" | "999") {
            let ack_id = acknowledge(ctx, &partner, &interchange, &ack, id).await?;
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
        Some(Ta1::of(&interchange))
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

async fn unreadable(ctx: &mut Ctx<'_>, id: Uuid, why: &str) -> Result<ReceiveOutput> {
    sqlx::query("UPDATE edi_interchange SET state = 'unreadable', error = $2 WHERE id = $1")
        .bind(id)
        .bind(why)
        .execute(&mut *ctx.conn)
        .await?;
    output(ctx.conn, id, false).await
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
    ack: &FunctionalAck,
    answers: Uuid,
) -> Result<Uuid> {
    let (form, set_id) = match partner.ack_form.as_str() {
        "999" => (
            AckForm::Ack999 {
                version: partner.version.clone(),
            },
            "999",
        ),
        _ => (AckForm::Ack997, "997"),
    };
    let d = partner.delimiters()?;
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
        interchange.isa.usage,
    )?;
    let text = x12::write(&envelope, set_id, &[body])
        .map_err(|e| Error::internal(format!("the acknowledgement could not be written: {e}")))?;
    // Read back as the partner will read it.
    let back = x12::read(text.as_bytes())
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
            usage: interchange.isa.usage,
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
    let envelope = partner.envelope("TA", at, interchange_control, 0, interchange.isa.usage)?;
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
            usage: interchange.isa.usage,
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
