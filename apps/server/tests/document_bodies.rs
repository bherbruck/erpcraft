//! A document body is accepted on write exactly as it is returned on read:
//! what the system sets is ignored, and a line sent back with its id keeps
//! it, so that an interface can load a draft, change it and save it.

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

struct World {
    company: String,
    cash: String,
    revenue: String,
    period: String,
    equity: String,
    offset: String,
    items: [String; 2],
    warehouses: [String; 2],
    vendor: String,
    customer: String,
    bom: String,
}

/// A company in which every document type can be drafted.
async fn world(t: &TestApp) -> World {
    let c = t.company("ACME", "USD").await;
    let mut accounts = vec![];
    for (code, name, kind) in [
        ("1200", "Accounts receivable", "asset"),
        ("1300", "Inventory", "asset"),
        ("2000", "Accounts payable", "liability"),
        ("2100", "Goods received not invoiced", "liability"),
        ("3100", "Retained earnings", "equity"),
        ("5000", "Cost of goods sold", "expense"),
        ("5100", "Stock adjustments", "expense"),
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
    let [
        receivable,
        inventory,
        payable,
        grni,
        equity,
        cogs,
        offset,
        variance,
    ] = <[String; 8]>::try_from(accounts).unwrap();
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
    let widget = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let gadget = t
        .create("item", json!({ "code": "GADGET", "name": "Gadget" }))
        .await;
    let main = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let annex = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "ANNEX", "name": "Annex" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C001", "name": "Buyers Ltd" }))
        .await;
    let bom = t
        .create(
            "bom",
            json!({ "item_id": gadget, "code": "GADGET-1", "name": "Gadget", "output_qty": "1" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": bom, "item_id": widget, "qty": "2" }),
    )
    .await;
    World {
        company: c.id,
        cash: c.cash,
        revenue: c.revenue,
        period: c.period,
        equity,
        offset,
        items: [widget, gadget],
        warehouses: [main, annex],
        vendor,
        customer,
        bom,
    }
}

async fn run(t: &TestApp, name: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, name, input).await;
    assert_eq!(status, StatusCode::OK, "{name}: {body}");
    body
}

/// Save `body` as the body of a draft and return the document.
async fn save(t: &TestApp, doc: &Value, body: &Value) -> Value {
    let doc_type = doc["doc_type"].as_str().unwrap();
    let (status, saved) = t
        .patch(
            &t.admin,
            &format!("/api/documents/{doc_type}/{}", id(doc)),
            json!({ "version": doc["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{doc_type}: {saved}");
    saved
}

fn line_ids(doc: &Value, collection: &str) -> Vec<Value> {
    doc["body"][collection]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["id"].clone())
        .collect()
}

/// The description of the document type names every field its body holds.
fn assert_described(meta: &Value, doc: &Value) {
    let doc_type = doc["doc_type"].as_str().unwrap();
    let described = meta["doc_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == doc_type)
        .unwrap();
    let names = |fields: &Value| -> Vec<String> {
        fields
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let fields = names(&described["fields"]);
    let collections = described["collections"].as_array().unwrap();
    for (key, value) in doc["body"].as_object().unwrap() {
        match collections.iter().find(|c| c["name"] == *key) {
            Some(collection) => {
                let declared = names(&collection["fields"]);
                for row in value.as_array().unwrap() {
                    for field in row.as_object().unwrap().keys() {
                        assert!(
                            declared.contains(field),
                            "{doc_type}.{key} returns '{field}', which is not described"
                        );
                    }
                }
            }
            None => assert!(
                fields.contains(key),
                "{doc_type} returns '{key}', which is not described"
            ),
        }
    }
}

/// Draft a document, then: send its body back as it was read; change one
/// field of its first line; and check each time that nothing else moved.
/// `collection` and `change` name the lines and the field to change, with
/// its new value as sent and as it reads afterwards.
async fn round_trip(
    t: &TestApp,
    meta: &Value,
    w: &World,
    doc_type: &str,
    date: &str,
    body: Value,
    lines: Option<(&str, &str, Value, Value)>,
) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": w.company, "doc_date": date, "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
    assert_described(meta, &draft);

    // Unchanged: accepted, and the body reads the same, ids included.
    let same = save(t, &draft, &draft["body"]).await;
    assert_eq!(same["body"], draft["body"], "{doc_type}");
    assert_eq!(
        same["version"],
        json!(draft["version"].as_i64().unwrap() + 1)
    );

    let Some((collection, field, sent, read)) = lines else {
        return same;
    };
    let ids = line_ids(&draft, collection);
    assert!(ids.len() >= 2, "{doc_type} needs two lines for this test");
    assert!(ids.iter().all(Value::is_string), "{doc_type}: {draft}");

    // One line changed: it and the others keep their ids.
    let mut changed = same["body"].clone();
    changed[collection][0][field] = sent;
    let saved = save(t, &same, &changed).await;
    assert_eq!(line_ids(&saved, collection), ids, "{doc_type}");
    assert_eq!(saved["body"][collection][0][field], read, "{doc_type}");
    assert_eq!(
        saved["body"][collection][1], draft["body"][collection][1],
        "{doc_type}"
    );
    saved
}

#[tokio::test]
async fn every_document_type_accepts_its_body_as_read() {
    let t = TestApp::start().await;
    let w = world(&t).await;
    let (_, meta) = t.get(&t.admin, "/api/meta").await;
    let [widget, gadget] = &w.items;
    let [main, annex] = &w.warehouses;
    let date = "2026-03-15";

    round_trip(
        &t,
        &meta,
        &w,
        "journal_entry",
        date,
        json!({ "lines": [
            { "account_id": w.cash, "debit": "150.00", "memo": "till" },
            { "account_id": w.revenue, "credit": "150.00" },
        ]}),
        Some(("lines", "debit", json!("175.50"), json!("175.5000"))),
    )
    .await;

    round_trip(
        &t,
        &meta,
        &w,
        "year_end_close",
        "2026-12-31",
        json!({ "fiscal_period_id": w.period, "retained_earnings_account_id": w.equity }),
        None,
    )
    .await;

    round_trip(
        &t,
        &meta,
        &w,
        "stock_adjustment",
        date,
        json!({ "offset_account_id": w.offset, "lines": [
            { "item_id": widget, "warehouse_id": main, "qty": "10", "unit_cost": "4.00" },
            { "item_id": gadget, "warehouse_id": main, "qty": "5", "unit_cost": "9.00" },
        ]}),
        Some(("lines", "qty", json!("12"), json!("12.000000"))),
    )
    .await;

    round_trip(
        &t,
        &meta,
        &w,
        "stock_transfer",
        date,
        json!({ "from_warehouse_id": main, "to_warehouse_id": annex, "lines": [
            { "item_id": widget, "qty": "3" },
            { "item_id": gadget, "qty": "1" },
        ]}),
        Some(("lines", "qty", json!("2"), json!("2.000000"))),
    )
    .await;

    round_trip(
        &t,
        &meta,
        &w,
        "production_order",
        date,
        json!({ "bom_id": w.bom, "qty": "4", "warehouse_id": main }),
        None,
    )
    .await;

    // Purchasing: an order, then a receipt and an invoice against it.
    let order_lines = |warehouse: &str| {
        json!([
            { "item_id": widget, "warehouse_id": warehouse, "qty": "10", "unit_price": "4.00" },
            { "item_id": gadget, "warehouse_id": warehouse, "qty": "5", "unit_price": "9.00" },
        ])
    };
    let order = round_trip(
        &t,
        &meta,
        &w,
        "purchase_order",
        date,
        json!({ "vendor_id": w.vendor, "lines": order_lines(main) }),
        Some(("lines", "qty", json!("12"), json!("12.000000"))),
    )
    .await;
    let order = run(&t, "purchase_order.approve", json!({ "id": order["id"] })).await;
    let against = |order: &Value, extra: Value| {
        let mut lines = vec![];
        for (line, qty) in order["body"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .zip(["12", "5"])
        {
            let mut row = json!({ "order_line_id": line["id"], "qty": qty });
            for (key, value) in extra.as_object().unwrap() {
                row[key] = line[value.as_str().unwrap()].clone();
            }
            lines.push(row);
        }
        Value::Array(lines)
    };
    let receipt = round_trip(
        &t,
        &meta,
        &w,
        "purchase_receipt",
        date,
        json!({ "purchase_order_id": order["id"], "lines": against(&order, json!({})) }),
        Some(("lines", "qty", json!("11"), json!("11.000000"))),
    )
    .await;
    let receipt = run(&t, "purchase_receipt.post", json!({ "id": receipt["id"] })).await;
    // Posting used the ids the draft had: each stock entry names its line.
    let stock = receipt["postings"][0]["entries"]["stock"]
        .as_array()
        .unwrap();
    let sources: Vec<Value> = stock.iter().map(|e| e["source_line_id"].clone()).collect();
    assert_eq!(sources, line_ids(&receipt, "lines"));

    let invoice = round_trip(
        &t,
        &meta,
        &w,
        "vendor_invoice",
        date,
        json!({
            "purchase_order_id": order["id"],
            "vendor_reference": "INV-77",
            "lines": against(&order, json!({ "unit_price": "unit_price" })),
        }),
        Some(("lines", "qty", json!("11"), json!("11.000000"))),
    )
    .await;

    // Sales: the same shape.
    let order = round_trip(
        &t,
        &meta,
        &w,
        "sales_order",
        date,
        json!({ "customer_id": w.customer, "lines": order_lines(main) }),
        Some(("lines", "qty", json!("12"), json!("12.000000"))),
    )
    .await;
    let order = run(&t, "sales_order.approve", json!({ "id": order["id"] })).await;
    round_trip(
        &t,
        &meta,
        &w,
        "sales_delivery",
        date,
        json!({ "sales_order_id": order["id"], "lines": against(&order, json!({})) }),
        Some(("lines", "qty", json!("11"), json!("11.000000"))),
    )
    .await;
    round_trip(
        &t,
        &meta,
        &w,
        "sales_invoice",
        date,
        json!({
            "sales_order_id": order["id"],
            "lines": against(&order, json!({ "unit_price": "unit_price" })),
        }),
        Some(("lines", "qty", json!("11"), json!("11.000000"))),
    )
    .await;

    // A payment needs two open items: split the vendor invoice in two.
    // Dropping a line leaves the other its id.
    let ids = line_ids(&invoice, "lines");
    let mut first = invoice["body"].clone();
    let second = first["lines"].as_array_mut().unwrap().remove(1);
    let invoice = save(&t, &invoice, &first).await;
    assert_eq!(line_ids(&invoice, "lines"), [ids[0].clone()]);
    let invoice = run(&t, "vendor_invoice.post", json!({ "id": invoice["id"] })).await;
    let (status, other) = t
        .post(
            &t.admin,
            "/api/documents/vendor_invoice",
            json!({
                "company_id": w.company,
                "doc_date": date,
                "body": {
                    "purchase_order_id": invoice["body"]["purchase_order_id"],
                    "lines": [{
                        "order_line_id": second["order_line_id"],
                        "qty": second["qty"],
                        "unit_price": second["unit_price"],
                    }],
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let other = run(&t, "vendor_invoice.post", json!({ "id": other["id"] })).await;
    let item = |doc: &Value| doc["postings"][0]["entries"]["open_items"][0].clone();
    let (a, b) = (item(&invoice), item(&other));
    round_trip(
        &t,
        &meta,
        &w,
        "payment",
        date,
        json!({ "direction": "outgoing", "bank_account_id": w.cash, "allocations": [
            { "open_item_id": a["id"], "amount": a["amount"] },
            { "open_item_id": b["id"], "amount": b["amount"] },
        ]}),
        Some(("allocations", "amount", json!("1.00"), json!("1.0000"))),
    )
    .await;

    t.stop().await;
}

#[tokio::test]
async fn lines_keep_their_ids_and_computed_fields_are_never_taken() {
    let t = TestApp::start().await;
    let w = world(&t).await;
    let [widget, gadget] = &w.items;
    let main = &w.warehouses[0];
    let draft = |body: Value| {
        let company = w.company.clone();
        let t = &t;
        async move {
            let (status, doc) = t
                .post(
                    &t.admin,
                    "/api/documents/purchase_order",
                    json!({ "company_id": company, "doc_date": "2026-03-15", "body": body }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{doc}");
            doc
        }
    };
    let line = |item: &str, qty: &str| json!({ "item_id": item, "warehouse_id": main, "qty": qty, "unit_price": "4.00" });
    let order = draft(json!({ "vendor_id": w.vendor, "lines": [
        line(widget, "1"), line(gadget, "2"), line(widget, "3"),
    ]}))
    .await;
    let ids = line_ids(&order, "lines");

    // Reordered, one dropped, one added: a line is known by its id, not
    // by its place, and gets its number from where it now stands.
    let stored = order["body"]["lines"].as_array().unwrap();
    let body = json!({ "vendor_id": w.vendor, "lines": [
        stored[2], line(gadget, "9"), stored[0],
    ]});
    let saved = save(&t, &order, &body).await;
    let now = line_ids(&saved, "lines");
    assert_eq!(now[0], ids[2]);
    assert_eq!(now[2], ids[0]);
    assert!(!ids.contains(&now[1]), "a new line gets a new id");
    let numbers: Vec<&Value> = saved["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| &l["line_no"])
        .collect();
    assert_eq!(numbers, [&json!(1), &json!(2), &json!(3)]);
    assert_eq!(saved["body"]["lines"][0]["qty"], "3.000000");

    // What the system sets is ignored, whatever is sent for it.
    let mut body = saved["body"].clone();
    body["lines"][0]["qty_received"] = json!("99");
    body["lines"][0]["qty_invoiced"] = json!("99");
    body["lines"][0]["line_no"] = json!(7);
    let again = save(&t, &saved, &body).await;
    assert_eq!(again["body"], saved["body"]);

    // The same id twice: the first line has it, the second is new.
    let mut body = again["body"].clone();
    body["lines"][1]["id"] = body["lines"][0]["id"].clone();
    let twice = save(&t, &again, &body).await;
    let now = line_ids(&twice, "lines");
    assert_eq!(now[0], ids[2]);
    assert_ne!(now[1], now[0]);

    // The id of another document's line is not taken over.
    let other = draft(json!({ "vendor_id": w.vendor, "lines": [line(widget, "1")] })).await;
    let mut body = other["body"].clone();
    body["lines"][0]["id"] = now[0].clone();
    let saved = save(&t, &other, &body).await;
    assert_ne!(line_ids(&saved, "lines")[0], now[0]);
    let (_, first) = t
        .get(
            &t.admin,
            &format!("/api/documents/purchase_order/{}", id(&twice)),
        )
        .await;
    assert_eq!(line_ids(&first, "lines"), now);

    // A field that is neither the caller's nor the system's is still refused.
    let mut body = saved["body"].clone();
    body["lines"][0]["colour"] = json!("red");
    let (status, refused) = t
        .patch(
            &t.admin,
            &format!("/api/documents/purchase_order/{}", id(&saved)),
            json!({ "version": saved["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["error"]["field"], "colour");
    assert_eq!(refused["error"]["line"], 1);

    t.stop().await;
}

#[tokio::test]
async fn refusals_in_the_other_modules_name_the_line() {
    let t = TestApp::start().await;
    let w = world(&t).await;
    let [widget, gadget] = &w.items;
    let main = &w.warehouses[0];
    let post = |doc_type: &'static str, body: Value| {
        let company = w.company.clone();
        let t = &t;
        async move {
            t.post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": company, "doc_date": "2026-03-15", "body": body }),
            )
            .await
        }
    };

    // A rule of the document type.
    let (status, body) = post(
        "purchase_order",
        json!({ "vendor_id": w.vendor, "lines": [
            { "item_id": widget, "warehouse_id": main, "qty": "1", "unit_price": "4.00" },
            { "item_id": gadget, "warehouse_id": main, "qty": "1", "unit_price": "-1" },
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"],
        json!({
            "code": "validation",
            "message": "line 2: the unit price cannot be negative",
            "collection": "lines",
            "line": 2,
            "field": "unit_price",
        })
    );
    let (_, body) = post(
        "stock_adjustment",
        json!({ "lines": [{ "item_id": widget, "warehouse_id": main, "qty": "5" }] }),
    )
    .await;
    assert_eq!(
        body["error"]["message"],
        "line 1: stock added needs a unit cost"
    );
    assert_eq!(body["error"]["field"], "unit_cost");
    let (_, body) = post(
        "sales_order",
        json!({ "customer_id": w.customer, "lines": [
            { "item_id": widget, "warehouse_id": main, "qty": "0", "unit_price": "4.00" },
        ]}),
    )
    .await;
    assert_eq!(body["error"]["line"], 1);
    assert_eq!(body["error"]["field"], "qty");

    // A body that cannot be read.
    let (_, body) = post(
        "stock_transfer",
        json!({ "from_warehouse_id": main, "to_warehouse_id": w.warehouses[1], "lines": [
            { "item_id": widget, "qty": "1" },
            { "item_id": gadget, "qty": "many" },
        ]}),
    )
    .await;
    assert_eq!(body["error"]["collection"], "lines");
    assert_eq!(body["error"]["line"], 2);
    assert_eq!(body["error"]["field"], "qty");
    let (_, body) = post(
        "production_order",
        json!({ "bom_id": w.bom, "qty": "0", "warehouse_id": main }),
    )
    .await;
    assert_eq!(body["error"]["field"], "qty");
    let (_, body) = post(
        "production_order",
        json!({ "bom_id": w.bom, "warehouse_id": main }),
    )
    .await;
    assert_eq!(
        body["error"]["message"],
        "invalid production order body: field 'qty' is required"
    );

    // Found only when posting: more than was ordered.
    let (_, order) = post(
        "purchase_order",
        json!({ "vendor_id": w.vendor, "lines": [
            { "item_id": widget, "warehouse_id": main, "qty": "1", "unit_price": "4.00" },
            { "item_id": gadget, "warehouse_id": main, "qty": "1", "unit_price": "4.00" },
        ]}),
    )
    .await;
    let order = run(&t, "purchase_order.approve", json!({ "id": order["id"] })).await;
    let (_, invoice) = post(
        "vendor_invoice",
        json!({ "purchase_order_id": order["id"], "lines": [
            { "order_line_id": order["body"]["lines"][0]["id"], "qty": "1", "unit_price": "4.00" },
            { "order_line_id": order["body"]["lines"][1]["id"], "qty": "1", "unit_price": "4.00" },
        ]}),
    )
    .await;
    let (status, body) = t
        .command(
            &t.admin,
            "vendor_invoice.post",
            json!({ "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["collection"], "lines");
    assert_eq!(body["error"]["line"], 1);
    assert_eq!(body["error"]["field"], "qty");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("line 1: "),
        "{body}"
    );
    // A state that does not allow it.
    run(&t, "purchase_order.close", json!({ "id": order["id"] })).await;
    let (status, body) = t
        .command(
            &t.admin,
            "vendor_invoice.post",
            json!({ "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["reason"], "state");

    // The general ledger refuses an entry of a journal entry: its line.
    let (_, journal) = post(
        "journal_entry",
        json!({ "lines": [
            { "account_id": w.cash, "debit": "10.00" },
            { "account_id": w.revenue, "credit": "10.00", "currency": "EUR" },
        ]}),
    )
    .await;
    let (status, body) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": journal["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["collection"], "lines");
    assert_eq!(body["error"]["line"], 2);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("line 2: "),
        "{body}"
    );

    t.stop().await;
}
