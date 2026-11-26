//! Review 9 of the review-8 fixes: Ledger::before_reverse (kernel) and the
//! stock ledger's use of it to ask stock.count_movement before a reversal.
//!
//! An ordinary movement locks its levels and only then asks the count
//! policy (erpcraft_stock::post), so a count starting meanwhile waits for it
//! or is seen. before_reverse asks with no lock held on the levels; the
//! stock ledger locks them only later, in Ledger::reverse. A count that
//! starts in between is not seen, and the reversal moves a frozen level.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

struct Fixture {
    company: String,
    main: String,
    offset: String,
    bolt: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("ACME", "USD").await;
    let account = |code: &'static str, kind: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": code, "account_type": kind }),
            )
            .await
        }
    };
    let inventory = account("1300", "asset").await;
    let offset = account("6000", "expense").await;
    let variance = account("6200", "expense").await;
    t.inventory_rule(&c.id, &inventory).await;
    t.create(
        "account_rule",
        json!({ "company_id": c.id, "purpose": "inventory_count_variance", "account_id": variance }),
    )
    .await;
    let main = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let bolt = t
        .create("item", json!({ "code": "BOLT", "name": "Bolt" }))
        .await;
    Fixture {
        company: c.id,
        main,
        offset,
        bolt,
    }
}

impl Fixture {
    async fn adjust(&self, t: &TestApp, qty: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/stock_adjustment",
                json!({ "company_id": self.company, "doc_date": "2026-03-01", "body": {
                    "offset_account_id": self.offset, "lines": [{
                        "item_id": self.bolt, "warehouse_id": self.main, "qty": qty,
                        "unit_cost": "1.00" }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, posted) = t
            .command(
                &t.admin,
                "stock_adjustment.post",
                json!({ "id": id(&draft) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        posted
    }
}

/// The company refuses every movement during a count. A reversal of a
/// receipt used to ask the policy (no count yet), then wait on something else
/// (here: an account row another transaction holds, as any concurrent
/// writer could); a count of the level starts and commits meanwhile; the
/// reversal then took the frozen level's lock and moved it anyway. Now it
/// locks the levels first, and the count waits.
#[tokio::test]
async fn a_count_starting_while_a_reversal_is_under_way_waits_for_it() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.adjust(&t, "10").await;
    let extra = f.adjust(&t, "5").await;
    t.create(
        "policy_rule",
        json!({ "company_id": f.company, "point": "stock.count_movement", "effect": "refuse",
                "condition": null, "message": "the warehouse is being counted" }),
    )
    .await;
    let (status, count) = t
        .post(
            &t.admin,
            "/api/documents/stock_count",
            json!({ "company_id": f.company, "doc_date": "2026-03-10",
                    "body": { "warehouse_id": f.main } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{count}");

    // Something the reversal's general ledger entries need, held a while.
    let mut holder = t.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM account WHERE id = $1::uuid FOR UPDATE")
        .bind(&f.offset)
        .execute(&mut *holder)
        .await
        .unwrap();

    let reversal = t.command(
        &t.admin,
        "stock_adjustment.reverse",
        json!({ "id": id(&extra) }),
    );
    let counter = async {
        // The reversal has asked the count policy by now and waits.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let started = tokio::time::timeout(
            Duration::from_secs(10),
            t.command(&t.admin, "stock_count.start", json!({ "id": id(&count) })),
        )
        .await;
        holder.commit().await.unwrap();
        started
    };
    let ((status, body), started) = tokio::join!(reversal, counter);
    match started {
        // The reversal locked the levels before it asked the policy, as a
        // post does: the count waited for it, and the reversal went
        // through before any count began.
        Err(_) => assert_eq!(status, StatusCode::OK, "{body}"),
        // Had the count started first, the reversal must have been
        // refused for it.
        Ok((count_status, count_body)) => {
            assert_eq!(count_status, StatusCode::OK, "{count_body}");
            assert_ne!(
                status,
                StatusCode::OK,
                "the count started (and froze the level) before the reversal wrote; the \
                 policy refuses every movement during a count, yet the reversal moved the \
                 level: {body}"
            );
        }
    }
    t.stop().await;
}
