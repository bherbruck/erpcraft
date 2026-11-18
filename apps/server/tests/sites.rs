//! Sites and the location tree: warehouses at sites, locations inside
//! warehouses, the site's dimension value on what moves there, and access
//! by site.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    expense: String,
    ohio_site: String,
    texas_site: String,
    main: String,
    bin: String,
    houston: String,
    item: String,
    site: String,
    ohio: String,
    texas: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("ACME", "USD").await;
    let inventory = t
        .create(
            "account",
            json!({ "company_id": c.id, "code": "1300", "name": "Inventory", "account_type": "asset" }),
        )
        .await;
    t.inventory_rule(&c.id, &inventory).await;
    let expense = t
        .create(
            "account",
            json!({ "company_id": c.id, "code": "6000", "name": "Shrinkage", "account_type": "expense" }),
        )
        .await;
    let ohio_site = t
        .create(
            "site",
            json!({ "company_id": c.id, "code": "OH", "name": "Ohio plant" }),
        )
        .await;
    let texas_site = t
        .create(
            "site",
            json!({ "company_id": c.id, "code": "TX", "name": "Texas plant" }),
        )
        .await;
    let main = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main", "site_id": ohio_site }),
        )
        .await;
    let bin = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN-A1", "name": "Bin A1", "parent_id": main }),
        )
        .await;
    let houston = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "HOU", "name": "Houston", "site_id": texas_site }),
        )
        .await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let site = t
        .create(
            "dimension",
            json!({ "company_id": c.id, "code": "SITE", "name": "Site" }),
        )
        .await;
    let value = |code: &'static str| {
        let (company, site) = (c.id.clone(), site.clone());
        async move {
            t.create(
                "dimension_value",
                json!({ "company_id": company, "dimension_id": site, "code": code, "name": code }),
            )
            .await
        }
    };
    let ohio = value("OHIO").await;
    let texas = value("TEXAS").await;
    // Each site's default value of the SITE dimension.
    for (scope, value) in [(&ohio_site, &ohio), (&texas_site, &texas)] {
        t.create(
            "dimension_rule",
            json!({ "company_id": c.id, "scope_type": "site", "scope_id": scope, "value_id": value }),
        )
        .await;
    }
    Fixture {
        c,
        expense,
        ohio_site,
        texas_site,
        main,
        bin,
        houston,
        item,
        site,
        ohio,
        texas,
    }
}

impl Fixture {
    async fn post(&self, t: &TestApp, doc_type: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, doc) = t
            .command(
                &t.admin,
                &format!("{doc_type}.post"),
                json!({ "id": id(&draft) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }

    async fn adjust(&self, t: &TestApp, warehouse: &str, qty: &str, cost: Option<&str>) -> Value {
        self.post(
            t,
            "stock_adjustment",
            json!({ "offset_account_id": self.expense, "lines": [
                { "item_id": self.item, "warehouse_id": warehouse, "qty": qty, "unit_cost": cost } ] }),
        )
        .await
    }

    /// The general ledger's inventory for one value of the SITE dimension
    /// equals the stock ledger's value at that site.
    async fn site_ties(&self, t: &TestApp, site: &str, value: &str) -> f64 {
        let (status, gl) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id, "dimension_value_ids": [value] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{gl}");
        let inventory: f64 = gl["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["code"] == "1300")
            .map(|r| dec(&r["balance"]))
            .sum();
        let (status, stock) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id, "site_id": site }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{stock}");
        let value = dec(&stock["total_value"]);
        assert!(
            (inventory - value).abs() < 0.00001,
            "general ledger {inventory} and stock {value} differ at a site"
        );
        value
    }
}

fn dims(doc: &Value, posting: usize) -> Vec<Value> {
    doc["postings"][posting]["entries"]["gl"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["dimensions"].clone())
        .collect()
}

#[tokio::test]
async fn locations_sit_inside_warehouses_at_their_site() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, bin) = t
        .get(&t.admin, &format!("/api/entities/warehouse/{}", f.bin))
        .await;
    assert_eq!(status, StatusCode::OK);
    // A location takes its warehouse's site.
    assert_eq!(bin["site_id"], json!(f.ohio_site));
    // Not another one, and never inside itself.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/warehouse",
            json!({ "company_id": f.c.id, "code": "MAIN-B1", "name": "B1",
                    "parent_id": f.main, "site_id": f.texas_site }),
        )
        .await;
    assert_ne!(status, StatusCode::CREATED);
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/warehouse/{}", f.main),
            json!({ "parent_id": f.bin }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);
    // A warehouse that moves takes its locations with it.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/warehouse/{}", f.main),
            json!({ "site_id": f.texas_site }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, bin) = t
        .get(&t.admin, &format!("/api/entities/warehouse/{}", f.bin))
        .await;
    assert_eq!(bin["site_id"], json!(f.texas_site));
    t.stop().await;
}

#[tokio::test]
async fn stock_carries_its_site_into_the_general_ledger() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // A rule for the warehouse reaches the bin inside it, and is more
    // specific than the site's.
    let cost_center = t
        .create(
            "dimension",
            json!({ "company_id": f.c.id, "code": "COSTCENTER", "name": "Cost center" }),
        )
        .await;
    let stores = t
        .create(
            "dimension_value",
            json!({ "company_id": f.c.id, "dimension_id": cost_center, "code": "STORES", "name": "Stores" }),
        )
        .await;
    t.create(
        "dimension_rule",
        json!({ "company_id": f.c.id, "scope_type": "warehouse", "scope_id": f.main, "value_id": stores }),
    )
    .await;

    let doc = f.adjust(&t, &f.bin, "10", Some("4")).await;
    assert_eq!(
        dims(&doc, 0),
        vec![
            json!("COSTCENTER:STORES, SITE:OHIO"),
            json!("COSTCENTER:STORES, SITE:OHIO")
        ]
    );
    f.adjust(&t, &f.houston, "5", Some("6")).await;
    assert_eq!(f.site_ties(&t, &f.ohio_site, &f.ohio).await, 40.0);
    assert_eq!(f.site_ties(&t, &f.texas_site, &f.texas).await, 30.0);

    // A transfer from Ohio to Texas on one inventory account still moves
    // the value from one site to the other in the general ledger. In one
    // step only where the company allows it.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "stock_transfer.between_sites", "effect": "allow",
                "condition": null }),
    )
    .await;
    let transfer = f
        .post(
            &t,
            "stock_transfer",
            json!({ "from_warehouse_id": f.bin, "to_warehouse_id": f.houston,
                    "lines": [{ "item_id": f.item, "qty": "4" }] }),
        )
        .await;
    assert_eq!(
        dims(&transfer, 0),
        vec![json!("SITE:TEXAS"), json!("COSTCENTER:STORES, SITE:OHIO")]
    );
    assert_eq!(f.site_ties(&t, &f.ohio_site, &f.ohio).await, 24.0);
    assert_eq!(f.site_ties(&t, &f.texas_site, &f.texas).await, 46.0);

    // Issuing at a site takes the site's value out.
    f.adjust(&t, &f.houston, "-1", None).await;
    f.site_ties(&t, &f.texas_site, &f.texas).await;
    let _ = f.site;
    t.stop().await;
}

#[tokio::test]
async fn access_rules_read_the_sites_of_a_user() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.adjust(&t, &f.bin, "2", Some("1")).await;
    f.adjust(&t, &f.houston, "3", Some("1")).await;
    let (token, user) = t
        .user_with_rules(
            "ohio@example.com",
            json!([
                { "allow": "warehouse:read", "where": "record.site_id in user.sites" },
                { "allow": "stock_entry:read", "where": "record.site_id in user.sites" },
            ]),
            Some(&f.c.id),
        )
        .await;
    t.set_attributes(&user, json!({ "sites": [f.ohio_site] }))
        .await;
    let (status, list) = t.get(&token, "/api/entities/warehouse").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let codes: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["MAIN", "MAIN-A1"]);
    let (status, entries) = t.get(&token, "/api/entities/stock_entry").await;
    assert_eq!(status, StatusCode::OK, "{entries}");
    assert_eq!(entries["total"], 1);
    assert_eq!(entries["items"][0]["site_id"], json!(f.ohio_site));
    t.stop().await;
}

/// A transfer says which sites it moves between, so rules by site can
/// limit who moves what.
#[tokio::test]
async fn a_user_limited_to_ohio_cannot_move_texas_stock() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.adjust(&t, &f.houston, "3", Some("1")).await;
    let dallas = t
        .create(
            "warehouse",
            json!({ "company_id": f.c.id, "code": "DAL", "name": "Dallas", "site_id": f.texas_site }),
        )
        .await;
    let ohio_only =
        "record.summary.from_site_id in user.sites && record.summary.to_site_id in user.sites";
    let (token, user) = t
        .user_with_rules(
            "ohio@example.com",
            json!([
                "stock_transfer:create", "stock_transfer:read", "stock_transfer:update",
                { "allow": "stock_transfer:post", "where": ohio_only },
            ]),
            Some(&f.c.id),
        )
        .await;
    t.set_attributes(&user, json!({ "sites": [f.ohio_site] }))
        .await;
    let draft = |from: String, to: String| {
        let (t, token, c, item) = (&t, token.clone(), f.c.id.clone(), f.item.clone());
        async move {
            let (status, draft) = t
                .post(
                    &token,
                    "/api/documents/stock_transfer",
                    json!({ "company_id": c, "doc_date": "2026-03-15", "body": {
                        "from_warehouse_id": from, "to_warehouse_id": to,
                        "lines": [{ "item_id": item, "qty": "1" }] } }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{draft}");
            draft
        }
    };
    let texas = draft(f.houston.clone(), dallas).await;
    assert_eq!(texas["summary"]["from_site_id"], json!(f.texas_site));
    let (status, body) = t
        .command(&token, "stock_transfer.post", json!({ "id": texas["id"] }))
        .await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
        "an Ohio-only user moved Texas stock: {status} {body}"
    );
    f.adjust(&t, &f.main, "2", Some("1")).await;
    let ohio = draft(f.main.clone(), f.bin.clone()).await;
    let (status, body) = t
        .command(&token, "stock_transfer.post", json!({ "id": ohio["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}
