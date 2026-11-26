//! For `document.blockers`: a document's posting is dated in a period
//! that is closed, or in none.

use erpcraft_kernel::blockers::{Blocker, BlockerCheck, Resolution};
use erpcraft_kernel::document::Document;
use erpcraft_kernel::{Ctx, Result, async_trait};
use serde_json::json;
use uuid::Uuid;

pub struct PeriodCheck;

#[async_trait]
impl BlockerCheck for PeriodCheck {
    async fn check(&self, ctx: &mut Ctx<'_>, doc: &Document, step: &str) -> Result<Vec<Blocker>> {
        // Only the document's own posting is dated by it.
        if step != format!("{}.post", doc.doc_type) {
            return Ok(vec![]);
        }
        let period: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, name, status FROM fiscal_period
             WHERE company_id = $1 AND $2 BETWEEN start_date AND end_date",
        )
        .bind(doc.company_id)
        .bind(doc.doc_date)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let move_date = || {
            Resolution::new("document.update", "date the document in an open period")
                .input(json!({ "doc_type": doc.doc_type, "id": doc.id, "version": doc.version }))
        };
        Ok(match period {
            None => vec![
                Blocker::new(
                    "no_period",
                    format!("no fiscal period of the company holds {}", doc.doc_date),
                )
                .facts(json!({ "doc_date": doc.doc_date }))
                .resolve(Resolution::new(
                    "entity.create",
                    "add the fiscal period (entity fiscal_period), or set up the year",
                ))
                .resolve(move_date())
                .covers_refusal(),
            ],
            Some((id, name, status)) if status == "closed" => vec![
                Blocker::new(
                    "period_closed",
                    format!(
                        "{} is dated {}, in the closed period {name}",
                        doc.doc_type, doc.doc_date
                    ),
                )
                .facts(json!({ "doc_date": doc.doc_date, "period_id": id, "period": name }))
                .resolve(
                    Resolution::new("fiscal_period.reopen", format!("reopen {name}"))
                        .input(json!({ "id": id })),
                )
                .resolve(move_date())
                .covers_refusal(),
            ],
            Some(_) => vec![],
        })
    }
}
