//! Gapless document numbers.

use erpcraft_core::Result;
use erpcraft_db::PgConnection;
use uuid::Uuid;

/// Take the next number of a series, formatted `PREFIX-YEAR-00001`.
///
/// The counter row stays locked until the caller's transaction ends, so a
/// number is only consumed if the transaction commits. That makes the
/// series gapless, at the cost of serializing numbering within a series.
pub async fn next(
    conn: &mut PgConnection,
    company_id: Uuid,
    prefix: &str,
    year: i32,
) -> Result<String> {
    let series = format!("{prefix}-{year}");
    let value: i64 = sqlx::query_scalar(
        "INSERT INTO number_series (company_id, series, last_value) VALUES ($1, $2, 1)
         ON CONFLICT (company_id, series) DO UPDATE SET last_value = number_series.last_value + 1
         RETURNING last_value",
    )
    .bind(company_id)
    .bind(&series)
    .fetch_one(&mut *conn)
    .await?;
    Ok(format!("{series}-{value:05}"))
}
