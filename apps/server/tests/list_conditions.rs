//! What a list's `where` can say: text tests, values sent as `params`,
//! dates relative to today in the reader's time zone, and a document's
//! summary fields.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};

fn codes(page: &Value) -> Vec<&str> {
    page["items"]
        .as_array()
        .unwrap_or_else(|| panic!("not a page: {page}"))
        .iter()
        .map(|item| item["code"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn text_tests_match_parts_of_a_field_and_wildcards_are_text() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    for (code, name) in [
        ("1100", "Petty Cash"),
        ("1200", "Bank of Ohio"),
        ("5000", "100% cotton"),
    ] {
        t.create(
            "account",
            json!({ "company_id": c.id, "code": code, "name": name, "account_type": "asset" }),
        )
        .await;
    }
    let list = |condition: &str| json!({ "entity": "account", "filters": { "company_id": c.id }, "where": condition, "sort": ["code"] });
    let cases = [
        (
            "record.name.containsIgnoreCase('CASH')",
            vec!["1000", "1100"],
        ),
        ("record.name.contains('Cash')", vec!["1000", "1100"]),
        ("record.name.contains('cash')", vec![]),
        ("record.name.startsWithIgnoreCase('bank')", vec!["1200"]),
        ("record.name.endsWith('Ohio')", vec!["1200"]),
        ("record.name.equalsIgnoreCase('petty cash')", vec!["1100"]),
        ("record.name.equalsIgnoreCase('petty')", vec![]),
        // % and _ are what they are, not wildcards.
        ("record.name.startsWith('100%')", vec!["5000"]),
        ("record.name.contains('_')", vec![]),
        (
            "record.code.startsWith('1') && !record.name.containsIgnoreCase('cash')",
            vec!["1", "1200"],
        ),
    ];
    for (condition, expected) in cases {
        let (status, page) = t.command(&t.admin, "entity.list", list(condition)).await;
        assert_eq!(status, StatusCode::OK, "{condition}: {page}");
        assert_eq!(codes(&page), expected, "{condition}");
    }
    let (status, body) = t
        .command(
            &t.admin,
            "entity.list",
            list("record.is_group.contains('x')"),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn params_carry_a_long_list_without_lengthening_the_condition() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    // 600 codes, two of which exist: far more than a condition may be long.
    let mut wanted: Vec<String> = (0..600).map(|i| format!("X{i:05}")).collect();
    wanted.push("1000".into());
    wanted.push("4000".into());
    let (status, page) = t
        .command(
            &t.admin,
            "entity.list",
            json!({ "entity": "account", "filters": { "company_id": c.id },
                    "where": "record.code in params.codes", "params": { "codes": wanted }, "sort": ["code"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(codes(&page), ["1000", "4000"]);

    // Over HTTP, params are a JSON object in the query string.
    let params = "%7B%22codes%22%3A%5B%221000%22%5D%7D"; // {"codes":["1000"]}
    let (status, page) = t
        .get(
            &t.admin,
            &format!("/api/entities/account?company_id={}&where=record.code%20in%20params.codes&params={params}", c.id),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(codes(&page), ["1000"]);

    // A param the condition reads but the caller did not send is the caller's mistake.
    let (status, body) = t
        .command(
            &t.admin,
            "entity.list",
            json!({ "entity": "account", "where": "record.code in params.codes" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("params.codes"), "{body}");
}

#[tokio::test]
async fn dates_relative_to_today_and_days_in_the_readers_zone() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    t.draft(&t.admin, &c, "10.00").await; // dated 2026-03-15
    let count = |condition: &'static str, zone: Option<&'static str>| {
        let t = &t;
        async move {
            let mut input = json!({ "doc_type": "journal_entry", "where": condition });
            if let Some(zone) = zone {
                input["time_zone"] = json!(zone);
            }
            let (status, page) = t.command(&t.admin, "document.list", input).await;
            assert_eq!(status, StatusCode::OK, "{condition} in {zone:?}: {page}");
            page["total"].as_i64().unwrap()
        }
    };
    // The month before 10 April 2026 is March.
    let march = "record.doc_date >= startOfMonth(addMonths('2026-04-10', -1)) \
                 && record.doc_date <= endOfMonth(addMonths('2026-04-10', -1))";
    assert_eq!(count(march, None).await, 1);
    assert_eq!(count("record.doc_date >= startOfQuarter('2026-03-31') && record.doc_date <= endOfQuarter('2026-01-01')", None).await, 1);
    assert_eq!(count("record.doc_date > endOfYear('2025-06-01') && record.doc_date < startOfYear('2027-02-02')", None).await, 1);
    assert_eq!(
        count("record.doc_date == addDays('2026-03-14', 1)", None).await,
        1
    );
    // The document was made a moment ago: on or after today's midnight,
    // and not on or after tomorrow's, wherever the reader is.
    for zone in [
        None,
        Some("Pacific/Kiritimati"),
        Some("Pacific/Pago_Pago"),
        Some("Europe/Berlin"),
    ] {
        assert_eq!(
            count("record.created_at >= today()", zone).await,
            1,
            "{zone:?}"
        );
        assert_eq!(
            count("record.created_at >= addDays(today(), 1)", zone).await,
            0,
            "{zone:?}"
        );
        assert_eq!(
            count("record.doc_date <= today()", zone).await,
            1,
            "{zone:?}"
        );
    }

    let (status, body) = t
        .command(
            &t.admin,
            "document.list",
            json!({ "doc_type": "journal_entry", "where": "record.doc_date <= today()", "time_zone": "Mars/Olympus" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("time_zone"), "{body}");
}

#[tokio::test]
async fn a_document_list_filters_by_its_summary_and_respects_what_a_reader_may_see() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let item = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let mut vendors = vec![];
    for (code, name, price) in [("V1", "Acme Supplies", "2"), ("V2", "Bolt Brothers", "150")] {
        let vendor = t
            .create("vendor", json!({ "code": code, "name": name }))
            .await;
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/purchase_order",
                json!({ "company_id": c.id, "doc_date": "2026-03-15", "body": {
                    "vendor_id": vendor,
                    "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "1", "unit_price": price }],
                } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        vendors.push(vendor);
    }
    let names = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap_or_else(|| panic!("not a page: {page}"))
            .iter()
            .map(|d| {
                d["summary"]["vendor_name"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned()
            })
            .collect()
    };
    let list = |condition: &str, params: Value| json!({ "doc_type": "purchase_order", "where": condition, "params": params, "sort": ["summary.vendor_name"] });
    let cases = [
        (
            "record.summary.total_net > '100'",
            json!({}),
            vec!["Bolt Brothers"],
        ),
        (
            "record.summary.total_net >= '1' && record.summary.total_net <= '10'",
            json!({}),
            vec!["Acme Supplies"],
        ),
        (
            "record.summary.vendor_id in params.vendors",
            json!({ "vendors": [vendors[0]] }),
            vec!["Acme Supplies"],
        ),
        (
            "record.summary.vendor_name.startsWithIgnoreCase('bolt')",
            json!({}),
            vec!["Bolt Brothers"],
        ),
        (
            "record.summary.vendor_id != null && record.status == 'draft'",
            json!({}),
            vec!["Acme Supplies", "Bolt Brothers"],
        ),
    ];
    for (condition, params, expected) in cases {
        let (status, page) = t
            .command(&t.admin, "document.list", list(condition, params))
            .await;
        assert_eq!(status, StatusCode::OK, "{condition}: {page}");
        assert_eq!(names(&page), expected, "{condition}");
    }

    // A clerk who may read orders and not vendors cannot learn a vendor's
    // name by filtering on it; the figures stay usable.
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!(["purchase_order:read"]),
            Some(&c.id),
        )
        .await;
    let (status, body) = t
        .command(
            &clerk,
            "document.list",
            list("record.summary.vendor_name.startsWith('B')", json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, page) = t
        .command(
            &clerk,
            "document.list",
            json!({ "doc_type": "purchase_order", "where": "record.summary.total_net > '100'" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 1);
}
