//! Control numbers: one sequence per partner and level (interchange,
//! group, set). The row is locked from the moment a number is taken until
//! the command commits, so two commands for one partner take different
//! numbers and a number is used once. Take it as the last step before the
//! bytes are fixed, so the lock is held briefly.
//!
//! After 999,999,999 the sequence starts again at 1, as a nine-digit ISA13
//! must.

use erpcraft_kernel::{PgConnection, Result};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Interchange,
    Group,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Interchange => "interchange",
            Kind::Group => "group",
        }
    }
}

pub async fn next(conn: &mut PgConnection, partner_id: Uuid, kind: Kind) -> Result<u32> {
    let value: i64 = sqlx::query_scalar(
        "INSERT INTO edi_control (partner_id, kind, next_value) VALUES ($1, $2, 2)
         ON CONFLICT (partner_id, kind) DO UPDATE SET next_value =
             CASE WHEN edi_control.next_value >= 999999999 THEN 1
                  ELSE edi_control.next_value + 1 END
         RETURNING CASE WHEN next_value = 1 THEN 999999999 ELSE next_value - 1 END",
    )
    .bind(partner_id)
    .bind(kind.as_str())
    .fetch_one(&mut *conn)
    .await?;
    u32::try_from(value).map_err(erpcraft_kernel::Error::internal)
}
