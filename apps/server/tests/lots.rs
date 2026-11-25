//! Lots and serial numbers: every movement of a tracked item names its
//! lots, lot levels never go below zero, a serial number is one unit on
//! hand once, and lots can be traced.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    offset: String,
    main: String,
    houston: String,
    transit: String,
    flour: String,
    scanner: String,
    plain: String,
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
    let offset = account("6000", "expense").await;
    let loss = account("6100", "expense").await;
    t.inventory_rule(&c.id, &inventory).await;
    t.create(
        "account_rule",
        json!({ "company_id": c.id, "purpose": "inventory_in_transit", "account_id": in_transit }),
    )
    .await;
    t.create(
        "account_rule",
        json!({ "company_id": c.id, "purpose": "transit_loss", "account_id": loss }),
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
    let flour = t
        .create(
            "item",
            json!({ "code": "FLOUR", "name": "Flour", "uom": "kg", "tracking": "lot" }),
        )
        .await;
    let scanner = t
        .create(
            "item",
            json!({ "code": "SCAN", "name": "Scanner", "tracking": "serial" }),
        )
        .await;
    let plain = t
        .create("item", json!({ "code": "BOLT", "name": "Bolt" }))
        .await;
    Fixture {
        c,
        offset,
        main,
        houston,
        transit,
        flour,
        scanner,
        plain,
    }
}

impl Fixture {
    async fn draft(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        draft
    }

    async fn run(&self, t: &TestApp, command: &str, doc: &Value) -> (StatusCode, Value) {
        t.command(&t.admin, command, json!({ "id": id(doc) })).await
    }

    async fn ok(&self, t: &TestApp, command: &str, doc: &Value) -> Value {
        let (status, body) = self.run(t, command, doc).await;
        assert_eq!(status, StatusCode::OK, "{command}: {body}");
        body
    }

    /// A stock adjustment of one line, posted; returns the status and body.
    async fn adjust(
        &self,
        t: &TestApp,
        item: &str,
        warehouse: &str,
        qty: &str,
        lots: Value,
    ) -> (StatusCode, Value) {
        let cost = (!qty.starts_with('-')).then_some("2.00");
        let draft = self
            .draft(
                t,
                "stock_adjustment",
                "2026-03-01",
                json!({ "offset_account_id": self.offset, "lines": [{
                    "item_id": item, "warehouse_id": warehouse, "qty": qty, "unit_cost": cost,
                    "lots": lots }] }),
            )
            .await;
        self.run(t, "stock_adjustment.post", &draft).await
    }

    /// Lot levels: (lot, warehouse code, qty), for one item.
    async fn lots(&self, t: &TestApp, item: &str) -> Vec<(String, String, f64)> {
        let (status, body) = t
            .command(
                &t.admin,
                "stock.lots",
                json!({ "company_id": self.c.id, "item_id": item }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut rows: Vec<(String, String, f64)> = body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["lot"].as_str().unwrap().to_owned(),
                    r["warehouse_code"].as_str().unwrap().to_owned(),
                    dec(&r["qty"]),
                )
            })
            .collect();
        rows.sort_by(|a, b| a.partial_cmp(b).unwrap());
        rows
    }
}

fn message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn every_movement_of_a_tracked_item_names_its_lots() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Without lots, with lots that do not add up, and lots on an item that
    // is not tracked: refused, on the line.
    let (status, body) = f.adjust(&t, &f.flour, &f.main, "10", json!([])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("tracked by lot"), "{body}");
    assert_eq!(body["error"]["field"], "lots", "{body}");
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "10",
            json!([{ "lot": "F-1", "qty": "4" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("add up to 4, not 10"), "{body}");
    let (status, body) = f
        .adjust(
            &t,
            &f.plain,
            &f.main,
            "10",
            json!([{ "lot": "B-1", "qty": "10" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Two lots in, with their dates.
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "10",
            json!([
                { "lot": "F-1", "qty": "4", "expiry_date": "2026-06-30",
                  "manufactured_date": "2026-01-10", "attributes": { "mill": "North" } },
                { "lot": "F-2", "qty": "6", "expiry_date": "2026-05-31" },
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["body"]["lines"][0]["lots"][1]["lot"], "F-2", "{body}");
    assert_eq!(
        f.lots(&t, &f.flour).await,
        vec![
            ("F-1".into(), "MAIN".into(), 4.0),
            ("F-2".into(), "MAIN".into(), 6.0)
        ]
    );
    // The posting shows each entry's lots.
    let (_, posted) = t
        .get(
            &t.admin,
            &format!("/api/documents/stock_adjustment/{}", id(&body)),
        )
        .await;
    let entries = posted["postings"][0]["entries"]["stock"].clone();
    assert_eq!(
        entries[0]["lots"].as_array().map(Vec::len),
        Some(2),
        "{posted}"
    );

    // Out: more of a lot than it holds is refused by the ledger, though
    // the item has enough.
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "-5",
            json!([{ "lot": "F-1", "qty": "5" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("of lot F-1"), "{body}");
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "-5",
            json!([{ "lot": "F-9", "qty": "5" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("no lot F-9"), "{body}");
    let (status, out) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "-5",
            json!([{ "lot": "F-2", "qty": "5" }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        f.lots(&t, &f.flour).await,
        vec![
            ("F-1".into(), "MAIN".into(), 4.0),
            ("F-2".into(), "MAIN".into(), 1.0)
        ]
    );
    // An existing lot coming in again with another expiry date is refused.
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "1",
            json!([{ "lot": "F-1", "qty": "1", "expiry_date": "2027-01-01" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Reversing takes back exactly the lots.
    f.ok(&t, "stock_adjustment.reverse", &out).await;
    assert_eq!(
        f.lots(&t, &f.flour).await,
        vec![
            ("F-1".into(), "MAIN".into(), 4.0),
            ("F-2".into(), "MAIN".into(), 6.0)
        ]
    );

    // Tracking changes only while the item has no stock anywhere.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/item/{}", f.flour),
            json!({ "tracking": "none" }),
        )
        .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_serial_number_is_one_unit_on_hand_once() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, body) = f
        .adjust(
            &t,
            &f.scanner,
            &f.main,
            "2",
            json!([{ "lot": "SN-1", "qty": "2" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("one unit"), "{body}");
    let (status, body) = f
        .adjust(
            &t,
            &f.scanner,
            &f.main,
            "2",
            json!([{ "lot": "SN-1", "qty": "1" }, { "lot": "SN-2", "qty": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Not again, in this warehouse or another.
    let (status, body) = f
        .adjust(
            &t,
            &f.scanner,
            &f.houston,
            "1",
            json!([{ "lot": "SN-1", "qty": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("already on hand"), "{body}");
    // Once gone, it may come back.
    let (status, body) = f
        .adjust(
            &t,
            &f.scanner,
            &f.main,
            "-1",
            json!([{ "lot": "SN-1", "qty": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f
        .adjust(
            &t,
            &f.scanner,
            &f.houston,
            "1",
            json!([{ "lot": "SN-1", "qty": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        f.lots(&t, &f.scanner).await,
        vec![
            ("SN-1".into(), "HOU".into(), 1.0),
            ("SN-2".into(), "MAIN".into(), 1.0)
        ]
    );
    t.stop().await;
}

#[tokio::test]
async fn the_ledger_itself_refuses_an_entry_without_its_lots() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, posted) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "3",
            json!([{ "lot": "F-1", "qty": "3" }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    let posting: String = sqlx::query_scalar(
        "SELECT id::text FROM posting WHERE document_id = $1::uuid AND reverses_id IS NULL",
    )
    .bind(id(&posted))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let entry = |line: i32, qty: &str| {
        format!(
            "INSERT INTO stock_entry (id, company_id, posting_id, line_no, item_id, warehouse_id,
                posting_date, qty, value, unit_cost, qty_after, value_after)
             VALUES (gen_random_uuid(), '{}', '{posting}', {line}, '{}', '{}', '2026-03-01',
                {qty}, 0, 0, 0, 0)",
            f.c.id, f.flour, f.main
        )
    };
    // An issue with no lots: refused when the transaction commits.
    let refused = sqlx::query(&entry(50, "-1")).execute(&t.pool).await;
    let error = refused.unwrap_err().to_string();
    assert!(error.contains("tracked by lot"), "{error}");
    // Lots added to an entry already recorded: refused.
    let entry_id: String =
        sqlx::query_scalar("SELECT e.id::text FROM stock_entry e WHERE e.posting_id = $1::uuid")
            .bind(&posting)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    let lot: String = sqlx::query_scalar("SELECT id::text FROM stock_lot WHERE code = 'F-1'")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let added = sqlx::query(&format!(
        "INSERT INTO stock_entry_lot (entry_id, lot_id, qty) VALUES ('{entry_id}', '{lot}', 1)"
    ))
    .execute(&t.pool)
    .await;
    assert!(added.is_err());
    // Lot levels change only through entries.
    let edited = sqlx::query("UPDATE stock_lot_level SET qty = 100")
        .execute(&t.pool)
        .await;
    assert!(edited.is_err());
    assert_eq!(
        f.lots(&t, &f.flour).await,
        vec![("F-1".into(), "MAIN".into(), 3.0)]
    );
    t.stop().await;
}

#[tokio::test]
async fn lots_travel_with_a_transfer_and_are_written_off_when_lost() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "10",
            json!([{ "lot": "F-1", "qty": "4" }, { "lot": "F-2", "qty": "6" }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // In one step, and back by reversing.
    let one_step = f
        .draft(
            &t,
            "stock_transfer",
            "2026-03-02",
            json!({ "from_warehouse_id": f.main, "to_warehouse_id": f.houston,
                    "lines": [{ "item_id": f.flour, "qty": "1",
                                "lots": [{ "lot": "F-1", "qty": "1" }] }] }),
        )
        .await;
    // Within one company between sites in one step is refused by default;
    // allow it here.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "stock_transfer.between_sites", "effect": "allow",
                "condition": null }),
    )
    .await;
    f.ok(&t, "stock_transfer.post", &one_step).await;
    assert!(
        f.lots(&t, &f.flour)
            .await
            .contains(&("F-1".into(), "HOU".into(), 1.0))
    );
    f.ok(&t, "stock_transfer.reverse", &one_step).await;

    // Shipped through the transit warehouse: the receipt drafted carries
    // the lots on their way.
    let shipped = f
        .draft(
            &t,
            "stock_transfer",
            "2026-03-02",
            json!({ "from_warehouse_id": f.main, "to_warehouse_id": f.houston,
                    "transit_warehouse_id": f.transit,
                    "lines": [{ "item_id": f.flour, "qty": "7",
                                "lots": [{ "lot": "F-1", "qty": "3" },
                                         { "lot": "F-2", "qty": "4" }] }] }),
        )
        .await;
    f.ok(&t, "stock_transfer.post", &shipped).await;
    assert_eq!(
        f.lots(&t, &f.flour).await,
        vec![
            ("F-1".into(), "MAIN".into(), 1.0),
            ("F-1".into(), "TX-TRANSIT".into(), 3.0),
            ("F-2".into(), "MAIN".into(), 2.0),
            ("F-2".into(), "TX-TRANSIT".into(), 4.0),
        ]
    );
    let receipt = f.ok(&t, "stock_transfer.receive", &shipped).await;
    let drafted: Vec<(String, f64)> = receipt["body"]["lines"][0]["lots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["lot"].as_str().unwrap().to_owned(), dec(&l["qty"])))
        .collect();
    assert_eq!(drafted, vec![("F-1".into(), 3.0), ("F-2".into(), 4.0)]);
    // A lot that is not on its way on this transfer is refused; part of
    // one that is arrives.
    let mut body = receipt["body"].clone();
    body["lines"][0]["qty"] = json!("2");
    body["lines"][0]["lots"] = json!([{ "lot": "F-9", "qty": "2" }]);
    let patch = |body: Value, version: Value| {
        let t = &t;
        let path = format!("/api/documents/stock_transfer_receipt/{}", id(&receipt));
        async move {
            let (status, changed) = t
                .patch(&t.admin, &path, json!({ "version": version, "body": body }))
                .await;
            assert_eq!(status, StatusCode::OK, "{changed}");
            changed
        }
    };
    let changed = patch(body.clone(), receipt["version"].clone()).await;
    let (status, refused) = f.run(&t, "stock_transfer_receipt.post", &changed).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(message(&refused).contains("not on its way"), "{refused}");
    body["lines"][0]["lots"] = json!([{ "lot": "F-2", "qty": "2" }]);
    let changed = patch(body, changed["version"].clone()).await;
    f.ok(&t, "stock_transfer_receipt.post", &changed).await;

    // The rest never arrives: F-1 3 and F-2 2 are written off.
    let (status, closed) = t
        .command(
            &t.admin,
            "stock_transfer.close_short",
            json!({ "id": id(&shipped), "posting_date": "2026-03-20" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(
        f.lots(&t, &f.flour).await,
        vec![
            ("F-1".into(), "MAIN".into(), 1.0),
            ("F-2".into(), "HOU".into(), 2.0),
            ("F-2".into(), "MAIN".into(), 2.0),
        ]
    );
    t.stop().await;
}

#[tokio::test]
async fn which_lot_goes_out_is_proposed_and_the_policy_decides() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "9",
            json!([
                { "lot": "LATE", "qty": "3", "expiry_date": "2026-12-31" },
                { "lot": "NONE", "qty": "3" },
                { "lot": "SOON", "qty": "3", "expiry_date": "2026-02-15" },
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let propose = |order: &'static str, not_expired_on: Option<&'static str>| {
        let t = &t;
        let input = json!({ "company_id": f.c.id, "item_id": f.flour, "warehouse_id": f.main,
                            "qty": "5", "order": order, "not_expired_on": not_expired_on });
        async move {
            let (status, body) = t.command(&t.admin, "stock.lot_proposal", input).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body
        }
    };
    assert_eq!(
        propose("fefo", None).await["lots"],
        json!([{ "lot": "SOON", "qty": "3" }, { "lot": "LATE", "qty": "2" }])
    );
    assert_eq!(
        propose("fefo", Some("2026-03-01")).await["lots"],
        json!([{ "lot": "LATE", "qty": "3" }, { "lot": "NONE", "qty": "2" }])
    );
    // Received in the order named: LATE, NONE, SOON.
    assert_eq!(
        propose("fifo", None).await["lots"],
        json!([{ "lot": "LATE", "qty": "3" }, { "lot": "NONE", "qty": "2" }])
    );

    // Nothing expired goes out, and lots go first expiring first: the
    // company's rules, not the ledger's.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "stock.lot_issue", "effect": "refuse",
                "condition": "facts.expired", "message": "expired goods stay" }),
    )
    .await;
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "-1",
            json!([{ "lot": "SOON", "qty": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("expired goods stay"), "{body}");
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "stock.lot_issue", "effect": "refuse",
                "condition": "!facts.expired && !facts.first_expiring",
                "message": "first expiring first" }),
    )
    .await;
    // SOON has expired, so LATE is the first expiring that may go; NONE
    // (no expiry date) may not before it.
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "-1",
            json!([{ "lot": "NONE", "qty": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("first expiring first"), "{body}");
    let (status, body) = f
        .adjust(
            &t,
            &f.flour,
            &f.main,
            "-1",
            json!([{ "lot": "LATE", "qty": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

// ------------------------------------------------------------ documents

/// A manufacturer from the template, with a plant warehouse and lot-tracked
/// items: what is bought, made and sold below.
struct Plant {
    c: String,
    plant: String,
    vendor: String,
    customer: String,
    flour: String,
    sugar: String,
    cake: String,
}

async fn plant(t: &TestApp) -> Plant {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_manufacturer", "code": "BAKE", "name": "Bakery Inc",
                    "first_year": 2026, "years": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let c = body["company"]["id"].as_str().unwrap().to_owned();
    let plant = t
        .create(
            "warehouse",
            json!({ "company_id": c, "code": "PLANT", "name": "Plant" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "MILL", "name": "North Mill" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "SHOP", "name": "Corner Shop" }))
        .await;
    let item = |code: &'static str| async move {
        t.create(
            "item",
            json!({ "code": code, "name": code, "uom": "kg", "tracking": "lot" }),
        )
        .await
    };
    Plant {
        flour: item("FLOUR").await,
        sugar: item("SUGAR").await,
        cake: item("CAKE").await,
        c,
        plant,
        vendor,
        customer,
    }
}

impl Plant {
    async fn draft(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        draft
    }

    async fn run(&self, t: &TestApp, command: &str, input: Value) -> (StatusCode, Value) {
        t.command(&t.admin, command, input).await
    }

    async fn ok(&self, t: &TestApp, command: &str, input: Value) -> Value {
        let (status, body) = self.run(t, command, input).await;
        assert_eq!(status, StatusCode::OK, "{command}: {body}");
        body
    }

    /// Edit a draft's body and save it.
    async fn edit(&self, t: &TestApp, draft: &Value, change: impl FnOnce(&mut Value)) -> Value {
        let mut body = draft["body"].clone();
        change(&mut body);
        let (status, changed) = t
            .patch(
                &t.admin,
                &format!(
                    "/api/documents/{}/{}",
                    draft["doc_type"].as_str().unwrap(),
                    id(draft)
                ),
                json!({ "version": draft["version"], "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        changed
    }

    /// An approved purchase order of `qty` of `item` at 1.00.
    async fn order(&self, t: &TestApp, item: &str, qty: &str) -> Value {
        let order = self
            .draft(
                t,
                "purchase_order",
                "2026-03-01",
                json!({ "vendor_id": self.vendor, "lines": [
                    { "item_id": item, "warehouse_id": self.plant, "qty": qty,
                      "unit_price": "1.00" }] }),
            )
            .await;
        self.ok(t, "purchase_order.approve", json!({ "id": id(&order) }))
            .await
    }

    async fn lots(&self, t: &TestApp, item: &str) -> Vec<(String, f64)> {
        let body = self
            .ok(
                t,
                "stock.lots",
                json!({ "company_id": self.c, "item_id": item }),
            )
            .await;
        body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["lot"].as_str().unwrap().to_owned(), dec(&r["qty"])))
            .collect()
    }
}

#[tokio::test]
async fn goods_bought_come_in_by_lot_and_go_back_by_lot() {
    let t = TestApp::start().await;
    let p = plant(&t).await;
    let order = p.order(&t, &p.flour, "100").await;
    let line = order["body"]["lines"][0]["id"].clone();
    let receipt = p
        .draft(
            &t,
            "purchase_receipt",
            "2026-03-02",
            json!({ "purchase_order_id": order["id"], "lines": [
                { "order_line_id": line, "qty": "100" }] }),
        )
        .await;
    let (status, body) = p
        .run(&t, "purchase_receipt.post", json!({ "id": id(&receipt) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("tracked by lot"), "{body}");
    let receipt = p
        .edit(&t, &receipt, |b| {
            b["lines"][0]["lots"] = json!([
                { "lot": "M-100", "qty": "60", "expiry_date": "2026-09-30",
                  "attributes": { "mill_lot": "NM-7781" } },
                { "lot": "M-101", "qty": "40", "expiry_date": "2026-10-31" }]);
        })
        .await;
    p.ok(&t, "purchase_receipt.post", json!({ "id": id(&receipt) }))
        .await;
    assert_eq!(
        p.lots(&t, &p.flour).await,
        vec![("M-100".into(), 60.0), ("M-101".into(), 40.0)]
    );
    let (_, lot) = t
        .get(
            &t.admin,
            &format!("/api/entities/stock_lot?company_id={}&code=M-100", p.c),
        )
        .await;
    assert_eq!(
        lot["items"][0]["attributes"]["mill_lot"], "NM-7781",
        "{lot}"
    );

    // A return drafted from the order names the lots received.
    let back = p
        .ok(&t, "purchase_order.return", json!({ "id": order["id"] }))
        .await;
    let drafted: Vec<(String, f64)> = back["body"]["lines"][0]["lots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["lot"].as_str().unwrap().to_owned(), dec(&l["qty"])))
        .collect();
    assert_eq!(
        drafted,
        vec![("M-100".into(), 60.0), ("M-101".into(), 40.0)]
    );
    let back = p
        .edit(&t, &back, |b| {
            b["lines"][0]["qty"] = json!("5");
            b["lines"][0]["lots"] = json!([{ "lot": "M-101", "qty": "5" }]);
        })
        .await;
    p.ok(&t, "purchase_return.post", json!({ "id": id(&back) }))
        .await;
    assert_eq!(
        p.lots(&t, &p.flour).await,
        vec![("M-100".into(), 60.0), ("M-101".into(), 35.0)]
    );
    t.stop().await;
}

/// Lots named as a production order is completed: what it consumed and
/// what it made.
fn lots_of(body: &Value) -> Vec<(String, f64)> {
    body.as_array()
        .unwrap()
        .iter()
        .map(|l| (l["lot"].as_str().unwrap().to_owned(), dec(&l["qty"])))
        .collect()
}

#[tokio::test]
async fn a_lot_is_traced_from_the_mill_through_the_oven_to_the_shop() {
    let t = TestApp::start().await;
    let p = plant(&t).await;

    // Flour and sugar bought, each in two lots.
    for (item, lots) in [
        (
            &p.flour,
            json!([{ "lot": "FL-1", "qty": "6" }, { "lot": "FL-2", "qty": "6" }]),
        ),
        (
            &p.sugar,
            json!([{ "lot": "SU-1", "qty": "6" }, { "lot": "SU-2", "qty": "6" }]),
        ),
    ] {
        let order = p.order(&t, item, "12").await;
        let receipt = p
            .draft(
                &t,
                "purchase_receipt",
                "2026-03-02",
                json!({ "purchase_order_id": order["id"], "lines": [
                    { "order_line_id": order["body"]["lines"][0]["id"], "qty": "12",
                      "lots": lots }] }),
            )
            .await;
        p.ok(&t, "purchase_receipt.post", json!({ "id": id(&receipt) }))
            .await;
    }

    // A cake is 2 flour and 1 sugar. The order bakes 3, consuming FL-1 and
    // SU-2, and makes lot CK-1.
    let bom = t
        .create(
            "bom",
            json!({ "item_id": p.cake, "code": "CAKE", "name": "Cake", "output_qty": "1" }),
        )
        .await;
    for (item, qty) in [(&p.flour, "2"), (&p.sugar, "1")] {
        t.create(
            "bom_line",
            json!({ "bom_id": bom, "item_id": item, "qty": qty }),
        )
        .await;
    }
    let order = p
        .draft(
            &t,
            "production_order",
            "2026-03-05",
            json!({ "bom_id": bom, "qty": "3", "warehouse_id": p.plant }),
        )
        .await;
    let released = p
        .ok(&t, "production_order.release", json!({ "id": id(&order) }))
        .await;
    let components = released["body"]["components"].as_array().unwrap().clone();
    let line_of = |item: &str| {
        components
            .iter()
            .find(|c| c["item_id"] == json!(item))
            .unwrap()["id"]
            .clone()
    };
    let (status, body) = p
        .run(&t, "production_order.complete", json!({ "id": id(&order) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("tracked by lot"), "{body}");
    let made = p
        .ok(
            &t,
            "production_order.complete",
            json!({ "id": id(&order),
                    "lots": [{ "lot": "CK-1", "qty": "3", "expiry_date": "2026-03-12",
                               "manufactured_date": "2026-03-05" }],
                    "components": [
                        { "line_id": line_of(&p.flour), "lots": [{ "lot": "FL-1", "qty": "6" }] },
                        { "line_id": line_of(&p.sugar), "lots": [{ "lot": "SU-2", "qty": "3" }] }] }),
        )
        .await;
    assert_eq!(made["status"], "completed", "{made}");
    assert_eq!(p.lots(&t, &p.cake).await, vec![("CK-1".into(), 3.0)]);

    // Two cakes of CK-1 go to the shop.
    let sale = p
        .draft(
            &t,
            "sales_order",
            "2026-03-06",
            json!({ "customer_id": p.customer, "lines": [
                { "item_id": p.cake, "warehouse_id": p.plant, "qty": "2",
                  "unit_price": "10.00" }] }),
        )
        .await;
    p.ok(&t, "sales_order.approve", json!({ "id": id(&sale) }))
        .await;
    let delivery = p
        .ok(&t, "sales_order.deliver", json!({ "id": id(&sale) }))
        .await;
    let delivery = p
        .edit(&t, &delivery, |b| {
            b["lines"][0]["lots"] = json!([{ "lot": "CK-1", "qty": "2" }]);
        })
        .await;
    p.ok(&t, "sales_delivery.post", json!({ "id": id(&delivery) }))
        .await;
    // The delivery note shows the lot.
    let printed = p
        .ok(
            &t,
            "document.render",
            json!({ "doc_type": "sales_delivery", "id": id(&delivery) }),
        )
        .await;
    assert!(
        printed["html"].as_str().unwrap().contains("CK-1 × 2"),
        "{}",
        printed["html"]
    );

    // One comes back: only a lot that went out to the shop can.
    let back = p
        .ok(&t, "sales_order.return", json!({ "id": id(&sale) }))
        .await;
    assert_eq!(
        lots_of(&back["body"]["lines"][0]["lots"]),
        vec![("CK-1".into(), 2.0)]
    );
    let wrong = p
        .edit(&t, &back, |b| {
            b["lines"][0]["qty"] = json!("1");
            b["lines"][0]["lots"] = json!([{ "lot": "CK-9", "qty": "1" }]);
        })
        .await;
    let (status, body) = p
        .run(&t, "sales_return.post", json!({ "id": id(&wrong) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("lot CK-9"), "{body}");
    let back = p
        .edit(&t, &wrong, |b| {
            b["lines"][0]["lots"] = json!([{ "lot": "CK-1", "qty": "1" }]);
        })
        .await;
    p.ok(&t, "sales_return.post", json!({ "id": id(&back) }))
        .await;
    assert_eq!(p.lots(&t, &p.cake).await, vec![("CK-1".into(), 2.0)]);

    // Forward from flour lot FL-1: into CK-1, and to the shop.
    let trace = |item: &str, lot: &str, direction: &str| {
        let input = json!({ "company_id": p.c, "item_id": item, "lot": lot,
                            "direction": direction });
        let t = &t;
        async move {
            let (status, body) = t.command(&t.admin, "stock.trace", input).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body
        }
    };
    let forward = trace(&p.flour, "FL-1", "forward").await;
    let reached: Vec<(String, i64)> = forward["lots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["lot"].as_str().unwrap().to_owned(),
                l["depth"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        reached,
        vec![("FL-1".into(), 0), ("CK-1".into(), 1)],
        "{forward}"
    );
    assert_eq!(forward["parties"][0]["name"], "Corner Shop", "{forward}");
    assert_eq!(forward["parties"].as_array().unwrap().len(), 1);
    // FL-2 went nowhere.
    let unused = trace(&p.flour, "FL-2", "forward").await;
    assert_eq!(unused["lots"].as_array().unwrap().len(), 1);
    assert_eq!(unused["parties"], json!([]));

    // Backward from CK-1: the flour and sugar lots it was made of, and the
    // mill that supplied them; not SU-1 or FL-2.
    let backward = trace(&p.cake, "CK-1", "backward").await;
    let mut from: Vec<String> = backward["lots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["lot"].as_str().unwrap().to_owned())
        .collect();
    from.sort();
    assert_eq!(from, vec!["CK-1", "FL-1", "SU-2"], "{backward}");
    assert_eq!(backward["parties"][0]["name"], "North Mill", "{backward}");
    let kinds: Vec<&str> = backward["movements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["doc_type"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"purchase_receipt") && kinds.contains(&"production_order"));
    t.stop().await;
}
