//! The log of raw payloads. Bytes received are stored before anything
//! reads them, in a transaction of their own, so that a file that breaks
//! the reader, the server or a rule is still on record; bytes sent are
//! stored once their control numbers are fixed.

use erpcraft_kernel::{PgConnection, PgPool, Result, new_id};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Stores bytes received and returns the interchange that holds them: a
/// new one in the state `stored`, or the one that already holds the same
/// bytes. Committed before it returns, whatever happens after.
pub async fn store_incoming(pool: &PgPool, bytes: &[u8], transport: &str) -> Result<Uuid> {
    let hash = sha256(bytes);
    let size = i32::try_from(bytes.len())
        .map_err(|_| erpcraft_kernel::Error::validation("the file is too large"))?;
    let mut tx = pool.begin().await?;
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO edi_interchange (id, direction, sha256, size, payload, transport, state)
         VALUES ($1, 'in', $2, $3, $4, $5, 'stored')
         ON CONFLICT (sha256) WHERE direction = 'in' DO NOTHING
         RETURNING id",
    )
    .bind(new_id())
    .bind(&hash)
    .bind(size)
    .bind(bytes)
    .bind(transport)
    .fetch_optional(&mut *tx)
    .await?;
    let id = match inserted {
        Some(id) => id,
        None => {
            sqlx::query_scalar(
                "SELECT id FROM edi_interchange WHERE sha256 = $1 AND direction = 'in'",
            )
            .bind(&hash)
            .fetch_one(&mut *tx)
            .await?
        }
    };
    tx.commit().await?;
    Ok(id)
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
         VALUES ($1, $2, $3, 'out', 'x12', $4, $5, $6, $7, $8, 'manual', 'ready')",
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
