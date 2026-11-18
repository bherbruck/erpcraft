//! Manufacturing: bills of materials and production orders. Completing an
//! order consumes the components at moving average and values the product
//! at exactly what was consumed, in the stock ledger and in the books.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec};
use serde_json::{Value, json};

struct Plant {
    c: Company,
    /// Components.
    flour: String,
    sugar: String,
    /// The product.
    cake: String,
    main: String,
    /// One run makes 3 cakes from 6 flour and 3 sugar.
    bom: String,
    flour_line: String,
    materials: String,
    finished: String,
    offset: String,
}

async fn setup(t: &TestApp) -> Plant {
    let c = t.company("ACME", "USD").await;
    let item = |code: &'static str, name: &'static str| async move {
        t.create("item", json!({ "code": code, "name": name }))
            .await
    };
    let flour = item("FLOUR", "Flour").await;
    let sugar = item("SUGAR", "Sugar").await;
    let cake = item("CAKE", "Cake").await;
    let main = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Bakery" }),
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
    let materials = account("1300", "Materials", "asset").await;
    let finished = account("1310", "Finished goods", "asset").await;
    let offset = account("5100", "Stock adjustments", "expense").await;
    // Stock is carried on the materials account, except what is made.
    t.inventory_rule(&c.id, &materials).await;
    let bom = t
        .create(
            "bom",
            json!({ "item_id": cake, "code": "CAKE-1", "name": "Plain cake", "output_qty": "3" }),
        )
        .await;
    let p = Plant {
        c,
        flour,
        sugar,
        cake,
        main,
        bom,
        flour_line: String::new(),
        materials,
        finished,
        offset,
    };
    p.made(t, &p.cake).await;
    let flour_line = t
        .create(
            "bom_line",
            json!({ "bom_id": p.bom, "item_id": p.flour, "qty": "6" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": p.bom, "item_id": p.sugar, "qty": "3" }),
    )
    .await;
    Plant { flour_line, ..p }
}

impl Plant {
    /// Carry `item` on the finished goods account; returns the rule's id.
    async fn made(&self, t: &TestApp, item: &str) -> String {
        t.create(
            "account_rule",
            json!({
                "company_id": self.c.id, "purpose": "inventory",
                "scope_type": "item", "scope_id": item, "account_id": self.finished,
            }),
        )
        .await
    }

    /// Post a stock adjustment of one item against the materials account.
    async fn adjust(&self, t: &TestApp, item: &str, qty: &str, unit_cost: Option<&str>) {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/stock_adjustment",
                json!({
                    "company_id": self.c.id,
                    "doc_date": "2026-03-15",
                    "body": {
                        "offset_account_id": self.offset,
                        "lines": [{
                            "item_id": item, "warehouse_id": self.main,
                            "qty": qty, "unit_cost": unit_cost,
                        }],
                    },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, body) = t
            .command(
                &t.admin,
                "stock_adjustment.post",
                json!({ "id": common::id(&draft) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// 20 flour worth 60.00 (10 at 2.00, then 10 at 4.00: 3.00 on average)
    /// and 3 sugar worth 10.00.
    async fn stock_up(&self, t: &TestApp) {
        self.adjust(t, &self.flour, "10", Some("2.00")).await;
        self.adjust(t, &self.flour, "10", Some("4.00")).await;
        self.adjust(t, &self.sugar, "3", Some("3.3333")).await;
    }

    async fn draft_with(&self, t: &TestApp, bom: &str, qty: &str) -> (StatusCode, Value) {
        t.post(
            &t.admin,
            "/api/documents/production_order",
            json!({
                "company_id": self.c.id,
                "doc_date": "2026-03-15",
                "body": { "bom_id": bom, "qty": qty, "warehouse_id": self.main },
            }),
        )
        .await
    }

    async fn draft(&self, t: &TestApp, qty: &str) -> Value {
        let (status, body) = self.draft_with(t, &self.bom, qty).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }

    /// Draft and release an order for `qty` cakes.
    async fn released(&self, t: &TestApp, qty: &str) -> Value {
        let draft = self.draft(t, qty).await;
        let (status, body) = step(t, "release", &draft).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "released");
        body
    }

    /// Quantity on hand, and its value in cents, exactly.
    async fn level(&self, t: &TestApp, item: &str) -> (String, i64) {
        sqlx::query_as(&format!(
            "SELECT trim_scale(qty)::text, (value * 100)::bigint FROM stock_level
             WHERE item_id = '{item}' AND warehouse_id = '{}'",
            self.main
        ))
        .fetch_optional(&t.pool)
        .await
        .unwrap()
        .unwrap_or(("0".into(), 0))
    }

    /// Flour, sugar and cake on hand.
    async fn levels(&self, t: &TestApp) -> [(String, i64); 3] {
        [
            self.level(t, &self.flour).await,
            self.level(t, &self.sugar).await,
            self.level(t, &self.cake).await,
        ]
    }

    /// The balances of materials and finished goods; the trial balance
    /// must balance.
    async fn books(&self, t: &TestApp) -> (f64, f64) {
        let (status, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{tb}");
        assert_eq!(tb["total_debit"], tb["total_credit"]);
        let balance = |code: &str| {
            tb["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["code"] == code)
                .map(|r| dec(&r["balance"]))
                .unwrap_or(0.0)
        };
        (balance("1300"), balance("1310"))
    }
}

fn on_hand(qty: &str, cents: i64) -> (String, i64) {
    (qty.to_owned(), cents)
}

/// Run `production_order.<action>` on a document.
async fn step(t: &TestApp, action: &str, doc: &Value) -> (StatusCode, Value) {
    t.command(
        &t.admin,
        &format!("production_order.{action}"),
        json!({ "id": common::id(doc) }),
    )
    .await
}

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn completing_an_order_turns_components_into_a_product_of_the_same_value() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    p.stock_up(&t).await;
    assert_eq!(p.books(&t).await, (70.0, 0.0));

    // One cake takes a third of a run: 2 flour and 1 sugar.
    let order = p.released(&t, "1").await;
    assert_eq!(order["number"], "MO-2026-00001");
    assert_eq!(order["body"]["item_id"], p.cake.as_str());
    let components = order["body"]["components"].as_array().unwrap();
    assert_eq!(components.len(), 2);
    assert_eq!(components[0]["item_id"], p.flour.as_str());
    assert_eq!(dec(&components[0]["qty"]), 2.0);
    assert_eq!(components[1]["item_id"], p.sugar.as_str());
    assert_eq!(dec(&components[1]["qty"]), 1.0);
    // Releasing moves nothing.
    assert_eq!(t.count("SELECT count(*) FROM stock_entry").await, 3);

    // The flour leaves at its average of 3.00, not at either price paid:
    // 6.00. A third of the sugar's 10.00 is 3.33. The cake is worth 9.33.
    let (status, done) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["status"], "completed");
    assert_eq!(done["postings"].as_array().unwrap().len(), 1);
    assert_eq!(
        p.levels(&t).await,
        [on_hand("18", 5400), on_hand("2", 667), on_hand("1", 933)]
    );
    assert_eq!(p.books(&t).await, (60.67, 9.33));

    // One posting carries it all, in both ledgers.
    let posting = done["postings"][0]["id"].as_str().unwrap();
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM stock_entry WHERE posting_id = '{posting}'"
        ))
        .await,
        3
    );
    assert_eq!(
        t.count(&format!(
            "SELECT (sum(value) * 100)::bigint FROM stock_entry WHERE posting_id = '{posting}'"
        ))
        .await,
        0,
        "what the product came in for is what the components went out for"
    );
    // Each credit to materials points at the component that caused it.
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM gl_entry g
             JOIN stock_entry s ON s.posting_id = g.posting_id AND s.source_line_id = g.source_line_id
             JOIN production_order_component c ON c.id = g.source_line_id
             WHERE g.posting_id = '{posting}' AND g.credit = -s.value"
        ))
        .await,
        2
    );

    // Completing it again changes nothing.
    let (status, again) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["version"], done["version"]);
    assert_eq!(t.count("SELECT count(*) FROM stock_entry").await, 6);

    // Two more cakes take the rest of the sugar, and with it the rest of
    // its value: 6.67, not twice 3.33. Flour: 4 at 3.00.
    let order = p.released(&t, "2").await;
    let (status, done) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(
        p.levels(&t).await,
        [on_hand("14", 4200), on_hand("0", 0), on_hand("3", 2800)]
    );
    assert_eq!(p.books(&t).await, (42.0, 28.0));

    t.stop().await;
}

#[tokio::test]
async fn a_product_is_worth_what_was_consumed_even_when_it_does_not_divide() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    // A bill that makes 3 candies from 3 sugar, which are worth 10.00.
    let candy = t
        .create("item", json!({ "code": "CANDY", "name": "Candy" }))
        .await;
    let bom = t
        .create(
            "bom",
            json!({ "item_id": candy, "code": "CANDY-1", "name": "Candy", "output_qty": "3" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": bom, "item_id": p.sugar, "qty": "3" }),
    )
    .await;
    let rule = p.made(&t, &candy).await;
    p.adjust(&t, &p.sugar, "3", Some("3.3333")).await;

    let (status, draft) = p.draft_with(&t, &bom, "3").await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, order) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::OK, "{order}");
    let (status, done) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{done}");

    // 3 candies worth 10.00 in all: not 3 times 3.33.
    assert_eq!(p.level(&t, &candy).await, on_hand("3", 1000));
    assert_eq!(p.level(&t, &p.sugar).await, on_hand("0", 0));
    assert_eq!(p.books(&t).await, (0.0, 10.0));
    // Selling them one by one later takes out exactly 10.00 again.
    for expected in [on_hand("2", 667), on_hand("1", 333), on_hand("0", 0)] {
        p.adjust(&t, &candy, "-1", None).await;
        assert_eq!(p.level(&t, &candy).await, expected);
    }

    // With the product on the same account as its components there is
    // nothing to move in the books: the stock moves alone.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account_rule/{rule}"),
            json!({ "account_id": p.materials }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    p.adjust(&t, &p.sugar, "3", Some("2.00")).await;
    let (_, draft) = p.draft_with(&t, &bom, "3").await;
    let (_, order) = step(&t, "release", &draft).await;
    let entries = t.count("SELECT count(*) FROM gl_entry").await;
    let (status, done) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(p.level(&t, &candy).await, on_hand("3", 600));
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, entries);

    t.stop().await;
}

#[tokio::test]
async fn a_shortage_of_one_component_refuses_the_whole_order() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    p.stock_up(&t).await;

    // 6 cakes take 12 flour, of which there are 20, and 6 sugar, of which
    // there are 3.
    let order = p.released(&t, "6").await;
    let before = (
        p.levels(&t).await,
        p.books(&t).await,
        t.count("SELECT count(*) FROM stock_entry").await,
        t.count("SELECT count(*) FROM gl_entry").await,
        t.count("SELECT count(*) FROM posting").await,
    );
    let (status, body) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "only 3 each of item SUGAR are on hand in warehouse MAIN"
    );

    // The flour that could have gone stayed too.
    let after = (
        p.levels(&t).await,
        p.books(&t).await,
        t.count("SELECT count(*) FROM stock_entry").await,
        t.count("SELECT count(*) FROM gl_entry").await,
        t.count("SELECT count(*) FROM posting").await,
    );
    assert_eq!(after, before);
    let (_, order) = t
        .get(
            &t.admin,
            &format!("/api/documents/production_order/{}", common::id(&order)),
        )
        .await;
    assert_eq!(order["status"], "released");
    assert!(order["postings"].as_array().unwrap().is_empty());

    // Once the sugar is there the same order goes through.
    p.adjust(&t, &p.sugar, "3", Some("3.00")).await;
    let (status, body) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(p.level(&t, &p.cake).await, on_hand("6", 5500));

    t.stop().await;
}

#[tokio::test]
async fn changing_the_bill_does_not_change_a_released_order() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    p.stock_up(&t).await;
    let order = p.released(&t, "3").await;

    // The bill now takes ten times the flour, and eggs, of which there
    // are none, and is then retired.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/bom_line/{}", p.flour_line),
            json!({ "qty": "60" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let eggs = t
        .create("item", json!({ "code": "EGGS", "name": "Eggs" }))
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": p.bom, "item_id": eggs, "qty": "2" }),
    )
    .await;

    // An order released now follows the new bill.
    let newer = p.released(&t, "3").await;
    let components = newer["body"]["components"].as_array().unwrap();
    assert_eq!(components.len(), 3);
    assert_eq!(dec(&components[0]["qty"]), 60.0);

    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/bom/{}", p.bom),
            json!({ "active": false, "output_qty": "1" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let draft = p.draft(&t, "3").await;
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "bill of materials CAKE-1 is inactive");

    // The first order still reads, and consumes, what it was released with.
    let (_, order) = t
        .get(
            &t.admin,
            &format!("/api/documents/production_order/{}", common::id(&order)),
        )
        .await;
    let components = order["body"]["components"].as_array().unwrap();
    assert_eq!(components.len(), 2);
    assert_eq!(dec(&components[0]["qty"]), 6.0);
    assert_eq!(dec(&components[1]["qty"]), 3.0);
    let (status, done) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(
        p.levels(&t).await,
        [on_hand("14", 4200), on_hand("0", 0), on_hand("3", 2800)]
    );

    // A released order's own content is closed to changes as well.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/documents/production_order/{}", common::id(&newer)),
            json!({
                "version": newer["version"],
                "body": { "bom_id": p.bom, "qty": "1", "warehouse_id": p.main },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        sqlx::query("UPDATE production_order_component SET qty = 1")
            .execute(&t.pool)
            .await
            .is_err()
    );

    t.stop().await;
}

#[tokio::test]
async fn a_reversal_puts_the_components_back_unless_the_product_was_used() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    p.stock_up(&t).await;
    let before = (p.levels(&t).await, p.books(&t).await);

    let order = p.released(&t, "1").await;
    let (status, _) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!((p.levels(&t).await, p.books(&t).await), before);

    // Reversed: every component is back at the value it left for, the
    // cake is gone, and the books are where they were.
    let (status, body) = step(&t, "reverse", &order).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "reversed");
    assert_eq!(body["postings"].as_array().unwrap().len(), 2);
    assert_eq!((p.levels(&t).await, p.books(&t).await), before);
    assert_eq!(
        t.count("SELECT count(*) FROM stock_entry WHERE reverses_id IS NOT NULL")
            .await,
        3
    );
    // Doing it again changes nothing.
    let (status, again) = step(&t, "reverse", &order).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["version"], body["version"]);
    assert_eq!((p.levels(&t).await, p.books(&t).await), before);
    // A reversed order is finished: it cannot be completed a second time.
    let (status, body) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Three more cakes are made and one of them leaves the warehouse.
    let order = p.released(&t, "3").await;
    let (status, _) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK);
    p.adjust(&t, &p.cake, "-1", None).await;
    let used = (p.levels(&t).await, p.books(&t).await);

    // The three cannot be taken back any more, and nothing is.
    let (status, body) = step(&t, "reverse", &order).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("already moved on"), "{body}");
    assert_eq!((p.levels(&t).await, p.books(&t).await), used);
    let (_, order) = t
        .get(
            &t.admin,
            &format!("/api/documents/production_order/{}", common::id(&order)),
        )
        .await;
    assert_eq!(order["status"], "completed");
    assert_eq!(order["postings"].as_array().unwrap().len(), 1);

    t.stop().await;
}

#[tokio::test]
async fn a_bill_of_materials_is_checked() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    let line = |bom: &str, item: &str, qty: &'static str| {
        let (t, body) = (&t, json!({ "bom_id": bom, "item_id": item, "qty": qty }));
        async move { t.post(&t.admin, "/api/entities/bom_line", body).await }
    };

    // A cake is not made of cake, whether the line is new or changed.
    let (status, body) = line(&p.bom, &p.cake, "1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "a bill of materials cannot list its own product as a component"
    );
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/bom_line/{}", p.flour_line),
            json!({ "item_id": p.cake }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Nor can the bill be pointed at one of its components afterwards.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/bom/{}", p.bom),
            json!({ "item_id": p.flour }),
        )
        .await;
    assert!(status.is_client_error(), "{body}");
    assert!(
        sqlx::query(&format!("UPDATE bom SET item_id = '{}'", p.flour))
            .execute(&t.pool)
            .await
            .is_err()
    );

    // A component appears once, with a quantity the database can keep.
    let (status, body) = line(&p.bom, &p.flour, "1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let eggs = t
        .create("item", json!({ "code": "EGGS", "name": "Eggs" }))
        .await;
    for (qty, why) in [
        ("0", "more than zero"),
        ("-1", "more than zero"),
        ("0.1234567", "at most 6 decimal places"),
        ("many", "must be a number"),
    ] {
        let (status, body) = line(&p.bom, &eggs, qty).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(message(&body).contains(why), "{body}");
    }
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/bom/{}", p.bom),
            json!({ "output_qty": "0" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "the output quantity must be more than zero");
    let (status, body) = line(&p.bom, &eggs, "0.123456").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Bills are found by code and by name.
    let (status, found) = t.get(&t.admin, "/api/entities/bom?q=plain").await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["code"], "CAKE-1");

    t.stop().await;
}

#[tokio::test]
async fn a_production_order_follows_its_workflow() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    p.stock_up(&t).await;
    let stocked = p.levels(&t).await;

    // A draft must make sense.
    let other = t.company("OTHER", "USD").await;
    let far = t
        .create(
            "warehouse",
            json!({ "company_id": other.id, "code": "FAR", "name": "Far away" }),
        )
        .await;
    for (body, why) in [
        (
            json!({ "bom_id": p.bom, "qty": "0", "warehouse_id": p.main }),
            "more than zero",
        ),
        (
            json!({ "bom_id": p.bom, "qty": "1.0000001", "warehouse_id": p.main }),
            "at most 6 decimal places",
        ),
        (
            json!({ "bom_id": p.bom, "qty": "1", "warehouse_id": far }),
            "warehouse must belong to the document's company",
        ),
        (
            json!({ "bom_id": p.cake, "qty": "1", "warehouse_id": p.main }),
            "must be a bill of materials",
        ),
    ] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/documents/production_order",
                json!({ "company_id": p.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(message(&body).contains(why), "{body}");
    }

    // A draft has no number and no components, and cannot be completed.
    let draft = p.draft(&t, "3").await;
    assert_eq!(draft["status"], "draft");
    assert_eq!(draft["number"], Value::Null);
    assert!(draft["body"]["components"].as_array().unwrap().is_empty());
    let (status, body) = step(&t, "complete", &draft).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        message(&body),
        "a draft production order cannot be completed"
    );
    let (status, body) = step(&t, "reverse", &draft).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Releasing twice gives one number and one set of components.
    let (status, released) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::OK, "{released}");
    assert_eq!(released["number"], "MO-2026-00001");
    let (status, again) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["number"], "MO-2026-00001");
    assert_eq!(again["version"], released["version"]);
    assert_eq!(again["body"], released["body"]);
    assert_eq!(
        t.count("SELECT count(*) FROM production_order_component")
            .await,
        2
    );

    // A released order can still be cancelled; a cancelled one is over.
    let (status, cancelled) = step(&t, "cancel", &draft).await;
    assert_eq!(status, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    for action in ["complete", "release", "reverse"] {
        let (status, body) = step(&t, action, &draft).await;
        assert_eq!(status, StatusCode::CONFLICT, "{action}: {body}");
    }
    assert_eq!(p.levels(&t).await, stocked);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 3);

    // A draft can be cancelled too. A completed order cannot: it is
    // reversed instead.
    let draft = p.draft(&t, "3").await;
    let (status, body) = step(&t, "cancel", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order = p.released(&t, "3").await;
    assert_eq!(order["number"], "MO-2026-00002");
    let (status, _) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = step(&t, "cancel", &order).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // A quantity whose components do not come out even is refused at
    // release, not rounded: a third of 6 flour is 2, but a seventh is not
    // a quantity the ledger can keep.
    let draft = p.draft(&t, "0.7").await;
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/bom/{}", p.bom),
            json!({ "output_qty": "7" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let draft = p.draft(&t, "1").await;
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        message(&body).contains("more than 6 decimal places"),
        "{body}"
    );
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM production_order_component WHERE document_id = '{}'",
            common::id(&draft)
        ))
        .await,
        0
    );

    // A bill without components cannot be released. A company that has not
    // said which account carries its stock can plan, but not complete.
    let empty = t
        .create(
            "bom",
            json!({ "item_id": p.cake, "code": "CAKE-2", "name": "Cake from nothing" }),
        )
        .await;
    let (_, draft) = p.draft_with(&t, &empty, "1").await;
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "bill of materials CAKE-2 has no components");
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/production_order",
            json!({
                "company_id": other.id,
                "doc_date": "2026-03-15",
                "body": { "bom_id": p.bom, "qty": "7", "warehouse_id": far },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, order) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::OK, "{order}");
    let (status, body) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        message(&body).contains("no 'inventory' account set up"),
        "{body}"
    );

    // Each step is a permission of its own.
    let planner = t
        .user_with_role(
            "planner@example.com",
            &["production_order:read", "production_order:release"],
            &p.c.id,
        )
        .await;
    let draft = p.draft(&t, "7").await;
    let run = |action: &'static str| {
        let (t, planner, id) = (&t, &planner, common::id(&draft));
        async move {
            t.command(
                planner,
                &format!("production_order.{action}"),
                json!({ "id": id }),
            )
            .await
        }
    };
    let (status, body) = run("release").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = run("complete").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn a_company_can_release_orders_for_a_retired_bill() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/bom/{}", p.bom),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let draft = p.draft(&t, "3").await;
    // By default, as before: refused.
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "bill of materials CAKE-1 is inactive");

    // A rule of the company's own words refuses first.
    let rule = |data: Value| {
        let t = &t;
        async move {
            let (status, body) = t.post(&t.admin, "/api/entities/policy_rule", data).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            common::id(&body)
        }
    };
    let words = rule(json!({
        "company_id": p.c.id, "point": "production_order.inactive_bom", "effect": "refuse",
        "condition": "!facts.active", "message": "{bom_code} is retired", "priority": 1,
    }))
    .await;
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "CAKE-1 is retired");

    // Without it, a rule that lets this one bill through.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/policy_rule/{words}"),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rule(json!({
        "company_id": p.c.id, "point": "production_order.inactive_bom", "effect": "allow",
        "condition": "facts.bom_code == 'CAKE-1'",
    }))
    .await;
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "released");
    t.stop().await;
}

/// Labor and overhead: a bill says how many hours of each activity a run
/// takes, the company charges each at its rate, and the product comes in
/// at its components plus what it absorbed, credited to the account for
/// absorbed cost. Reversing the order takes all of it back.
#[tokio::test]
async fn an_order_absorbs_labor_and_overhead_into_the_product() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    p.stock_up(&t).await;
    let absorbed = t
        .create(
            "account",
            json!({ "company_id": p.c.id, "code": "5900", "name": "Absorbed cost",
                    "account_type": "expense" }),
        )
        .await;
    let baking = t
        .create(
            "cost_activity",
            json!({ "code": "BAKE", "name": "Baking", "kind": "labor", "unit": "h" }),
        )
        .await;
    let oven = t
        .create(
            "cost_activity",
            json!({ "code": "OVEN", "name": "Oven", "kind": "machine", "unit": "h" }),
        )
        .await;
    // A run of three cakes takes 1.5 hours of baking and 1 of the oven.
    for (activity, qty) in [(&baking, "1.5"), (&oven, "1")] {
        t.create(
            "bom_activity",
            json!({ "bom_id": p.bom, "activity_id": activity, "qty": qty }),
        )
        .await;
    }
    t.create(
        "account_rule",
        json!({ "company_id": p.c.id, "purpose": "absorbed_cost", "account_id": absorbed }),
    )
    .await;

    // Without a rate for every activity the order is not released.
    t.create(
        "cost_activity_rate",
        json!({ "company_id": p.c.id, "activity_id": baking, "rate": "40.00" }),
    )
    .await;
    let draft = p.draft(&t, "1").await;
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("OVEN"), "{body}");
    t.create(
        "cost_activity_rate",
        json!({ "company_id": p.c.id, "activity_id": oven, "rate": "12.00" }),
    )
    .await;

    // One cake: half an hour of baking (20.00) and a third of an hour of
    // the oven (4.00), on top of its 9.33 of components.
    let order = p.released(&t, "1").await;
    let activities = order["body"]["activities"].as_array().unwrap();
    assert_eq!(activities.len(), 2);
    assert_eq!(dec(&activities[0]["qty"]), 0.5);
    assert_eq!(dec(&activities[0]["rate"]), 40.0);
    assert_eq!(dec(&activities[1]["qty"]), 0.333333);
    // A later rate does not reach a released order.
    let rate: String = sqlx::query_scalar(
        "UPDATE cost_activity_rate SET rate = 99 WHERE activity_id = $1::uuid
         RETURNING id::text",
    )
    .bind(&baking)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert!(!rate.is_empty());
    let (status, done) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(dec(&done["summary"]["total_value"]), 33.33, "{done}");
    assert_eq!(p.level(&t, &p.cake).await, on_hand("1", 3333));
    let (materials, finished) = p.books(&t).await;
    assert_eq!(materials, 70.0 - 9.33);
    assert_eq!(finished, 33.33);
    let absorbed_balance: f64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(debit - credit), 0)::float8 FROM gl_entry WHERE account_id = $1::uuid",
    )
    .bind(&absorbed)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(absorbed_balance, -24.0);

    // Reversed, everything goes back.
    let (status, body) = step(&t, "reverse", &done).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(p.books(&t).await, (70.0, 0.0));
    t.stop().await;
}

/// A routing: operations at work centers, standard hours absorbed into the
/// product, the actual hours recorded and the difference posted as a
/// production variance, and each work center's load day by day.
#[tokio::test]
async fn a_routing_absorbs_standard_time_and_posts_the_variance() {
    let t = TestApp::start().await;
    let p = setup(&t).await;
    p.stock_up(&t).await;
    let account = |code: &'static str, name: &'static str| {
        let t = &t;
        let company = p.c.id.clone();
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": name,
                        "account_type": "expense" }),
            )
            .await
        }
    };
    let absorbed = account("5900", "Absorbed cost").await;
    let variance = account("5950", "Production variance").await;
    for (purpose, id) in [
        ("absorbed_cost", &absorbed),
        ("production_variance", &variance),
    ] {
        t.create(
            "account_rule",
            json!({ "company_id": p.c.id, "purpose": purpose, "account_id": id }),
        )
        .await;
    }
    let assembly = t
        .create(
            "cost_activity",
            json!({ "code": "ASSY", "name": "Assembly", "kind": "labor" }),
        )
        .await;
    t.create(
        "cost_activity_rate",
        json!({ "company_id": p.c.id, "activity_id": assembly, "rate": "30.00" }),
    )
    .await;
    // One hour to set up, half an hour per cake, on line 1.
    t.create(
        "bom_operation",
        json!({ "bom_id": p.bom, "seq": 10, "work_center_code": "LINE1",
                "setup_hours": "1", "run_hours": "0.5", "description": "Bake" }),
    )
    .await;
    // The company has no line 1 yet: the order cannot be released.
    let draft = p.draft(&t, "3").await;
    let (status, body) = step(&t, "release", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("LINE1"), "{body}");
    let line1 = t
        .create(
            "work_center",
            json!({ "company_id": p.c.id, "code": "LINE1", "name": "Line 1",
                    "activity_id": assembly, "hours_per_day": "8" }),
        )
        .await;

    // Three cakes: 1 + 1.5 = 2.5 standard hours at 30.00.
    let order = p.released(&t, "3").await;
    let operations = order["body"]["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(dec(&operations[0]["hours"]), 2.5);
    assert_eq!(operations[0]["work_center_id"], line1.as_str());

    let load = |from: &'static str| {
        let t = &t;
        let company = p.c.id.clone();
        async move {
            let (status, out) = t
                .command(
                    &t.admin,
                    "work_center.load",
                    json!({ "company_id": company, "from": from, "to": "2026-03-16" }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{out}");
            out["rows"].as_array().unwrap().clone()
        }
    };
    let rows = load("2026-03-15").await;
    assert_eq!(rows.len(), 2, "two days of one work center");
    assert_eq!(dec(&rows[0]["load_hours"]), 2.5);
    assert_eq!(dec(&rows[0]["load_percent"]), 31.3);
    assert_eq!(dec(&rows[1]["load_hours"]), 0.0);

    // It took three hours.
    let (status, body) = t
        .command(
            &t.admin,
            "production_order.record_time",
            json!({ "id": order["id"], "line_no": 1, "hours": "3", "worked_on": "2026-03-15" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(dec(&body["body"]["operations"][0]["actual_hours"]), 3.0);
    assert_eq!(dec(&load("2026-03-15").await[0]["load_hours"]), 0.0);

    // The cakes carry standard: 18.00 of flour, 10.00 of sugar, 75.00 of
    // time. The half hour more is a variance of 15.00.
    let (status, done) = step(&t, "complete", &order).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(dec(&done["summary"]["total_value"]), 103.0, "{done}");
    let balance = |id: &String| {
        let t = &t;
        let id = id.clone();
        async move {
            sqlx::query_scalar::<_, f64>(
                "SELECT COALESCE(sum(debit - credit), 0)::float8 FROM gl_entry
                 WHERE account_id = $1::uuid",
            )
            .bind(id)
            .fetch_one(&t.pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(balance(&variance).await, 15.0);
    assert_eq!(balance(&absorbed).await, -90.0, "what the time was worth");
    let (_, finished) = p.books(&t).await;
    assert_eq!(finished, 103.0);
    // Time is recorded only while the order is released.
    let (status, _) = t
        .command(
            &t.admin,
            "production_order.record_time",
            json!({ "id": order["id"], "line_no": 1, "hours": "1" }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);
    t.stop().await;
}
