//! Webhooks: committed changes sent, signed, to HTTP endpoints outside.

mod common;

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use common::{TestApp, id};
use erpcraft_kernel::outbox::{self, Delivered};
use serde_json::{Value, json};

/// The setting is read from the environment when a server starts, and the
/// environment belongs to the whole test process: one start at a time.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A server that lets webhooks point at this machine, or does not.
async fn start(allow_private: bool) -> TestApp {
    let _one_at_a_time = STARTING.lock().await;
    // SAFETY: every test of this file starts its server here, under the
    // lock, and nothing else in the process reads this variable.
    unsafe {
        if allow_private {
            std::env::set_var(erpcraft_webhooks::ALLOW_PRIVATE, "1");
        } else {
            std::env::remove_var(erpcraft_webhooks::ALLOW_PRIVATE);
        }
    }
    TestApp::start().await
}

/// An endpoint in this process that remembers what it was sent.
#[derive(Clone)]
struct Receiver {
    url: String,
    requests: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>,
    /// What it answers.
    status: Arc<AtomicU16>,
}

async fn receive(State(r): State<Receiver>, headers: HeaderMap, body: Bytes) -> StatusCode {
    r.requests.lock().unwrap().push((headers, body));
    StatusCode::from_u16(r.status.load(Ordering::SeqCst)).unwrap()
}

impl Receiver {
    async fn start() -> Receiver {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let receiver = Receiver {
            url: format!("http://{}/hook", listener.local_addr().unwrap()),
            requests: Arc::default(),
            status: Arc::new(AtomicU16::new(200)),
        };
        let app = Router::new()
            .route("/hook", post(receive))
            .with_state(receiver.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        receiver
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The bodies received so far, parsed.
    fn bodies(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, body)| serde_json::from_slice(body).unwrap())
            .collect()
    }

    fn topics(&self) -> Vec<String> {
        self.bodies()
            .iter()
            .map(|b| b["topic"].as_str().unwrap().to_owned())
            .collect()
    }

    fn forget(&self) {
        self.requests.lock().unwrap().clear();
    }
}

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

async fn webhook(t: &TestApp, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, "webhook.create", input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn deliver(t: &TestApp) -> Delivered {
    outbox::deliver_batch(&t.kernel, 100).await.unwrap()
}

/// Make every waiting event due now.
async fn due(t: &TestApp) {
    sqlx::query("UPDATE outbox_event SET next_attempt_at = now() WHERE published_at IS NULL")
        .execute(&t.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_event_is_posted_signed_to_the_webhooks_that_want_it() {
    let t = start(true).await;
    let c = t.company("ACME", "USD").await;
    let other = t.company("OTHER", "USD").await;
    let (all, journals, closes, acme, off) = (
        Receiver::start().await,
        Receiver::start().await,
        Receiver::start().await,
        Receiver::start().await,
        Receiver::start().await,
    );

    // No topics: every event. A secret is generated when none is given,
    // and returned this once.
    let created = webhook(&t, json!({ "name": "everything", "url": all.url })).await;
    let secret = created["secret"].as_str().unwrap().to_owned();
    assert!(
        secret.starts_with("whsec_") && secret.len() > 60,
        "{secret}"
    );
    assert_eq!(created["active"], true);
    assert_eq!(created["topics"], json!([]));
    webhook(
        &t,
        json!({ "name": "journals", "url": journals.url, "topics": ["journal_entry.*"],
                "secret": "agreed-between-us-0001" }),
    )
    .await;
    webhook(
        &t,
        json!({ "name": "closes", "url": closes.url, "topics": ["fiscal_period.close", "fiscal_period.close"] }),
    )
    .await;
    webhook(
        &t,
        json!({ "name": "acme only", "url": acme.url, "company_id": c.id }),
    )
    .await;
    webhook(
        &t,
        json!({ "name": "switched off", "url": off.url, "active": false }),
    )
    .await;

    // What has happened so far goes out. The events of the commands that
    // handle secrets say only that they ran, and by whom.
    let delivered = deliver(&t).await;
    assert_eq!(delivered.failed, 0);
    let about_webhooks: Vec<Value> = all
        .bodies()
        .into_iter()
        .filter(|b| b["topic"] == "webhook.create")
        .collect();
    assert_eq!(about_webhooks.len(), 5);
    for body in &about_webhooks {
        let payload = body["payload"].as_object().unwrap();
        assert_eq!(
            payload.keys().collect::<Vec<_>>(),
            ["command", "user_id"],
            "{body}"
        );
    }
    for receiver in [&all, &journals, &closes, &acme, &off] {
        receiver.forget();
    }

    // A journal entry is drafted and posted in one company, and a record
    // is created in each company and in none.
    let draft = t.draft(&t.admin, &c, "10.00").await;
    let (status, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    t.create(
        "account",
        json!({ "company_id": other.id, "code": "1100", "name": "Till", "account_type": "asset" }),
    )
    .await;
    t.create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    assert_eq!(
        deliver(&t).await,
        Delivered {
            published: 4,
            failed: 0
        }
    );

    assert_eq!(
        all.topics(),
        [
            "document.create",
            "journal_entry.post",
            "entity.create",
            "entity.create"
        ]
    );
    assert_eq!(journals.topics(), ["journal_entry.post"], "a prefix with *");
    assert_eq!(closes.count(), 0, "an exact topic that did not occur");
    assert_eq!(
        acme.topics(),
        ["document.create", "journal_entry.post"],
        "only events that name its company"
    );
    assert_eq!(off.count(), 0, "an inactive webhook gets nothing");

    // The request: the event as the outbox holds it, and three headers.
    let (headers, raw) = journals.requests.lock().unwrap()[0].clone();
    let body: Value = serde_json::from_slice(&raw).unwrap();
    let event: (uuid::Uuid, Value) =
        sqlx::query_as("SELECT id, payload FROM outbox_event WHERE topic = 'journal_entry.post'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    // JSON objects keep their keys in the order written; which keys is
    // what counts.
    let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["created_at", "id", "payload", "topic"]);
    assert_eq!(body["id"], json!(event.0));
    assert_eq!(body["topic"], "journal_entry.post");
    assert_eq!(body["payload"], event.1);
    assert_eq!(body["payload"]["command"], "journal_entry.post");
    assert_eq!(body["payload"]["output"]["status"], "posted");
    assert!(body["created_at"].as_str().unwrap().starts_with("20"));
    let header = |name: &str| headers[name].to_str().unwrap().to_owned();
    assert_eq!(header("content-type"), "application/json");
    assert_eq!(header("x-erpcraft-event"), "journal_entry.post");
    assert_eq!(header("x-erpcraft-delivery"), event.0.to_string());
    // The signature is over the bytes as sent, under that webhook's secret.
    assert_eq!(
        header("x-erpcraft-signature"),
        erpcraft_webhooks::signature("agreed-between-us-0001", &raw)
    );
    let (headers, raw) = all.requests.lock().unwrap()[1].clone();
    assert_eq!(
        headers["x-erpcraft-signature"].to_str().unwrap(),
        erpcraft_webhooks::signature(&secret, &raw)
    );
    assert_ne!(
        headers["x-erpcraft-signature"].to_str().unwrap(),
        erpcraft_webhooks::signature("agreed-between-us-0001", &raw)
    );

    // Delivered once: another pass sends nothing.
    assert_eq!(deliver(&t).await, Delivered::default());
    assert_eq!(all.count(), 4);

    // What a webhook listens to can be changed; a topic no command has is
    // refused, because nothing would ever match it.
    let (status, body) = t
        .command(
            &t.admin,
            "webhook.update",
            json!({ "id": created["id"], "topics": ["entity.*"], "company_id": null, "name": "records" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["topics"], json!(["entity.*"]));
    assert_eq!(body["name"], "records");
    assert_eq!(body["url"], created["url"], "what was left out stays");
    for bad in ["journal_entry.psot", "Journal Entry", ""] {
        let (status, body) = t
            .command(
                &t.admin,
                "webhook.update",
                json!({ "id": created["id"], "topics": [bad] }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let (status, body) = t
        .command(
            &t.admin,
            "webhook.create",
            json!({ "name": "records", "url": all.url }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(message(&body), "a webhook with this name already exists");

    // A deleted webhook is gone with what was noted about it.
    let (status, body) = t
        .command(&t.admin, "webhook.delete", json!({ "id": created["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        t.count(&format!(
            "SELECT (SELECT count(*) FROM webhook_secret WHERE webhook_id = '{0}')
                  + (SELECT count(*) FROM webhook_delivery WHERE webhook_id = '{0}')",
            created["id"].as_str().unwrap()
        ))
        .await,
        0
    );

    t.stop().await;
}

#[tokio::test]
async fn a_failing_webhook_is_retried_without_sending_again_to_the_others() {
    let t = start(true).await;
    let (healthy, broken) = (Receiver::start().await, Receiver::start().await);
    // An address where nothing listens any more.
    let nobody = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}/hook", listener.local_addr().unwrap())
    };
    let topics = json!(["entity.create"]);
    webhook(
        &t,
        json!({ "name": "healthy", "url": healthy.url, "topics": topics }),
    )
    .await;
    let failing = webhook(
        &t,
        json!({ "name": "broken", "url": broken.url, "topics": topics }),
    )
    .await;
    let dead = webhook(
        &t,
        json!({ "name": "dead", "url": nobody, "topics": topics }),
    )
    .await;
    assert_eq!(deliver(&t).await.failed, 0);
    broken.status.store(500, Ordering::SeqCst);

    t.create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    assert_eq!(
        deliver(&t).await,
        Delivered {
            published: 0,
            failed: 1
        }
    );
    // Every webhook was tried, whatever the others did.
    assert_eq!((healthy.count(), broken.count()), (1, 1));

    // The outbox says which webhooks failed and why, by name.
    let (status, waiting) = t.command(&t.admin, "outbox.list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{waiting}");
    assert_eq!(waiting["total"], 1);
    let why = waiting["items"][0]["last_error"].as_str().unwrap();
    assert!(
        why.starts_with("webhooks: internal error: 2 of 3 webhooks failed: "),
        "{why}"
    );
    assert!(
        why.contains("broken: the endpoint answered 500 Internal Server Error"),
        "{why}"
    );
    assert!(why.contains("dead: "), "{why}");
    assert!(!why.contains("127.0.0.1"), "no URL in an error: {why}");

    // So does the webhook itself, and the record of its deliveries.
    let read = |webhook: &Value| {
        let (t, path) = (&t, format!("/api/entities/webhook/{}", id(webhook)));
        async move { t.get(&t.admin, &path).await.1 }
    };
    let seen = read(&failing).await;
    assert_eq!(seen["last_status"], 500);
    assert_eq!(
        seen["last_error"],
        "the endpoint answered 500 Internal Server Error"
    );
    assert!(seen["failed_at"].is_string());
    let seen = read(&dead).await;
    assert_eq!(seen["last_status"], Value::Null, "it never answered");
    assert!(seen["last_error"].is_string());
    let deliveries = |webhook: &Value, outcome: &'static str| {
        let (t, webhook_id) = (&t, webhook["id"].clone());
        async move {
            let (status, page) = t
                .command(
                    &t.admin,
                    "webhook.deliveries",
                    json!({ "webhook_id": webhook_id, "outcome": outcome }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{page}");
            page
        }
    };

    // Due again, it fails again: only the two that failed are sent it.
    due(&t).await;
    assert_eq!(deliver(&t).await.failed, 1);
    assert_eq!((healthy.count(), broken.count()), (1, 2));
    let failed = deliveries(&failing, "failed").await;
    assert_eq!(failed["total"], 1);
    assert_eq!(failed["items"][0]["topic"], "entity.create");
    assert_eq!(failed["items"][0]["attempts"], 2);
    assert_eq!(failed["items"][0]["status_code"], 500);
    assert_eq!(failed["items"][0]["delivered_at"], Value::Null);
    assert_eq!(deliveries(&failing, "delivered").await["total"], 0);

    // The endpoint recovers. The one nobody answers is switched off, which
    // stops the attempts: an inactive webhook is not waited for.
    broken.status.store(204, Ordering::SeqCst);
    let (status, body) = t
        .command(
            &t.admin,
            "webhook.update",
            json!({ "id": dead["id"], "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    due(&t).await;
    let delivered = deliver(&t).await;
    assert_eq!(
        (delivered.failed, healthy.count(), broken.count()),
        (0, 1, 3)
    );
    let seen = read(&failing).await;
    assert_eq!(seen["last_error"], Value::Null, "{seen}");
    assert_eq!(seen["last_status"], Value::Null);
    let arrived = deliveries(&failing, "all").await;
    assert_eq!(arrived["items"][0]["attempts"], 3);
    assert_eq!(arrived["items"][0]["status_code"], 204);
    assert!(arrived["items"][0]["delivered_at"].is_string());
    assert_eq!(arrived["items"][0]["error"], Value::Null);
    assert_eq!(deliver(&t).await, Delivered::default());

    t.stop().await;
}

#[tokio::test]
async fn the_secret_is_shown_once_and_recorded_nowhere() {
    let t = start(true).await;
    let receiver = Receiver::start().await;
    let (given, replaced) = ("first-secret-of-the-hook", "second-secret-of-the-hook");

    // Created with a secret and an idempotency key, then given another
    // secret, then a generated one.
    let (status, created) = t
        .send(
            "POST",
            "/api/commands/webhook.create",
            Some(&t.admin),
            Some(json!({ "name": "hook", "url": receiver.url, "secret": given })),
            &[("idempotency-key", "hook-1")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["secret"], given);
    let (status, body) = t
        .command(
            &t.admin,
            "webhook.set_secret",
            json!({ "id": created["id"], "secret": replaced }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["secret"],
        Value::Null,
        "a secret sent in is not sent back"
    );
    let (status, body) = t
        .command(
            &t.admin,
            "webhook.set_secret",
            json!({ "id": created["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let generated = body["secret"].as_str().unwrap().to_owned();
    // A refused command is recorded too, and must not carry its input.
    let (status, body) = t
        .command(
            &t.admin,
            "webhook.set_secret",
            json!({ "id": created["id"], "secret": "short" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("at least 16 characters"), "{body}");

    // Requests are signed with the secret in force when they are sent.
    t.create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    assert_eq!(deliver(&t).await.failed, 0);
    let sent = receiver.requests.lock().unwrap().clone();
    assert!(!sent.is_empty());
    for (headers, raw) in &sent {
        assert_eq!(
            headers["x-erpcraft-signature"].to_str().unwrap(),
            erpcraft_webhooks::signature(&generated, raw)
        );
    }

    // No reader is shown a secret, and the entity cannot be written around
    // the commands.
    let path = format!("/api/entities/webhook/{}", id(&created));
    let (status, one) = t.get(&t.admin, &path).await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["name"], "hook");
    let (_, list) = t.get(&t.admin, "/api/entities/webhook").await;
    let (_, deliveries) = t
        .command(
            &t.admin,
            "webhook.deliveries",
            json!({ "webhook_id": created["id"] }),
        )
        .await;
    assert!(deliveries["total"].as_i64().unwrap() > 0);
    let (_, history) = t
        .command(
            &t.admin,
            "audit.history",
            json!({ "resource": "webhook", "id": created["id"] }),
        )
        .await;
    let mut shown = format!("{one}{list}{deliveries}{history}");
    for (_, raw) in &sent {
        shown.push_str(&String::from_utf8_lossy(raw));
    }
    for (status, body) in [
        t.post(
            &t.admin,
            "/api/entities/webhook",
            json!({ "name": "x", "url": receiver.url }),
        )
        .await,
        t.patch(&t.admin, &path, json!({ "active": false })).await,
        t.patch(&t.admin, &path, json!({ "url": "https://example.com/" }))
            .await,
    ] {
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(message(&body), "webhook records cannot be written directly");
    }

    // Nor is one written down anywhere but where it is kept.
    let recorded: String = sqlx::query_scalar(
        "SELECT concat(
            (SELECT string_agg(a::text, ' ') FROM audit_log a),
            (SELECT string_agg(o::text, ' ') FROM outbox_event o),
            (SELECT string_agg(k::text, ' ') FROM idempotency_key k),
            (SELECT string_agg(w::text, ' ') FROM webhook w),
            (SELECT string_agg(d::text, ' ') FROM webhook_delivery d))",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert!(recorded.contains("webhook.set_secret"), "it was audited");
    for secret in [given, replaced, generated.as_str(), "short"] {
        assert!(!shown.contains(secret), "{secret} was shown");
        assert!(!recorded.contains(secret), "{secret} was recorded");
    }
    assert!(!shown.contains("\"secret\""), "no such field: {shown}");
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM webhook_secret WHERE secret = '{generated}'"
        ))
        .await,
        1
    );
    // The same idempotency key does not replay a command that handles
    // secrets: nothing of it was stored to replay.
    assert_eq!(
        t.count("SELECT count(*) FROM idempotency_key WHERE key = 'hook-1'")
            .await,
        0
    );

    // Webhooks are shared by all companies: a grant for one is not enough
    // to manage them.
    let c = t.company("ACME", "USD").await;
    let local = t
        .user_with_role("local@example.com", &["webhook:*"], &c.id)
        .await;
    for (command, input) in [
        (
            "webhook.create",
            json!({ "name": "mine", "url": receiver.url }),
        ),
        ("webhook.set_secret", json!({ "id": created["id"] })),
        (
            "webhook.update",
            json!({ "id": created["id"], "active": false }),
        ),
        ("webhook.delete", json!({ "id": created["id"] })),
        ("webhook.deliveries", json!({ "webhook_id": created["id"] })),
    ] {
        let (status, body) = t.command(&local, command, input).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{command}: {body}");
    }

    t.stop().await;
}

#[tokio::test]
async fn a_webhook_cannot_point_into_the_private_network() {
    /// The one refusal for every address that is not public, and for a
    /// name that leads to one or to nothing.
    const INWARD: &str = "the URL does not lead to a public address";
    // A server as in production: the environment does not allow it.
    let t = start(false).await;
    let receiver = Receiver::start().await;

    for (url, why) in [
        (receiver.url.as_str(), INWARD),
        ("http://localhost:8080/hook", INWARD),
        ("https://localhost/hook", INWARD),
        ("https://10.0.0.8/hook", INWARD),
        ("https://192.168.1.20:8443/hook", INWARD),
        ("https://169.254.169.254/latest/meta-data", INWARD),
        ("https://[::1]/hook", INWARD),
        ("https://[fd00::1]/hook", INWARD),
        ("https://[::ffff:127.0.0.1]/hook", INWARD),
        ("https://[::ffff:a9fe:a9fe]/latest", INWARD),
        ("https://[64:ff9b::10.0.0.8]/hook", INWARD),
        ("https://[2002:a00:8::1]/hook", INWARD),
        (
            "https://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/hook",
            INWARD,
        ),
        ("https://no-such-host.invalid/hook", INWARD),
        ("http://example.com/hook", "must use https"),
        (
            "https://user:password@example.com/hook",
            "user name or password",
        ),
        ("file:///etc/passwd", "needs a host"),
        ("not a url", "not valid"),
    ] {
        let (status, body) = t
            .command(
                &t.admin,
                "webhook.create",
                json!({ "name": "inward", "url": url }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{url}: {body}");
        assert!(message(&body).contains(why), "{url}: {body}");
    }
    assert_eq!(t.count("SELECT count(*) FROM webhook").await, 0);

    // A name that leads inward and a name that leads nowhere get the same
    // words, which name no address: whoever may save a webhook learns
    // nothing about the names inside the network.
    let mut said = Vec::new();
    for url in [
        "https://localhost/hook",
        "https://no-such-host.invalid/hook",
        "https://127.0.0.1/hook",
    ] {
        let (_, body) = t
            .command(
                &t.admin,
                "webhook.create",
                json!({ "name": "inward", "url": url }),
            )
            .await;
        let message = message(&body).to_owned();
        for detail in ["127.", "::1", "found", "resolve", "localhost", ".invalid"] {
            assert!(!message.contains(detail), "{url}: {message}");
        }
        said.push(message);
    }
    assert!(said.iter().all(|m| m == &said[0]), "{said:?}");

    // The address is checked again when an event is sent: a webhook that
    // got into the table some other way is not called either.
    let webhook_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO webhook (id, name, url) VALUES ($1, 'inward', $2)")
        .bind(webhook_id)
        .bind(&receiver.url)
        .execute(&t.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO webhook_secret VALUES ($1, 'a-secret-put-there-by-hand')")
        .bind(webhook_id)
        .execute(&t.pool)
        .await
        .unwrap();
    t.create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    assert_eq!(
        deliver(&t).await,
        Delivered {
            published: 0,
            failed: 1
        }
    );
    assert_eq!(receiver.count(), 0, "nothing was sent");
    let (_, seen) = t
        .get(&t.admin, &format!("/api/entities/webhook/{webhook_id}"))
        .await;
    assert!(
        seen["last_error"].as_str().unwrap().contains(INWARD),
        "{seen}"
    );
    // The same for a name that leads there.
    sqlx::query("UPDATE webhook SET url = $2 WHERE id = $1")
        .bind(webhook_id)
        .bind(receiver.url.replace("127.0.0.1", "localhost"))
        .execute(&t.pool)
        .await
        .unwrap();
    due(&t).await;
    assert_eq!(deliver(&t).await.failed, 1);
    assert_eq!(receiver.count(), 0, "nothing was sent");

    t.stop().await;
}
