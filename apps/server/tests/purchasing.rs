//! Purchasing across documents and both ledgers: order, partial receipts,
//! invoices, and taking them back.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    item: String,
    warehouse: String,
    vendor: String,
}

async fn setup(t: &TestApp) -> Fixture {
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
    let payable = account("2000", "Accounts payable", "liability").await;
    let grni = account("2100", "Goods received not invoiced", "liability").await;
    let variance = account("5200", "Purchase price variance", "expense").await;
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id,
            "inventory_account_id": inventory,
            "grni_account_id": grni,
            "payable_account_id": payable,
            "price_variance_account_id": variance,
        }),
    )
    .await;
    Fixture {
        c,
        item,
        warehouse,
        vendor,
    }
}

impl Fixture {
    async fn document(&self, t: &TestApp, doc_type: &str, body: Value) -> Value {
        let (status, doc) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{doc}");
        doc
    }

    /// Draft a document and post it with `command`. If the draft itself is
    /// refused, that refusal is returned.
    async fn posted(
        &self,
        t: &TestApp,
        doc_type: &str,
        command: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let (status, doc) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        if status != StatusCode::CREATED {
            return (status, doc);
        }
        t.command(&t.admin, command, json!({ "id": id(&doc) }))
            .await
    }

    /// An approved order for `qty` widgets at `price`. Returns the order.
    async fn order(&self, t: &TestApp, qty: &str, price: &str) -> Value {
        let draft = self
            .document(t, "purchase_order", json!({
                    "vendor_id": self.vendor,
                    "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }],
                }),
            )
            .await;
        let (status, order) = t
            .command(
                &t.admin,
                "purchase_order.approve",
                json!({ "id": id(&draft) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{order}");
        order
    }

    async fn receive(&self, t: &TestApp, order: &Value, qty: &str) -> (StatusCode, Value) {
        let body = json!({
            "purchase_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }],
        });
        self.posted(t, "purchase_receipt", "purchase_receipt.post", body)
            .await
    }

    async fn invoice(
        &self,
        t: &TestApp,
        order: &Value,
        qty: &str,
        price: &str,
    ) -> (StatusCode, Value) {
        let body = json!({
            "purchase_order_id": order["id"],
            "vendor_reference": "INV-77",
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty, "unit_price": price }],
        });
        self.posted(t, "vendor_invoice", "vendor_invoice.post", body)
            .await
    }

    /// Balances of inventory, payables, GRNI and price variance, then the
    /// quantity on hand. Liabilities are negative.
    async fn books(&self, t: &TestApp) -> [f64; 5] {
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        let balance = |code: &str| {
            tb["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["code"] == code)
                .map(|r| dec(&r["balance"]))
                .unwrap()
        };
        let (_, on_hand) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id }),
            )
            .await;
        let qty = on_hand["rows"]
            .as_array()
            .unwrap()
            .first()
            .map_or(0.0, |r| dec(&r["qty"]));
        assert_eq!(
            dec(&on_hand["total_value"]),
            balance("1300"),
            "stock value and the inventory account agree"
        );
        [
            balance("1300"),
            balance("2000"),
            balance("2100"),
            balance("5200"),
            qty,
        ]
    }

    async fn order_now(&self, t: &TestApp, order: &Value) -> Value {
        let (status, doc) = t
            .get(
                &t.admin,
                &format!("/api/documents/purchase_order/{}", id(order)),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }
}

#[tokio::test]
async fn an_order_is_received_in_parts_invoiced_and_closed() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "10", "2.50").await;
    assert_eq!(order["status"], "open");
    assert_eq!(order["number"], "PO-2026-00001");
    assert_eq!(
        f.books(&t).await,
        [0.0, 0.0, 0.0, 0.0, 0.0],
        "an order posts nothing"
    );

    // Four arrive: into stock at the order price, owed but not yet billed.
    let (status, receipt) = f.receive(&t, &order, "4").await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["number"], "GR-2026-00001");
    assert_eq!(f.books(&t).await, [10.0, 0.0, -10.0, 0.0, 4.0]);

    // The documents know each other.
    assert_eq!(receipt["links"][0]["relation"], "source");
    assert_eq!(receipt["links"][0]["number"], "PO-2026-00001");
    let now = f.order_now(&t, &order).await;
    assert_eq!(now["links"][0]["relation"], "result");
    assert_eq!(now["links"][0]["kind"], "receipt");
    assert_eq!(now["links"][0]["number"], "GR-2026-00001");
    assert_eq!(dec(&now["body"]["lines"][0]["qty_received"]), 4.0);
    assert_eq!(now["status"], "open");

    // Not more than was ordered, and no invoice for what has not arrived.
    let (status, body) = f.receive(&t, &order, "7").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["message"],
        "line 1: 6 are still to be received on order line 1, not 7"
    );
    let (status, body) = f.invoice(&t, &order, "5", "2.50").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(f.books(&t).await, [10.0, 0.0, -10.0, 0.0, 4.0]);

    // The vendor bills the four at 2.75, not 2.50: one dollar of variance.
    let (status, invoice) = f.invoice(&t, &order, "4", "2.75").await;
    assert_eq!(status, StatusCode::OK, "{invoice}");
    assert_eq!(invoice["number"], "VI-2026-00001");
    assert_eq!(invoice["body"]["vendor_reference"], "INV-77");
    assert_eq!(f.books(&t).await, [10.0, -11.0, 0.0, 1.0, 4.0]);

    // The other six arrive and are billed at the order price. The order
    // closes by itself.
    let (status, second) = f.receive(&t, &order, "6").await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(f.books(&t).await, [25.0, -11.0, -15.0, 1.0, 10.0]);
    let (status, last_invoice) = f.invoice(&t, &order, "6", "2.50").await;
    assert_eq!(status, StatusCode::OK, "{last_invoice}");
    assert_eq!(f.books(&t).await, [25.0, -26.0, 0.0, 1.0, 10.0]);
    let now = f.order_now(&t, &order).await;
    assert_eq!(now["status"], "closed");
    assert_eq!(
        now["links"].as_array().unwrap().len(),
        6,
        "every attempt is linked"
    );

    // Taking it back, newest first. The order reopens.
    let reverse = |name: &'static str, doc: &Value| {
        let (t, doc_id) = (&t, id(doc));
        async move { t.command(&t.admin, name, json!({ "id": doc_id })).await }
    };
    let (status, body) = reverse("purchase_receipt.reverse", &second).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["message"],
        "line 1: these goods have been invoiced; reverse the invoice first"
    );
    let (status, body) = reverse("vendor_invoice.reverse", &last_invoice).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.books(&t).await, [25.0, -11.0, -15.0, 1.0, 10.0]);
    assert_eq!(f.order_now(&t, &order).await["status"], "open");
    let (status, body) = reverse("purchase_receipt.reverse", &second).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.books(&t).await, [10.0, -11.0, 0.0, 1.0, 4.0]);
    let now = f.order_now(&t, &order).await;
    assert_eq!(dec(&now["body"]["lines"][0]["qty_received"]), 4.0);
    assert_eq!(dec(&now["body"]["lines"][0]["qty_invoiced"]), 4.0);

    // The vendor will not deliver the rest: the order is closed short.
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.close",
            json!({ "id": id(&order) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "closed");
    let (status, body) = f.receive(&t, &order, "1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn rounding_leaves_nothing_in_goods_received_not_invoiced() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Three for 10.00 in all.
    let order = f.order(&t, "3", "3.3333").await;
    let (status, _) = f.receive(&t, &order, "3").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(f.books(&t).await, [10.0, 0.0, -10.0, 0.0, 3.0]);

    let (status, body) = f.invoice(&t, &order, "1", "3.3333").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.books(&t).await, [10.0, -3.33, -6.67, 0.0, 3.0]);
    let (status, body) = f.invoice(&t, &order, "2", "3.3333").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 2 x 3.3333 is 6.67 charged, and 6.67 was left to clear.
    assert_eq!(f.books(&t).await, [10.0, -10.0, 0.0, 0.0, 3.0]);
    assert_eq!(f.order_now(&t, &order).await["status"], "closed");

    t.stop().await;
}

#[tokio::test]
async fn two_receipts_at_once_cannot_exceed_the_order() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "10", "1.00").await;
    let (a, b) = tokio::join!(f.receive(&t, &order, "6"), f.receive(&t, &order, "6"));
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::UNPROCESSABLE_ENTITY],
        "{} / {}",
        a.1,
        b.1
    );
    assert_eq!(f.books(&t).await, [6.0, 0.0, -6.0, 0.0, 6.0]);

    t.stop().await;
}

#[tokio::test]
async fn orders_follow_their_workflow_and_their_permissions() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // An order nothing was received against can be cancelled, and is then
    // closed to receipts.
    let order = f.order(&t, "5", "1.00").await;
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.cancel",
            json!({ "id": id(&order) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");
    let (status, _) = f.receive(&t, &order, "1").await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Once goods have arrived it cannot be cancelled, and it cannot be
    // closed while they are unbilled.
    let order = f.order(&t, "5", "1.00").await;
    let (status, _) = f.receive(&t, &order, "2").await;
    assert_eq!(status, StatusCode::OK);
    for command in ["purchase_order.cancel", "purchase_order.close"] {
        let (status, body) = t
            .command(&t.admin, command, json!({ "id": id(&order) }))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{command}: {body}");
    }

    // An approved order can no longer be edited.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/documents/purchase_order/{}", id(&order)),
            json!({ "version": order["version"], "memo": "changed my mind" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // A draft must make sense before it is approved or posted.
    let empty = f
        .document(
            &t,
            "purchase_order",
            json!({ "vendor_id": f.vendor, "lines": [] }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.approve",
            json!({ "id": id(&empty) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/purchase_receipt",
            json!({
                "company_id": f.c.id, "doc_date": "2026-03-15",
                "body": { "purchase_order_id": f.c.cash, "lines": [] },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Receiving needs the right to post receipts and to read the order.
    let draft = f
        .document(
            &t,
            "purchase_receipt",
            json!({
                "purchase_order_id": order["id"],
                "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": "1" }],
            }),
        )
        .await;
    let dock = t
        .user_with_role(
            "dock@example.com",
            &["purchase_receipt:read", "purchase_receipt:post"],
            &f.c.id,
        )
        .await;
    let (status, body) = t
        .command(&dock, "purchase_receipt.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // And the receipt does not show them an order they may not read.
    let (_, seen) = t
        .get(
            &dock,
            &format!("/api/documents/purchase_receipt/{}", id(&draft)),
        )
        .await;
    assert_eq!(seen["links"], json!([]));

    let receiver = t
        .user_with_role(
            "receiver@example.com",
            &[
                "purchase_receipt:read",
                "purchase_receipt:post",
                "purchase_order:read",
            ],
            &f.c.id,
        )
        .await;
    let (status, body) = t
        .command(
            &receiver,
            "purchase_receipt.post",
            json!({ "id": id(&draft) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["links"][0]["doc_type"], "purchase_order");

    // A company without purchasing settings cannot approve an order.
    let other = t.company("OTHER", "USD").await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": other.id, "code": "W", "name": "W" }),
        )
        .await;
    let (_, draft) = t
        .post(
            &t.admin,
            "/api/documents/purchase_order",
            json!({
                "company_id": other.id, "doc_date": "2026-03-15",
                "body": { "vendor_id": f.vendor, "lines": [
                    { "item_id": f.item, "warehouse_id": warehouse, "qty": "1", "unit_price": "1" },
                ]},
            }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.approve",
            json!({ "id": id(&draft) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("purchasing settings"),
        "{body}"
    );

    t.stop().await;
}
