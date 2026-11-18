//! Questions every module asks about a company: its accounting currency,
//! and whether records belong to it.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use erpcraft_core::{Error, Result};
use erpcraft_db::PgConnection;
use uuid::Uuid;

/// A company's accounting currency and its decimal places. Neither can
/// change once the company exists (both fields are immutable), so they are
/// read once per company and remembered for the life of the process: they
/// were read several times in every posting. Changing them in the database
/// by hand needs a restart.
async fn currency_of(conn: &mut PgConnection, company_id: Uuid) -> Result<(String, u32)> {
    static KNOWN: OnceLock<RwLock<HashMap<Uuid, (String, u32)>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(found) = known.read().ok().and_then(|k| k.get(&company_id).cloned()) {
        return Ok(found);
    }
    let row: Option<(String, i16)> = sqlx::query_as(
        "SELECT c.base_currency, cur.decimals FROM company c
         JOIN currency cur ON cur.code = c.base_currency WHERE c.id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *conn)
    .await?;
    let (code, decimals) = row.ok_or_else(|| Error::not_found("company"))?;
    let found = (code, u32::try_from(decimals).map_err(Error::internal)?);
    if let Ok(mut known) = known.write() {
        known.insert(company_id, found.clone());
    }
    Ok(found)
}

/// The code of the company's accounting currency, such as `USD`.
pub async fn base_currency(conn: &mut PgConnection, company_id: Uuid) -> Result<String> {
    Ok(currency_of(conn, company_id).await?.0)
}

/// How many decimal places amounts in the company's accounting currency
/// have: 2 for USD, 0 for JPY. This is what [`crate::round_money`] takes.
pub async fn currency_decimals(conn: &mut PgConnection, company_id: Uuid) -> Result<u32> {
    Ok(currency_of(conn, company_id).await?.1)
}

/// Whether every one of `ids` is a row of `table` that belongs to
/// `company_id`. An id given twice counts once; no ids at all is `true`.
///
/// The table must have the columns `id` and `company_id`. Its name is
/// `&'static str` so that it can only be written in the calling module's
/// source: text from a caller can never reach the SQL.
pub async fn owns_all(
    conn: &mut PgConnection,
    table: &'static str,
    ids: &[Uuid],
    company_id: Uuid,
) -> Result<bool> {
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::internal(format!("'{table}' is not a table name")));
    }
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    let known: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {table} WHERE id = ANY($1) AND company_id = $2"
    ))
    .bind(&ids)
    .bind(company_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(known == ids.len() as i64)
}
