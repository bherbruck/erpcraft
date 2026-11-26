//! Receiving EDIFACT: as X12 (`inbound`), with the UNB in place of the
//! ISA and a CONTRL in place of the 997.
//!
//! The bytes are read on the blocking pool, stored once their partner is
//! known (the same bytes again are a repeat), routed by the UNB's sender
//! and recipient with their qualifiers, and each message becomes a row:
//! an ORDERS `proposed` (or `test`), a CONTRL matched to what it answers,
//! anything else refused. A partner whose acknowledgement is `contrl` gets
//! a CONTRL back for every interchange that is not itself only CONTRLs:
//! an acknowledgement is never acknowledged.

use chrono::Utc;
use erpcraft_kernel::{Ctx, Error, Result, new_id};
use serde_json::json;
use uuid::Uuid;

use crate::control::{self, Kind};
use crate::edifact::contrl::{ACKNOWLEDGED, Contrl, NOT_SUPPORTED, REJECTED, Verdict};
use crate::edifact::{self, Interchange, Outgoing, orders};
use crate::inbound::{PERMISSION, ReceiveOutput, blocking, output, unrouted_with};
use crate::partner::{self, Partner};
use crate::proposal::Finding;
use crate::store;
use crate::x12::po850::PurchaseOrder;

/// What a syntax error code (0085) means, for a person.
fn describe(code: u8) -> String {
    match code {
        13 => "a segment or value it must have is missing".into(),
        14 => "the message is not one this module reads".into(),
        26 => "it was received before".into(),
        28 => "its trailer's reference is not its header's".into(),
        29 => "its trailer's count is not the number of segments or messages received".into(),
        other => format!("syntax error {other}"),
    }
}

/// One message as read.
enum Read {
    Order(Box<PurchaseOrder>, Verdict),
    Contrl(Option<Contrl>),
    Other(Verdict),
}

pub async fn receive(
    ctx: &mut Ctx<'_>,
    bytes: Vec<u8>,
    company_id: Option<Uuid>,
) -> Result<ReceiveOutput> {
    if let Some(company) = company_id {
        ctx.require(PERMISSION, company)?;
    }
    let hash = store::sha256(&bytes);
    let owned = bytes.clone();
    let read = blocking(move || edifact::read(&owned).map_err(|e| e.to_string())).await;
    let mut interchanges = match read.and_then(|r| r) {
        Ok(all) if all.len() == 1 => all,
        Ok(all) => {
            let why = format!(
                "the file holds {} interchanges; each is received on its own",
                all.len()
            );
            return unrouted_with(ctx, &bytes, &hash, "unreadable", None, &why).await;
        }
        Err(why) => return unrouted_with(ctx, &bytes, &hash, "unreadable", None, &why).await,
    };
    let interchange = interchanges.remove(0);
    let usage = if interchange.test { "T" } else { "P" };

    let partner = match partner::route_edifact(&mut *ctx.conn, &interchange, company_id).await? {
        Ok(partner) => partner,
        Err(why) => {
            return unrouted_with(ctx, &bytes, &hash, "unroutable", Some(usage.into()), &why).await;
        }
    };
    ctx.require(PERMISSION, partner.company_id)?;
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

    let first: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM edi_interchange
         WHERE partner_id = $1 AND direction = 'in' AND control = $2 AND id <> $3
           AND duplicate_of IS NULL",
    )
    .bind(partner.id)
    .bind(&interchange.control)
    .bind(id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let duplicate = first.is_some();
    let test = interchange.test || partner.is_test();
    let envelope_rejected = !interchange.errors.is_empty();
    sqlx::query(
        "UPDATE edi_interchange SET control = $2, usage = $3, state = 'read', error = $4,
             duplicate_of = $5
         WHERE id = $1",
    )
    .bind(id)
    .bind(&interchange.control)
    .bind(usage)
    .bind(if duplicate {
        Some("this control reference came from this partner before".to_owned())
    } else {
        interchange
            .errors
            .first()
            .map(|c| format!("the interchange was rejected: {}", describe(*c)))
    })
    .bind(first)
    .execute(&mut *ctx.conn)
    .await?;

    let d = interchange.delimiters;
    let reads: Vec<Read> = interchange
        .messages
        .iter()
        .map(|m| match m.kind.as_str() {
            "ORDERS" => {
                let (order, verdict) = orders::read(m, &d);
                Read::Order(Box::new(order), verdict)
            }
            "CONTRL" => Read::Contrl(Contrl::read(m)),
            _ => Read::Other(Verdict {
                codes: vec![NOT_SUPPORTED],
            }),
        })
        .collect();

    let mut answered: Vec<(Uuid, Verdict, usize)> = vec![];
    for (index, (message, read)) in interchange.messages.iter().zip(reads).enumerate() {
        let position = index as i32 + 1;
        let mut findings: Vec<Finding> = vec![];
        let (verdict, order, contrl) = match read {
            Read::Order(order, verdict) => (verdict, Some(order), None),
            Read::Contrl(contrl) => (Verdict::default(), None, Some(contrl)),
            Read::Other(verdict) => (verdict, None, None),
        };
        let codes: Vec<u8> = message
            .errors
            .iter()
            .chain(&verdict.codes)
            .copied()
            .collect();
        let state = if contrl.is_some() {
            "received"
        } else if duplicate {
            findings.push(Finding::blocking(
                "duplicate_interchange",
                None,
                format!(
                    "The interchange's control reference {} came from this partner before, \
                     with other content; it was not worked on.",
                    interchange.control
                ),
            ));
            "rejected"
        } else if envelope_rejected {
            findings.push(Finding::blocking(
                "interchange_rejected",
                None,
                "The interchange's trailer does not agree with its header: it was rejected \
                 and not worked on."
                    .to_owned(),
            ));
            "rejected"
        } else if !codes.is_empty() {
            for code in &codes {
                findings.push(Finding::blocking(
                    "syntax",
                    None,
                    format!(
                        "Message {} was rejected: {}.",
                        message.reference,
                        describe(*code)
                    ),
                ));
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
        .bind(code_of(&message.kind))
        .bind(&interchange.control)
        .bind(&message.reference)
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
        match contrl {
            Some(contrl) if !duplicate && !envelope_rejected => {
                match_contrl(ctx, &partner, message_id, contrl).await?
            }
            Some(_) => {}
            None => answered.push((message_id, verdict, index)),
        }
    }

    if partner.ack_form == "contrl" && !duplicate && !answered.is_empty() {
        // Only what is not itself an acknowledgement is answered.
        let mut answering = interchange.clone();
        answering.messages = answered
            .iter()
            .map(|(_, _, index)| interchange.messages[*index].clone())
            .collect();
        let verdicts: Vec<Verdict> = answered.iter().map(|(_, v, _)| v.clone()).collect();
        let contrl = Contrl::of(&answering, &verdicts);
        let ack = send_contrl(ctx, &partner, &contrl, id, interchange.test).await?;
        let ids: Vec<Uuid> = answered.iter().map(|(m, _, _)| *m).collect();
        sqlx::query("UPDATE edi_message SET ack_message_id = $2 WHERE id = ANY($1)")
            .bind(&ids)
            .bind(ack)
            .execute(&mut *ctx.conn)
            .await?;
    }
    output(ctx.conn, id, false).await
}

/// A message's type as the database keeps it.
fn code_of(kind: &str) -> &str {
    if (2..=6).contains(&kind.len()) && kind.bytes().all(|b| b.is_ascii_uppercase()) {
        kind
    } else {
        "UNKNWN"
    }
}

/// Marks what a partner's CONTRL answers: the messages it names, of the
/// interchange it names, acknowledged or rejected; the whole interchange
/// when it names no message.
async fn match_contrl(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
    ack_message: Uuid,
    contrl: Option<Contrl>,
) -> Result<()> {
    let Some(contrl) = contrl else {
        let finding = Finding::note(
            "unreadable_ack",
            None,
            "This CONTRL has no UCI segment: it says nothing about what it answers.",
        );
        sqlx::query("UPDATE edi_message SET findings = $2 WHERE id = $1")
            .bind(ack_message)
            .bind(json!([finding]))
            .execute(&mut *ctx.conn)
            .await?;
        return Ok(());
    };
    let state_of = |action: &str| match action {
        ACKNOWLEDGED => ("A", "acknowledged"),
        REJECTED => ("R", "rejected"),
        _ => ("E", "acknowledged_with_errors"),
    };
    for m in &contrl.messages {
        let (code, state) = state_of(&m.action);
        let findings: Vec<Finding> = match m.code {
            Some(c) if m.action == REJECTED => vec![Finding::blocking(
                "partner_rejected",
                None,
                format!(
                    "The partner rejected message {}: {}.",
                    m.reference,
                    describe(c)
                ),
            )],
            _ => vec![],
        };
        sqlx::query(
            "UPDATE edi_message SET ack_status = $4, state = $5, ack_message_id = $6,
                 findings = findings || $7
             WHERE partner_id = $1 AND direction = 'out' AND group_control = $2
               AND control = $3 AND message <> 'CONTRL'",
        )
        .bind(partner.id)
        .bind(&contrl.interchange)
        .bind(&m.reference)
        .bind(code)
        .bind(state)
        .bind(ack_message)
        .bind(json!(findings))
        .execute(&mut *ctx.conn)
        .await?;
    }
    if contrl.messages.is_empty() {
        let (code, state) = state_of(&contrl.action);
        sqlx::query(
            "UPDATE edi_message SET ack_status = $3, state = $4, ack_message_id = $5
             WHERE partner_id = $1 AND direction = 'out' AND group_control = $2
               AND message <> 'CONTRL'",
        )
        .bind(partner.id)
        .bind(&contrl.interchange)
        .bind(code)
        .bind(state)
        .bind(ack_message)
        .execute(&mut *ctx.conn)
        .await?;
    }
    Ok(())
}

/// Writes, checks and stores a CONTRL answering interchange `answers`.
async fn send_contrl(
    ctx: &mut Ctx<'_>,
    partner: &Partner,
    contrl: &Contrl,
    answers: Uuid,
    test: bool,
) -> Result<Uuid> {
    let control = control::next(&mut *ctx.conn, partner.id, Kind::Interchange).await?;
    let mut envelope = partner.edifact_envelope(Utc::now().naive_utc(), control)?;
    envelope.test = test || envelope.test;
    let bytes = edifact::write(
        &envelope,
        &[Outgoing {
            kind: "CONTRL".into(),
            version: "2".into(),
            release: "2".into(),
            agency: "UN".into(),
            association: String::new(),
            reference: "1".into(),
            body: contrl.body(),
        }],
    )
    .map_err(|e| Error::internal(format!("the CONTRL could not be written: {e}")))?;
    let back = edifact::read(&bytes)
        .ok()
        .and_then(|mut all| all.pop())
        .and_then(|i| i.messages.into_iter().next())
        .and_then(|m| Contrl::read(&m));
    if back.as_ref() != Some(contrl) {
        return Err(Error::internal(
            "the CONTRL written does not read back as what was meant",
        ));
    }
    let out = store::store_outgoing(
        &mut *ctx.conn,
        store::Outgoing {
            company_id: partner.company_id,
            partner_id: partner.id,
            control: &envelope.control,
            usage: if envelope.test { 'T' } else { 'P' },
            bytes: &bytes,
        },
    )
    .await?;
    ctx.touch("edi_interchange", out);
    let message_id = new_id();
    sqlx::query(
        "INSERT INTO edi_message (id, interchange_id, company_id, partner_id, direction, message,
             group_control, control, position, state, answers_interchange_id)
         VALUES ($1, $2, $3, $4, 'out', 'CONTRL', $5, '1', 1, 'ready', $6)",
    )
    .bind(message_id)
    .bind(out)
    .bind(partner.company_id)
    .bind(partner.id)
    .bind(&envelope.control)
    .bind(answers)
    .execute(&mut *ctx.conn)
    .await?;
    ctx.touch("edi_message", message_id);
    Ok(message_id)
}

/// The ORDERS at `position` of an EDIFACT interchange's bytes.
pub fn order_at(
    payload: &[u8],
    position: usize,
) -> std::result::Result<Option<PurchaseOrder>, String> {
    let mut all: Vec<Interchange> = edifact::read(payload).map_err(|e| e.to_string())?;
    let Some(first) = all.pop() else {
        return Ok(None);
    };
    let d = first.delimiters;
    Ok(first
        .messages
        .get(position.saturating_sub(1))
        .filter(|m| m.kind == "ORDERS")
        .map(|m| orders::read(m, &d).0))
}
