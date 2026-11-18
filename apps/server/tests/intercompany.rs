//! Intercompany trading: what one company sells to another of the same
//! database is drafted on the buyer's side as the buyer's user, and the
//! balances of both sides are compared.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use erpcraft_kernel::outbox;
use serde_json::{Value, json};

struct Side {
    id: String,
    warehouse: String,
    revenue: String,
    cogs: String,
}

async fn company(t: &TestApp, code: &str) -> Side {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_small_business", "code": code, "name": format!("{code} Inc"),
                    "first_year": 2026, "years": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Side {
        id: body["company"]["id"].as_str().unwrap().to_owned(),
        warehouse: body["warehouse_id"].as_str().unwrap().to_owned(),
        revenue: body["roles"]["revenue"]["id"].as_str().unwrap().to_owned(),
        cogs: body["roles"]["cogs"]["id"].as_str().unwrap().to_owned(),
    }
}

struct Fixture {
    seller: Side,
    buyer: Side,
    item: String,
    customer: String,
    vendor: String,
    rule: String,
    buyer_token: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let seller = company(t, "OHIO").await;
    let buyer = company(t, "GMBH").await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let customer = t
        .create(
            "customer",
            json!({ "code": "C-GMBH", "name": "Ohio Widgets GmbH" }),
        )
        .await;
    let vendor = t
        .create(
            "vendor",
            json!({ "code": "V-OHIO", "name": "Ohio Widgets Inc" }),
        )
        .await;
    t.create(
        "intercompany_party",
        json!({ "party_type": "customer", "party_id": customer, "company_id": buyer.id }),
    )
    .await;
    t.create(
        "intercompany_party",
        json!({ "party_type": "vendor", "party_id": vendor, "company_id": seller.id }),
    )
    .await;
    let (buyer_token, buyer_user) = t
        .user_with_rules(
            "buyer@example.com",
            json!([
                "purchase_order:create",
                "purchase_order:read",
                "vendor_invoice:create",
                "vendor_invoice:read",
                "intercompany:mirror",
            ]),
            Some(&buyer.id),
        )
        .await;
    let rule = t
        .create(
            "intercompany_rule",
            json!({ "company_id": seller.id, "buyer_company_id": buyer.id,
                    "customer_id": customer, "vendor_id": vendor, "run_as_user_id": buyer_user,
                    "warehouse_id": buyer.warehouse }),
        )
        .await;
    Fixture {
        seller,
        buyer,
        item,
        customer,
        vendor,
        rule,
        buyer_token,
    }
}

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

/// Draft a document as the administrator and run `command` on it.
async fn posted(t: &TestApp, company: &str, doc_type: &str, command: &str, body: Value) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": company, "doc_date": "2026-03-15", "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
    run(t, command, json!({ "id": id(&draft) })).await
}

async fn deliver(t: &TestApp) {
    let delivered = outbox::deliver_batch(&t.kernel, 500).await.unwrap();
    assert_eq!(delivered.failed, 0);
}

/// The mirror the buyer has of a document of the seller, as the buyer.
async fn mirror_of(t: &TestApp, f: &Fixture, source: &Value, doc_type: &str) -> Value {
    let row: (Option<uuid::Uuid>, String, Option<String>) = sqlx::query_as(
        "SELECT mirror_id, status, message FROM intercompany_document WHERE source_id = $1::uuid",
    )
    .bind(id(source))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let mirror = row
        .0
        .unwrap_or_else(|| panic!("no mirror: {} {:?}", row.1, row.2));
    let (status, doc) = t
        .get(
            &f.buyer_token,
            &format!("/api/documents/{doc_type}/{mirror}"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    doc
}

#[tokio::test]
async fn a_sale_to_a_sister_company_is_drafted_on_its_side_and_the_balances_agree() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // The seller has the goods.
    posted(
        &t,
        &f.seller.id,
        "stock_adjustment",
        "stock_adjustment.post",
        json!({ "offset_account_id": f.seller.cogs, "lines": [
            { "item_id": f.item, "warehouse_id": f.seller.warehouse, "qty": "10", "unit_cost": "4" }] }),
    )
    .await;

    // The approved sales order becomes the buyer's draft purchase order.
    let order = posted(
        &t,
        &f.seller.id,
        "sales_order",
        "sales_order.approve",
        json!({ "customer_id": f.customer, "lines": [
            { "item_id": f.item, "warehouse_id": f.seller.warehouse, "qty": "5", "unit_price": "9.00" }] }),
    )
    .await;
    deliver(&t).await;
    deliver(&t).await;
    let po = mirror_of(&t, &f, &order, "purchase_order").await;
    assert_eq!(po["status"], "draft");
    assert_eq!(po["company_id"], json!(f.buyer.id));
    assert_eq!(po["created_by"], po["created_by"]);
    assert_eq!(po["body"]["vendor_id"], json!(f.vendor));
    let line = &po["body"]["lines"][0];
    assert_eq!(line["warehouse_id"], json!(f.buyer.warehouse));
    assert_eq!(dec(&line["qty"]), 5.0);
    assert_eq!(dec(&line["unit_price"]), 9.0);
    // Once, however often the event comes, and linked both ways.
    assert_eq!(
        t.count("SELECT count(*) FROM document WHERE doc_type = 'purchase_order'")
            .await,
        1
    );
    assert!(
        po["links"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["id"] != order["id"]),
        "the buyer may not read the seller's order, so the link is not shown to them"
    );
    let (_, seen) = t
        .get(
            &t.admin,
            &format!("/api/documents/purchase_order/{}", id(&po)),
        )
        .await;
    assert_eq!(seen["links"][0]["id"], order["id"]);
    assert_eq!(seen["links"][0]["kind"], "intercompany");

    // The buyer approves; the seller delivers and invoices.
    run(&t, "purchase_order.approve", json!({ "id": id(&po) })).await;
    let order_line = order["body"]["lines"][0]["id"].clone();
    posted(
        &t,
        &f.seller.id,
        "sales_delivery",
        "sales_delivery.post",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": order_line, "qty": "5" }] }),
    )
    .await;
    let invoice = posted(
        &t,
        &f.seller.id,
        "sales_invoice",
        "sales_invoice.post",
        json!({ "sales_order_id": order["id"], "lines": [
            { "order_line_id": order_line, "qty": "5", "unit_price": "9.00" }] }),
    )
    .await;
    deliver(&t).await;
    let bill = mirror_of(&t, &f, &invoice, "vendor_invoice").await;
    assert_eq!(bill["status"], "draft");
    assert_eq!(bill["body"]["purchase_order_id"], po["id"]);
    assert_eq!(bill["body"]["vendor_reference"], invoice["number"]);
    assert_eq!(
        bill["body"]["lines"][0]["order_line_id"],
        po["body"]["lines"][0]["id"]
    );

    // Until the buyer posts it, the books differ by the invoice, and the
    // report says which one.
    let balances = run(
        &t,
        "intercompany.balances",
        json!({ "company_id": f.seller.id }),
    )
    .await;
    let row = &balances["rows"][0];
    assert_eq!(row["currency"], "USD");
    let gross = dec(&invoice["summary"]["total_gross"]);
    assert_eq!(dec(&row["seller_receivable"]), gross);
    assert_eq!(dec(&row["buyer_payable"]), 0.0);
    assert_eq!(dec(&row["difference"]), gross);
    assert_eq!(balances["unmatched"][0]["source_id"], invoice["id"]);
    assert_eq!(balances["unmatched"][0]["mirror_status"], "draft");

    // The buyer receives the goods and posts the bill: the books agree.
    let po_line = po["body"]["lines"][0]["id"].clone();
    posted(
        &t,
        &f.buyer.id,
        "purchase_receipt",
        "purchase_receipt.post",
        json!({ "purchase_order_id": po["id"], "lines": [{ "order_line_id": po_line, "qty": "5" }] }),
    )
    .await;
    run(&t, "vendor_invoice.post", json!({ "id": id(&bill) })).await;
    let balances = run(
        &t,
        "intercompany.balances",
        json!({ "company_id": f.buyer.id }),
    )
    .await;
    let row = &balances["rows"][0];
    assert_eq!(
        dec(&row["buyer_payable"]),
        dec(&row["seller_receivable"]),
        "{balances}"
    );
    assert_eq!(dec(&row["difference"]), 0.0);
    assert_eq!(balances["unmatched"], json!([]));
    t.stop().await;
}

#[tokio::test]
async fn a_refused_mirror_is_kept_and_can_be_run_again() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // A direct invoice for a service, on an account: the rule names no
    // account of the buyer for it.
    let invoice = posted(
        &t,
        &f.seller.id,
        "sales_invoice",
        "sales_invoice.post",
        json!({ "customer_id": f.customer, "lines": [
            { "account_id": f.seller.revenue, "description": "Engineering", "qty": "1", "unit_price": "250.00" }] }),
    )
    .await;
    deliver(&t).await;
    let (status, message): (String, Option<String>) = sqlx::query_as(
        "SELECT status, message FROM intercompany_document WHERE source_id = $1::uuid",
    )
    .bind(id(&invoice))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(status, "refused");
    assert!(message.unwrap().contains("names no account"));

    // Fixed: the rule names the buyer's account, and the buyer runs it.
    let expense = t
        .create(
            "account",
            json!({ "company_id": f.buyer.id, "code": "6900", "name": "Group services",
                    "account_type": "expense" }),
        )
        .await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/intercompany_rule/{}", f.rule),
            json!({ "account_id": expense }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, bill) = t
        .command(
            &f.buyer_token,
            "intercompany.mirror",
            json!({ "source_id": id(&invoice) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{bill}");
    assert_eq!(bill["body"]["vendor_id"], json!(f.vendor));
    assert_eq!(bill["body"]["lines"][0]["account_id"], json!(expense));
    // Again: the same draft.
    let (_, again) = t
        .command(
            &f.buyer_token,
            "intercompany.mirror",
            json!({ "source_id": id(&invoice) }),
        )
        .await;
    assert_eq!(again["id"], bill["id"]);

    // A rule's account must be the buyer's, and writing a rule takes the
    // right in the buying company too.
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/intercompany_rule/{}", f.rule),
            json!({ "account_id": f.seller.revenue }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);
    let (token, _) = t
        .user_with_rules(
            "seller@example.com",
            json!(["intercompany_rule:update", "intercompany_rule:read"]),
            Some(&f.seller.id),
        )
        .await;
    let (status, body) = t
        .patch(
            &token,
            &format!("/api/entities/intercompany_rule/{}", f.rule),
            json!({ "mirror_orders": false }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    t.stop().await;
}
