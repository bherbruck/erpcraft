//! Sending events to webhooks.

use std::sync::Arc;
use std::time::Duration;

use erpcraft_kernel::{Error, Event, Kernel, Result, Subscriber, async_trait};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::address::{self, Guarded};

/// How long an endpoint has to answer.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The value of the `X-Erpcraft-Signature` header for `body`: `sha256=`
/// and the HMAC-SHA256 of the body under the webhook's secret, in hex. A
/// receiver computes the same over the bytes it received and compares.
pub fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(body);
    let digest: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256={digest}")
}

/// Whether a webhook subscribed to `topics` wants events of `topic`. No
/// topics at all means every event. `purchase_order.*` means every topic
/// that starts with `purchase_order.`.
pub fn wants(topics: &[String], topic: &str) -> bool {
    topics.is_empty()
        || topics.iter().any(|wanted| match wanted.strip_suffix('*') {
            Some(prefix) => topic.starts_with(prefix),
            None => wanted == topic,
        })
}

/// The company an event belongs to, where its payload says so. The event
/// of a command that handles secrets carries neither input nor output, so
/// it has none.
fn company_of(payload: &Value) -> Option<Uuid> {
    ["output", "input"]
        .iter()
        .find_map(|part| payload[part]["company_id"].as_str()?.parse().ok())
}

#[derive(sqlx::FromRow)]
struct Target {
    id: Uuid,
    name: String,
    url: String,
    topics: Vec<String>,
    company_id: Option<Uuid>,
    secret: String,
}

/// Why a request did not count as delivered.
struct Failure {
    /// What the endpoint answered, if it answered.
    status: Option<u16>,
    error: String,
}

pub(crate) struct Webhooks {
    client: reqwest::Client,
    allow_private: bool,
}

impl Webhooks {
    pub(crate) fn new(allow_private: bool) -> Result<Arc<Webhooks>> {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            // A redirect could lead anywhere, past every check made here.
            .redirect(reqwest::redirect::Policy::none())
            // Straight to the endpoint: a proxy would look the name up
            // itself, past the check below.
            .no_proxy()
            .dns_resolver(Arc::new(Guarded { allow_private }))
            .user_agent(concat!("erpcraft-webhooks/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(Error::internal)?;
        Ok(Arc::new(Webhooks {
            client,
            allow_private,
        }))
    }

    /// POST `body` to one webhook. Only a 2xx answer counts.
    async fn send(&self, target: &Target, event: &Event, body: &[u8]) -> Result<u16, Failure> {
        // Checked when the webhook was saved, and again now: the setting
        // may have changed, and the row may have been written another way.
        let url = address::check_url(&target.url, self.allow_private).map_err(|e| Failure {
            status: None,
            error: e.to_string(),
        })?;
        let answer = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .header("x-erpcraft-event", &event.topic)
            .header("x-erpcraft-delivery", event.id.to_string())
            .header("x-erpcraft-signature", signature(&target.secret, body))
            .body(body.to_vec())
            .send()
            .await
            .map_err(|error| Failure {
                status: None,
                // Without the URL, which may itself be a credential.
                error: describe(&error.without_url()),
            })?;
        let status = answer.status();
        if status.is_success() {
            Ok(status.as_u16())
        } else {
            Err(Failure {
                status: Some(status.as_u16()),
                error: format!("the endpoint answered {status}"),
            })
        }
    }
}

/// A request error with its causes, which is where the reason is.
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return format!("no answer within {} seconds", TIMEOUT.as_secs());
    }
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[async_trait]
impl Subscriber for Webhooks {
    fn name(&self) -> &'static str {
        "webhooks"
    }

    /// Which webhooks want a topic is in the database, so every event is
    /// looked at.
    fn wants(&self, _topic: &str) -> bool {
        true
    }

    /// Send the event to every active webhook that wants it and has not
    /// had it. All of them are tried; if any fails the event is retried
    /// later, and then goes only to those that failed.
    async fn handle(&self, kernel: &Kernel, event: &Event) -> Result<()> {
        let targets: Vec<Target> = sqlx::query_as(
            "SELECT w.id, w.name, w.url, w.topics, w.company_id, s.secret
             FROM webhook w JOIN webhook_secret s ON s.webhook_id = w.id
             WHERE w.active AND NOT EXISTS (
                SELECT 1 FROM webhook_delivery d
                WHERE d.event_id = $1 AND d.webhook_id = w.id AND d.delivered_at IS NOT NULL)
             ORDER BY w.created_at, w.id",
        )
        .bind(event.id)
        .fetch_all(kernel.pool())
        .await?;
        let company = company_of(&event.payload);
        let targets: Vec<Target> = targets
            .into_iter()
            .filter(|t| wants(&t.topics, &event.topic))
            .filter(|t| t.company_id.is_none() || t.company_id == company)
            .collect();
        if targets.is_empty() {
            return Ok(());
        }

        // The event as the outbox holds it and nothing else: what a command
        // kept out of the outbox stays out of the request.
        let body = serde_json::to_vec(&json!({
            "id": event.id,
            "topic": event.topic,
            "created_at": event.created_at,
            "payload": event.payload,
        }))
        .map_err(Error::internal)?;

        let mut failed = vec![];
        for target in &targets {
            let outcome = self.send(target, event, &body).await;
            let (status, error) = match &outcome {
                Ok(status) => (Some(i32::from(*status)), None),
                Err(failure) => (failure.status.map(i32::from), Some(failure.error.as_str())),
            };
            sqlx::query(
                "INSERT INTO webhook_delivery
                    (event_id, webhook_id, topic, status_code, error, delivered_at)
                 VALUES ($1, $2, $3, $4, $5, CASE WHEN $5 IS NULL THEN now() END)
                 ON CONFLICT (event_id, webhook_id) DO UPDATE SET
                    attempts = webhook_delivery.attempts + 1,
                    status_code = EXCLUDED.status_code,
                    error = EXCLUDED.error,
                    attempted_at = now(),
                    delivered_at = EXCLUDED.delivered_at",
            )
            .bind(event.id)
            .bind(target.id)
            .bind(&event.topic)
            .bind(status)
            .bind(error)
            .execute(kernel.pool())
            .await?;
            sqlx::query(
                "UPDATE webhook SET last_error = $2, last_status = CASE WHEN $2 IS NULL THEN NULL ELSE $3 END,
                    failed_at = CASE WHEN $2 IS NULL THEN NULL ELSE now() END
                 WHERE id = $1 AND (last_error IS NOT NULL OR $2 IS NOT NULL)",
            )
            .bind(target.id)
            .bind(error)
            .bind(status)
            .execute(kernel.pool())
            .await?;
            if let Some(error) = error {
                tracing::warn!(webhook = %target.id, event = %event.id, error, "webhook delivery failed");
                failed.push(format!("{}: {error}", target.name));
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(Error::internal(format!(
                "{} of {} webhooks failed: {}",
                failed.len(),
                targets.len(),
                failed.join("; ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_hmac_sha256_in_hex() {
        // RFC 4231, test case 2.
        assert_eq!(
            signature("Jefe", b"what do ya want for nothing?"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn topics_match_exactly_or_by_prefix() {
        let topics = |list: &[&str]| list.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert!(wants(&[], "anything.at_all"));
        let some = topics(&["purchase_order.*", "journal_entry.post"]);
        assert!(wants(&some, "purchase_order.approve"));
        assert!(wants(&some, "journal_entry.post"));
        assert!(!wants(&some, "journal_entry.reverse"));
        assert!(!wants(&some, "purchase_receipt.post"));
        assert!(wants(&topics(&["*"]), "entity.create"));
    }

    #[test]
    fn the_company_comes_from_the_payload() {
        let id = Uuid::new_v4();
        assert_eq!(
            company_of(&json!({ "output": { "company_id": id } })),
            Some(id)
        );
        assert_eq!(
            company_of(&json!({ "input": { "company_id": id }, "output": { "rows": [] } })),
            Some(id)
        );
        assert_eq!(company_of(&json!({ "command": "user.create" })), None);
    }
}
