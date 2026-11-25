//! Review 8: stock counts racing each other and the stock ledger.
#![allow(dead_code)]

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    main: String,
    bin: String,
    offset: String,
    variance: String,
    bolt: String,
    nut: String,
    flour: String,
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
    let bin = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN-A1", "name": "Bin A1", "parent_id": main }),
        )
        .await;
    let bolt = t
        .create("item", json!({ "code": "BOLT", "name": "Bolt" }))
        .await;
    let nut = t
        .create("item", json!({ "code": "NUT", "name": "Nut" }))
        .await;
    let flour = t
        .create(
            "item",
            json!({ "code": "FLOUR", "name": "Flour", "tracking": "lot" }),
        )
        .await;
    Fixture {
        c,
        main,
        bin,
        offset,
        variance,
        bolt,
        nut,
        flour,
    }
}

impl Fixture {
    async fn ok(&self, t: &TestApp, command: &str, input: Value) -> Value {
        let (status, body) = t.command(&t.admin, command, input).await;
        assert_eq!(status, StatusCode::OK, "{command}: {body}");
        body
    }

    /// Stock in or out through a posted adjustment.
    async fn adjust(
        &self,
        t: &TestApp,
        item: &str,
        warehouse: &str,
        qty: &str,
        cost: Option<&str>,
        lots: Value,
    ) -> (StatusCode, Value) {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/stock_adjustment",
                json!({ "company_id": self.c.id, "doc_date": "2026-03-01", "body": {
                    "offset_account_id": self.offset, "lines": [{
                        "item_id": item, "warehouse_id": warehouse, "qty": qty,
                        "unit_cost": cost, "lots": lots }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        t.command(
            &t.admin,
            "stock_adjustment.post",
            json!({ "id": id(&draft) }),
        )
        .await
    }

    async fn count(&self, t: &TestApp, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/stock_count",
                json!({ "company_id": self.c.id, "doc_date": "2026-03-10", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        self.ok(t, "stock_count.start", json!({ "id": id(&draft) }))
            .await
    }

    /// Save counted quantities on a count in progress: `set` changes the
    /// lines of its body.
    async fn save(
        &self,
        t: &TestApp,
        count: &Value,
        set: impl FnOnce(&mut Vec<Value>),
    ) -> (StatusCode, Value) {
        let mut body = count["body"].clone();
        let mut lines: Vec<Value> = body["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                json!({ "id": l["id"], "counted_qty": l["counted_qty"],
                        "recount_qty": l["recount_qty"] })
            })
            .collect();
        set(&mut lines);
        body["lines"] = json!(lines);
        // Read-only: not sent back.
        body.as_object_mut().unwrap().remove("cut_off_seq");
        t.patch(
            &t.admin,
            &format!("/api/documents/stock_count/{}", id(count)),
            json!({ "version": count["version"], "body": body }),
        )
        .await
    }

    async fn level(&self, t: &TestApp, item: &str, warehouse: &str) -> f64 {
        let (_, body) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id, "item_id": item, "warehouse_id": warehouse }),
            )
            .await;
        body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["warehouse_id"] == json!(warehouse))
            .map(|r| dec(&r["qty"]))
            .sum()
    }

    async fn balance(&self, t: &TestApp, account: &str) -> f64 {
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["account_id"] == json!(account))
            .map(|r| dec(&r["debit"]) - dec(&r["credit"]))
            .sum()
    }
}

fn line<'a>(count: &'a Value, item: &str) -> &'a Value {
    count["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["item_id"] == json!(item))
        .unwrap()
}

fn message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn two_counts_of_one_level_adjust_the_same_difference_twice() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, body) = f
        .adjust(&t, &f.bolt, &f.main, "10", Some("1.00"), json!([]))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // A full count of the warehouse and a cycle count of bolts, both started
    // before either is posted: the second is not refused.
    let full = f.count(&t, json!({ "warehouse_id": f.main })).await;
    let cycle = f
        .count(&t, json!({ "warehouse_id": f.main, "item_ids": [f.bolt] }))
        .await;
    // Both find the 8 bolts that are really there.
    for count in [&full, &cycle] {
        let (status, saved) = f
            .save(&t, count, |lines| {
                for l in lines.iter_mut() {
                    l["counted_qty"] = json!("8");
                }
            })
            .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        f.ok(&t, "stock_count.post", json!({ "id": id(&saved) }))
            .await;
    }
    // 8 on the shelf; the book says 6: the 2 missing were taken out twice.
    assert_eq!(f.level(&t, &f.bolt, &f.main).await, 8.0);
    t.stop().await;
}

#[tokio::test]
async fn a_frozen_warehouse_still_takes_reversals_during_a_count() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, received) = f
        .adjust(&t, &f.bolt, &f.main, "10", Some("1.00"), json!([]))
        .await;
    assert_eq!(status, StatusCode::OK, "{received}");
    let (status, extra) = f
        .adjust(&t, &f.bolt, &f.main, "5", Some("1.00"), json!([]))
        .await;
    assert_eq!(status, StatusCode::OK, "{extra}");
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "stock.count_movement", "effect": "refuse",
                "condition": null, "message": "the warehouse is being counted" }),
    )
    .await;
    let _count = f.count(&t, json!({ "warehouse_id": f.main })).await;
    // A movement is refused ...
    let (status, body) = f.adjust(&t, &f.bolt, &f.main, "-1", None, json!([])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // ... but taking back a receipt moves the frozen level all the same.
    let (status, body) = t
        .command(
            &t.admin,
            "stock_adjustment.reverse",
            json!({ "id": id(&extra) }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a reversal moved goods on a frozen level: {body}"
    );
    t.stop().await;
}

#[tokio::test]
async fn propose_lists_what_another_company_has_on_hand() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // ACME's bolts in ACME's warehouse.
    let (status, body) = f
        .adjust(&t, &f.bolt, &f.main, "10", Some("1.00"), json!([]))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Another company, whose stock counter may read its own counts only.
    let other = t.company("OTHER", "USD").await;
    t.create(
        "cycle_count_rule",
        json!({ "company_id": other.id, "class": "C", "every_days": 30 }),
    )
    .await;
    let token = t
        .user_with_role("counter@other.test", &["stock_count:read"], &other.id)
        .await;
    let (status, body) = t
        .command(
            &token,
            "stock_count.propose",
            json!({ "company_id": other.id, "warehouse_id": f.main, "date": "2026-03-10" }),
        )
        .await;
    assert_ne!(
        (
            status,
            body["due"].as_array().map(|d| d.len()).unwrap_or(0) > 0
        ),
        (StatusCode::OK, true),
        "OTHER's user sees what ACME's warehouse holds: {body}"
    );
    t.stop().await;
}
