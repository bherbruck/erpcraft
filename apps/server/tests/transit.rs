//! Transfers between sites in two steps: shipped into a warehouse for goods
//! in transit, received at the destination in parts, and reversed, with
//! the general ledger equal to the stock ledger on every account.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    expense: String,
    main: String,
    houston: String,
    transit: String,
    item: String,
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
    let in_transit = account("1350", "asset").await;
    let expense = account("6000", "expense").await;
    t.inventory_rule(&c.id, &inventory).await;
    t.create(
        "account_rule",
        json!({ "company_id": c.id, "purpose": "inventory_in_transit", "account_id": in_transit }),
    )
    .await;
    let site = |code: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "site",
                json!({ "company_id": company, "code": code, "name": code }),
            )
            .await
        }
    };
    let ohio = site("OH").await;
    let texas = site("TX").await;
    let warehouse = |code: &'static str, site: &String, transit: bool| {
        let (company, site) = (c.id.clone(), site.clone());
        async move {
            t.create(
                "warehouse",
                json!({ "company_id": company, "code": code, "name": code, "site_id": site,
                        "transit": transit }),
            )
            .await
        }
    };
    let main = warehouse("MAIN", &ohio, false).await;
    let houston = warehouse("HOU", &texas, false).await;
    let transit = warehouse("TX-TRANSIT", &texas, true).await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    Fixture {
        c,
        expense,
        main,
        houston,
        transit,
        item,
    }
}

impl Fixture {
    async fn draft(&self, t: &TestApp, doc_type: &str, body: Value) -> (StatusCode, Value) {
        t.post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
        )
        .await
    }

    async fn run(&self, t: &TestApp, command: &str, doc: &str) -> (StatusCode, Value) {
        t.command(&t.admin, command, json!({ "id": doc })).await
    }

    async fn ok(&self, t: &TestApp, command: &str, doc: &str) -> Value {
        let (status, body) = self.run(t, command, doc).await;
        assert_eq!(status, StatusCode::OK, "{command}: {body}");
        body
    }

    async fn transfer(&self, t: &TestApp, qty: &str, through: Option<&str>) -> (StatusCode, Value) {
        let (status, draft) = self
            .draft(
                t,
                "stock_transfer",
                json!({ "from_warehouse_id": self.main, "to_warehouse_id": self.houston,
                        "transit_warehouse_id": through,
                        "lines": [{ "item_id": self.item, "qty": qty }] }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        self.run(t, "stock_transfer.post", &id(&draft)).await
    }

    /// Receive `qty` of what is on its way on `transfer`.
    async fn receive(&self, t: &TestApp, transfer: &Value, qty: &str) -> (StatusCode, Value) {
        let (status, draft) = self.run(t, "stock_transfer.receive", &id(transfer)).await;
        assert_eq!(status, StatusCode::OK, "{draft}");
        let mut body = draft["body"].clone();
        body["lines"][0]["qty"] = json!(qty);
        let (status, changed) = t
            .patch(
                &t.admin,
                &format!("/api/documents/stock_transfer_receipt/{}", id(&draft)),
                json!({ "version": draft["version"], "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        self.run(t, "stock_transfer_receipt.post", &id(&draft))
            .await
    }

    /// Every inventory account equals the stock it carries: 1300 the stock
    /// in ordinary warehouses, 1350 the goods in transit. Returns both.
    async fn ties(&self, t: &TestApp) -> (f64, f64) {
        let (status, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{tb}");
        let balance = |code: &str| {
            tb["rows"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["code"] == code)
                .map(|r| dec(&r["balance"]))
                .sum::<f64>()
        };
        let mut stock = vec![];
        for transit in [false, true] {
            let (status, on_hand) = t
                .command(
                    &t.admin,
                    "stock.on_hand",
                    json!({ "company_id": self.c.id, "transit": transit }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{on_hand}");
            stock.push(dec(&on_hand["total_value"]));
        }
        assert!(
            (balance("1300") - stock[0]).abs() < 1e-9,
            "1300 {} vs {}",
            balance("1300"),
            stock[0]
        );
        assert!(
            (balance("1350") - stock[1]).abs() < 1e-9,
            "1350 {} vs {}",
            balance("1350"),
            stock[1]
        );
        (stock[0], stock[1])
    }

    async fn status(&self, t: &TestApp, doc: &Value) -> String {
        let (_, now) = t
            .get(
                &t.admin,
                &format!("/api/documents/stock_transfer/{}", id(doc)),
            )
            .await;
        now["status"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn goods_travel_between_sites_in_two_steps() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, body) = f
        .draft(
            &t,
            "stock_adjustment",
            json!({ "offset_account_id": f.expense, "lines": [
                { "item_id": f.item, "warehouse_id": f.main, "qty": "10", "unit_cost": "4" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    f.ok(&t, "stock_adjustment.post", &id(&body)).await;
    assert_eq!(f.ties(&t).await, (40.0, 0.0));

    // Between sites in one step: refused by default.
    let (status, body) = f.transfer(&t, "6", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "transit_warehouse_id");
    // A transit warehouse is the way through, never an end.
    let (status, _) = f
        .draft(
            &t,
            "stock_transfer",
            json!({ "from_warehouse_id": f.main, "to_warehouse_id": f.transit,
                    "lines": [{ "item_id": f.item, "qty": "1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, shipped) = f.transfer(&t, "6", Some(&f.transit)).await;
    assert_eq!(status, StatusCode::OK, "{shipped}");
    assert_eq!(shipped["status"], "in_transit");
    assert_eq!(f.ties(&t).await, (16.0, 24.0));

    // Planning counts the goods as coming in at the destination, not as
    // stock where they wait.
    let (status, availability) = t
        .command(
            &t.admin,
            "stock.availability",
            json!({ "company_id": f.c.id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{availability}");
    let rows = availability["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{availability}");
    let houston = rows.iter().find(|r| r["warehouse_code"] == "HOU").unwrap();
    assert_eq!(dec(&houston["in_transit"]), 6.0);
    assert_eq!(dec(&houston["projected"]), 6.0);

    // Four arrive, then not more than the two left.
    let (status, first) = f.receive(&t, &shipped, "4").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(dec(&first["body"]["lines"][0]["value"]), 16.0);
    assert_eq!(f.status(&t, &shipped).await, "in_transit");
    assert_eq!(f.ties(&t).await, (32.0, 8.0));
    let (status, report) = t
        .command(
            &t.admin,
            "stock_transfer.in_transit",
            json!({ "company_id": f.c.id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(dec(&report["rows"][0]["qty_left"]), 2.0);
    assert_eq!(dec(&report["total_value"]), 8.0);
    let (status, body) = f.receive(&t, &shipped, "3").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("2 of transfer line 1")
    );

    // A receipt can be taken back while the goods are still there.
    f.ok(&t, "stock_transfer_receipt.reverse", &id(&first))
        .await;
    assert_eq!(f.ties(&t).await, (16.0, 24.0));
    let (status, all) = f.receive(&t, &shipped, "6").await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(f.status(&t, &shipped).await, "received");
    assert_eq!(f.ties(&t).await, (40.0, 0.0));
    let (_, report) = t
        .command(
            &t.admin,
            "stock_transfer.in_transit",
            json!({ "company_id": f.c.id }),
        )
        .await;
    assert_eq!(report["rows"], json!([]));

    // A received transfer is undone receipt first, then shipment.
    let (status, _) = f.run(&t, "stock_transfer.reverse", &id(&shipped)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    f.ok(&t, "stock_transfer_receipt.reverse", &id(&all)).await;
    assert_eq!(f.status(&t, &shipped).await, "in_transit");
    f.ok(&t, "stock_transfer.reverse", &id(&shipped)).await;
    assert_eq!(f.status(&t, &shipped).await, "reversed");
    assert_eq!(f.ties(&t).await, (40.0, 0.0));
    let (_, on_hand) = t
        .command(
            &t.admin,
            "stock.on_hand",
            json!({ "company_id": f.c.id, "warehouse_id": f.main }),
        )
        .await;
    assert_eq!(dec(&on_hand["rows"][0]["qty"]), 10.0);

    // A company that allows it moves between sites in one step.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "stock_transfer.between_sites",
                "effect": "allow", "condition": "facts.to_site == 'TX'" }),
    )
    .await;
    let (status, body) = f.transfer(&t, "1", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.ties(&t).await, (40.0, 0.0));
    t.stop().await;
}

#[tokio::test]
async fn goods_in_transit_move_only_with_transfers() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Only a transfer's way through holds goods in transit; nothing else
    // takes them from there.
    let (status, body) = f
        .draft(
            &t,
            "stock_adjustment",
            json!({ "offset_account_id": f.expense, "lines": [
                { "item_id": f.item, "warehouse_id": f.transit, "qty": "2", "unit_cost": "5" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // An adjustment counts goods in transit on their own account.
    f.ok(&t, "stock_adjustment.post", &id(&body)).await;
    assert_eq!(f.ties(&t).await, (0.0, 10.0));
    t.stop().await;
}
