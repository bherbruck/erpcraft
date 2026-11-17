//! Sales across documents and both ledgers: order, partial deliveries,
//! invoices, and taking them back.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    item: String,
    warehouse: String,
    customer: String,
    inventory: String,
    adjustments: String,
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
    let customer = t
        .create("customer", json!({ "code": "C001", "name": "Buyers Ltd" }))
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
    let receivable = account("1200", "Accounts receivable", "asset").await;
    let inventory = account("1300", "Inventory", "asset").await;
    let cogs = account("5000", "Cost of goods sold", "expense").await;
    let adjustments = account("5100", "Stock adjustments", "expense").await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id,
            "receivable_account_id": receivable,
            "revenue_account_id": c.revenue,
            "cogs_account_id": cogs,
            "inventory_account_id": inventory,
        }),
    )
    .await;
    Fixture {
        c,
        item,
        warehouse,
        customer,
        inventory,
        adjustments,
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

    /// Put `qty` widgets on hand at `unit_cost`, with a stock adjustment.
    async fn stock_in(&self, t: &TestApp, qty: &str, unit_cost: &str) {
        let draft = self
            .document(t, "stock_adjustment", json!({
                    "inventory_account_id": self.inventory,
                    "offset_account_id": self.adjustments,
                    "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_cost": unit_cost }],
                }),
            )
            .await;
        let (status, body) = t
            .command(
                &t.admin,
                "stock_adjustment.post",
                json!({ "id": id(&draft) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// An approved order for `qty` widgets at `price`. Returns the order.
    async fn order(&self, t: &TestApp, qty: &str, price: &str) -> Value {
        let draft = self
            .document(
                t,
                "sales_order",
                json!({
                    "customer_id": self.customer,
                    "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }],
                }),
            )
            .await;
        let (status, order) = t
            .command(&t.admin, "sales_order.approve", json!({ "id": id(&draft) }))
            .await;
        assert_eq!(status, StatusCode::OK, "{order}");
        order
    }

    async fn deliver(&self, t: &TestApp, order: &Value, qty: &str) -> (StatusCode, Value) {
        let body = json!({
            "sales_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }],
        });
        self.posted(t, "sales_delivery", "sales_delivery.post", body)
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
            "sales_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty, "unit_price": price }],
        });
        self.posted(t, "sales_invoice", "sales_invoice.post", body)
            .await
    }

    /// Balances of inventory, receivables, revenue and cost of goods sold,
    /// then the quantity on hand. Revenue is negative.
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
            balance("1200"),
            balance("4000"),
            balance("5000"),
            qty,
        ]
    }

    async fn order_now(&self, t: &TestApp, order: &Value) -> Value {
        let (status, doc) = t
            .get(
                &t.admin,
                &format!("/api/documents/sales_order/{}", id(order)),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }
}

#[tokio::test]
async fn an_order_is_delivered_in_parts_invoiced_and_closed() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // 10 at 2.00, then 10 at 4.00: 20 on hand worth 60, average 3.00.
    f.stock_in(&t, "10", "2.00").await;
    f.stock_in(&t, "10", "4.00").await;
    let order = f.order(&t, "10", "5.00").await;
    assert_eq!(order["status"], "open");
    assert_eq!(order["number"], "SO-2026-00001");
    assert_eq!(
        f.books(&t).await,
        [60.0, 0.0, 0.0, 0.0, 20.0],
        "an order posts nothing"
    );

    // Four leave: out of stock at the average cost, not yet billed.
    let (status, delivery) = f.deliver(&t, &order, "4").await;
    assert_eq!(status, StatusCode::OK, "{delivery}");
    assert_eq!(delivery["number"], "DN-2026-00001");
    assert_eq!(dec(&delivery["body"]["lines"][0]["value"]), 12.0);
    assert_eq!(f.books(&t).await, [48.0, 0.0, 0.0, 12.0, 16.0]);

    // The documents know each other.
    assert_eq!(delivery["links"][0]["relation"], "source");
    assert_eq!(delivery["links"][0]["number"], "SO-2026-00001");
    let now = f.order_now(&t, &order).await;
    assert_eq!(now["links"][0]["relation"], "result");
    assert_eq!(now["links"][0]["kind"], "delivery");
    assert_eq!(now["links"][0]["number"], "DN-2026-00001");
    assert_eq!(dec(&now["body"]["lines"][0]["qty_delivered"]), 4.0);
    assert_eq!(now["status"], "open");

    // Not more than was ordered, and no invoice for what has not left.
    let (status, body) = f.deliver(&t, &order, "7").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["message"],
        "line 1: 6 are still to be delivered on order line 1, not 7"
    );
    let (status, body) = f.invoice(&t, &order, "5", "5.00").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["message"],
        "line 1: 4 have been delivered and not yet invoiced on order line 1, not 5"
    );
    assert_eq!(f.books(&t).await, [48.0, 0.0, 0.0, 12.0, 16.0]);

    // The four are billed at the order price.
    let (status, invoice) = f.invoice(&t, &order, "4", "5.00").await;
    assert_eq!(status, StatusCode::OK, "{invoice}");
    assert_eq!(invoice["number"], "SI-2026-00001");
    assert_eq!(f.books(&t).await, [48.0, 20.0, -20.0, 12.0, 16.0]);

    // Dearer stock arrives: 16 worth 48 and 4 at 5.50 make 20 worth 70,
    // average 3.50. The other six leave at that, and are billed. The order
    // closes by itself.
    f.stock_in(&t, "4", "5.50").await;
    let (status, second) = f.deliver(&t, &order, "6").await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(dec(&second["body"]["lines"][0]["value"]), 21.0);
    assert_eq!(f.books(&t).await, [49.0, 20.0, -20.0, 33.0, 14.0]);
    let (status, last_invoice) = f.invoice(&t, &order, "6", "5.00").await;
    assert_eq!(status, StatusCode::OK, "{last_invoice}");
    assert_eq!(f.books(&t).await, [49.0, 50.0, -50.0, 33.0, 14.0]);
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
    let (status, body) = reverse("sales_delivery.reverse", &second).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["message"],
        "line 1: these goods have been invoiced; reverse the invoice first"
    );
    let (status, body) = reverse("sales_invoice.reverse", &last_invoice).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.books(&t).await, [49.0, 20.0, -20.0, 33.0, 14.0]);
    assert_eq!(f.order_now(&t, &order).await["status"], "open");
    // The goods come back for exactly what they left for.
    let (status, body) = reverse("sales_delivery.reverse", &second).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "reversed");
    assert_eq!(f.books(&t).await, [70.0, 20.0, -20.0, 12.0, 20.0]);
    let now = f.order_now(&t, &order).await;
    assert_eq!(dec(&now["body"]["lines"][0]["qty_delivered"]), 4.0);
    assert_eq!(dec(&now["body"]["lines"][0]["qty_invoiced"]), 4.0);

    // The customer does not want the rest: the order is closed short.
    let (status, body) = t
        .command(&t.admin, "sales_order.close", json!({ "id": id(&order) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "closed");
    let (status, body) = f.deliver(&t, &order, "1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn goods_that_are_not_on_hand_cannot_be_delivered() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock_in(&t, "3", "2.00").await;
    let order = f.order(&t, "10", "5.00").await;
    let postings = t.count("SELECT count(*) FROM posting").await;

    // Five are ordered for, but only three are there.
    let (status, body) = f.deliver(&t, &order, "5").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("are on hand in warehouse MAIN"),
        "{body}"
    );
    // Nothing happened: no posting, no number used, no progress.
    assert_eq!(f.books(&t).await, [6.0, 0.0, 0.0, 0.0, 3.0]);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, postings);
    let now = f.order_now(&t, &order).await;
    assert_eq!(dec(&now["body"]["lines"][0]["qty_delivered"]), 0.0);
    assert_eq!(now["status"], "open");

    // The three that are there can go, and take all of the value along.
    let (status, delivery) = f.deliver(&t, &order, "3").await;
    assert_eq!(status, StatusCode::OK, "{delivery}");
    assert_eq!(delivery["number"], "DN-2026-00001");
    assert_eq!(f.books(&t).await, [0.0, 0.0, 0.0, 6.0, 0.0]);

    t.stop().await;
}

#[tokio::test]
async fn an_invoice_is_rounded_to_the_currency() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock_in(&t, "3", "1.00").await;
    let order = f.order(&t, "3", "3.3333").await;
    let (status, _) = f.deliver(&t, &order, "3").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = f.invoice(&t, &order, "1", "3.3333").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.books(&t).await, [0.0, 3.33, -3.33, 3.0, 0.0]);
    // 2 x 3.3333 is 6.67.
    let (status, body) = f.invoice(&t, &order, "2", "3.3333").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.books(&t).await, [0.0, 10.0, -10.0, 3.0, 0.0]);
    assert_eq!(f.order_now(&t, &order).await["status"], "closed");

    t.stop().await;
}

#[tokio::test]
async fn two_deliveries_at_once_cannot_exceed_the_order() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock_in(&t, "20", "1.00").await;
    let order = f.order(&t, "10", "2.00").await;
    let (a, b) = tokio::join!(f.deliver(&t, &order, "6"), f.deliver(&t, &order, "6"));
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::UNPROCESSABLE_ENTITY],
        "{} / {}",
        a.1,
        b.1
    );
    assert_eq!(f.books(&t).await, [14.0, 0.0, 0.0, 6.0, 14.0]);
    let now = f.order_now(&t, &order).await;
    assert_eq!(dec(&now["body"]["lines"][0]["qty_delivered"]), 6.0);

    t.stop().await;
}

#[tokio::test]
async fn orders_follow_their_workflow_and_their_permissions() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock_in(&t, "20", "1.00").await;

    // An order nothing was delivered against can be cancelled, and is then
    // closed to deliveries.
    let order = f.order(&t, "5", "2.00").await;
    let (status, body) = t
        .command(&t.admin, "sales_order.cancel", json!({ "id": id(&order) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");
    let (status, _) = f.deliver(&t, &order, "1").await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Once goods have left it cannot be cancelled, and it cannot be closed
    // while they are unbilled.
    let order = f.order(&t, "5", "2.00").await;
    let (status, _) = f.deliver(&t, &order, "2").await;
    assert_eq!(status, StatusCode::OK);
    for command in ["sales_order.cancel", "sales_order.close"] {
        let (status, body) = t
            .command(&t.admin, command, json!({ "id": id(&order) }))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{command}: {body}");
    }

    // An approved order can no longer be edited.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_order/{}", id(&order)),
            json!({ "version": order["version"], "memo": "changed my mind" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // A draft must make sense before it is approved or posted.
    let empty = f
        .document(
            &t,
            "sales_order",
            json!({ "customer_id": f.customer, "lines": [] }),
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "sales_order.approve", json!({ "id": id(&empty) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/sales_delivery",
            json!({
                "company_id": f.c.id, "doc_date": "2026-03-15",
                "body": { "sales_order_id": f.c.cash, "lines": [] },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Delivering needs the right to post deliveries and to read the order.
    let draft = f
        .document(
            &t,
            "sales_delivery",
            json!({
                "sales_order_id": order["id"],
                "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": "1" }],
            }),
        )
        .await;
    let dock = t
        .user_with_role(
            "dock@example.com",
            &["sales_delivery:read", "sales_delivery:post"],
            &f.c.id,
        )
        .await;
    let (status, body) = t
        .command(&dock, "sales_delivery.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // And the delivery does not show them an order they may not read.
    let (_, seen) = t
        .get(
            &dock,
            &format!("/api/documents/sales_delivery/{}", id(&draft)),
        )
        .await;
    assert_eq!(seen["status"], "draft");
    assert_eq!(seen["links"], json!([]));

    let shipper = t
        .user_with_role(
            "shipper@example.com",
            &[
                "sales_delivery:read",
                "sales_delivery:post",
                "sales_order:read",
            ],
            &f.c.id,
        )
        .await;
    let (status, body) = t
        .command(&shipper, "sales_delivery.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["links"][0]["doc_type"], "sales_order");

    // An order for a customer who is no longer active cannot be approved.
    let gone = t
        .create(
            "customer",
            json!({ "code": "C002", "name": "Gone Ltd", "active": false }),
        )
        .await;
    let draft = f
        .document(
            &t,
            "sales_order",
            json!({
                "customer_id": gone,
                "lines": [{ "item_id": f.item, "warehouse_id": f.warehouse, "qty": "1", "unit_price": "1" }],
            }),
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "sales_order.approve", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["message"], "the customer is inactive");

    // A company without sales settings cannot approve an order.
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
            "/api/documents/sales_order",
            json!({
                "company_id": other.id, "doc_date": "2026-03-15",
                "body": { "customer_id": f.customer, "lines": [
                    { "item_id": f.item, "warehouse_id": warehouse, "qty": "1", "unit_price": "1" },
                ]},
            }),
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "sales_order.approve", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("sales settings"),
        "{body}"
    );
    // Nor can it be given accounts of another company.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/sales_settings",
            json!({
                "company_id": other.id,
                "receivable_account_id": f.c.cash,
                "revenue_account_id": other.revenue,
                "cogs_account_id": other.cash,
                "inventory_account_id": other.cash,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}
