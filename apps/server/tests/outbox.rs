//! Outbox delivery to subscribers, and cleanup.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use common::TestApp;
use erpcraft_kernel::maintenance::{self, Retention};
use erpcraft_kernel::outbox::{self, Delivered};
use erpcraft_kernel::{Error, Event, Kernel, Module, Result, Subscriber, async_trait};
use serde_json::json;

/// Remembers every `entity.*` event it is handed.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<Event>>,
}

#[async_trait]
impl Subscriber for Recorder {
    fn name(&self) -> &'static str {
        "recorder"
    }

    fn wants(&self, topic: &str) -> bool {
        topic.starts_with("entity.")
    }

    async fn handle(&self, _kernel: &Kernel, event: &Event) -> Result<()> {
        self.seen.lock().unwrap().push(event.clone());
        Ok(())
    }
}

/// Fails as many times as it is told to, then succeeds.
#[derive(Default)]
struct Flaky {
    failures_left: AtomicUsize,
    handled: AtomicUsize,
}

#[async_trait]
impl Subscriber for Flaky {
    fn name(&self) -> &'static str {
        "flaky"
    }

    fn wants(&self, topic: &str) -> bool {
        topic == "entity.create"
    }

    async fn handle(&self, _kernel: &Kernel, _event: &Event) -> Result<()> {
        let failing = self
            .failures_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failing {
            return Err(Error::internal("the far end is down"));
        }
        self.handled.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Hooks {
    recorder: Arc<Recorder>,
    flaky: Arc<Flaky>,
}

impl Module for Hooks {
    fn name(&self) -> &'static str {
        "test_hooks"
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![self.recorder.clone(), self.flaky.clone()]
    }
}

async fn start() -> (TestApp, Arc<Recorder>, Arc<Flaky>) {
    let (recorder, flaky) = (Arc::new(Recorder::default()), Arc::new(Flaky::default()));
    let hooks = Hooks {
        recorder: recorder.clone(),
        flaky: flaky.clone(),
    };
    let t = TestApp::start_with(|builder| builder.module(hooks)).await;
    (t, recorder, flaky)
}

#[tokio::test]
async fn committed_changes_reach_the_subscribers_that_want_them() {
    let (t, recorder, flaky) = start().await;

    // Nothing has happened yet.
    assert_eq!(
        outbox::deliver_batch(&t.kernel, 100).await.unwrap(),
        Delivered::default()
    );

    // Five entity.create, one document.create, and a command that fails.
    let c = t.company("ACME", "USD").await;
    t.draft(&t.admin, &c, "10.00").await;
    let (status, _) = t
        .post(&t.admin, "/api/entities/account", json!({ "code": "x" }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let delivered = outbox::deliver_batch(&t.kernel, 100).await.unwrap();
    assert_eq!(
        delivered,
        Delivered {
            published: 6,
            failed: 0
        }
    );
    {
        let seen = recorder.seen.lock().unwrap();
        assert_eq!(seen.len(), 5, "only the topics it asked for");
        assert!(seen.iter().all(|e| e.topic == "entity.create"));
        assert_eq!(seen[0].payload["input"]["data"]["code"], "ACME");
        assert_eq!(seen[0].payload["output"]["code"], "ACME");
    }
    assert_eq!(flaky.handled.load(Ordering::SeqCst), 5);

    // Delivered once: a second pass finds nothing.
    assert_eq!(
        outbox::deliver_batch(&t.kernel, 100).await.unwrap(),
        Delivered::default()
    );
    assert_eq!(recorder.seen.lock().unwrap().len(), 5);

    t.stop().await;
}

#[tokio::test]
async fn a_failed_delivery_is_retried_without_repeating_the_ones_that_worked() {
    let (t, recorder, flaky) = start().await;
    flaky.failures_left.store(2, Ordering::SeqCst);
    t.create(
        "company",
        json!({ "code": "ACME", "name": "Acme", "base_currency": "USD" }),
    )
    .await;

    // The recorder gets the event; the flaky subscriber fails.
    let delivered = outbox::deliver_batch(&t.kernel, 100).await.unwrap();
    assert_eq!(
        delivered,
        Delivered {
            published: 0,
            failed: 1
        }
    );
    assert_eq!(recorder.seen.lock().unwrap().len(), 1);

    // It is visible as failing, with the reason, and is not due yet.
    let (status, failing) = t.command(&t.admin, "outbox.list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{failing}");
    assert_eq!(failing["total"], 1);
    assert_eq!(failing["items"][0]["attempts"], 1);
    assert_eq!(
        failing["items"][0]["last_error"],
        "flaky: internal error: the far end is down"
    );
    assert_eq!(
        outbox::deliver_batch(&t.kernel, 100).await.unwrap(),
        Delivered::default(),
        "it waits before the next attempt"
    );

    // When it is due again it fails once more, and the wait grows.
    let due = "UPDATE outbox_event SET next_attempt_at = now() WHERE published_at IS NULL";
    sqlx::query(due).execute(&t.pool).await.unwrap();
    assert_eq!(
        outbox::deliver_batch(&t.kernel, 100).await.unwrap().failed,
        1
    );
    let wait = t
        .count(
            "SELECT extract(epoch FROM next_attempt_at - now())::bigint FROM outbox_event
             WHERE published_at IS NULL",
        )
        .await;
    assert!(
        (15..=20).contains(&wait),
        "second wait is 20 seconds, got {wait}"
    );

    // The third attempt succeeds. The recorder is not called again.
    sqlx::query(due).execute(&t.pool).await.unwrap();
    let delivered = outbox::deliver_batch(&t.kernel, 100).await.unwrap();
    assert_eq!(
        delivered,
        Delivered {
            published: 1,
            failed: 0
        }
    );
    assert_eq!(flaky.handled.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.seen.lock().unwrap().len(), 1);
    let (_, failing) = t.command(&t.admin, "outbox.list", json!({})).await;
    assert_eq!(failing["total"], 0);
    let (_, published) = t
        .command(&t.admin, "outbox.list", json!({ "state": "published" }))
        .await;
    assert_eq!(published["total"], 1);
    assert_eq!(published["items"][0]["attempts"], 2);

    // The outbox is not open to everyone.
    let c = t.company("OTHER", "USD").await;
    let reader = t
        .user_with_role("reader@example.com", &["account:read"], &c.id)
        .await;
    let (status, _) = t.command(&reader, "outbox.list", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    t.stop().await;
}

#[tokio::test]
async fn expired_keys_and_old_events_are_cleaned_up() {
    let (t, _, _) = start().await;
    let company = json!({ "code": "ACME", "name": "Acme", "base_currency": "USD" });
    let (status, first) = t
        .send(
            "POST",
            "/api/entities/company",
            Some(&t.admin),
            Some(company.clone()),
            &[("idempotency-key", "k1")],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    t.create(
        "company",
        json!({ "code": "B", "name": "B", "base_currency": "USD" }),
    )
    .await;
    outbox::deliver_batch(&t.kernel, 100).await.unwrap();

    // Nothing is old enough yet.
    let cleaned = maintenance::clean(&t.kernel, Retention::default())
        .await
        .unwrap();
    assert_eq!((cleaned.idempotency_keys, cleaned.events), (0, 0));

    // Two days pass. One event is still undelivered.
    for sql in [
        "UPDATE idempotency_key SET created_at = now() - interval '2 days'",
        "UPDATE outbox_event SET published_at = now() - interval '2 days'
         WHERE payload->'input'->'data'->>'code' = 'ACME'",
        "UPDATE outbox_event SET published_at = NULL, created_at = now() - interval '2 days'
         WHERE payload->'input'->'data'->>'code' = 'B'",
    ] {
        sqlx::query(sql).execute(&t.pool).await.unwrap();
    }
    let retention = Retention {
        idempotency_hours: 24,
        published_event_hours: 24,
    };
    let cleaned = maintenance::clean(&t.kernel, retention).await.unwrap();
    assert_eq!((cleaned.idempotency_keys, cleaned.events), (1, 1));
    assert_eq!(
        t.count("SELECT count(*) FROM outbox_event").await,
        1,
        "an undelivered event is kept however old it is"
    );
    assert_eq!(
        t.count("SELECT count(*) FROM outbox_delivery").await,
        2,
        "the delivery notes of a removed event go with it; the other event keeps its two"
    );

    // The business record and its audit trail are untouched.
    assert_eq!(t.count("SELECT count(*) FROM company").await, 2);
    assert_eq!(
        t.count("SELECT count(*) FROM audit_log WHERE command = 'entity.create'")
            .await,
        2
    );

    t.stop().await;
}
