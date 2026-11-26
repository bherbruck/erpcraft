//! Review 9, operations: `erpcraft verify` giving a false green.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

/// `erpcraft verify --company OHIO` with a typo (or run against the wrong
/// database) checks nothing and says "the books agree", exit 0.
#[tokio::test]
async fn verify_of_a_company_that_does_not_exist_is_not_green() {
    let t = TestApp::start().await;
    let _c = t.company("REAL", "USD").await;
    let report = erpcraft_server::verify::verify(&t.pool, Some("NOPE"))
        .await
        .unwrap();
    let ok = report.ok;
    let text = report.text();
    t.stop().await;
    assert!(
        !ok,
        "verify of a company code that matches nothing reported success:\n{text}"
    );
}

/// The receivables account of the sales settings carries a balance that no
/// open item stands for (a manual journal entry onto it, as the docs name),
/// but it has no open items at all, so the open-items check never looks at
/// it and verify is green.
#[tokio::test]
async fn verify_checks_a_receivable_account_that_has_no_open_items() {
    let t = TestApp::start().await;
    let c = t.company("ARCO", "USD").await;
    let account = |code: &'static str, name: &'static str, kind: &'static str| {
        let company = c.id.clone();
        let t = &t;
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": name, "account_type": kind }),
            )
            .await
        }
    };
    let receivable = account("1200", "Accounts receivable", "asset").await;
    let cogs = account("5000", "Cost of goods sold", "expense").await;
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
    let (status, doc) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({
                "company_id": c.id,
                "doc_date": "2026-03-15",
                "memo": "straight onto receivables",
                "body": { "lines": [
                    { "account_id": receivable, "debit": "100.00" },
                    { "account_id": c.revenue, "credit": "100.00" },
                ]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc}");
    let (status, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");

    let report = erpcraft_server::verify::verify(&t.pool, None)
        .await
        .unwrap();
    let ok = report.ok;
    let text = report.text();
    t.stop().await;
    assert!(
        !ok,
        "the receivables account holds 100.00 that no open item stands for, yet:\n{text}"
    );
}

/// A payment dated before the invoice it settles was allowed until
/// openitems 11. Such a settlement recorded before that migration was
/// applied is legacy: a warning, and verify still passes. One recorded
/// after it (here with the rule's trigger switched off, as drift) fails.
#[tokio::test]
async fn an_early_settlement_from_before_openitems_11_is_a_warning_and_a_later_one_fails() {
    let u = common::upgrade::Upgrade::at(&[("open_items", 10)]).await;
    let rows = |settlement: &str, payment: &str, posting: &str| {
        format!(
            "INSERT INTO document (id, company_id, doc_type, status, doc_date, created_by)
                 VALUES ('{payment}', '00000000-0000-0000-0000-0000000000c1', 'payment',
                         'posted', '2026-03-05', '00000000-0000-0000-0000-0000000000e1');
             INSERT INTO posting (id, company_id, document_id, command, posting_date, created_by)
                 VALUES ('{posting}', '00000000-0000-0000-0000-0000000000c1', '{payment}',
                         'payment.post', '2026-03-05', '00000000-0000-0000-0000-0000000000e1');
             INSERT INTO settlement (id, company_id, posting_id, open_item_id, amount, txn_amount)
                 VALUES ('{settlement}', '00000000-0000-0000-0000-0000000000c1', '{posting}',
                         '00000000-0000-0000-0000-0000000000f1', 10, 10);"
        )
    };
    u.sql(
        "INSERT INTO app_user (id, email, name) VALUES
             ('00000000-0000-0000-0000-0000000000e1', 'old@example.com', 'Old');
         INSERT INTO company (id, code, name, base_currency)
             VALUES ('00000000-0000-0000-0000-0000000000c1', 'OLD', 'Old Inc', 'USD');
         INSERT INTO account (id, company_id, code, name, account_type)
             VALUES ('00000000-0000-0000-0000-0000000000a1', '00000000-0000-0000-0000-0000000000c1',
                     '1200', 'Receivables', 'asset');
         INSERT INTO document (id, company_id, doc_type, status, doc_date, created_by)
             VALUES ('00000000-0000-0000-0000-0000000000d1', '00000000-0000-0000-0000-0000000000c1',
                     'sales_invoice', 'posted', '2026-03-10', '00000000-0000-0000-0000-0000000000e1');
         INSERT INTO posting (id, company_id, document_id, command, posting_date, created_by)
             VALUES ('00000000-0000-0000-0000-0000000000b1', '00000000-0000-0000-0000-0000000000c1',
                     '00000000-0000-0000-0000-0000000000d1', 'sales_invoice.post', '2026-03-10',
                     '00000000-0000-0000-0000-0000000000e1');
         INSERT INTO open_item (id, company_id, posting_id, kind, party_type, party_id, account_id,
                                amount, txn_amount, currency, doc_date)
             VALUES ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-0000000000c1',
                     '00000000-0000-0000-0000-0000000000b1', 'receivable', 'customer',
                     '00000000-0000-0000-0000-0000000000e1', '00000000-0000-0000-0000-0000000000a1',
                     100, 100, 'USD', '2026-03-10');",
    )
    .await;
    u.sql(&rows(
        "00000000-0000-0000-0000-000000000051",
        "00000000-0000-0000-0000-0000000000d2",
        "00000000-0000-0000-0000-0000000000b2",
    ))
    .await;
    u.upgrade().await.unwrap();
    let check = |report: &erpcraft_server::verify::Report| {
        report.companies[0]
            .checks
            .iter()
            .find(|c| c.name == "settlement_dates")
            .cloned()
            .unwrap()
    };
    let report = erpcraft_server::verify::verify(&u.pool, Some("OLD"))
        .await
        .unwrap();
    let legacy = check(&report);
    let text = report.text();

    // Drift after the rule: the trigger off, an early settlement written.
    u.sql(&format!(
        "ALTER TABLE settlement DISABLE TRIGGER settlement_after_item;
         {}
         ALTER TABLE settlement ENABLE TRIGGER settlement_after_item;",
        rows(
            "00000000-0000-0000-0000-000000000052",
            "00000000-0000-0000-0000-0000000000d3",
            "00000000-0000-0000-0000-0000000000b3",
        )
    ))
    .await;
    let later = erpcraft_server::verify::verify(&u.pool, Some("OLD"))
        .await
        .unwrap();
    let drift = check(&later);
    u.stop().await;

    assert!(legacy.ok, "{text}");
    assert!(
        legacy
            .warning
            .as_deref()
            .is_some_and(|w| w.contains("legacy: 1")),
        "{text}"
    );
    assert!(text.contains("warn settlement_dates"), "{text}");
    assert!(!drift.ok, "{}", later.text());
    assert!(
        drift.detail.starts_with("1 settlements"),
        "{}",
        later.text()
    );
    assert!(drift.warning.is_some(), "the legacy one is still named");
}
