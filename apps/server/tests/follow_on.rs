//! References an interface can follow, and the commands that draft the
//! next document from the one it follows from.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

struct World {
    company: String,
    cash: String,
    widget: String,
    gadget: String,
    warehouse: String,
    vendor: String,
    customer: String,
}

async fn world(t: &TestApp) -> World {
    let c = t.company("ACME", "USD").await;
    let mut accounts = vec![];
    for (code, name, kind) in [
        ("1200", "Accounts receivable", "asset"),
        ("1300", "Inventory", "asset"),
        ("2000", "Accounts payable", "liability"),
        ("2100", "Goods received not invoiced", "liability"),
        ("5000", "Cost of goods sold", "expense"),
        ("5200", "Purchase price variance", "expense"),
    ] {
        accounts.push(
            t.create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": name, "account_type": kind }),
            )
            .await,
        );
    }
    let [receivable, inventory, payable, grni, cogs, variance] =
        <[String; 6]>::try_from(accounts).unwrap();
    t.inventory_rule(&c.id, &inventory).await;
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id,
            "grni_account_id": grni,
            "payable_account_id": payable,
            "price_variance_account_id": variance,
        }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id,
            "receivable_account_id": receivable,
            "revenue_account_id": c.revenue,
            "cogs_account_id": cogs,
        }),
    )
    .await;
    World {
        widget: t
            .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
            .await,
        gadget: t
            .create("item", json!({ "code": "GADGET", "name": "Gadget" }))
            .await,
        warehouse: t
            .create(
                "warehouse",
                json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
            )
            .await,
        vendor: t
            .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
            .await,
        customer: t
            .create("customer", json!({ "code": "C001", "name": "Buyers Ltd" }))
            .await,
        company: c.id,
        cash: c.cash,
    }
}

async fn run(t: &TestApp, name: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, name, input).await;
    assert_eq!(status, StatusCode::OK, "{name}: {body}");
    body
}

impl World {
    /// An approved order of `doc_type` for 10 widgets at 4.00 and 5 gadgets
    /// at 9.00.
    async fn order(&self, t: &TestApp, doc_type: &str, party: Value) -> Value {
        let mut body = json!({ "lines": [
            { "item_id": self.widget, "warehouse_id": self.warehouse, "qty": "10", "unit_price": "4.00" },
            { "item_id": self.gadget, "warehouse_id": self.warehouse, "qty": "5", "unit_price": "9.00" },
        ]});
        for (key, value) in party.as_object().unwrap() {
            body[key] = value.clone();
        }
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.company, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(
            t,
            &format!("{doc_type}.approve"),
            json!({ "id": draft["id"] }),
        )
        .await
    }
}

/// The lines of a draft as `(order line, quantity)`.
fn quantities(doc: &Value) -> Vec<(Value, f64)> {
    doc["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["order_line_id"].clone(), dec(&l["qty"])))
        .collect()
}

/// Save a changed body of a draft.
async fn save(t: &TestApp, doc: &Value, body: Value) -> Value {
    let (status, saved) = t
        .patch(
            &t.admin,
            &format!(
                "/api/documents/{}/{}",
                doc["doc_type"].as_str().unwrap(),
                id(doc)
            ),
            json!({ "version": doc["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

#[tokio::test]
async fn an_order_drafts_what_follows_from_it() {
    let t = TestApp::start().await;
    let w = world(&t).await;
    let order = w
        .order(&t, "purchase_order", json!({ "vendor_id": w.vendor }))
        .await;
    let lines: Vec<Value> = order["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].clone())
        .collect();

    // Nothing received yet: nothing to invoice.
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.invoice",
            json!({ "id": order["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nothing has been received"),
        "{body}"
    );

    // A receipt for everything ordered, as a draft linked to the order.
    let receipt = run(
        &t,
        "purchase_order.receive",
        json!({ "id": order["id"], "doc_date": "2026-03-20" }),
    )
    .await;
    assert_eq!(receipt["doc_type"], "purchase_receipt");
    assert_eq!(receipt["status"], "draft");
    assert_eq!(receipt["number"], Value::Null);
    assert_eq!(receipt["doc_date"], "2026-03-20");
    assert_eq!(receipt["postings"], json!([]));
    assert_eq!(receipt["body"]["purchase_order_id"], order["id"]);
    assert_eq!(
        quantities(&receipt),
        [(lines[0].clone(), 10.0), (lines[1].clone(), 5.0)]
    );
    assert_eq!(receipt["links"][0]["relation"], "source");
    assert_eq!(receipt["links"][0]["id"], order["id"]);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 0);
    assert_eq!(t.count("SELECT count(*) FROM stock_entry").await, 0);

    // Only 6 widgets arrived. The draft is an ordinary one: change, post.
    let mut body = receipt["body"].clone();
    body["lines"][0]["qty"] = json!("6");
    let receipt = save(&t, &receipt, body).await;
    run(&t, "purchase_receipt.post", json!({ "id": receipt["id"] })).await;

    // The next receipt is for what is still to come; a complete line is
    // left out. Without a date it is dated today.
    let next = run(&t, "purchase_order.receive", json!({ "id": order["id"] })).await;
    assert_eq!(quantities(&next), [(lines[0].clone(), 4.0)]);
    let today: String = sqlx::query_scalar("SELECT current_date::text")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let utc_today: String = sqlx::query_scalar("SELECT (now() AT TIME ZONE 'UTC')::date::text")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert!(next["doc_date"] == json!(today) || next["doc_date"] == json!(utc_today));

    // The invoice: what was received and not invoiced, at the order's price.
    let invoice = run(&t, "purchase_order.invoice", json!({ "id": order["id"] })).await;
    assert_eq!(invoice["doc_type"], "vendor_invoice");
    assert_eq!(invoice["status"], "draft");
    assert_eq!(
        quantities(&invoice),
        [(lines[0].clone(), 6.0), (lines[1].clone(), 5.0)]
    );
    assert_eq!(dec(&invoice["body"]["lines"][0]["unit_price"]), 4.0);
    assert_eq!(dec(&invoice["body"]["lines"][1]["unit_price"]), 9.0);
    let invoice = run(&t, "vendor_invoice.post", json!({ "id": invoice["id"] })).await;
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.invoice",
            json!({ "id": order["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // A payment for the open item the invoice left: 6 x 4 + 5 x 9 = 69.
    let item = invoice["postings"][0]["entries"]["open_items"][0].clone();
    let payment = run(
        &t,
        "payment.draft",
        json!({
            "company_id": w.company,
            "open_item_ids": [item["id"]],
            "bank_account_id": w.cash,
            "doc_date": "2026-04-01",
        }),
    )
    .await;
    assert_eq!(payment["doc_type"], "payment");
    assert_eq!(payment["status"], "draft");
    assert_eq!(payment["body"]["direction"], "outgoing");
    assert_eq!(payment["body"]["bank_account_id"], json!(w.cash));
    let allocations = payment["body"]["allocations"].as_array().unwrap();
    assert_eq!(allocations.len(), 1);
    assert_eq!(allocations[0]["open_item_id"], item["id"]);
    assert_eq!(dec(&allocations[0]["amount"]), 69.0);
    assert_eq!(t.count("SELECT count(*) FROM settlement").await, 0);
    run(&t, "payment.post", json!({ "id": payment["id"] })).await;
    // Paid: nothing left to draft a payment for.
    let (status, body) = t
        .command(
            &t.admin,
            "payment.draft",
            json!({ "company_id": w.company, "open_item_ids": [item["id"]], "bank_account_id": w.cash }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nothing is open"),
        "{body}"
    );
    let (status, body) = t
        .command(
            &t.admin,
            "payment.draft",
            json!({ "company_id": w.company, "open_item_ids": [], "bank_account_id": w.cash }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "open_item_ids");

    // Sales: the same from a sales order, out of the stock just received.
    let sale = w
        .order(&t, "sales_order", json!({ "customer_id": w.customer }))
        .await;
    let sold: Vec<Value> = sale["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].clone())
        .collect();
    let delivery = run(&t, "sales_order.deliver", json!({ "id": sale["id"] })).await;
    assert_eq!(delivery["doc_type"], "sales_delivery");
    assert_eq!(delivery["status"], "draft");
    assert_eq!(
        quantities(&delivery),
        [(sold[0].clone(), 10.0), (sold[1].clone(), 5.0)]
    );
    assert_eq!(delivery["links"][0]["id"], sale["id"]);
    // Only 6 widgets and 5 gadgets are there.
    let mut body = delivery["body"].clone();
    body["lines"][0]["qty"] = json!("6");
    let delivery = save(&t, &delivery, body).await;
    run(&t, "sales_delivery.post", json!({ "id": delivery["id"] })).await;
    let bill = run(&t, "sales_order.invoice", json!({ "id": sale["id"] })).await;
    assert_eq!(bill["doc_type"], "sales_invoice");
    assert_eq!(
        quantities(&bill),
        [(sold[0].clone(), 6.0), (sold[1].clone(), 5.0)]
    );
    assert_eq!(dec(&bill["body"]["lines"][0]["unit_price"]), 4.0);
    let bill = run(&t, "sales_invoice.post", json!({ "id": bill["id"] })).await;
    // What the customer owes becomes an incoming payment.
    let owed = bill["postings"][0]["entries"]["open_items"][0].clone();
    let receipt = run(
        &t,
        "payment.draft",
        json!({ "company_id": w.company, "open_item_ids": [owed["id"]], "bank_account_id": w.cash }),
    )
    .await;
    assert_eq!(receipt["body"]["direction"], "incoming");
    assert_eq!(dec(&receipt["body"]["allocations"][0]["amount"]), 69.0);

    // An order that is not open drafts nothing.
    run(&t, "purchase_order.close", json!({ "id": order["id"] })).await;
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.receive",
            json!({ "id": order["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["reason"], "state");

    t.stop().await;
}

#[tokio::test]
async fn drafting_needs_the_right_to_create_the_new_document() {
    let t = TestApp::start().await;
    let w = world(&t).await;
    let order = w
        .order(&t, "purchase_order", json!({ "vendor_id": w.vendor }))
        .await;
    let input = json!({ "id": order["id"] });

    // Reading the order is not enough.
    let reader = t
        .user_with_role("reader@example.com", &["purchase_order:read"], &w.company)
        .await;
    let (status, body) = t
        .command(&reader, "purchase_order.receive", input.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // Nor is creating receipts without reading the order.
    let blind = t
        .user_with_role(
            "blind@example.com",
            &["purchase_receipt:create"],
            &w.company,
        )
        .await;
    let (status, body) = t
        .command(&blind, "purchase_order.receive", input.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        t.count("SELECT count(*) FROM document WHERE doc_type = 'purchase_receipt'")
            .await,
        0
    );

    let clerk = t
        .user_with_role(
            "clerk@example.com",
            &[
                "purchase_order:read",
                "purchase_receipt:create",
                "purchase_receipt:read",
            ],
            &w.company,
        )
        .await;
    let (status, body) = t
        .command(&clerk, "purchase_order.receive", input.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // A receipt, not an invoice.
    let (status, body) = t.command(&clerk, "purchase_order.invoice", input).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // A payment draft needs the open items to be readable.
    let payer = t
        .user_with_role("payer@example.com", &["payment:create"], &w.company)
        .await;
    let (status, body) = t
        .command(
            &payer,
            "payment.draft",
            json!({ "company_id": w.company, "open_item_ids": [order["id"]], "bank_account_id": w.cash }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn the_description_says_what_references_and_follow_ons_there_are() {
    let t = TestApp::start().await;
    let (_, meta) = t.get(&t.admin, "/api/meta").await;
    let doc_type = |name: &str| {
        meta["doc_types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == name)
            .unwrap()
            .clone()
    };
    let field = |fields: &Value, name: &str| {
        fields
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("no field {name}"))["type"]
            .clone()
    };

    // A document, limited to the states it can be chosen in; and a line
    // of that document.
    let receipt = doc_type("purchase_receipt");
    assert_eq!(
        field(&receipt["fields"], "purchase_order_id"),
        json!({ "kind": "document_link", "doc_type": "purchase_order", "states": ["open"] })
    );
    assert_eq!(
        field(&receipt["collections"][0]["fields"], "order_line_id"),
        json!({
            "kind": "line_link",
            "document_field": "purchase_order_id",
            "doc_type": "purchase_order",
            "collection": "lines",
        })
    );
    let invoice = doc_type("sales_invoice");
    assert_eq!(
        field(&invoice["fields"], "sales_order_id")["doc_type"],
        "sales_order"
    );
    assert_eq!(
        field(&invoice["collections"][0]["fields"], "order_line_id")["document_field"],
        "sales_order_id"
    );

    // Something found through a query command: an open item.
    let payment = doc_type("payment");
    assert_eq!(
        field(&payment["collections"][0]["fields"], "open_item_id"),
        json!({
            "kind": "lookup",
            "command": "open_item.list",
            "rows": "items",
            "key": "id",
            "display": ["number", "doc_date", "open"],
            "by_keys": "ids",
            "params": {
                "company_id": { "from": "field", "field": "company_id", "map": null },
                "kind": {
                    "from": "field",
                    "field": "direction",
                    "map": { "outgoing": "payable", "incoming": "receivable" },
                },
            },
        })
    );

    // The record of whatever entity another field names.
    let rule = meta["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "account_rule")
        .unwrap();
    assert_eq!(
        field(&rule["fields"], "scope_id"),
        json!({ "kind": "entity_link", "entity_field": "scope_type" })
    );
    // A posting.
    let close = doc_type("year_end_close");
    assert_eq!(
        field(&close["fields"], "closing_posting_id"),
        json!({ "kind": "posting" })
    );

    // The commands that draft what follows are actions on the order.
    let command = |name: &str| {
        meta["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no command {name}"))
            .clone()
    };
    for (name, label, source, created) in [
        (
            "purchase_order.receive",
            "Receive",
            "purchase_order",
            "purchase_receipt",
        ),
        (
            "purchase_order.invoice",
            "Invoice",
            "purchase_order",
            "vendor_invoice",
        ),
        (
            "sales_order.deliver",
            "Deliver",
            "sales_order",
            "sales_delivery",
        ),
        (
            "sales_order.invoice",
            "Invoice",
            "sales_order",
            "sales_invoice",
        ),
    ] {
        let c = command(name);
        assert_eq!(c["label"], label, "{name}");
        assert_eq!(c["resource"], source, "{name}");
        assert_eq!(c["resource_kind"], "document", "{name}");
        assert_eq!(c["target"], true, "{name}");
        assert_eq!(c["from_states"], json!(["open"]), "{name}");
        assert_eq!(c["creates"], created, "{name}");
        assert_eq!(c["destructive"], false, "{name}");
        assert_eq!(c["permission"], format!("{created}:create"), "{name}");
    }
    let pay = command("payment.draft");
    assert_eq!(pay["label"], "Pay");
    assert_eq!(pay["creates"], "payment");
    assert_eq!(pay["target"], false);
    // A command that creates nothing says so.
    assert_eq!(command("journal_entry.post")["creates"], Value::Null);

    t.stop().await;
}
