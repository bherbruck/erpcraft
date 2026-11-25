//! What an operator runs: migration status and the refusal of a database
//! a newer binary migrated, and the check that the books agree.

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

#[tokio::test]
async fn a_database_migrated_by_a_newer_binary_is_refused() {
    let t = TestApp::start().await;
    let kernel = erpcraft_server::kernel(t.pool.clone()).unwrap();
    let status = kernel.migration_status().await.unwrap();
    assert!(status.is_current(), "{status:?}");
    assert!(status.applied > 100);

    // A migration this binary does not know, as a newer one would leave.
    sqlx::query(
        "INSERT INTO erpcraft_migration (module, version, name) VALUES ('stock', 999, 'later')",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let status = kernel.migration_status().await.unwrap();
    assert_eq!(status.unknown.len(), 1);
    assert_eq!(status.unknown[0].name, "later");
    let refused = kernel.migrate().await.unwrap_err().to_string();
    assert!(refused.contains("stock 999 (later)"), "{refused}");
    assert!(refused.contains("newer erpcraft"), "{refused}");
    sqlx::query("DELETE FROM erpcraft_migration WHERE version = 999")
        .execute(&t.pool)
        .await
        .unwrap();

    // One forgotten: pending, and applied again by the next start.
    sqlx::query("DELETE FROM erpcraft_migration WHERE module = 'budgets' AND version = 1")
        .execute(&t.pool)
        .await
        .unwrap();
    let status = kernel.migration_status().await.unwrap();
    assert_eq!(status.pending.len(), 1, "{status:?}");
    assert_eq!(status.pending[0].module, "budgets");
    t.stop().await;
}

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

#[tokio::test]
async fn the_books_are_verified_and_a_disagreement_is_named() {
    let t = TestApp::start().await;
    let setup = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "SHOP", "name": "Shop Inc",
                "first_year": 2026, "years": 1 }),
    )
    .await;
    let c = setup["company"]["id"].as_str().unwrap().to_owned();
    let role = |name: &str| setup["roles"][name]["id"].as_str().unwrap().to_owned();
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c, "code": "BACK", "name": "Back room" }),
        )
        .await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V", "name": "Vendor" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C", "name": "Customer" }))
        .await;
    let draft = |doc_type: &'static str, body: Value| {
        let (t, c) = (&t, c.clone());
        async move {
            let (status, draft) = t
                .post(
                    &t.admin,
                    &format!("/api/documents/{doc_type}"),
                    json!({ "company_id": c, "doc_date": "2026-03-02", "body": body }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{draft}");
            draft
        }
    };
    // Bought, received, invoiced; sold, delivered, invoiced, partly paid.
    let po = draft(
        "purchase_order",
        json!({ "vendor_id": vendor, "lines": [
            { "item_id": item, "warehouse_id": warehouse, "qty": "10", "unit_price": "4" }] }),
    )
    .await;
    let po = run(&t, "purchase_order.approve", json!({ "id": id(&po) })).await;
    let receipt = run(&t, "purchase_order.receive", json!({ "id": id(&po) })).await;
    run(&t, "purchase_receipt.post", json!({ "id": id(&receipt) })).await;
    let bill = run(&t, "purchase_order.invoice", json!({ "id": id(&po) })).await;
    run(&t, "vendor_invoice.post", json!({ "id": id(&bill) })).await;
    let so = draft(
        "sales_order",
        json!({ "customer_id": customer, "lines": [
            { "item_id": item, "warehouse_id": warehouse, "qty": "4", "unit_price": "10" }] }),
    )
    .await;
    let so = run(&t, "sales_order.approve", json!({ "id": id(&so) })).await;
    let delivery = run(&t, "sales_order.deliver", json!({ "id": id(&so) })).await;
    run(&t, "sales_delivery.post", json!({ "id": id(&delivery) })).await;
    let invoice = run(&t, "sales_order.invoice", json!({ "id": id(&so) })).await;
    let invoice = run(&t, "sales_invoice.post", json!({ "id": id(&invoice) })).await;
    let item_id = invoice["postings"][0]["entries"]["open_items"][0]["id"].clone();
    let payment = draft(
        "payment",
        json!({ "direction": "incoming", "bank_account_id": role("bank"),
                "allocations": [{ "open_item_id": item_id, "amount": "15" }] }),
    )
    .await;
    run(&t, "payment.post", json!({ "id": id(&payment) })).await;

    let report = erpcraft_server::verify::verify(&t.pool, None)
        .await
        .unwrap();
    assert!(report.ok, "{}", report.text());
    let shop = report.companies.iter().find(|c| c.code == "SHOP").unwrap();
    let inventory = shop.checks.iter().find(|c| c.name == "inventory").unwrap();
    assert_eq!(inventory.expected.unwrap().to_string(), "24.0000");

    // A journal entry straight onto the inventory and receivable accounts:
    // the books still balance, but the ledgers no longer agree.
    let inventory_account: String = sqlx::query_scalar(
        "SELECT account_id::text FROM account_rule
         WHERE company_id = $1::uuid AND purpose = 'inventory' LIMIT 1",
    )
    .bind(&c)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let je = draft(
        "journal_entry",
        json!({ "lines": [
            { "account_id": inventory_account, "debit": "5.00" },
            { "account_id": role("receivable"), "debit": "3.00" },
            { "account_id": role("revenue"), "credit": "8.00" }] }),
    )
    .await;
    let (status, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": id(&je) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    let report = erpcraft_server::verify::verify(&t.pool, Some("SHOP"))
        .await
        .unwrap();
    assert!(!report.ok);
    let failed: Vec<&str> = report.companies[0]
        .checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.name)
        .collect();
    assert_eq!(failed, vec!["inventory", "open_items"], "{}", report.text());
    assert!(report.text().contains("the books do NOT agree"));
    t.stop().await;
}
