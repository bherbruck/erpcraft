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
