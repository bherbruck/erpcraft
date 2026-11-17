//! Cases found by review: each was a bug once.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

struct Purchasing {
    company: String,
    item: String,
    warehouse: String,
    vendor: String,
}

async fn purchasing(t: &TestApp) -> Purchasing {
    let c = t.company("ACME", "USD").await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let mut accounts = vec![];
    for (code, kind) in [
        ("1300", "asset"),
        ("2100", "liability"),
        ("2000", "liability"),
        ("5200", "expense"),
    ] {
        accounts.push(
            t.create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind }),
            )
            .await,
        );
    }
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id,
            "inventory_account_id": accounts[0],
            "grni_account_id": accounts[1],
            "payable_account_id": accounts[2],
            "price_variance_account_id": accounts[3],
        }),
    )
    .await;
    Purchasing {
        company: c.id,
        item,
        warehouse,
        vendor,
    }
}

impl Purchasing {
    async fn create(&self, t: &TestApp, doc_type: &str, body: Value) -> (StatusCode, Value) {
        t.post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": self.company, "doc_date": "2026-03-15", "body": body }),
        )
        .await
    }

    async fn draft_order(&self, t: &TestApp, qty: &str, price: &str) -> Value {
        let (status, order) = self
            .create(
                t,
                "purchase_order",
                json!({
                    "vendor_id": self.vendor,
                    "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }],
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{order}");
        order
    }

    async fn order(&self, t: &TestApp, qty: &str, price: &str) -> Value {
        let draft = self.draft_order(t, qty, price).await;
        let (status, order) = run(t, "purchase_order.approve", &draft).await;
        assert_eq!(status, StatusCode::OK, "{order}");
        order
    }

    async fn receive(&self, t: &TestApp, order: &Value, qty: &str) -> Value {
        let (status, draft) = self
            .create(
                t,
                "purchase_receipt",
                json!({
                    "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }],
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, posted) = run(t, "purchase_receipt.post", &draft).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        posted
    }

    async fn invoice(&self, t: &TestApp, order: &Value, qty: &str, price: &str) -> Value {
        let (status, draft) = self
            .create(
                t,
                "vendor_invoice",
                json!({
                    "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty, "unit_price": price }],
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, posted) = run(t, "vendor_invoice.post", &draft).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        posted
    }
}

async fn run(t: &TestApp, command: &str, doc: &Value) -> (StatusCode, Value) {
    t.command(&t.admin, command, json!({ "id": id(doc) })).await
}

#[tokio::test]
async fn reversing_a_receipt_after_a_partial_invoice_is_refused_in_plain_words() {
    let t = TestApp::start().await;
    let p = purchasing(&t).await;
    // Four at 0.125: receipts of 1, 1 and 2 come in for 0.13, 0.13 and
    // 0.25. An invoice for two clears the average, 0.26.
    let order = p.order(&t, "4", "0.125").await;
    let first = p.receive(&t, &order, "1").await;
    let second = p.receive(&t, &order, "1").await;
    p.receive(&t, &order, "2").await;
    let invoice = p.invoice(&t, &order, "2", "0.125").await;

    let (status, body) = run(&t, "purchase_receipt.reverse", &first).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Taking the second one back would leave 0.25 received against 0.26
    // cleared. That is refused with a way forward, not a database error.
    let (status, body) = run(&t, "purchase_receipt.reverse", &second).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reverse that invoice first"),
        "{body}"
    );
    // And the way forward works.
    let (status, body) = run(&t, "vendor_invoice.reverse", &invoice).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = run(&t, "purchase_receipt.reverse", &second).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn an_order_moves_only_along_its_workflow() {
    let t = TestApp::start().await;
    let p = purchasing(&t).await;

    // Received two, invoiced two, closed short by a person.
    let order = p.order(&t, "5", "1.00").await;
    p.receive(&t, &order, "2").await;
    let invoice = p.invoice(&t, &order, "2", "1.00").await;
    let (status, closed) = run(&t, "purchase_order.close", &order).await;
    assert_eq!(status, StatusCode::OK, "{closed}");

    // Approving is for drafts: it does not reopen a closed order.
    let (status, body) = run(&t, "purchase_order.approve", &order).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["message"],
        "a closed purchase order cannot be approved"
    );
    let (status, _) = run(&t, "purchase_order.cancel", &order).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Reversing its invoice leaves goods received and unbilled, so the
    // order has to be open again for the corrected invoice.
    let (status, body) = run(&t, "vendor_invoice.reverse", &invoice).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, now) = t
        .get(
            &t.admin,
            &format!("/api/documents/purchase_order/{}", id(&order)),
        )
        .await;
    assert_eq!(now["status"], "open");

    // A receipt or an invoice can be drafted only against an approved
    // order, so a draft order stays free to change.
    let draft = p.draft_order(&t, "5", "1.00").await;
    let (status, body) = p
        .create(
            &t,
            "purchase_receipt",
            json!({
                "purchase_order_id": draft["id"],
                "lines": [{ "order_line_id": draft["body"]["lines"][0]["id"], "qty": "1" }],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/documents/purchase_order/{}", id(&draft)),
            json!({ "version": draft["version"], "body": { "vendor_id": p.vendor, "lines": [] } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A price finer than the system keeps is refused, not rounded quietly.
    let (status, body) = p
        .create(
            &t,
            "purchase_order",
            json!({
                "vendor_id": p.vendor,
                "lines": [{ "item_id": p.item, "warehouse_id": p.warehouse, "qty": "1", "unit_price": "1.0000001" }],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn writing_a_record_does_not_show_what_reading_would_not() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // May rename accounts, may not read them.
    let (writer, _) = t
        .user_with_rules("writer@example.com", json!(["account:update"]), Some(&c.id))
        .await;
    let (status, body) = t
        .patch(
            &writer,
            &format!("/api/entities/account/{}", c.cash),
            json!({ "name": "Till" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], c.cash);
    assert!(body["_version"].is_string());
    assert!(body.get("name").is_none(), "{body}");
    assert!(body.get("code").is_none(), "{body}");

    // The reasons other people's attempts failed can quote what they
    // typed, so they are for auditors only.
    let (reader, _) = t
        .user_with_rules("reader@example.com", json!(["account:read"]), Some(&c.id))
        .await;
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account/{}", c.cash),
            json!({ "account_type": "secret-plan" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let history = json!({ "resource": "account", "id": c.cash });
    let (_, seen) = t.command(&reader, "audit.history", history.clone()).await;
    let failed = &seen["items"][0];
    assert_eq!(failed["outcome"], "error");
    assert_eq!(failed["error_code"], "validation");
    assert!(failed["error_message"].is_null(), "{failed}");
    let (_, seen) = t.command(&t.admin, "audit.history", history).await;
    assert!(seen["items"][0]["error_message"].is_string());

    t.stop().await;
}

#[tokio::test]
async fn the_on_hand_report_withholds_what_stock_entries_withhold() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/stock_adjustment",
            json!({
                "company_id": c.id, "doc_date": "2026-03-15",
                "body": {
                    "inventory_account_id": c.cash, "offset_account_id": c.revenue,
                    "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "10", "unit_cost": "2.00" }],
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, body) = run(&t, "stock_adjustment.post", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let on_hand = json!({ "company_id": c.id });

    // A picker counts stock but is not shown what it is worth, and has no
    // access to the item list.
    let (picker, _) = t
        .user_with_rules(
            "picker@example.com",
            json!([
                "stock_entry:read",
                { "deny": "stock_entry:read", "fields": ["value", "unit_cost", "value_after"] },
            ]),
            Some(&c.id),
        )
        .await;
    let (status, seen) = t.command(&picker, "stock.on_hand", on_hand.clone()).await;
    assert_eq!(status, StatusCode::OK, "{seen}");
    let row = &seen["rows"][0];
    assert_eq!(dec(&row["qty"]), 10.0);
    for hidden in [
        "value",
        "average_cost",
        "item_code",
        "item_name",
        "warehouse_code",
    ] {
        assert!(row[hidden].is_null(), "{hidden}: {row}");
    }
    assert!(seen["total_value"].is_null(), "{seen}");

    let (_, seen) = t.command(&t.admin, "stock.on_hand", on_hand).await;
    assert_eq!(dec(&seen["rows"][0]["value"]), 20.0);
    assert_eq!(seen["rows"][0]["item_code"], "WIDGET");
    assert_eq!(dec(&seen["total_value"]), 20.0);

    t.stop().await;
}
