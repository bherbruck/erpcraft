//! The log of raw payloads. Bytes received are stored in the transaction
//! that reads them, once it is known whose they are: what a partner sent
//! is kept for its company, what nobody could be found for is kept for the
//! caller who sent it. Bytes sent are stored once their control numbers
//! are fixed.

use erpcraft_kernel::{PgConnection, Result, new_id};

use crate::partner::Partner;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn size_of(bytes: &[u8]) -> Result<i32> {
    i32::try_from(bytes.len())
        .map_err(|_| erpcraft_kernel::Error::validation("the file is too large"))
}

/// Stores bytes received from a partner, read and routed, in the
/// caller's transaction. Nothing when the same bytes from a partner were
/// stored before: that is a repeat ([`routed_repeat`]).
pub async fn insert_routed(
    conn: &mut PgConnection,
    bytes: &[u8],
    hash: &str,
    partner: &Partner,
    received_by: Uuid,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "INSERT INTO edi_interchange (id, company_id, partner_id, direction, syntax, sha256, size,
             payload, transport, state, received_by)
         VALUES ($1, $2, $3, 'in', (SELECT syntax FROM edi_partner WHERE id = $3), $4, $5, $6,
                 'manual', 'stored', $7)
         ON CONFLICT (company_id, sha256) WHERE direction = 'in' AND partner_id IS NOT NULL
         DO NOTHING
         RETURNING id",
    )
    .bind(new_id())
    .bind(partner.company_id)
    .bind(partner.id)
    .bind(hash)
    .bind(size_of(bytes)?)
    .bind(bytes)
    .bind(received_by)
    .fetch_optional(&mut *conn)
    .await?)
}

/// The interchange that holds bytes received and routed before.
pub async fn routed_repeat(conn: &mut PgConnection, company_id: Uuid, hash: &str) -> Result<Uuid> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM edi_interchange
         WHERE company_id = $1 AND sha256 = $2 AND direction = 'in' AND partner_id IS NOT NULL
         FOR UPDATE",
    )
    .bind(company_id)
    .bind(hash)
    .fetch_one(&mut *conn)
    .await?)
}

/// Stores bytes nobody could be found for, kept for the caller who sent
/// them. Returns the interchange and whether this caller sent them before.
pub async fn insert_unrouted(
    conn: &mut PgConnection,
    bytes: &[u8],
    hash: &str,
    received_by: Uuid,
    state: &str,
    usage: Option<String>,
    error: &str,
) -> Result<(Uuid, bool)> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO edi_interchange (id, direction, syntax, usage, sha256, size, payload,
             transport, state, error, received_by)
         VALUES ($1, 'in', $2, $3, $4, $5, $6, 'manual', $7, $8, $9)
         ON CONFLICT (sha256, received_by) WHERE direction = 'in' AND partner_id IS NULL
         DO NOTHING RETURNING id",
    )
    .bind(new_id())
    .bind(usage.as_ref().and_then(|_| {
        crate::syntax_of(bytes).map(|syntax| match syntax {
            crate::Syntax::X12 => "x12",
            crate::Syntax::Edifact => "edifact",
        })
    }))
    .bind(&usage)
    .bind(hash)
    .bind(size_of(bytes)?)
    .bind(bytes)
    .bind(state)
    .bind(error)
    .bind(received_by)
    .fetch_optional(&mut *conn)
    .await?;
    match inserted {
        Some(id) => Ok((id, false)),
        None => Ok((
            sqlx::query_scalar(
                "SELECT id FROM edi_interchange WHERE sha256 = $1 AND direction = 'in'
                   AND partner_id IS NULL AND received_by = $2",
            )
            .bind(hash)
            .bind(received_by)
            .fetch_one(&mut *conn)
            .await?,
            true,
        )),
    }
}

/// What an interchange to send is stored with.
pub struct Outgoing<'a> {
    pub company_id: Uuid,
    pub partner_id: Uuid,
    pub control: &'a str,
    pub usage: char,
    pub bytes: &'a [u8],
}

/// Stores the bytes of an interchange to send, ready to go. In the
/// command's transaction: they exist if and only if what made them did.
pub async fn store_outgoing(conn: &mut PgConnection, out: Outgoing<'_>) -> Result<Uuid> {
    let id = new_id();
    let size = i32::try_from(out.bytes.len())
        .map_err(|_| erpcraft_kernel::Error::internal("an interchange to send is too large"))?;
    sqlx::query(
        "INSERT INTO edi_interchange (id, company_id, partner_id, direction, syntax, control,
             usage, sha256, size, payload, transport, state)
         VALUES ($1, $2, $3, 'out', (SELECT syntax FROM edi_partner WHERE id = $3), $4, $5, $6,
                 $7, $8, 'manual', 'ready')",
    )
    .bind(id)
    .bind(out.company_id)
    .bind(out.partner_id)
    .bind(out.control)
    .bind(out.usage.to_string())
    .bind(sha256(out.bytes))
    .bind(size)
    .bind(out.bytes)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}
