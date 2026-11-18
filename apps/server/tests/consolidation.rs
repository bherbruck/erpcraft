//! Consolidation: a group of companies in one reporting currency,
//! translated by the current rate method, with intercompany balances and
//! trading eliminated. A report only.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use erpcraft_kernel::outbox;
use serde_json::{Value, json};

struct Side {
    id: String,
    bank: String,
    revenue: String,
    equity: String,
}

async fn company(t: &TestApp, code: &str, currency: &str) -> Side {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_small_business", "code": code, "name": format!("{code} Inc"),
                    "base_currency": currency, "first_year": 2026, "years": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let role = |name: &str| body["roles"][name]["id"].as_str().unwrap().to_owned();
    Side {
        id: body["company"]["id"].as_str().unwrap().to_owned(),
        bank: role("bank"),
        revenue: role("revenue"),
        equity: role("retained_earnings"),
    }
}

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn posted(
    t: &TestApp,
    company: &str,
    doc_type: &str,
    date: &str,
    command: &str,
    body: Value,
) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": company, "doc_date": date, "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
    run(t, command, json!({ "id": id(&draft) })).await
}

fn row<'a>(section: &'a Value, name: &str) -> &'a Value {
    section["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no row {name}: {section}"))
}

async fn report(t: &TestApp, group: String) -> Value {
    run(
        t,
        "consolidation.report",
        json!({ "group_id": group, "from": "2026-01-01", "to": "2026-03-31" }),
    )
    .await
}

#[tokio::test]
async fn a_group_is_translated_and_its_intercompany_dealings_eliminated() {
    let t = TestApp::start().await;
    let ohio = company(&t, "OHIO", "USD").await;
    let gmbh = company(&t, "GMBH", "EUR").await;
    for (date, rate) in [("2026-01-01", "1.10"), ("2026-03-01", "1.20")] {
        t.create(
            "exchange_rate",
            json!({ "from_currency": "EUR", "to_currency": "USD", "rate_date": date, "rate": rate }),
        )
        .await;
    }
    let group = t
        .create(
            "consolidation_group",
            json!({ "code": "OW", "name": "Ohio Widgets group", "currency": "USD" }),
        )
        .await;
    for member in [&ohio.id, &gmbh.id] {
        t.create(
            "consolidation_member",
            json!({ "group_id": group, "company_id": member }),
        )
        .await;
    }

    // The GmbH is founded with 1,000 EUR when a euro is 1.10 dollars.
    posted(
        &t,
        &gmbh.id,
        "journal_entry",
        "2026-01-02",
        "journal_entry.post",
        json!({ "lines": [{ "account_id": gmbh.bank, "debit": "1000" },
                          { "account_id": gmbh.equity, "credit": "1000" }] }),
    )
    .await;

    // Ohio bills the GmbH 250 USD of engineering, mirrored as its bill.
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
    for (party_type, party, company) in [
        ("customer", &customer, &gmbh.id),
        ("vendor", &vendor, &ohio.id),
    ] {
        t.create(
            "intercompany_party",
            json!({ "party_type": party_type, "party_id": party, "company_id": company }),
        )
        .await;
    }
    let services = t
        .create(
            "account",
            json!({ "company_id": gmbh.id, "code": "6950", "name": "Group services",
                    "account_type": "expense" }),
        )
        .await;
    let admin_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM app_user WHERE email = 'admin@example.com'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    t.create(
        "intercompany_rule",
        json!({ "company_id": ohio.id, "buyer_company_id": gmbh.id, "customer_id": customer,
                "vendor_id": vendor, "run_as_user_id": admin_id, "account_id": services }),
    )
    .await;
    posted(
        &t,
        &ohio.id,
        "sales_invoice",
        "2026-03-15",
        "sales_invoice.post",
        json!({ "customer_id": customer, "lines": [
            { "account_id": ohio.revenue, "description": "Engineering", "qty": "1", "unit_price": "250.00" }] }),
    )
    .await;
    let delivered = outbox::deliver_batch(&t.kernel, 500).await.unwrap();
    assert_eq!(delivered.failed, 0);

    // Before the GmbH posts its bill: the receivable stands alone.
    let before = report(&t, group.clone()).await;
    let sheet = &before["balance_sheet"];
    assert_eq!(
        sheet["total_assets"], sheet["total_liabilities_and_equity"],
        "{before}"
    );
    let balance = before["eliminations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "balance")
        .unwrap();
    assert_eq!(dec(&balance["seller_amount"]), 250.0);
    assert_eq!(dec(&balance["buyer_amount"]), 0.0);
    assert_eq!(dec(&sheet["intercompany_difference"]), 250.0);

    // The GmbH posts the mirrored bill.
    let mirror: uuid::Uuid =
        sqlx::query_scalar("SELECT mirror_id FROM intercompany_document WHERE status = 'drafted'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    run(&t, "vendor_invoice.post", json!({ "id": mirror })).await;

    let after = report(&t, group).await;
    let sheet = &after["balance_sheet"];
    assert_eq!(
        sheet["total_assets"], sheet["total_liabilities_and_equity"],
        "{after}"
    );
    // The closing rate is the rate on record on the last day.
    let gmbh_rates = after["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["code"] == "GMBH")
        .unwrap();
    assert_eq!(dec(&gmbh_rates["closing_rate"]), 1.2);
    let average = dec(&gmbh_rates["average_rate"]);
    assert!(average > 1.1 && average < 1.2, "{average}");
    // The capital came in at 1.10 and the cash is worth 1.20 now: 100.
    // The bill's cost is translated at the average rate and its payable at
    // the closing one, so the group's result (Ohio's 250 of revenue less
    // the GmbH's cost at the average) takes the rest of the 100.
    assert!(
        (dec(&sheet["cta"]) + dec(&sheet["net_income"]) - 100.0).abs() < 0.005,
        "{sheet}"
    );
    assert!(dec(&sheet["cta"]) > 80.0 && dec(&sheet["cta"]) < 100.0);
    // Both sides of the balance agree to the cent of rounding, and the
    // revenue is taken off income and cost alike.
    let eliminations = after["eliminations"].as_array().unwrap();
    let balance = eliminations
        .iter()
        .find(|e| e["kind"] == "balance")
        .unwrap();
    assert!(dec(&balance["difference"]).abs() <= 0.01, "{balance}");
    let trading = eliminations
        .iter()
        .find(|e| e["kind"] == "trading")
        .unwrap();
    assert_eq!(dec(&trading["seller_amount"]), 250.0);
    assert!(
        (dec(&trading["buyer_amount"]) - 250.0).abs() < 25.0,
        "{trading}"
    );
    let statement = &after["income_statement"];
    let cost = row(&statement["expenses"], "Intercompany cost eliminated");
    assert_eq!(dec(&cost["consolidated"]), -250.0);
    let revenue_rows: f64 = statement["income"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| dec(&r["eliminated"]))
        .sum();
    assert_eq!(revenue_rows, -250.0);
    // The group's result is the members' results, nothing taken out of it.
    let members_result: f64 = statement["income"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| dec(&r["translated"]))
        .sum::<f64>()
        - statement["expenses"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| dec(&r["translated"]))
            .sum::<f64>();
    assert!((dec(&statement["net_income"]) - members_result).abs() < 1e-9);
    // Nothing was posted by the report.
    let entries = t.count("SELECT count(*) FROM gl_entry").await;
    report(&t, after["group_id"].as_str().unwrap().to_owned()).await;
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, entries);
    t.stop().await;
}
