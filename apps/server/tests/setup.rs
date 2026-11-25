//! Setting up a company from a template, and a whole year of business in
//! it: buying, selling, tax, paying and being paid, and the year-end close.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

async fn setup(t: &TestApp, input: Value) -> (StatusCode, Value) {
    t.command(&t.admin, "company.setup", input).await
}

/// A company set up from `template` for 2026 and 2027.
async fn company(t: &TestApp, template: &str, code: &str) -> Value {
    let (status, body) = setup(
        t,
        json!({ "template": template, "code": code, "name": format!("{code} GmbH"),
                "first_year": 2026, "years": 2 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

/// Draft a document and run `command` on it.
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

/// Pay what is open on `invoice` but `short`, out or in, from the
/// company's bank.
async fn pay(
    t: &TestApp,
    company: &str,
    bank: &Value,
    invoice: &Value,
    kind: &str,
    short: f64,
) -> Value {
    let open = run(
        t,
        "open_item.list",
        json!({ "company_id": company, "kind": kind }),
    )
    .await;
    let item = open["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["document_id"] == invoice["id"])
        .unwrap()
        .clone();
    let direction = if kind == "payable" {
        "outgoing"
    } else {
        "incoming"
    };
    posted(
        t,
        company,
        "payment",
        "payment.post",
        json!({ "direction": direction, "bank_account_id": bank,
                "allocations": [{ "open_item_id": item["id"],
                                  "amount": format!("{:.2}", dec(&item["open"]) - short) }] }),
    )
    .await
}

/// A year of business in a company set up from a template, then the
/// books are checked against each other.
async fn a_year_in(template: &str, purchase_tax: &str, sales_tax: &str, expected: Expected) {
    let t = TestApp::start().await;
    let created = company(&t, template, "ACME").await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let role = |name: &str| created["roles"][name]["id"].clone();
    let code = |name: &str| created["roles"][name]["code"].as_str().unwrap().to_owned();
    // A period a month, twelve to a fiscal year.
    let months: Vec<String> = (2026..=2027)
        .flat_map(|y| (1..=12).map(move |m| format!("{y}-{m:02}")))
        .collect();
    assert_eq!(created["fiscal_periods"], json!(months));
    let years = created["fiscal_years"].as_array().unwrap().clone();
    assert_eq!(years.len(), 2, "{created}");
    assert_eq!(years[0]["name"], "FY2026");
    assert_eq!(years[1]["name"], "FY2027");
    let warehouse = created["warehouse_id"].clone();
    assert!(warehouse.is_string(), "{created}");
    assert_eq!(created["notes"], json!([]));

    let item = t
        .create(
            "item",
            json!({ "code": "WIDGET", "name": "Widget", "uom": "pcs" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C001", "name": "Buyer Ltd" }))
        .await;
    let tax_code = |code: &str| {
        let t = &t;
        let code = code.to_owned();
        async move {
            let (_, list) = t
                .get(&t.admin, &format!("/api/entities/tax_code?code={code}"))
                .await;
            list["items"][0]["id"].clone()
        }
    };
    let (purchase_tax, sales_tax) = (tax_code(purchase_tax).await, tax_code(sales_tax).await);
    assert!(purchase_tax.is_string() && sales_tax.is_string());

    // Buy 10 at 4.00, receive them, be invoiced with tax, pay.
    let order = posted(
        &t,
        &c,
        "purchase_order",
        "purchase_order.approve",
        json!({ "vendor_id": vendor, "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "10", "unit_price": "4.00" }] }),
    )
    .await;
    let line = order["body"]["lines"][0]["id"].clone();
    posted(
        &t,
        &c,
        "purchase_receipt",
        "purchase_receipt.post",
        json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10" }] }),
    )
    .await;
    let bill = posted(
        &t,
        &c,
        "vendor_invoice",
        "vendor_invoice.post",
        json!({ "purchase_order_id": order["id"],
                "lines": [{ "order_line_id": line, "qty": "10", "unit_price": "4.00", "tax_code_id": purchase_tax }] }),
    )
    .await;
    pay(&t, &c, &role("bank"), &bill, "payable", 0.0).await;

    // Sell 4 at 9.00, deliver, invoice with tax, be paid.
    let order = posted(
        &t,
        &c,
        "sales_order",
        "sales_order.approve",
        json!({ "customer_id": customer, "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "4", "unit_price": "9.00" }] }),
    )
    .await;
    let line = order["body"]["lines"][0]["id"].clone();
    posted(
        &t,
        &c,
        "sales_delivery",
        "sales_delivery.post",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "4" }] }),
    )
    .await;
    let invoice = posted(
        &t,
        &c,
        "sales_invoice",
        "sales_invoice.post",
        json!({ "sales_order_id": order["id"],
                "lines": [{ "order_line_id": line, "qty": "4", "unit_price": "9.00", "tax_code_id": sales_tax }] }),
    )
    .await;
    // The customer pays all but 10.00, so that something stays owed.
    pay(&t, &c, &role("bank"), &invoice, "receivable", 10.0).await;

    // Close 2026, its twelve months at once: the result goes to retained
    // earnings.
    let (status, close) = t
        .post(
            &t.admin,
            "/api/documents/year_end_close",
            json!({ "company_id": c, "doc_date": "2026-12-31", "body": {
                "fiscal_period_id": years[0]["last_period_id"],
                "first_period_id": years[0]["first_period_id"],
                "retained_earnings_account_id": role("retained_earnings"),
            } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{close}");
    run(&t, "year_end_close.post", json!({ "id": id(&close) })).await;

    // The books.
    let tb = run(&t, "gl.trial_balance", json!({ "company_id": c })).await;
    assert_eq!(tb["total_debit"], tb["total_credit"], "{tb}");
    let balance = |code: &str| {
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["code"] == code)
            .map_or(0.0, |r| dec(&r["balance"]))
    };
    let stock = run(&t, "stock.on_hand", json!({ "company_id": c })).await;
    assert_eq!(dec(&stock["total_value"]), balance(&code("inventory")));
    assert_eq!(balance(&code("inventory")), 24.0);
    assert_eq!(balance(&code("grni")), 0.0);
    for (kind, control, sign) in [
        ("payable", "payable", -1.0),
        ("receivable", "receivable", 1.0),
    ] {
        let open = run(
            &t,
            "open_item.list",
            json!({ "company_id": c, "kind": kind }),
        )
        .await;
        assert_eq!(
            dec(&open["total_open"]),
            sign * balance(&code(control)),
            "{kind}"
        );
    }
    let tax = run(
        &t,
        "tax.report",
        json!({ "company_id": c, "from": "2026-01-01", "to": "2027-12-31" }),
    )
    .await;
    assert_eq!(dec(&tax["total_paid"]), expected.tax_paid, "{tax}");
    assert_eq!(
        dec(&tax["total_collected"]),
        expected.tax_collected,
        "{tax}"
    );
    assert_eq!(balance(expected.paid_account), expected.tax_paid);
    assert_eq!(-balance(expected.collected_account), expected.tax_collected);
    // 36.00 of revenue less 16.00 of cost: 20.00 of profit, now retained.
    assert_eq!(balance(&code("revenue")), 0.0);
    assert_eq!(balance(&code("cogs")), 0.0);
    assert_eq!(balance(&code("retained_earnings")), -20.0);
    assert!(
        (balance(&code("bank"))
            - (-(40.0 + expected.tax_paid) + 36.0 + expected.tax_collected - 10.0))
            .abs()
            < 0.001
    );
    assert_eq!(balance(&code("receivable")), 10.0);
    let sheet = run(&t, "gl.balance_sheet", json!({ "company_id": c })).await;
    assert_eq!(
        sheet["total_assets"], sheet["total_liabilities_and_equity"],
        "{sheet}"
    );
    t.stop().await;
}

struct Expected {
    tax_paid: f64,
    tax_collected: f64,
    paid_account: &'static str,
    collected_account: &'static str,
}

#[tokio::test]
async fn a_us_small_business_runs_a_year() {
    a_year_in(
        "us_small_business",
        "US-SALES-TAX",
        "US-SALES-TAX",
        Expected {
            tax_paid: 0.0,
            tax_collected: 0.0,
            paid_account: "6130",
            collected_account: "2100",
        },
    )
    .await;
}

#[tokio::test]
async fn a_german_company_on_skr03_runs_a_year() {
    // Bought at 19 % (Vorsteuer 1576), sold at 7 % (Umsatzsteuer 1771).
    a_year_in(
        "de_skr03",
        "DE-USt19",
        "DE-USt7",
        Expected {
            tax_paid: 7.6,
            tax_collected: 2.52,
            paid_account: "1576",
            collected_account: "1771",
        },
    )
    .await;
}

#[tokio::test]
async fn a_canadian_company_runs_a_year() {
    // Ontario: HST 13 % both ways, claimed back as input tax credits.
    a_year_in(
        "ca_small_business",
        "CA-HST-ON",
        "CA-HST-ON",
        Expected {
            tax_paid: 5.2,
            tax_collected: 4.68,
            paid_account: "1350",
            collected_account: "2100",
        },
    )
    .await;
}

#[tokio::test]
async fn a_us_manufacturer_runs_a_year() {
    a_year_in(
        "us_manufacturer",
        "US-SALES-TAX",
        "US-SALES-TAX",
        Expected {
            tax_paid: 0.0,
            tax_collected: 0.0,
            paid_account: "6130",
            collected_account: "2100",
        },
    )
    .await;
}

#[tokio::test]
async fn a_german_manufacturer_on_skr03_runs_a_year() {
    a_year_in(
        "de_skr03_manufacturer",
        "DE-USt19",
        "DE-USt7",
        Expected {
            tax_paid: 7.6,
            tax_collected: 2.52,
            paid_account: "1576",
            collected_account: "1771",
        },
    )
    .await;
}

#[tokio::test]
async fn a_canadian_manufacturer_runs_a_year() {
    a_year_in(
        "ca_manufacturer",
        "CA-HST-ON",
        "CA-HST-ON",
        Expected {
            tax_paid: 5.2,
            tax_collected: 4.68,
            paid_account: "1350",
            collected_account: "2100",
        },
    )
    .await;
}

/// A manufacturer's template sets up every account a manufacturer posts
/// to, and the setup returns each role.
#[tokio::test]
async fn a_manufacturer_template_has_the_roles_making_needs() {
    let t = TestApp::start().await;
    for (template, code) in [
        ("us_manufacturer", "M1"),
        ("ca_manufacturer", "M2"),
        ("de_skr03_manufacturer", "M3"),
    ] {
        let (status, body) = setup(
            &t,
            json!({ "template": template, "code": code, "name": code, "first_year": 2026 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let company = body["company"]["id"].clone();
        for role in [
            "inventory",
            "finished_goods",
            "inventory_in_transit",
            "absorbed_cost",
            "production_variance",
            "write_off",
            "fx_unrealized_gain",
            "fx_unrealized_loss",
            "fx_gain",
            "fx_loss",
        ] {
            assert!(
                body["roles"][role]["id"].is_string(),
                "{template} {role}: {body}"
            );
        }
        assert_ne!(
            body["roles"]["inventory"]["id"],
            body["roles"]["finished_goods"]["id"]
        );
        // Each role but finished goods is the company's rule for its purpose.
        for purpose in [
            "inventory_in_transit",
            "absorbed_cost",
            "production_variance",
            "write_off",
            "fx_unrealized_gain",
            "fx_unrealized_loss",
        ] {
            let account: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT account_id FROM account_rule WHERE company_id = $1::text::uuid AND purpose = $2 AND scope_type IS NULL",
            )
            .bind(company.as_str().unwrap())
            .bind(purpose)
            .fetch_optional(&t.pool)
            .await
            .unwrap();
            assert_eq!(
                account.map(|a| a.to_string()),
                body["roles"][purpose]["id"].as_str().map(str::to_owned),
                "{template} {purpose}"
            );
        }
    }
    t.stop().await;
}

#[tokio::test]
async fn templates_are_listed_and_the_skr03_chart_is_as_datev_numbers_it() {
    let t = TestApp::start().await;
    let list = run(&t, "setup.templates", json!({})).await;
    let codes: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    for code in [
        "us_small_business",
        "ca_small_business",
        "de_skr03",
        "us_manufacturer",
        "ca_manufacturer",
        "de_skr03_manufacturer",
    ] {
        assert!(codes.contains(&code), "{code}");
    }
    // Every Canadian GST/HST code, at the rate the CRA publishes.
    let canada = run(&t, "setup.templates", json!({ "country": "CA" })).await;
    assert_eq!(
        canada["items"][0]["tax_codes"],
        json!([
            "CA-GST",
            "CA-HST-ON",
            "CA-HST-NB",
            "CA-HST-NL",
            "CA-HST-NS",
            "CA-HST-PE",
            "CA-ZERO",
            "CA-EXEMPT",
            "CA-PST-BC-SA",
            "CA-PST-SK-SA",
            "CA-RST-MB-SA",
            "CA-PST-BC",
            "CA-PST-SK",
            "CA-RST-MB",
            "CA-QST",
            "CA-GST-PST-BC",
            "CA-GST-PST-SK",
            "CA-GST-RST-MB",
            "CA-GST-QST"
        ])
    );
    let german = run(&t, "setup.templates", json!({ "country": "de" })).await;
    assert!(
        german["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["country"] == "DE")
    );
    assert_eq!(german["items"][0]["currency"], "EUR");

    let created = company(&t, "de_skr03", "DE1").await;
    let c = created["company"]["id"].as_str().unwrap();
    assert_eq!(created["company"]["base_currency"], "EUR");
    let roles: Vec<(&str, &str)> = [
        ("bank", "1200"),
        ("receivable", "1400"),
        ("payable", "1600"),
        ("inventory", "3980"),
        ("revenue", "8400"),
        ("cogs", "3200"),
        ("retained_earnings", "0860"),
    ]
    .to_vec();
    for (role, code) in roles {
        assert_eq!(created["roles"][role]["code"], code, "{role}");
    }
    let accounts = run(
        &t,
        "entity.list",
        json!({ "entity": "account", "filters": { "company_id": c }, "limit": 500 }),
    )
    .await;
    let name = |code: &str| {
        accounts["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["code"] == code)
            .map(|a| a["name"].as_str().unwrap().to_owned())
            .unwrap_or_default()
    };
    assert_eq!(name("1576"), "Abziehbare Vorsteuer 19 %");
    assert_eq!(name("1571"), "Abziehbare Vorsteuer 7 %");
    assert_eq!(name("1776"), "Umsatzsteuer 19 %");
    assert_eq!(name("1771"), "Umsatzsteuer 7 %");
    assert_eq!(
        name("8125"),
        "Steuerfreie innergemeinschaftliche Lieferungen nach § 4 Nr. 1b UStG"
    );
    assert!(name("8120").starts_with("Steuerfreie Umsätze nach § 4 Nr. 1a UStG"));
    let settings = run(
        &t,
        "entity.list",
        json!({ "entity": "tax_settings", "filters": { "company_id": c } }),
    )
    .await;
    assert_eq!(settings["total"], 4);
    // The tax-free sales say what they are on an e-invoice.
    let categories: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT code, vat_category, exemption_reason_code FROM tax_code
         WHERE code LIKE 'DE-%' ORDER BY code",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        categories,
        vec![
            (
                "DE-AUSFUHR".into(),
                Some("G".into()),
                Some("VATEX-EU-G".into())
            ),
            (
                "DE-IGL".into(),
                Some("K".into()),
                Some("VATEX-EU-IC".into())
            ),
            ("DE-USt19".into(), Some("S".into()), None),
            ("DE-USt7".into(), Some("S".into()), None),
        ]
    );

    // A bank account of one's own, and a fiscal year from July.
    let (status, body) = setup(
        &t,
        json!({ "template": "us_small_business", "code": "US2", "name": "US Two",
                "first_year": 2026, "start_month": 7,
                "bank_account_code": "1025", "bank_account_name": "First National checking" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["roles"]["bank"]["code"], "1025");
    assert_eq!(body["fiscal_periods"][0], "2026-07");
    assert_eq!(body["fiscal_periods"][11], "2027-06");
    assert_eq!(body["fiscal_years"][0]["name"], "FY2026/27");

    // Or one period a year, for a company that closes only the year.
    let (status, body) = setup(
        &t,
        json!({ "template": "us_small_business", "code": "US3", "name": "US Three",
                "first_year": 2026, "years": 2, "periods": "yearly" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fiscal_periods"], json!(["FY2026", "FY2027"]));
    assert_eq!(
        body["fiscal_years"][0]["first_period_id"],
        body["fiscal_years"][0]["last_period_id"]
    );
    t.stop().await;
}

#[tokio::test]
async fn a_setup_that_cannot_finish_leaves_nothing() {
    let t = TestApp::start().await;
    let companies = "SELECT count(*) FROM company";
    let accounts = "SELECT count(*) FROM account";

    let (status, body) = setup(
        &t,
        json!({ "template": "fr_pcg", "code": "X", "name": "X", "first_year": 2026 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "template");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("de_skr03")
    );

    company(&t, "us_small_business", "ACME").await;
    let (before_companies, before_accounts) = (t.count(companies).await, t.count(accounts).await);
    // The same code again.
    let (status, body) = setup(
        &t,
        json!({ "template": "de_skr03", "code": "ACME", "name": "Again", "first_year": 2026 }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // A tax code that exists with another rate: refused after the company
    // and its chart were written, so all of it is rolled back.
    t.create(
        "tax_code",
        json!({ "code": "DE-USt7", "name": "Old reduced rate", "rate": "5" }),
    )
    .await;
    let (status, body) = setup(
        &t,
        json!({ "template": "de_skr03", "code": "DE1", "name": "DE", "first_year": 2026 }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("DE-USt7")
    );
    assert_eq!(t.count(companies).await, before_companies);
    assert_eq!(t.count(accounts).await, before_accounts);
    assert_eq!(
        t.count("SELECT count(*) FROM tax_code WHERE code = 'DE-USt19'")
            .await,
        0
    );
    for bad in [
        json!({ "years": 0 }),
        json!({ "years": 6 }),
        json!({ "start_month": 13 }),
    ] {
        let mut input = json!({ "template": "us_small_business", "code": "Y", "name": "Y", "first_year": 2026 });
        input
            .as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        let (status, body) = setup(&t, input).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {body}");
    }
    t.stop().await;
}

#[tokio::test]
async fn setting_up_takes_the_permission_for_all_companies() {
    let t = TestApp::start().await;
    let c = t.company("HQ", "USD").await;
    let creates = [
        "company:create",
        "account:create",
        "fiscal_period:create",
        "warehouse:create",
        "purchasing_settings:create",
        "sales_settings:create",
        "account_rule:create",
        "tax_code:create",
        "tax_code_component:create",
        "tax_settings:create",
        "payment_terms:create",
        "asset_class:create",
    ];
    let input = json!({ "template": "us_small_business", "code": "NEW", "name": "New", "first_year": 2026 });

    // Every record it makes, but not the setup itself.
    let (clerk, _) = t
        .user_with_rules("clerk@example.com", json!(creates), None)
        .await;
    let (status, body) = t.command(&clerk, "company.setup", input.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // The setup for one company only.
    let mut rules = creates.to_vec();
    rules.push("company:setup");
    let (one, _) = t
        .user_with_rules("one@example.com", json!(rules), Some(&c.id))
        .await;
    let (status, body) = t.command(&one, "company.setup", input.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // The setup, but not the right to create what it creates.
    let (setter, _) = t
        .user_with_rules("setter@example.com", json!(["company:setup"]), None)
        .await;
    let (status, body) = t.command(&setter, "company.setup", input.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // Both, for all companies.
    let (admin_like, _) = t
        .user_with_rules("setup@example.com", json!(rules), None)
        .await;
    let (status, body) = t.command(&admin_like, "company.setup", input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        t.count("SELECT count(*) FROM company WHERE code = 'NEW'")
            .await,
        1
    );
    // The audit log names what was created, under the one command.
    assert!(
        t.count(
            "SELECT count(*) FROM audit_subject s JOIN audit_log l ON l.id = s.audit_id
             WHERE l.command = 'company.setup' AND s.resource = 'account'"
        )
        .await
            >= 40
    );
    t.stop().await;
}

#[tokio::test]
async fn a_module_adds_a_template_of_its_own() {
    let t = TestApp::start().await;
    let mut template: Value = serde_json::from_str(include_str!(
        "../../../modules/setup/src/templates/us_small_business.json"
    ))
    .unwrap();
    template["code"] = json!("zz_test");
    template["name"] = json!("A country of a module's own (test)");
    template["country"] = json!("ZZ");
    template["tax_codes"] = json!([{ "code": "ZZ-VAT-TEST", "name": "VAT", "rate": "5",
                                      "collected": "2100", "paid": "6130" }]);
    erpcraft_setup::register(
        erpcraft_setup::SetupTemplate::from_json(&template.to_string()).unwrap(),
    )
    .unwrap();
    // A code is registered once.
    assert!(
        erpcraft_setup::register(
            erpcraft_setup::SetupTemplate::from_json(&template.to_string()).unwrap()
        )
        .is_err()
    );
    let created = company(&t, "zz_test", "ZZ1").await;
    assert_eq!(created["template"], "zz_test");
    assert_eq!(created["tax_codes"], json!(["ZZ-VAT-TEST"]));
    t.stop().await;
}

#[tokio::test]
async fn an_existing_tax_code_is_reused_only_if_it_is_the_same_tax() {
    let t = TestApp::start().await;
    // The same code and rate, but compounding: another tax.
    t.create(
        "tax_code",
        json!({ "code": "DE-USt7", "name": "Reduced", "rate": "7", "compound": true }),
    )
    .await;
    let (status, body) = setup(
        &t,
        json!({ "template": "de_skr03", "code": "DE1", "name": "DE", "first_year": 2026 }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("DE-USt7") && message.contains("compounding true"),
        "{message}"
    );

    // Someone who may not read tax codes learns only that it differs.
    let (setter, _) = t
        .user_with_rules(
            "setter@example.com",
            json!([
                "company:setup",
                "company:create",
                "account:create",
                "fiscal_period:create",
                "warehouse:create",
                "purchasing_settings:create",
                "sales_settings:create",
                "account_rule:create",
                "tax_code:create",
                "tax_settings:create"
            ]),
            None,
        )
        .await;
    let (status, body) = t
        .command(
            &setter,
            "company.setup",
            json!({ "template": "de_skr03", "code": "DE2", "name": "DE", "first_year": 2026 }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("DE-USt7"), "{message}");
    assert!(
        !message.contains("compounding") && !message.contains("rate"),
        "{message}"
    );
    t.stop().await;
}
