//! The stock ledger, through stock adjustments: quantities, moving average
//! value, and agreement with the general ledger.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec};
use serde_json::{Value, json};

struct Stock {
    c: Company,
    item: String,
    main: String,
    offset: String,
}

async fn setup(t: &TestApp) -> Stock {
    let c = t.company("ACME", "USD").await;
    let item = t
        .create(
            "item",
            json!({ "code": "EGG-L", "name": "Large eggs", "uom": "case" }),
        )
        .await;
    let main = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main cooler" }),
        )
        .await;
    let account = |code: &'static str, name: &'static str, kind: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": name, "account_type": kind }),
            )
            .await
        }
    };
    let inventory = account("1300", "Inventory", "asset").await;
    let offset = account("5100", "Stock adjustments", "expense").await;
    t.inventory_rule(&c.id, &inventory).await;
    Stock {
        c,
        item,
        main,
        offset,
    }
}

impl Stock {
    fn line(&self, qty: &str, unit_cost: Option<&str>) -> Value {
        json!({ "item_id": self.item, "warehouse_id": self.main, "qty": qty, "unit_cost": unit_cost })
    }

    async fn draft(&self, t: &TestApp, lines: Value) -> Value {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/documents/stock_adjustment",
                json!({
                    "company_id": self.c.id,
                    "doc_date": "2026-03-15",
                    "body": {
                        "offset_account_id": self.offset,
                        "lines": lines,
                    },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }

    /// Draft and post an adjustment; returns the response to the post.
    async fn adjust(&self, t: &TestApp, lines: Value) -> (StatusCode, Value) {
        let draft = self.draft(t, lines).await;
        t.command(
            &t.admin,
            "stock_adjustment.post",
            json!({ "id": common::id(&draft) }),
        )
        .await
    }

    /// Quantity and value on hand, and the inventory account's balance.
    async fn state(&self, t: &TestApp) -> (f64, f64, f64) {
        let (status, on_hand) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{on_hand}");
        let (qty, value) = match on_hand["rows"].as_array().unwrap().first() {
            Some(row) => (dec(&row["qty"]), dec(&row["value"])),
            None => (0.0, 0.0),
        };
        assert_eq!(dec(&on_hand["total_value"]), value);
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        let balance = tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["code"] == "1300")
            .map(|r| dec(&r["balance"]))
            .unwrap();
        (qty, value, balance)
    }
}

#[tokio::test]
async fn stock_is_valued_at_moving_average_and_agrees_with_the_books() {
    let t = TestApp::start().await;
    let s = setup(&t).await;

    // 10 at 2.00, then 10 at 4.00: 20 on hand worth 60, average 3.00.
    let (status, body) = s.adjust(&t, json!([s.line("10", Some("2.00"))])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "posted");
    assert_eq!(body["number"], "SA-2026-00001");
    let (status, body) = s.adjust(&t, json!([s.line("10", Some("4.00"))])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(s.state(&t).await, (20.0, 60.0, 60.0));

    // 5 go out. Nobody states their cost: they leave at 3.00 each.
    let (status, body) = s.adjust(&t, json!([s.line("-5", None)])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(s.state(&t).await, (15.0, 45.0, 45.0));

    let (_, entries) = t
        .get(&t.admin, "/api/entities/stock_entry?sort=created_at")
        .await;
    let issue = &entries["items"][2];
    assert_eq!(dec(&issue["qty"]), -5.0);
    assert_eq!(dec(&issue["value"]), -15.0);
    assert_eq!(dec(&issue["unit_cost"]), 3.0);
    assert_eq!(dec(&issue["qty_after"]), 15.0);
    assert_eq!(dec(&issue["value_after"]), 45.0);

    // The books: inventory 45 debit, the offset account 45 credit, and
    // each ledger entry points at the document line that caused it.
    let (_, tb) = t
        .command(
            &t.admin,
            "gl.trial_balance",
            json!({ "company_id": s.c.id }),
        )
        .await;
    assert_eq!(tb["total_debit"], tb["total_credit"]);
    let offset = tb["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"] == "5100")
        .unwrap();
    assert_eq!(dec(&offset["balance"]), -45.0);
    assert_eq!(
        t.count(
            "SELECT count(*) FROM gl_entry g JOIN stock_entry s
               ON s.posting_id = g.posting_id AND s.source_line_id = g.source_line_id"
        )
        .await,
        6
    );

    // One document can carry several movements; they apply in order.
    let (status, body) = s
        .adjust(
            &t,
            json!([
                s.line("-15", None),
                s.line("4", Some("1.00")),
                s.line("-1", None)
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(s.state(&t).await, (3.0, 3.0, 3.0));

    t.stop().await;
}

#[tokio::test]
async fn rounding_never_leaves_value_on_an_empty_shelf() {
    let t = TestApp::start().await;
    let s = setup(&t).await;

    // Three cases for 10.00 in all: 3.3333 each.
    let (status, body) = s.adjust(&t, json!([s.line("3", Some("3.3333"))])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(s.state(&t).await, (3.0, 10.0, 10.0));

    for expected in [(2.0, 6.67), (1.0, 3.33), (0.0, 0.0)] {
        let (status, body) = s.adjust(&t, json!([s.line("-1", None)])).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (qty, value, balance) = s.state(&t).await;
        assert_eq!((qty, value), expected);
        assert_eq!(balance, value, "the books follow the stock to the cent");
    }
    let issued = t
        .count("SELECT (sum(value) * 100)::bigint FROM stock_entry WHERE qty < 0")
        .await;
    assert_eq!(issued, -1000, "what went out is worth exactly what came in");

    t.stop().await;
}

#[tokio::test]
async fn stock_cannot_go_below_zero() {
    let t = TestApp::start().await;
    let s = setup(&t).await;
    let (status, _) = s.adjust(&t, json!([s.line("10", Some("2.00"))])).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = s.adjust(&t, json!([s.line("-11", None)])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["message"],
        "only 10 case of item EGG-L are on hand in warehouse MAIN"
    );
    // Nothing of the failed posting remains, in either ledger.
    assert_eq!(s.state(&t).await, (10.0, 20.0, 20.0));
    assert_eq!(t.count("SELECT count(*) FROM stock_entry").await, 1);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 1);

    // Two people each take 6 of the 10 at the same moment: one of them is
    // refused.
    let first = s.draft(&t, json!([s.line("-6", None)])).await;
    let second = s.draft(&t, json!([s.line("-6", None)])).await;
    let (a, b) = {
        let post = |draft: &Value| {
            let id = common::id(draft);
            let t = &t;
            async move {
                t.command(&t.admin, "stock_adjustment.post", json!({ "id": id }))
                    .await
            }
        };
        tokio::join!(post(&first), post(&second))
    };
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::UNPROCESSABLE_ENTITY],
        "{} / {}",
        a.1,
        b.1
    );
    assert_eq!(s.state(&t).await, (4.0, 8.0, 8.0));

    // The level cannot be changed behind the ledger's back, and entries
    // cannot be changed at all.
    for sql in [
        "UPDATE stock_level SET qty = 1000",
        "DELETE FROM stock_level",
        "UPDATE stock_entry SET qty = 1000",
        "DELETE FROM stock_entry",
    ] {
        assert!(sqlx::query(sql).execute(&t.pool).await.is_err(), "{sql}");
    }

    t.stop().await;
}

#[tokio::test]
async fn an_adjustment_is_reversed_in_both_ledgers_or_not_at_all() {
    let t = TestApp::start().await;
    let s = setup(&t).await;

    let (_, receipt) = s.adjust(&t, json!([s.line("10", Some("2.00"))])).await;
    let (_, issue) = s.adjust(&t, json!([s.line("-4", None)])).await;
    assert_eq!(s.state(&t).await, (6.0, 12.0, 12.0));
    let reverse = |doc: &Value| {
        let id = common::id(doc);
        let t = &t;
        async move {
            t.command(&t.admin, "stock_adjustment.reverse", json!({ "id": id }))
                .await
        }
    };

    // The receipt cannot be taken back: four of its ten are gone.
    let (status, body) = reverse(&receipt).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already moved on"),
        "{body}"
    );
    assert_eq!(s.state(&t).await, (6.0, 12.0, 12.0));

    // Reversing the issue puts the four back at what they left for.
    let (status, body) = reverse(&issue).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "reversed");
    assert_eq!(body["postings"].as_array().unwrap().len(), 2);
    assert_eq!(s.state(&t).await, (10.0, 20.0, 20.0));
    // Doing it again changes nothing.
    let (status, _) = reverse(&issue).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s.state(&t).await, (10.0, 20.0, 20.0));

    // Now the receipt can go too, leaving nothing anywhere.
    let (status, body) = reverse(&receipt).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(s.state(&t).await, (0.0, 0.0, 0.0));
    let (_, tb) = t
        .command(
            &t.admin,
            "gl.trial_balance",
            json!({ "company_id": s.c.id }),
        )
        .await;
    assert_eq!(tb["total_debit"], tb["total_credit"]);

    t.stop().await;
}

#[tokio::test]
async fn an_adjustment_is_checked_before_and_at_posting() {
    let t = TestApp::start().await;
    let s = setup(&t).await;
    let other = t.company("OTHER", "USD").await;
    let foreign = t
        .create(
            "warehouse",
            json!({ "company_id": other.id, "code": "FAR", "name": "Far away" }),
        )
        .await;
    let create = |body: Value| {
        let (t, company) = (&t, s.c.id.clone());
        async move {
            t.post(
                &t.admin,
                "/api/documents/stock_adjustment",
                json!({ "company_id": company, "doc_date": "2026-03-15", "body": body }),
            )
            .await
        }
    };

    // Refused as a draft already.
    for (lines, why) in [
        (json!([s.line("0", None)]), "zero"),
        (json!([s.line("5", None)]), "unit cost"),
        (json!([s.line("-5", Some("1.00"))]), "average cost"),
        (
            json!([{ "item_id": s.item, "warehouse_id": foreign, "qty": "1", "unit_cost": "1" }]),
            "warehouse of the document's company",
        ),
    ] {
        let (status, body) = create(json!({ "lines": lines })).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(why),
            "{body}"
        );
    }
    let (status, body) = create(json!({ "offset_account_id": other.cash, "lines": [] })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // A draft may lack its offset account, but cannot be posted without it.
    let (status, draft) = create(json!({ "lines": [s.line("5", Some("1.00"))] })).await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, body) = t
        .command(
            &t.admin,
            "stock_adjustment.post",
            json!({ "id": common::id(&draft) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // An inactive item takes no new movements.
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/item/{}", s.item),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = s.adjust(&t, json!([s.line("5", Some("1.00"))])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["message"], "item EGG-L is inactive");

    // Posting into a closed period fails as a whole: the stock ledger does
    // not move if the general ledger refuses.
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/item/{}", s.item),
            json!({ "active": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = t
        .command(&t.admin, "fiscal_period.close", json!({ "id": s.c.period }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = s.adjust(&t, json!([s.line("5", Some("1.00"))])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(t.count("SELECT count(*) FROM stock_entry").await, 0);
    assert_eq!(
        t.count("SELECT count(*) FROM stock_level WHERE qty > 0")
            .await,
        0
    );

    // Who may see stock is a permission like any other.
    let reader = t
        .user_with_role("reader@example.com", &["account:read"], &s.c.id)
        .await;
    let (status, _) = t
        .command(&reader, "stock.on_hand", json!({ "company_id": s.c.id }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    t.stop().await;
}

/// Quantity and value of the test item on hand in one warehouse.
async fn level(t: &TestApp, s: &Stock, warehouse: &str) -> (f64, f64) {
    let (status, on_hand) = t
        .command(
            &t.admin,
            "stock.on_hand",
            json!({ "company_id": s.c.id, "warehouse_id": warehouse }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{on_hand}");
    match on_hand["rows"].as_array().unwrap().first() {
        Some(row) => (dec(&row["qty"]), dec(&row["value"])),
        None => (0.0, 0.0),
    }
}

#[tokio::test]
async fn a_transfer_moves_stock_and_exactly_its_value() {
    let t = TestApp::start().await;
    let s = setup(&t).await;
    let dock = t
        .create(
            "warehouse",
            json!({ "company_id": s.c.id, "code": "DOCK", "name": "Loading dock" }),
        )
        .await;
    let transfer = |from: String, to: String, qty: &'static str| {
        let (t, s) = (&t, &s);
        async move {
            let (status, draft) = t
                .post(
                    &t.admin,
                    "/api/documents/stock_transfer",
                    json!({
                        "company_id": s.c.id,
                        "doc_date": "2026-03-15",
                        "body": {
                            "from_warehouse_id": from,
                            "to_warehouse_id": to,
                            "lines": [{ "item_id": s.item, "qty": qty }],
                        },
                    }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{draft}");
            t.command(
                &t.admin,
                "stock_transfer.post",
                json!({ "id": common::id(&draft) }),
            )
            .await
        }
    };

    // Three cases worth 10.00 in the main cooler.
    let (status, _) = s.adjust(&t, json!([s.line("3", Some("3.3333"))])).await;
    assert_eq!(status, StatusCode::OK);

    // One case moves to the dock, with a third of the value, rounded; the
    // two halves still add up to the whole.
    let (status, first) = transfer(s.main.clone(), dock.clone(), "1").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["number"], "ST-2026-00001");
    assert_eq!(level(&t, &s, &s.main).await, (2.0, 6.67));
    assert_eq!(level(&t, &s, &dock).await, (1.0, 3.33));
    // The company's stock value has not changed, so neither have the books.
    let (_, tb) = t
        .command(
            &t.admin,
            "gl.trial_balance",
            json!({ "company_id": s.c.id }),
        )
        .await;
    assert_eq!(dec(&tb["total_debit"]), 10.0);

    // More than is there.
    let (status, body) = transfer(s.main.clone(), dock.clone(), "5").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(level(&t, &s, &s.main).await, (2.0, 6.67));

    // Two transfers of the same item in opposite directions at the same
    // moment both go through.
    let (a, b) = tokio::join!(
        transfer(s.main.clone(), dock.clone(), "1"),
        transfer(dock.clone(), s.main.clone(), "1"),
    );
    assert_eq!(
        (a.0, b.0),
        (StatusCode::OK, StatusCode::OK),
        "{} / {}",
        a.1,
        b.1
    );
    let (main, at_dock) = (level(&t, &s, &s.main).await, level(&t, &s, &dock).await);
    assert_eq!(main.0 + at_dock.0, 3.0);
    assert!(
        (main.1 + at_dock.1 - 10.0).abs() < 1e-9,
        "no value appears or vanishes"
    );

    // A transfer can be taken back while the stock is still where it went.
    let reverse = |doc: &Value| {
        let id = common::id(doc);
        let t = &t;
        async move {
            t.command(&t.admin, "stock_transfer.reverse", json!({ "id": id }))
                .await
        }
    };
    let (status, moved) = transfer(s.main.clone(), dock.clone(), "1").await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    let before = (level(&t, &s, &s.main).await, level(&t, &s, &dock).await);
    let (status, body) = reverse(&moved).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "reversed");
    assert_ne!(
        (level(&t, &s, &s.main).await, level(&t, &s, &dock).await),
        before
    );

    // Once the dock is empty, what went there cannot come back.
    let (_, moved) = transfer(s.main.clone(), dock.clone(), "1").await;
    let at_dock = level(&t, &s, &dock).await.0;
    let (status, body) = s
        .adjust(
            &t,
            json!([{ "item_id": s.item, "warehouse_id": dock, "qty": format!("-{at_dock}"), "unit_cost": null }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = reverse(&moved).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // A transfer stays inside one company and between two warehouses.
    let other = t.company("OTHER", "USD").await;
    let far = t
        .create(
            "warehouse",
            json!({ "company_id": other.id, "code": "FAR", "name": "Far away" }),
        )
        .await;
    for to in [far, s.main.clone()] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/documents/stock_transfer",
                json!({
                    "company_id": s.c.id,
                    "doc_date": "2026-03-15",
                    "body": { "from_warehouse_id": s.main, "to_warehouse_id": to, "lines": [] },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }

    t.stop().await;
}
