//! What EDI does by itself once a change is committed: drafts an order
//! that may be drafted without a person, and makes the 855, 856 or 810
//! when the document they come from is approved or posted, and the
//! remittance advice of a posted payment run.
//!
//! Everything runs as the partner's run-as user, through the commands a
//! person would run, with that user's permissions and every policy: the
//! automation can do less than a person and never more. A partner with no
//! run-as user gets nothing made by itself; `edi.pending` shows what
//! waits. Refusals are not retried: a policy or a missing permission does
//! not change by trying again. Anything else is, as the outbox retries.

use erpcraft_kernel::authz::load_actor;
use erpcraft_kernel::command::{self, Options};
use erpcraft_kernel::outbox::{Event, Subscriber};
use erpcraft_kernel::{Error, Kernel, Result, async_trait};
use serde_json::{Value, json};
use uuid::Uuid;

pub struct EdiSubscriber;

const TOPICS: [&str; 6] = [
    "edi.receive",
    "payment_run.post",
    "sales_order.approve",
    "sales_delivery.post",
    "sales_invoice.post",
    "sales_credit_note.post",
];

/// Runs a command as a user. A refusal is the end of it; an internal
/// failure is retried.
async fn run_as(kernel: &Kernel, user_id: Uuid, name: &str, input: Value) -> Result<()> {
    let actor = match load_actor(kernel.pool(), user_id, None).await {
        Ok(actor) => actor,
        Err(Error::Unauthorized) => {
            tracing::warn!(%user_id, "an EDI partner's run-as user is not active");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    match command::execute(kernel, &actor, name, input, Options::default()).await {
        Ok(_) => Ok(()),
        Err(e @ Error::Internal(_)) => Err(e),
        Err(e) => {
            tracing::info!(command = name, error = %e, "EDI automation stopped short");
            Ok(())
        }
    }
}

/// The run-as user of the partner a sales order came from.
async fn order_partner(kernel: &Kernel, order_id: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT p.run_as_user_id FROM edi_document d JOIN edi_partner p ON p.id = d.partner_id
         WHERE d.doc_type = 'sales_order' AND d.document_id = $1 AND p.active",
    )
    .bind(order_id)
    .fetch_optional(kernel.pool())
    .await?
    .flatten())
}

/// The sales order a delivery, invoice or credit note follows from, by
/// the links the kernel keeps.
async fn order_of(kernel: &Kernel, doc_id: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "WITH RECURSIVE up (id, depth) AS (
             SELECT $1::uuid, 0
             UNION ALL
             SELECT l.from_id, up.depth + 1 FROM document_link l JOIN up ON l.to_id = up.id
             WHERE up.depth < 3)
         SELECT d.document_id FROM up
         JOIN edi_document d ON d.doc_type = 'sales_order' AND d.document_id = up.id
         LIMIT 1",
    )
    .bind(doc_id)
    .fetch_optional(kernel.pool())
    .await?)
}

#[async_trait]
impl Subscriber for EdiSubscriber {
    fn name(&self) -> &'static str {
        "edi"
    }

    fn wants(&self, topic: &str) -> bool {
        TOPICS.contains(&topic)
    }

    async fn handle(&self, kernel: &Kernel, event: &Event) -> Result<()> {
        let output = &event.payload["output"];
        let id = output["id"].as_str().and_then(|s| Uuid::parse_str(s).ok());
        match event.topic.as_str() {
            "edi.receive" => {
                for message in output["messages"].as_array().into_iter().flatten() {
                    if message["state"] != "proposed" {
                        continue;
                    }
                    let Some(message_id) =
                        message["id"].as_str().and_then(|s| s.parse::<Uuid>().ok())
                    else {
                        continue;
                    };
                    let user: Option<Uuid> = sqlx::query_scalar(
                        "SELECT p.run_as_user_id FROM edi_message m
                         JOIN edi_partner p ON p.id = m.partner_id
                         WHERE m.id = $1 AND m.state = 'proposed' AND p.active
                           AND m.message IN ('850', 'ORDERS') AND COALESCE(m.purpose, '00') = '00'",
                    )
                    .bind(message_id)
                    .fetch_optional(kernel.pool())
                    .await?
                    .flatten();
                    if let Some(user) = user {
                        let input = json!({ "message_id": message_id, "automatic": true });
                        run_as(kernel, user, "edi.draft", input).await?;
                    }
                }
                Ok(())
            }
            "payment_run.post" => {
                // A remittance advice to each vendor paid whose partner
                // wants one.
                let Some(run) = id else { return Ok(()) };
                let payments: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT payment_id FROM payment_run_payment WHERE document_id = $1
                     ORDER BY payment_id",
                )
                .bind(run)
                .fetch_all(kernel.pool())
                .await?;
                for payment in payments {
                    let mut conn = kernel.pool().acquire().await?;
                    let partner = crate::remit_out::partner_of_payment(&mut conn, payment).await?;
                    drop(conn);
                    if let Some(user) = partner.and_then(|p| p.run_as_user_id) {
                        let input = json!({ "payment_id": payment });
                        run_as(kernel, user, "edi.remittance_advice", input).await?;
                    }
                }
                Ok(())
            }
            "sales_order.approve" => {
                let Some(order) = id else { return Ok(()) };
                let Some(user) = order_partner(kernel, order).await? else {
                    return Ok(());
                };
                let input = json!({ "doc_type": "sales_order", "id": order, "message": "855" });
                run_as(kernel, user, "edi.generate", input).await
            }
            topic => {
                let Some(doc) = id else { return Ok(()) };
                let (doc_type, message) = match topic {
                    "sales_delivery.post" => ("sales_delivery", "856"),
                    "sales_invoice.post" => ("sales_invoice", "810"),
                    _ => ("sales_credit_note", "810"),
                };
                let Some(order) = order_of(kernel, doc).await? else {
                    return Ok(());
                };
                let Some(user) = order_partner(kernel, order).await? else {
                    return Ok(());
                };
                if message == "856" {
                    // A notice is made from the packing; a delivery posted
                    // without it waits in edi.pending.
                    let packed: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM edi_shipment
                                        WHERE delivery_id = $1 AND state = 'packed')",
                    )
                    .bind(doc)
                    .fetch_one(kernel.pool())
                    .await?;
                    if !packed {
                        return Ok(());
                    }
                }
                let input = json!({ "doc_type": doc_type, "id": doc, "message": message });
                run_as(kernel, user, "edi.generate", input).await
            }
        }
    }
}
