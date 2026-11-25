//! Physical inventory and cycle counts: the book frozen at a cut-off,
//! counted (blind or not), recounted where too far off, the differences
//! adjusted; ABC classes and a schedule that proposes what to count.

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
async fn a_count_is_taken_at_its_cut_off_while_goods_keep_moving() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    for (item, place, qty) in [(&f.bolt, &f.main, "10"), (&f.nut, &f.bin, "4")] {
        let (status, body) = f
            .adjust(&t, item, place, qty, Some("2.00"), json!([]))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    // The warehouse with its bin: a line each, at the book.
    let count = f.count(&t, json!({ "warehouse_id": f.main })).await;
    assert_eq!(count["status"], "counting");
    assert_eq!(
        count["body"]["lines"].as_array().unwrap().len(),
        2,
        "{count}"
    );
    assert_eq!(dec(&line(&count, &f.bolt)["book_qty"]), 10.0);
    assert_eq!(dec(&line(&count, &f.nut)["book_qty"]), 4.0);

    // Two bolts leave while the count is on; the shelf held 10 at the
    // cut-off, but only 7 were really there: 3 missing.
    let (status, body) = f.adjust(&t, &f.bolt, &f.main, "-2", None, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, saved) = f
        .save(&t, &count, |lines| {
            for l in lines.iter_mut() {
                l["counted_qty"] = json!(if l["id"] == line(&count, &f.bolt)["id"] {
                    "7"
                } else {
                    "4"
                });
            }
        })
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(dec(&line(&saved, &f.bolt)["moved_since"]), -2.0, "{saved}");
    assert_eq!(dec(&line(&saved, &f.bolt)["difference"]), -3.0, "{saved}");
    assert_eq!(saved["summary"]["differences"], 1);

    // Lines are not removed from a count in progress.
    let (status, _) = f
        .save(&t, &saved, |lines| {
            lines.pop();
        })
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let posted = f
        .ok(&t, "stock_count.post", json!({ "id": id(&saved) }))
        .await;
    assert_eq!(posted["status"], "posted");
    // 10 - 2 moved - 3 missing; the 3 at 2.00 on the variance account.
    assert_eq!(f.level(&t, &f.bolt, &f.main).await, 5.0);
    assert_eq!(f.balance(&t, &f.variance).await, 6.0);
    let adjustment = posted["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["doc_type"] == "stock_adjustment")
        .unwrap();
    assert_eq!(adjustment["status"], "posted", "{posted}");
    t.stop().await;
}

#[tokio::test]
async fn a_company_that_freezes_its_warehouse_refuses_movements_during_a_count() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.adjust(&t, &f.bolt, &f.main, "10", Some("1.00"), json!([]))
        .await;
    f.adjust(&t, &f.nut, &f.main, "10", Some("1.00"), json!([]))
        .await;
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "stock.count_movement", "effect": "refuse",
                "condition": null, "message": "the warehouse is being counted" }),
    )
    .await;
    // A cycle count of bolts only, blind.
    let count = f
        .count(
            &t,
            json!({ "warehouse_id": f.main, "item_ids": [f.bolt], "blind": true }),
        )
        .await;
    let lines = count["body"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1);
    assert!(lines[0]["book_qty"].is_null(), "blind: {count}");
    let (status, body) = f.adjust(&t, &f.bolt, &f.main, "-1", None, json!([])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("being counted"), "{body}");
    // Nuts are not counted, and move.
    let (status, body) = f.adjust(&t, &f.nut, &f.main, "-1", None, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Cancelled: the bolts move again.
    f.ok(&t, "stock_count.cancel", json!({ "id": id(&count) }))
        .await;
    let (status, body) = f.adjust(&t, &f.bolt, &f.main, "-1", None, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_line_too_far_off_is_counted_again() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.adjust(&t, &f.bolt, &f.main, "100", Some("1.00"), json!([]))
        .await;
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "stock_count.tolerance", "effect": "refuse",
                "condition": "facts.difference_percent > 5" }),
    )
    .await;
    let count = f.count(&t, json!({ "warehouse_id": f.main })).await;
    let (_, saved) = f
        .save(&t, &count, |lines| lines[0]["counted_qty"] = json!("80"))
        .await;
    // Not posted while 20 % off.
    let (status, body) = t
        .command(&t.admin, "stock_count.post", json!({ "id": id(&saved) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("counted again"), "{body}");
    let reviewed = f
        .ok(&t, "stock_count.review", json!({ "id": id(&saved) }))
        .await;
    assert_eq!(reviewed["body"]["lines"][0]["recount"], true, "{reviewed}");
    // The recount stands, however far off it still is.
    let (status, recounted) = f
        .save(&t, &reviewed, |lines| lines[0]["recount_qty"] = json!("90"))
        .await;
    assert_eq!(status, StatusCode::OK, "{recounted}");
    f.ok(&t, "stock_count.post", json!({ "id": id(&recounted) }))
        .await;
    assert_eq!(f.level(&t, &f.bolt, &f.main).await, 90.0);
    assert_eq!(f.balance(&t, &f.variance).await, 10.0);
    t.stop().await;
}

#[tokio::test]
async fn lots_are_counted_one_by_one_and_lots_found_come_in() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "10",
            Some("1.00"),
            json!([{ "lot": "F-1", "qty": "6" }, { "lot": "F-2", "qty": "4" }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let count = f.count(&t, json!({ "warehouse_id": f.main })).await;
    let lots: Vec<&str> = count["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["lot"].as_str().unwrap())
        .collect();
    assert_eq!(lots, vec!["F-1", "F-2"]);
    // F-1 all there, F-2 short by 1, and a sack of F-3 nobody booked.
    let (status, saved) = f
        .save(&t, &count, |lines| {
            lines[0]["counted_qty"] = json!("6");
            lines[1]["counted_qty"] = json!("3");
            lines.push(
                json!({ "item_id": f.flour, "warehouse_id": f.main, "lot": "F-3",
                               "counted_qty": "2" }),
            );
        })
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["body"]["lines"].as_array().unwrap().len(), 3);
    f.ok(&t, "stock_count.post", json!({ "id": id(&saved) }))
        .await;
    let (_, lots) = t
        .command(
            &t.admin,
            "stock.lots",
            json!({ "company_id": f.c.id, "item_id": f.flour }),
        )
        .await;
    let held: Vec<(String, f64)> = lots["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["lot"].as_str().unwrap().to_owned(), dec(&r["qty"])))
        .collect();
    assert_eq!(
        held,
        vec![
            ("F-1".into(), 6.0),
            ("F-2".into(), 3.0),
            ("F-3".into(), 2.0)
        ]
    );
    t.stop().await;
}

#[tokio::test]
async fn items_are_classed_abc_and_the_schedule_proposes_what_is_due() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Bolts worth 900, nuts 90, flour 10.
    f.adjust(&t, &f.bolt, &f.main, "90", Some("10.00"), json!([]))
        .await;
    f.adjust(&t, &f.nut, &f.bin, "90", Some("1.00"), json!([]))
        .await;
    f.adjust(
        &t,
        &f.flour,
        &f.main,
        "10",
        Some("1.00"),
        json!([{ "lot": "F-1", "qty": "10" }]),
    )
    .await;
    let classed = f
        .ok(
            &t,
            "stock.abc_classify",
            json!({ "company_id": f.c.id, "basis": "value" }),
        )
        .await;
    assert_eq!(
        (
            classed["a"].as_u64(),
            classed["b"].as_u64(),
            classed["c"].as_u64()
        ),
        (Some(1), Some(1), Some(1)),
        "{classed}"
    );
    let class_of = |item: &str| {
        classed["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["item_id"] == json!(item))
            .unwrap()["class"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        (class_of(&f.bolt), class_of(&f.nut), class_of(&f.flour)),
        ("A".into(), "B".into(), "C".into())
    );

    // A counted every 30 days, B every 90; C never.
    for (class, days) in [("A", 30), ("B", 90)] {
        t.create(
            "cycle_count_rule",
            json!({ "company_id": f.c.id, "class": class, "every_days": days }),
        )
        .await;
    }
    let propose = |date: &'static str, draft: bool| {
        let t = &t;
        let input = json!({ "company_id": f.c.id, "warehouse_id": f.main, "date": date,
                            "draft": draft });
        async move {
            let (status, body) = t.command(&t.admin, "stock_count.propose", input).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body
        }
    };
    let first = propose("2026-03-10", true).await;
    let due: Vec<&str> = first["due"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["item_code"].as_str().unwrap())
        .collect();
    assert_eq!(due, vec!["BOLT", "NUT"], "{first}");

    // Counted as drafted: a cycle count of those two only.
    let draft = first["draft"].clone();
    let started = f
        .ok(&t, "stock_count.start", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(started["body"]["lines"].as_array().unwrap().len(), 2);
    let (_, saved) = f
        .save(&t, &started, |lines| {
            for l in lines.iter_mut() {
                l["counted_qty"] = json!("90");
            }
        })
        .await;
    f.ok(&t, "stock_count.post", json!({ "id": id(&saved) }))
        .await;
    // Counted on 2026-03-10: nothing due a week later; bolts a month on.
    assert_eq!(propose("2026-03-17", false).await["due"], json!([]));
    let later = propose("2026-04-10", false).await;
    assert_eq!(later["due"][0]["item_code"], "BOLT", "{later}");
    assert_eq!(later["due"].as_array().unwrap().len(), 1);
    t.stop().await;
}
