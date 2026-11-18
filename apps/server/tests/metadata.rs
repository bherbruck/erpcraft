//! What the registry tells an interface beyond names and types: what a
//! number means, what a field starts as, how a record is shown and
//! retired, what a document comes to, and one search over everything.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};
use uuid::Uuid;

async fn describe(t: &TestApp) -> Value {
    let (status, meta) = t.get(&t.admin, "/api/meta").await;
    assert_eq!(status, StatusCode::OK, "{meta}");
    meta
}

fn named<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == name)
        .unwrap_or_else(|| panic!("nothing named {name}"))
}

async fn run(t: &TestApp, name: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, name, input).await;
    assert_eq!(status, StatusCode::OK, "{name}: {body}");
    body
}

async fn draft(t: &TestApp, company: &str, doc_type: &str, date: &str, body: Value) -> Value {
    let (status, doc) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": company, "doc_date": date, "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc_type}: {doc}");
    doc
}

// ------------------------------------------------------------- description

#[tokio::test]
async fn fields_say_what_they_mean_and_what_they_start_as() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    let entity = |name: &str| named(&meta["entities"], name).clone();
    let doc_type = |name: &str| named(&meta["doc_types"], name).clone();
    let company = json!({ "from": "company" });

    // A decimal says what kind of number it is, how many decimal places
    // it can have, and where its currency or unit is.
    let gl_entry = entity("gl_entry");
    assert_eq!(
        named(&gl_entry["fields"], "debit")["decimal"],
        json!({ "role": "money", "scale": 4, "currency": company, "unit": null })
    );
    assert_eq!(
        named(&gl_entry["fields"], "txn_debit")["decimal"]["currency"],
        json!({ "from": "field", "field": "currency" })
    );
    assert_eq!(
        named(&gl_entry["fields"], "fx_rate")["decimal"],
        json!({ "role": "rate", "scale": 18, "currency": null, "unit": null })
    );
    assert_eq!(
        named(&entity("tax_code")["fields"], "rate")["decimal"],
        json!({ "role": "percent", "scale": 6, "currency": null, "unit": null })
    );
    let item_unit = json!({ "from": "linked", "field": "item_id", "key": "uom" });
    assert_eq!(
        named(&entity("stock_entry")["fields"], "qty")["decimal"],
        json!({ "role": "quantity", "scale": 6, "currency": null, "unit": item_unit })
    );
    // A price on a line is in the currency of its document.
    let order_lines = &doc_type("purchase_order")["collections"][0]["fields"];
    assert_eq!(
        named(order_lines, "unit_price")["decimal"],
        json!({ "role": "unit_price", "scale": 6,
                "currency": { "from": "document", "field": "currency" }, "unit": null })
    );
    assert_eq!(named(order_lines, "qty")["decimal"]["unit"], item_unit);
    // A receipt line has no item of its own: the unit is not known there.
    let receipt_lines = &doc_type("purchase_receipt")["collections"][0]["fields"];
    assert_eq!(named(receipt_lines, "qty")["decimal"]["unit"], Value::Null);
    assert_eq!(named(receipt_lines, "value")["decimal"]["role"], "money");
    // A field that is not a decimal has none.
    assert_eq!(named(order_lines, "item_id")["decimal"], Value::Null);

    // What a new record starts with, where the table says so.
    let account = entity("account");
    assert_eq!(named(&account["fields"], "active")["default"], true);
    assert_eq!(named(&account["fields"], "is_group")["default"], false);
    assert_eq!(named(&account["fields"], "name")["default"], Value::Null);
    // Not for what the system sets itself.
    assert_eq!(
        named(&account["fields"], "created_at")["default"],
        Value::Null
    );
    assert_eq!(
        named(&entity("currency")["fields"], "decimals")["default"],
        2
    );

    // Every value of a choice has a label.
    assert_eq!(
        named(&account["fields"], "account_type")["type"]["labels"],
        json!({
            "asset": "Asset", "liability": "Liability", "equity": "Equity",
            "income": "Income", "expense": "Expense",
        })
    );

    // How a record is shown, called in the plural, and retired.
    assert_eq!(account["display"], json!(["code", "name"]));
    assert_eq!(account["plural"], "Accounts");
    assert_eq!(account["active_field"], "active");
    assert_eq!(account["parent"], Value::Null);
    assert_eq!(entity("company")["plural"], "Companies");
    assert_eq!(entity("company")["active_field"], Value::Null);
    assert_eq!(entity("stock_entry")["plural"], "Stock entries");
    assert_eq!(entity("bom")["plural"], "Bills of materials");
    assert_eq!(
        entity("exchange_rate")["display"],
        json!(["from_currency", "to_currency", "rate_date"])
    );
    // A record that is edited inside another.
    assert_eq!(
        entity("bom_line")["parent"],
        json!({ "entity": "bom", "field": "bom_id" })
    );

    // Document types and modules have names for a menu.
    assert_eq!(doc_type("purchase_order")["plural"], "Purchase orders");
    assert_eq!(doc_type("sales_delivery")["plural"], "Sales deliveries");
    assert_eq!(named(&meta["modules"], "gl")["label"], "General ledger");
    assert_eq!(named(&meta["modules"], "open_items")["label"], "Open items");

    // The header every document has is described like any other field.
    let header = &meta["document_header"];
    assert_eq!(named(header, "number")["label"], "Number");
    assert_eq!(named(header, "number")["readonly"], true);
    assert_eq!(named(header, "doc_date")["label"], "Date");
    assert_eq!(named(header, "doc_date")["required"], true);
    assert!(named(header, "doc_date")["help"].is_string());
    assert_eq!(named(header, "created_by_name")["label"], "Created by");
    for field in ["status", "memo", "company_id", "created_at", "updated_at"] {
        assert!(named(header, field)["label"].is_string());
    }

    // What holds for everything described.
    let mut fields: Vec<(String, &Value)> = vec![];
    for e in meta["entities"].as_array().unwrap() {
        let name = e["name"].as_str().unwrap();
        assert!(!e["plural"].as_str().unwrap().is_empty(), "{name}");
        for f in e["fields"].as_array().unwrap() {
            fields.push((format!("{name}.{}", f["name"].as_str().unwrap()), f));
        }
        for shown in e["display"].as_array().unwrap() {
            assert!(
                e["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f["name"] == *shown),
                "{name} is displayed by {shown}"
            );
        }
    }
    for d in meta["doc_types"].as_array().unwrap() {
        let name = d["name"].as_str().unwrap();
        assert!(!d["plural"].as_str().unwrap().is_empty(), "{name}");
        let mut lists = vec![&d["fields"], &d["summary"]];
        lists.extend(
            d["collections"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| &c["fields"]),
        );
        for list in lists {
            for f in list.as_array().unwrap() {
                fields.push((format!("{name}.{}", f["name"].as_str().unwrap()), f));
            }
        }
        // A summary is worked out, never entered.
        for f in d["summary"].as_array().unwrap() {
            assert_eq!(f["readonly"], true, "{name}.summary.{}", f["name"]);
        }
    }
    for f in meta["document_header"].as_array().unwrap() {
        fields.push((format!("header.{}", f["name"].as_str().unwrap()), f));
    }
    for (name, field) in &fields {
        let kind = field["type"]["kind"].as_str().unwrap();
        // No decimal is left for an interface to guess at.
        assert_eq!(
            field["decimal"].is_object(),
            kind == "decimal",
            "{name}: a decimal says what it is, and only a decimal"
        );
        if kind == "decimal" {
            assert!(field["decimal"]["scale"].is_u64(), "{name} has no scale");
            let money =
                ["money", "unit_price"].contains(&field["decimal"]["role"].as_str().unwrap());
            assert_eq!(field["decimal"]["currency"].is_object(), money, "{name}");
        }
        if kind == "enum" {
            for value in field["type"]["values"].as_array().unwrap() {
                let label = &field["type"]["labels"][value.as_str().unwrap()];
                assert!(label.is_string(), "{name}: no label for {value}");
            }
        }
    }
    for module in meta["modules"].as_array().unwrap() {
        assert!(!module["label"].as_str().unwrap().is_empty(), "{module}");
    }

    t.stop().await;
}

#[tokio::test]
async fn a_lookup_can_leave_retired_records_out() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    t.create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let old = t
        .create("vendor", json!({ "code": "V002", "name": "Gone Ltd" }))
        .await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/vendor/{old}"),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let codes = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["code"].as_str().unwrap().to_owned())
            .collect()
    };

    // A list that manages the records shows them all.
    let all = run(&t, "entity.list", json!({ "entity": "vendor" })).await;
    assert_eq!(codes(&all), ["V001", "V002"]);
    assert_eq!(all["total"], 2);
    // A lookup asks for those still in use; the total follows.
    let active = run(
        &t,
        "entity.list",
        json!({ "entity": "vendor", "active_only": true }),
    )
    .await;
    assert_eq!(codes(&active), ["V001"]);
    assert_eq!(active["total"], 1);
    // Together with a search, and through the REST route.
    let found = run(
        &t,
        "entity.list",
        json!({ "entity": "vendor", "active_only": true, "q": "gone" }),
    )
    .await;
    assert_eq!(found["total"], 0);
    let (status, page) = t
        .get(&t.admin, "/api/entities/vendor?active_only=true")
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(codes(&page), ["V001"]);
    // An entity whose records are never retired ignores the flag.
    let companies = run(
        &t,
        "entity.list",
        json!({ "entity": "company", "active_only": true }),
    )
    .await;
    assert_eq!(companies["total"], 1);
    assert_eq!(companies["items"][0]["id"], json!(c.id));

    // Someone from whom the field is withheld cannot filter by it.
    let (limited, _) = t
        .user_with_rules(
            "limited@example.com",
            json!(["vendor:read", { "deny": "vendor:read", "fields": ["active"] }]),
            None,
        )
        .await;
    let (status, body) = t
        .command(
            &limited,
            "entity.list",
            json!({ "entity": "vendor", "active_only": true }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["field"], "active");

    t.stop().await;
}

// ---------------------------------------------------------------- summaries

struct World {
    company: String,
    cash: String,
    revenue: String,
    period: String,
    /// By code.
    account: std::collections::BTreeMap<&'static str, String>,
    widget: String,
    gadget: String,
    main: String,
    annex: String,
    vendor: String,
    customer: String,
    tax: String,
    bom: String,
}

/// A company in which every document type can be posted, with a tax of
/// 7.25 % and prices that do not divide evenly.
async fn world(t: &TestApp) -> World {
    let c = t.company("ACME", "USD").await;
    let mut account = std::collections::BTreeMap::new();
    for (code, name, kind) in [
        ("1200", "Accounts receivable", "asset"),
        ("1300", "Inventory", "asset"),
        ("1310", "Finished goods", "asset"),
        ("1400", "Tax paid", "asset"),
        ("2000", "Accounts payable", "liability"),
        ("2100", "Goods received not invoiced", "liability"),
        ("2300", "Tax collected", "liability"),
        ("3100", "Retained earnings", "equity"),
        ("5000", "Cost of goods sold", "expense"),
        ("5100", "Stock adjustments", "expense"),
        ("5200", "Purchase price variance", "expense"),
    ] {
        let id = t
            .create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": name, "account_type": kind }),
            )
            .await;
        account.insert(code, id);
    }
    t.inventory_rule(&c.id, &account["1300"]).await;
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id,
            "grni_account_id": account["2100"],
            "payable_account_id": account["2000"],
            "price_variance_account_id": account["5200"],
        }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id,
            "receivable_account_id": account["1200"],
            "revenue_account_id": c.revenue,
            "cogs_account_id": account["5000"],
        }),
    )
    .await;
    let tax = t
        .create(
            "tax_code",
            json!({ "code": "OH-CUY", "name": "Ohio, Cuyahoga County", "rate": "7.25" }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({
            "company_id": c.id,
            "tax_code_id": tax,
            "collected_account_id": account["2300"],
            "paid_account_id": account["1400"],
        }),
    )
    .await;
    let widget = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let gadget = t
        .create("item", json!({ "code": "GADGET", "name": "Gadget" }))
        .await;
    // What is made is carried on its own account.
    t.create(
        "account_rule",
        json!({
            "company_id": c.id, "purpose": "inventory",
            "scope_type": "item", "scope_id": gadget, "account_id": account["1310"],
        }),
    )
    .await;
    let warehouse = |code: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "warehouse",
                json!({ "company_id": company, "code": code, "name": code }),
            )
            .await
        }
    };
    let bom = t
        .create(
            "bom",
            json!({ "item_id": gadget, "code": "GADGET-1", "name": "Gadget", "output_qty": "1" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": bom, "item_id": widget, "qty": "3" }),
    )
    .await;
    World {
        main: warehouse("MAIN").await,
        annex: warehouse("ANNEX").await,
        vendor: t
            .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
            .await,
        customer: t
            .create("customer", json!({ "code": "C001", "name": "Buyers Ltd" }))
            .await,
        company: c.id,
        cash: c.cash,
        revenue: c.revenue,
        period: c.period,
        account,
        widget,
        gadget,
        tax,
        bom,
    }
}

/// Debit minus credit that the document's own posting wrote to `account`.
async fn posted(t: &TestApp, doc: &Value, account: &str) -> f64 {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(g.debit - g.credit), 0)::float8 FROM gl_entry g
         JOIN posting p ON p.id = g.posting_id
         WHERE p.document_id = $1 AND p.reverses_id IS NULL AND g.account_id = $2",
    )
    .bind(Uuid::parse_str(doc["id"].as_str().unwrap()).unwrap())
    .bind(Uuid::parse_str(account).unwrap())
    .fetch_one(&t.pool)
    .await
    .unwrap()
}

fn cents(value: f64) -> i64 {
    (value * 100.0).round() as i64
}

#[tokio::test]
async fn a_summary_total_is_what_is_posted() {
    let t = TestApp::start().await;
    let w = world(&t).await;
    let c = &w.company;
    let date = "2026-03-15";
    let total = |doc: &Value, field: &str| cents(dec(&doc["summary"][field]));

    // Journal entry: the debit side.
    let journal = draft(
        &t,
        c,
        "journal_entry",
        date,
        json!({ "lines": [
            { "account_id": w.cash, "debit": "150.25" },
            { "account_id": w.cash, "debit": "49.75" },
            { "account_id": w.revenue, "credit": "200.00" },
        ]}),
    )
    .await;
    assert_eq!(total(&journal, "total_debit"), 20000);
    let journal = run(&t, "journal_entry.post", json!({ "id": journal["id"] })).await;
    assert_eq!(total(&journal, "total_debit"), 20000);
    assert_eq!(cents(posted(&t, &journal, &w.cash).await), 20000);
    // A line in another currency without a rate: not known until posted.
    let foreign = draft(
        &t,
        c,
        "journal_entry",
        date,
        json!({ "lines": [
            { "account_id": w.cash, "debit": "10.00", "currency": "EUR" },
            { "account_id": w.revenue, "credit": "10.00", "currency": "EUR" },
        ]}),
    )
    .await;
    assert_eq!(foreign["summary"]["total_debit"], Value::Null);

    // Stock adjustment: the value is known once the ledger has spoken.
    let adjustment = draft(
        &t,
        c,
        "stock_adjustment",
        date,
        json!({ "offset_account_id": w.account["5100"], "lines": [
            { "item_id": w.widget, "warehouse_id": w.main, "qty": "30", "unit_cost": "0.335" },
        ]}),
    )
    .await;
    assert_eq!(adjustment["summary"]["total_value"], Value::Null);
    let adjustment = run(
        &t,
        "stock_adjustment.post",
        json!({ "id": adjustment["id"] }),
    )
    .await;
    assert_eq!(total(&adjustment, "total_value"), 1005);
    assert_eq!(
        cents(posted(&t, &adjustment, &w.account["1300"]).await),
        1005
    );

    // Stock transfer: the value that moved.
    let transfer = draft(
        &t,
        c,
        "stock_transfer",
        date,
        json!({ "from_warehouse_id": w.main, "to_warehouse_id": w.annex, "lines": [
            { "item_id": w.widget, "qty": "7" },
        ]}),
    )
    .await;
    assert_eq!(transfer["summary"]["from_warehouse_id"], json!(w.main));
    assert_eq!(transfer["summary"]["total_value"], Value::Null);
    let transfer = run(&t, "stock_transfer.post", json!({ "id": transfer["id"] })).await;
    let moved: f64 = sqlx::query_scalar(
        "SELECT sum(e.value)::float8 FROM stock_entry e JOIN posting p ON p.id = e.posting_id
         WHERE p.document_id = $1 AND e.value > 0",
    )
    .bind(Uuid::parse_str(transfer["id"].as_str().unwrap()).unwrap())
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(total(&transfer, "total_value"), cents(moved));
    assert!(cents(moved) > 0);

    // Purchase order: lines at their prices, each rounded.
    // 3 x 0.335 = 1.005 -> 1.01 and 7 x 1.115 = 7.805 -> 7.81.
    let order = draft(
        &t,
        c,
        "purchase_order",
        date,
        json!({ "vendor_id": w.vendor, "lines": [
            { "item_id": w.widget, "warehouse_id": w.main, "qty": "3", "unit_price": "0.335" },
            { "item_id": w.gadget, "warehouse_id": w.main, "qty": "7", "unit_price": "1.115" },
        ]}),
    )
    .await;
    assert_eq!(order["summary"]["vendor_id"], json!(w.vendor));
    assert_eq!(order["summary"]["vendor_name"], "Parts Inc");
    assert_eq!(total(&order, "total_net"), 882);
    let order = run(&t, "purchase_order.approve", json!({ "id": order["id"] })).await;
    assert_eq!(total(&order, "total_net"), 882);

    // Receipt: what the goods came into stock for, which is what GRNI owes.
    let receipt = run(
        &t,
        "purchase_order.receive",
        json!({ "id": order["id"], "doc_date": date }),
    )
    .await;
    assert_eq!(receipt["summary"]["purchase_order_id"], order["id"]);
    assert_eq!(receipt["summary"]["vendor_name"], "Parts Inc");
    assert_eq!(receipt["summary"]["total_value"], Value::Null);
    let receipt = run(&t, "purchase_receipt.post", json!({ "id": receipt["id"] })).await;
    let grni = -posted(&t, &receipt, &w.account["2100"]).await;
    assert_eq!(total(&receipt, "total_value"), cents(grni));
    assert_eq!(cents(grni), 882);

    // Vendor invoice with tax on both lines: 7.25 % of 1.01 is 0.07 and of
    // 7.81 is 0.57. The draft already shows what posting will owe.
    let invoice = run(
        &t,
        "purchase_order.invoice",
        json!({ "id": order["id"], "doc_date": date }),
    )
    .await;
    assert_eq!(total(&invoice, "total_net"), 882);
    assert_eq!(total(&invoice, "total_tax"), 0);
    let mut body = invoice["body"].clone();
    for line in body["lines"].as_array_mut().unwrap() {
        line["tax_code_id"] = json!(w.tax);
    }
    let (status, invoice) = t
        .patch(
            &t.admin,
            &format!("/api/documents/vendor_invoice/{}", id(&invoice)),
            json!({ "version": invoice["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{invoice}");
    let before = invoice["summary"].clone();
    assert_eq!(total(&invoice, "total_net"), 882);
    assert_eq!(total(&invoice, "total_tax"), 64);
    assert_eq!(total(&invoice, "total_gross"), 946);
    let invoice = run(&t, "vendor_invoice.post", json!({ "id": invoice["id"] })).await;
    assert_eq!(invoice["summary"], before, "posting changed the totals");
    let owed = -posted(&t, &invoice, &w.account["2000"]).await;
    assert_eq!(total(&invoice, "total_gross"), cents(owed));
    assert_eq!(
        total(&invoice, "total_tax"),
        cents(posted(&t, &invoice, &w.account["1400"]).await)
    );
    let item = invoice["postings"][0]["entries"]["open_items"][0].clone();
    assert_eq!(cents(dec(&item["amount"])), 946);

    // Payment: what is allocated is what leaves the bank.
    let payment = run(
        &t,
        "payment.draft",
        json!({ "company_id": c, "open_item_ids": [item["id"]], "bank_account_id": w.cash, "doc_date": date }),
    )
    .await;
    assert_eq!(payment["summary"]["direction"], "outgoing");
    assert_eq!(payment["summary"]["party_type"], "vendor");
    assert_eq!(payment["summary"]["party_id"], json!(w.vendor));
    assert_eq!(total(&payment, "total"), 946);
    let payment = run(&t, "payment.post", json!({ "id": payment["id"] })).await;
    assert_eq!(total(&payment, "total"), 946);
    assert_eq!(cents(-posted(&t, &payment, &w.cash).await), 946);

    // Production order: what the product was made for.
    let production = draft(
        &t,
        c,
        "production_order",
        date,
        json!({ "bom_id": w.bom, "qty": "2", "warehouse_id": w.main }),
    )
    .await;
    assert_eq!(production["summary"]["item_id"], json!(w.gadget));
    assert_eq!(dec(&production["summary"]["qty"]), 2.0);
    assert_eq!(production["summary"]["total_value"], Value::Null);
    run(
        &t,
        "production_order.release",
        json!({ "id": production["id"] }),
    )
    .await;
    let production = run(
        &t,
        "production_order.complete",
        json!({ "id": production["id"] }),
    )
    .await;
    let made = posted(&t, &production, &w.account["1310"]).await;
    assert_eq!(total(&production, "total_value"), cents(made));
    assert!(cents(made) > 0);

    // Sales order, delivery and invoice. 2 x 9.995 = 19.99; tax 1.45.
    let sale = draft(
        &t,
        c,
        "sales_order",
        date,
        json!({ "customer_id": w.customer, "lines": [
            { "item_id": w.widget, "warehouse_id": w.main, "qty": "2", "unit_price": "9.995" },
        ]}),
    )
    .await;
    assert_eq!(sale["summary"]["customer_name"], "Buyers Ltd");
    assert_eq!(total(&sale, "total_net"), 1999);
    let sale = run(&t, "sales_order.approve", json!({ "id": sale["id"] })).await;
    let delivery = run(
        &t,
        "sales_order.deliver",
        json!({ "id": sale["id"], "doc_date": date }),
    )
    .await;
    assert_eq!(delivery["summary"]["sales_order_id"], sale["id"]);
    assert_eq!(delivery["summary"]["total_value"], Value::Null);
    let delivery = run(&t, "sales_delivery.post", json!({ "id": delivery["id"] })).await;
    let cost = posted(&t, &delivery, &w.account["5000"]).await;
    assert_eq!(total(&delivery, "total_value"), cents(cost));
    assert!(cents(cost) > 0);
    let bill = run(
        &t,
        "sales_order.invoice",
        json!({ "id": sale["id"], "doc_date": date }),
    )
    .await;
    let mut body = bill["body"].clone();
    body["lines"][0]["tax_code_id"] = json!(w.tax);
    let (status, bill) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_invoice/{}", id(&bill)),
            json!({ "version": bill["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{bill}");
    let before = bill["summary"].clone();
    assert_eq!(
        (
            total(&bill, "total_net"),
            total(&bill, "total_tax"),
            total(&bill, "total_gross")
        ),
        (1999, 145, 2144)
    );
    let bill = run(&t, "sales_invoice.post", json!({ "id": bill["id"] })).await;
    assert_eq!(bill["summary"], before, "posting changed the totals");
    assert_eq!(
        total(&bill, "total_gross"),
        cents(posted(&t, &bill, &w.account["1200"]).await)
    );
    assert_eq!(
        total(&bill, "total_tax"),
        cents(-posted(&t, &bill, &w.account["2300"]).await)
    );
    assert_eq!(
        total(&bill, "total_net"),
        cents(-posted(&t, &bill, &w.revenue).await)
    );

    // A draft whose tax cannot be worked out yet says so with a null.
    let other_tax = t
        .create(
            "tax_code",
            json!({ "code": "NONE", "name": "No accounts", "rate": "5" }),
        )
        .await;
    let order2 = draft(
        &t,
        c,
        "sales_order",
        date,
        json!({ "customer_id": w.customer, "lines": [
            { "item_id": w.widget, "warehouse_id": w.main, "qty": "1", "unit_price": "1.00" },
        ]}),
    )
    .await;
    let order2 = run(&t, "sales_order.approve", json!({ "id": order2["id"] })).await;
    let unknown = draft(
        &t,
        c,
        "sales_invoice",
        date,
        json!({ "sales_order_id": order2["id"], "lines": [{
            "order_line_id": order2["body"]["lines"][0]["id"],
            "qty": "1", "unit_price": "1.00", "tax_code_id": other_tax,
        }]}),
    )
    .await;
    assert_eq!(total(&unknown, "total_net"), 100);
    assert_eq!(unknown["summary"]["total_tax"], Value::Null);
    assert_eq!(unknown["summary"]["total_gross"], Value::Null);

    // Year-end close: the result that went to retained earnings.
    let close = draft(
        &t,
        c,
        "year_end_close",
        "2026-12-31",
        json!({ "fiscal_period_id": w.period, "retained_earnings_account_id": w.account["3100"] }),
    )
    .await;
    assert_eq!(close["summary"]["fiscal_period_id"], json!(w.period));
    assert_eq!(close["summary"]["result"], Value::Null);
    let close = run(&t, "year_end_close.post", json!({ "id": close["id"] })).await;
    let retained = -posted(&t, &close, &w.account["3100"]).await;
    assert_eq!(total(&close, "result"), cents(retained));
    assert_ne!(cents(retained), 0);

    t.stop().await;
}

#[tokio::test]
async fn lists_show_sort_and_filter_by_the_summary() {
    let t = TestApp::start().await;
    let w = world(&t).await;
    let c = &w.company;
    let other_vendor = t
        .create("vendor", json!({ "code": "V002", "name": "Bolts Co" }))
        .await;
    let order = |vendor: &str, qty: &str| {
        draft(
            &t,
            c,
            "purchase_order",
            "2026-03-15",
            json!({ "vendor_id": vendor, "lines": [
                { "item_id": w.widget, "warehouse_id": w.main, "qty": qty, "unit_price": "2.00" },
            ]}),
        )
    };
    order(&w.vendor, "5").await;
    order(&other_vendor, "50").await;
    order(&w.vendor, "500").await;
    let list = |input: Value| {
        let mut input = input;
        input["doc_type"] = json!("purchase_order");
        let t = &t;
        async move { run(t, "document.list", input).await }
    };
    let totals = |page: &Value| -> Vec<f64> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| dec(&d["summary"]["total_net"]))
            .collect()
    };

    // The list carries the figures, and who made each document.
    let page = list(json!({ "sort": ["summary.total_net"] })).await;
    assert_eq!(totals(&page), [10.0, 100.0, 1000.0]);
    assert_eq!(page["items"][0]["summary"]["vendor_name"], "Parts Inc");
    let admin: String = sqlx::query_scalar("SELECT name FROM app_user WHERE email = $1")
        .bind(common::ADMIN_EMAIL)
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(page["items"][0]["created_by_name"], json!(admin));

    // Amounts sort as numbers, not as text; names as text.
    let page = list(json!({ "sort": ["-summary.total_net"] })).await;
    assert_eq!(totals(&page), [1000.0, 100.0, 10.0]);
    let page = list(json!({ "sort": ["summary.vendor_name", "-summary.total_net"] })).await;
    assert_eq!(totals(&page), [100.0, 1000.0, 10.0]);

    // Filter by a summary field, with the total of the filtered list.
    let page = list(json!({ "summary": { "vendor_id": w.vendor } })).await;
    assert_eq!(page["total"], 2);
    let (status, page) = t
        .get(
            &t.admin,
            &format!(
                "/api/documents/purchase_order?summary.vendor_id={other_vendor}&sort=-summary.total_net"
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(totals(&page), [100.0]);

    // Only what the type declares can be named.
    for input in [
        json!({ "doc_type": "purchase_order", "sort": ["summary.colour"] }),
        json!({ "doc_type": "purchase_order", "summary": { "colour": "red" } }),
        json!({ "doc_type": "purchase_order", "sort": ["summary.total_net'; --"] }),
    ] {
        let (status, body) = t.command(&t.admin, "document.list", input).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }

    // The document itself names its maker, and so does each posting.
    let journal = draft(
        &t,
        c,
        "journal_entry",
        "2026-03-15",
        json!({ "lines": [
            { "account_id": w.cash, "debit": "1.00" },
            { "account_id": w.revenue, "credit": "1.00" },
        ]}),
    )
    .await;
    assert_eq!(journal["created_by_name"], json!(admin));
    let journal = run(&t, "journal_entry.post", json!({ "id": journal["id"] })).await;
    assert_eq!(journal["postings"][0]["created_by_name"], json!(admin));
    // A reader of the document sees the name without any right over users.
    let reader = t
        .user_with_role("reader@example.com", &["journal_entry:read"], c)
        .await;
    let (status, seen) = t
        .get(
            &reader,
            &format!("/api/documents/journal_entry/{}", id(&journal)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{seen}");
    assert_eq!(seen["created_by_name"], json!(admin));

    t.stop().await;
}

// ------------------------------------------------------------------- search

#[tokio::test]
async fn one_search_finds_records_and_documents() {
    let t = TestApp::start().await;
    let w = world(&t).await;
    let c = &w.company;
    let journal = draft(
        &t,
        c,
        "journal_entry",
        "2026-03-15",
        json!({ "lines": [
            { "account_id": w.cash, "debit": "1.00" },
            { "account_id": w.revenue, "credit": "1.00" },
        ]}),
    )
    .await;
    let (status, journal) = t
        .patch(
            &t.admin,
            &format!("/api/documents/journal_entry/{}", id(&journal)),
            json!({ "version": 1, "memo": "Widget samples written off" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{journal}");
    let group = |result: &Value, name: &str| -> Option<Value> {
        result["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == name)
            .cloned()
    };

    // One call: an item by its name, a document by its memo.
    let found = run(&t, "search.records", json!({ "q": "widget" })).await;
    let items = group(&found, "item").unwrap();
    assert_eq!(items["kind"], "entity");
    assert_eq!(items["label"], "Items");
    assert_eq!(items["more"], false);
    assert_eq!(
        items["items"],
        json!([{
            "id": w.widget, "title": "WIDGET · Widget", "subtitle": null,
            "company_id": null, "status": null, "doc_date": null,
        }])
    );
    let journals = group(&found, "journal_entry").unwrap();
    assert_eq!(journals["kind"], "document");
    assert_eq!(journals["label"], "Journal entries");
    assert_eq!(journals["items"][0]["id"], journal["id"]);
    assert_eq!(journals["items"][0]["title"], "Journal entry");
    assert_eq!(
        journals["items"][0]["subtitle"],
        "Widget samples written off"
    );
    assert_eq!(journals["items"][0]["status"], "draft");
    assert_eq!(journals["items"][0]["doc_date"], "2026-03-15");
    assert_eq!(journals["items"][0]["company_id"], json!(c));
    assert_eq!(found["truncated"], false);
    // Nothing else matched.
    assert_eq!(found["groups"].as_array().unwrap().len(), 2, "{found}");
    // A posted document is found by its number, and shown by it.
    let posted = run(&t, "journal_entry.post", json!({ "id": journal["id"] })).await;
    let found = run(&t, "search.records", json!({ "q": posted["number"] })).await;
    assert_eq!(
        group(&found, "journal_entry").unwrap()["items"][0]["title"],
        posted["number"]
    );

    // A few per kind; the rest is for the kind's own list.
    let found = run(&t, "search.records", json!({ "q": "account", "limit": 1 })).await;
    let accounts = group(&found, "account").unwrap();
    assert_eq!(accounts["items"].as_array().unwrap().len(), 1);
    assert_eq!(accounts["more"], true);
    assert_eq!(accounts["items"][0]["company_id"], json!(c));
    let found = run(&t, "search.records", json!({ "q": "account" })).await;
    let accounts = group(&found, "account").unwrap();
    assert_eq!(accounts["items"].as_array().unwrap().len(), 2);
    assert_eq!(accounts["more"], false);

    // One letter is not a search.
    let (status, body) = t
        .command(&t.admin, "search.records", json!({ "q": " a " }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "q");

    // Within one company: its own records, and those shared by all.
    let other = t.company("OTHER", "USD").await;
    t.create(
        "account",
        json!({ "company_id": other.id, "code": "1210", "name": "Accounts elsewhere", "account_type": "asset" }),
    )
    .await;
    let everywhere = run(&t, "search.records", json!({ "q": "accounts" })).await;
    assert_eq!(
        group(&everywhere, "account").unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let here = run(
        &t,
        "search.records",
        json!({ "q": "accounts", "company_id": c }),
    )
    .await;
    assert_eq!(
        group(&here, "account").unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let here = run(
        &t,
        "search.records",
        json!({ "q": "widget", "company_id": other.id }),
    )
    .await;
    assert!(
        group(&here, "item").is_some(),
        "items are shared by all companies"
    );
    assert!(group(&here, "journal_entry").is_none(), "{here}");

    // The search sees what the lists would show the caller, no more.
    t.create("item", json!({ "code": "X-9", "name": "Sprocket" }))
        .await;
    let storekeeper = t
        .user_with_role("store@example.com", &["item:read"], c)
        .await;
    let (status, found) = t
        .command(&storekeeper, "search.records", json!({ "q": "widget" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let kinds: Vec<&str> = found["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["item"], "only what the caller may read is searched");
    // A withheld field is neither searched nor shown.
    let (blinkered, _) = t
        .user_with_rules(
            "blinkered@example.com",
            json!(["item:read", { "deny": "item:read", "fields": ["name"] }]),
            Some(c),
        )
        .await;
    let (_, found) = t
        .command(&blinkered, "search.records", json!({ "q": "sprocket" }))
        .await;
    assert_eq!(found["groups"], json!([]), "the name is withheld");
    let (_, found) = t
        .command(&blinkered, "search.records", json!({ "q": "x-9" }))
        .await;
    assert_eq!(group(&found, "item").unwrap()["items"][0]["title"], "X-9");
    // Documents of another company stay out of reach.
    let outsider = t
        .user_with_role("outsider@example.com", &["journal_entry:read"], &other.id)
        .await;
    let (_, found) = t
        .command(&outsider, "search.records", json!({ "q": "widget" }))
        .await;
    assert_eq!(found["groups"], json!([]));

    t.stop().await;
}

#[tokio::test]
async fn lines_say_which_of_their_fields_are_alternatives() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    let kinds = |doc_type: &str| {
        let alternatives = &named(&meta["doc_types"], doc_type)["alternatives"];
        assert_eq!(alternatives[0]["collection"], "lines", "{doc_type}");
        alternatives[0]["one_of"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["label"].as_str().unwrap().to_owned(), a["fields"].clone()))
            .collect::<Vec<_>>()
    };
    for order in ["sales_order", "purchase_order"] {
        assert_eq!(
            kinds(order),
            [
                ("Stock item".to_owned(), json!(["item_id", "warehouse_id"])),
                ("Charge".to_owned(), json!(["charge_id"])),
                ("Account".to_owned(), json!(["account_id"])),
            ]
        );
    }
    for invoice in ["sales_invoice", "vendor_invoice"] {
        assert_eq!(
            kinds(invoice)[0],
            ("Order line".to_owned(), json!(["order_line_id"]))
        );
    }
    // A type without them says so with an empty list.
    assert_eq!(
        named(&meta["doc_types"], "journal_entry")["alternatives"],
        json!([])
    );
    t.stop().await;
}
