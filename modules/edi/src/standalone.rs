//! Sending a message that answers no order: an 846 (INVRPT) made from
//! stock, an 820 (REMADV) made from a payment run. Like the messages of
//! `outbound`, it is written, read back as the partner will read it, and
//! only then numbered, written again with its numbers, read back again and
//! stored; a preview stops before the numbers.

use chrono::Utc;
use erpcraft_kernel::{Ctx, Decimal, Error, Result, new_id};
use serde_json::json;
use uuid::Uuid;

use crate::control::{self, Kind};
use crate::inbound::{MESSAGE_COLUMNS, MessageOut};
use crate::outbound::{Generated, explained};
use crate::partner::Partner;
use crate::policy::SEND;
use crate::proposal::{Finding, Severity};
use crate::segment::Segment;
use crate::store;

/// A message to send that no order is behind.
pub struct Standalone {
    pub partner: Partner,
    /// The message as stored: `846`, `INVRPT`, `820`, `REMADV`.
    pub code: String,
    /// The X12 functional group it travels in, such as `IB`.
    pub functional_id: &'static str,
    /// Its segments after the ST or UNH and before the SE or UNT, in the
    /// partner's syntax; none when it could not be made.
    pub body: Option<Vec<Segment>>,
    pub findings: Vec<Finding>,
    pub business_key: Option<String>,
    pub doc_type: Option<String>,
    pub document_id: Option<Uuid>,
    /// For the policy `edi.send`: what the message is worth, if anything.
    pub total: Decimal,
}

/// Checks, numbers and stores a standalone message; `reads_back` says
/// whether the bytes written read back as meant.
pub async fn send(
    ctx: &mut Ctx<'_>,
    mut s: Standalone,
    preview: bool,
    reads_back: impl Fn(&[u8]) -> bool,
) -> Result<Generated> {
    let partner = s.partner.clone();
    let at = Utc::now().naive_utc();
    let usage = if partner.is_test() { 'T' } else { 'P' };
    let segments = s
        .body
        .as_deref()
        .map(|b| explained(b, &s.code))
        .unwrap_or_default();
    let mut fixed: Option<(Vec<u8>, String, String, String)> = None;
    if s.findings.iter().all(|f| f.severity != Severity::Blocking)
        && let Some(body) = s.body.clone()
    {
        let write = |interchange: u32, group: u32| -> Result<(Vec<u8>, String, String, String)> {
            if partner.is_edifact() {
                let envelope = partner.edifact_envelope(at, interchange)?;
                let bytes = crate::edifact::write(
                    &envelope,
                    &[crate::edifact::Outgoing {
                        kind: s.code.clone(),
                        version: "D".into(),
                        release: "96A".into(),
                        agency: "UN".into(),
                        association: String::new(),
                        reference: "1".into(),
                        body: body.clone(),
                    }],
                )
                .map_err(|e| Error::validation(e.message))?;
                let control = envelope.control.clone();
                Ok((bytes, control.clone(), control, "1".into()))
            } else {
                let envelope = partner.envelope(s.functional_id, at, interchange, group, usage)?;
                let text = crate::x12::write(&envelope, &s.code, std::slice::from_ref(&body))
                    .map_err(|e| Error::validation(e.message))?;
                Ok((
                    partner.finish(text),
                    format!("{interchange:09}"),
                    group.to_string(),
                    "0001".into(),
                ))
            }
        };
        match write(1, 1) {
            Err(e) => s.findings.push(Finding::blocking(
                "write",
                None,
                format!("The message cannot be written: {e}."),
            )),
            Ok((bytes, ..)) if !reads_back(&bytes) => s.findings.push(Finding::blocking(
                "read_back",
                None,
                "The message written does not read back as what was meant.",
            )),
            Ok(_) if preview => {}
            Ok(_) => {
                ctx.decide(
                    SEND,
                    partner.company_id,
                    json!({
                        "partner_code": partner.code,
                        "message": s.code,
                        "total": s.total,
                        "test": partner.is_test(),
                    }),
                )
                .await?;
                let interchange =
                    control::next(&mut *ctx.conn, partner.id, Kind::Interchange).await?;
                let group = if partner.is_edifact() {
                    0
                } else {
                    control::next(&mut *ctx.conn, partner.id, Kind::Group).await?
                };
                let written = write(interchange, group)?;
                if !reads_back(&written.0) {
                    return Err(Error::internal(
                        "the message with its control numbers does not read back",
                    ));
                }
                fixed = Some(written);
            }
        }
    }
    let valid = s.findings.iter().all(|f| f.severity != Severity::Blocking);
    if preview {
        return Ok(Generated {
            message: None,
            state: if valid { "ready" } else { "invalid" }.into(),
            findings: s.findings,
            segments,
            supersedes: None,
        });
    }
    let (interchange_id, group, set, state) = match &fixed {
        Some((bytes, interchange, group, set)) => {
            let id = store::store_outgoing(
                &mut *ctx.conn,
                store::Outgoing {
                    company_id: partner.company_id,
                    partner_id: partner.id,
                    control: interchange,
                    usage,
                    bytes,
                },
            )
            .await?;
            ctx.touch("edi_interchange", id);
            (Some(id), Some(group.clone()), Some(set.clone()), "ready")
        }
        None => (None, None, None, "invalid"),
    };
    let message_id = new_id();
    sqlx::query(
        "INSERT INTO edi_message (id, interchange_id, company_id, partner_id, direction, message,
             group_control, control, position, state, business_key, doc_type, document_id,
             findings)
         VALUES ($1, $2, $3, $4, 'out', $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(message_id)
    .bind(interchange_id)
    .bind(partner.company_id)
    .bind(partner.id)
    .bind(&s.code)
    .bind(&group)
    .bind(&set)
    .bind(interchange_id.map(|_| 1i32))
    .bind(state)
    .bind(&s.business_key)
    .bind(&s.doc_type)
    .bind(s.document_id)
    .bind(json!(s.findings))
    .execute(&mut *ctx.conn)
    .await?;
    ctx.touch("edi_message", message_id);
    let out: MessageOut = sqlx::query_as(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM edi_message WHERE id = $1"
    ))
    .bind(message_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    Ok(Generated {
        message: Some(out),
        state: state.into(),
        findings: s.findings,
        segments: if fixed.is_some() { segments } else { vec![] },
        supersedes: None,
    })
}
