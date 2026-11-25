//! Tax by destination: the code a line starts with chosen by where the
//! goods go, from data. US sales tax by state, county and post code with
//! nexus per state and exemption certificates; Canada's GST, HST, PST and
//! QST by province of supply with PST self-assessed on purchases; the EU
//! One-Stop Shop past the distance-selling threshold. At every step the
//! tax report equals the tax accounts.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

struct Books {
    company: String,
    accounts: BTreeMap<String, String>,
    revenue: String,
}

async fn set_up(t: &TestApp, template: &str, code: &str) -> Books {
    let created = run(
        t,
        "company.setup",
        json!({ "template": template, "code": code, "name": format!("{code} Ltd"),
                "first_year": 2026 }),
    )
    .await;
    let company = created["company"]["id"].as_str().unwrap().to_owned();
    let (_, list) = t
        .get(
            &t.admin,
            &format!("/api/entities/account?company_id={company}&limit=500"),
        )
        .await;
    let accounts = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["code"].as_str().unwrap().to_owned(),
                a["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    Books {
        company,
        accounts,
        revenue: created["roles"]["revenue"]["id"]
            .as_str()
            .unwrap()
            .to_owned(),
    }
}

async fn tax_code(t: &TestApp, code: &str) -> String {
    let (_, list) = t
        .get(
            &t.admin,
            &format!("/api/entities/tax_code?code={}", code.replace('+', "%2B")),
        )
        .await;
    list["items"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no tax code {code}: {list}"))
        .to_owned()
}

/// A customer or vendor at an address; with a VAT identifier when given.
async fn party(
    t: &TestApp,
    kind: &str,
    code: &str,
    (country, region, postcode, city): (&str, Option<&str>, &str, &str),
    vat: Option<&str>,
) -> String {
    let id = t
        .create(kind, json!({ "code": code, "name": format!("{code} Co") }))
        .await;
    let mut input = json!({
        "party_type": kind, "party_id": id,
        "addresses": [{ "kind": "billing", "is_default": true, "street": "1 Main St",
                        "city": city, "postcode": postcode, "region": region,
                        "country_code": country }],
    });
    if let Some(vat) = vat {
        input["tax_ids"] = json!([{ "scheme": "eu_vat", "value": vat, "country_code": country }]);
    }
    run(t, "party.set", input).await;
    id
}

impl Books {
    fn account(&self, code: &str) -> &str {
        self.accounts
            .get(code)
            .unwrap_or_else(|| panic!("no account {code}"))
    }

    /// Draft a direct sales invoice of one line of `amount` on the revenue
    /// account; the draft as saved.
    async fn draft_sale(&self, t: &TestApp, customer: &str, date: &str, amount: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/sales_invoice",
                json!({ "company_id": self.company, "doc_date": date, "body": {
                    "customer_id": customer,
                    "lines": [{ "account_id": self.revenue, "description": "Goods",
                                "qty": "1", "unit_price": amount }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        draft
    }

    /// Sell for `amount` and post; the posted invoice.
    async fn sell(&self, t: &TestApp, customer: &str, date: &str, amount: &str) -> Value {
        let draft = self.draft_sale(t, customer, date, amount).await;
        run(t, "sales_invoice.post", json!({ "id": id(&draft) })).await
    }

    /// Buy for `amount`, booked to `expense`, and post.
    async fn buy(&self, t: &TestApp, vendor: &str, expense: &str, amount: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/vendor_invoice",
                json!({ "company_id": self.company, "doc_date": "2026-03-12", "body": {
                    "vendor_id": vendor,
                    "lines": [{ "account_id": self.account(expense), "description": "Supplies",
                                "qty": "1", "unit_price": amount }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(t, "vendor_invoice.post", json!({ "id": id(&draft) })).await
    }

    /// The balances by account code, the books checked to balance.
    async fn balances(&self, t: &TestApp) -> BTreeMap<String, f64> {
        let tb = run(t, "gl.trial_balance", json!({ "company_id": self.company })).await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["code"].as_str().unwrap().to_owned(), dec(&r["balance"])))
            .collect()
    }

    /// The tax report for 2026 equals the tax accounts: what was collected
    /// is the credit balance of `collected`, what was paid the debit
    /// balance of `paid`. Returns the report (by place).
    async fn report_ties(&self, t: &TestApp, collected: &[&str], paid: &[&str]) -> Value {
        let balances = self.balances(t).await;
        let at = |code: &&str| balances.get(*code).copied().unwrap_or(0.0);
        let report = run(
            t,
            "tax.report",
            json!({ "company_id": self.company, "from": "2026-01-01", "to": "2026-12-31",
                    "by_place": true }),
        )
        .await;
        let collected_books: f64 = -collected.iter().map(at).sum::<f64>();
        let paid_books: f64 = paid.iter().map(at).sum();
        assert!(
            close(dec(&report["total_collected"]), collected_books),
            "tax collected {} equals the accounts {collected_books}: {report}",
            report["total_collected"]
        );
        assert!(
            close(dec(&report["total_paid"]), paid_books),
            "tax paid {} equals the accounts {paid_books}: {report}",
            report["total_paid"]
        );
        report
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// The tax entries of a posted document: (code, place, base, amount).
async fn entries(t: &TestApp, document: &Value) -> Vec<(String, String, f64, f64)> {
    let (_, doc) = t
        .get(
            &t.admin,
            &format!("/api/documents/sales_invoice/{}", id(document)),
        )
        .await;
    let doc = if doc["postings"].is_array() {
        doc
    } else {
        document.clone()
    };
    let mut out = vec![];
    for posting in doc["postings"].as_array().unwrap() {
        for entry in posting["entries"]["tax"].as_array().into_iter().flatten() {
            out.push((
                entry["code"].as_str().unwrap_or_default().to_owned(),
                format!(
                    "{}-{}",
                    entry["country_code"].as_str().unwrap_or("?"),
                    entry["region"].as_str().unwrap_or("")
                ),
                dec(&entry["base"]),
                dec(&entry["amount"]),
            ));
        }
    }
    out
}

fn line_tax(invoice: &Value) -> f64 {
    dec(&invoice["body"]["lines"][0]["tax_amount"])
}

#[tokio::test]
async fn us_sales_tax_follows_the_ship_to_where_the_company_has_nexus() {
    let t = TestApp::start().await;
    let us = set_up(&t, "us_small_business", "USA").await;

    // A rate table: Franklin County (post codes 432xx) charges the state's
    // tax and its own; the rest of Ohio the state's; California its own.
    // The rates are made up for the test.
    let csv = "country,region,postcode_from,postcode_to,code,name,rate,authority,source\n\
               US,OH,43200,43299,US-OH-T,Ohio (test),5.75,Ohio Department of Taxation,test\n\
               US,OH,43200,43299,US-OH-T-049,Franklin County (test),1.25,Franklin County,test\n\
               US,OH,,,US-OH-T,Ohio (test),5.75,,test\n\
               US,CA,,,US-CA-T,California (test),7.25,,test\n";
    let imported = run(
        &t,
        "tax_area.import",
        json!({ "csv": csv, "company_id": us.company,
                "collected_account_id": us.account("2100"), "paid_account_id": us.account("6130") }),
    )
    .await;
    assert_eq!(imported["codes_created"], 3, "{imported}");
    assert_eq!(imported["groups_created"], 1, "{imported}");
    assert_eq!(imported["areas_created"], 3, "{imported}");
    assert_eq!(imported["settings_created"], 3, "{imported}");
    // Imported again: nothing changes.
    let again = run(&t, "tax_area.import", json!({ "csv": csv })).await;
    assert_eq!(again["areas_unchanged"], 3, "{again}");
    assert_eq!(again["codes_created"], 0, "{again}");
    // A rate change is a new code, not a changed one.
    let (status, body) = t
        .command(
            &t.admin,
            "tax_area.import",
            json!({ "csv": "country,region,code,rate\nUS,CA,US-CA-T,7.5\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("rate change"), "{body}");

    // Nexus in Ohio only.
    t.create(
        "tax_registration",
        json!({ "company_id": us.company, "country_code": "US", "region": "OH",
                "number": "OH-123" }),
    )
    .await;

    let columbus = party(
        &t,
        "customer",
        "COL",
        ("US", Some("OH"), "43215-1234", "Columbus"),
        None,
    )
    .await;
    let cleveland = party(
        &t,
        "customer",
        "CLE",
        ("US", Some("OH"), "44114", "Cleveland"),
        None,
    )
    .await;
    let los_angeles = party(
        &t,
        "customer",
        "LAX",
        ("US", Some("CA"), "90001", "Los Angeles"),
        None,
    )
    .await;
    let reseller = party(
        &t,
        "customer",
        "RES",
        ("US", Some("OH"), "43230", "Gahanna"),
        None,
    )
    .await;

    // Franklin County: state and county.
    let sale = us.sell(&t, &columbus, "2026-03-10", "100.00").await;
    assert!(close(line_tax(&sale), 7.0), "{sale}");
    let mut got = entries(&t, &sale).await;
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("US-OH-T".into(), "US-OH".into(), 100.0, 5.75),
            ("US-OH-T-049".into(), "US-OH".into(), 100.0, 1.25),
        ],
        "{sale}"
    );
    // Elsewhere in Ohio: the state alone.
    let sale = us.sell(&t, &cleveland, "2026-03-10", "100.00").await;
    assert!(close(line_tax(&sale), 5.75), "{sale}");
    // No nexus in California: nothing by destination, and no default.
    let draft = us
        .draft_sale(&t, &los_angeles, "2026-03-10", "100.00")
        .await;
    assert!(
        draft["body"]["lines"][0]["tax_code_id"].is_null(),
        "{draft}"
    );

    // A resale certificate for Ohio makes the line exempt, with its reason.
    let certificate_code = tax_code(&t, "US-EXEMPT-CERT").await;
    let certificate = t
        .create(
            "tax_exemption",
            json!({ "company_id": us.company, "customer_id": reseller, "country_code": "US",
                    "region": "OH", "certificate_number": "STEC-B 0001", "reason": "Resale",
                    "valid_from": "2026-01-01", "expires_on": "2026-03-31",
                    "tax_code_id": certificate_code }),
        )
        .await;
    let sale = us.sell(&t, &reseller, "2026-03-11", "250.00").await;
    assert_eq!(
        sale["body"]["lines"][0]["tax_code_id"],
        json!(certificate_code)
    );
    assert!(close(line_tax(&sale), 0.0), "{sale}");
    // After it expires the county's tax is charged again.
    let sale = us.sell(&t, &reseller, "2026-04-02", "100.00").await;
    assert!(close(line_tax(&sale), 7.0), "{sale}");
    // A draft made under the certificate is refused once it has expired
    // by the posting date.
    let draft = us.draft_sale(&t, &reseller, "2026-03-25", "80.00").await;
    assert_eq!(
        draft["body"]["lines"][0]["tax_code_id"],
        json!(certificate_code)
    );
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/tax_exemption/{certificate}"),
            json!({ "expires_on": "2026-03-20" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = t
        .command(&t.admin, "sales_invoice.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("exemption certificate"), "{body}");

    let exempt = run(
        &t,
        "tax.exemptions",
        json!({ "company_id": us.company, "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert_eq!(exempt["rows"].as_array().unwrap().len(), 1, "{exempt}");
    assert_eq!(exempt["rows"][0]["certificate_number"], "STEC-B 0001");
    assert_eq!(exempt["rows"][0]["reason"], "Resale");
    assert_eq!(exempt["rows"][0]["region"], "OH");
    assert!(close(dec(&exempt["total_base"]), 250.0), "{exempt}");

    // A reversal is for the place of the sale.
    let reversed = us.sell(&t, &columbus, "2026-03-15", "10.00").await;
    run(&t, "sales_invoice.reverse", json!({ "id": id(&reversed) })).await;

    let report = us.report_ties(&t, &["2100", "2110"], &["6130"]).await;
    let ohio: f64 = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["region"] == "OH")
        .map(|r| dec(&r["collected"]))
        .sum();
    // 7.00 + 5.75 + 7.00 on the sales that stood.
    assert!(close(ohio, 19.75), "{report}");
    t.stop().await;
}

#[tokio::test]
async fn canada_charges_by_province_of_supply_and_self_assesses_pst() {
    let t = TestApp::start().await;
    let ca = set_up(&t, "ca_small_business", "CAN").await;
    // The company is in Vancouver.
    run(
        &t,
        "party.set",
        json!({ "party_type": "company", "party_id": ca.company,
                "addresses": [{ "kind": "billing", "is_default": true, "city": "Vancouver",
                                "postcode": "V6B 1A1", "region": "BC", "country_code": "CA" }] }),
    )
    .await;
    let toronto = party(
        &t,
        "customer",
        "TOR",
        ("CA", Some("ON"), "M5V 2T6", "Toronto"),
        None,
    )
    .await;
    let victoria = party(
        &t,
        "customer",
        "VIC",
        ("CA", Some("BC"), "V8W 1P6", "Victoria"),
        None,
    )
    .await;
    let montreal = party(
        &t,
        "customer",
        "MTL",
        ("CA", Some("QC"), "H2Y 1C6", "Montréal"),
        None,
    )
    .await;
    let calgary = party(
        &t,
        "customer",
        "YYC",
        ("CA", Some("AB"), "T2P 1J9", "Calgary"),
        None,
    )
    .await;

    // Registered for GST/HST only (the template's registration).
    let sale = ca.sell(&t, &toronto, "2026-03-10", "100.00").await;
    assert!(close(line_tax(&sale), 13.0), "HST in Ontario: {sale}");
    assert_eq!(
        sale["body"]["lines"][0]["tax_code_id"],
        json!(tax_code(&t, "CA-HST-ON").await)
    );
    let sale = ca.sell(&t, &victoria, "2026-03-10", "100.00").await;
    assert!(
        close(line_tax(&sale), 5.0),
        "GST only without a PST registration: {sale}"
    );
    let sale = ca.sell(&t, &montreal, "2026-03-10", "100.00").await;
    assert!(
        close(line_tax(&sale), 5.0),
        "GST only without a QST registration: {sale}"
    );
    let sale = ca.sell(&t, &calgary, "2026-03-10", "100.00").await;
    assert!(close(line_tax(&sale), 5.0), "GST in Alberta: {sale}");

    // Registered for British Columbia PST: GST and PST together.
    t.create(
        "tax_registration",
        json!({ "company_id": ca.company, "country_code": "CA", "region": "BC",
                "number": "PST-1234-5678" }),
    )
    .await;
    let sale = ca.sell(&t, &victoria, "2026-03-11", "100.00").await;
    assert!(close(line_tax(&sale), 12.0), "{sale}");
    assert_eq!(
        sale["body"]["lines"][0]["tax_code_id"],
        json!(tax_code(&t, "CA-GST-PST-BC").await)
    );

    // Bought in British Columbia from a British Columbia vendor: it charges
    // GST and PST.
    let local = party(
        &t,
        "vendor",
        "VAN-SUP",
        ("CA", Some("BC"), "V5K 0A1", "Vancouver"),
        None,
    )
    .await;
    let bought = ca.buy(&t, &local, "6050", "200.00").await;
    assert!(close(line_tax(&bought), 24.0), "{bought}");
    // From an Ontario vendor: GST on the invoice, the PST self-assessed.
    let away = party(
        &t,
        "vendor",
        "TOR-SUP",
        ("CA", Some("ON"), "M5V 2T6", "Toronto"),
        None,
    )
    .await;
    let bought = ca.buy(&t, &away, "6050", "100.00").await;
    assert!(
        close(line_tax(&bought), 5.0),
        "the vendor charges the GST only: {bought}"
    );
    assert_eq!(
        bought["body"]["lines"][0]["tax_code_id"],
        json!(tax_code(&t, "CA-GST+CA-PST-BC-SA").await),
        "{bought}"
    );
    let balances = ca.balances(&t).await;
    // PST paid: 14.00 charged by the local vendor and 7.00 self-assessed.
    assert!(close(balances["6130"], 21.0), "{balances:?}");
    // PST owed: 7.00 on the sale to Victoria and 7.00 self-assessed.
    assert!(close(balances["2110"], -14.0), "{balances:?}");

    let report = ca
        .report_ties(
            &t,
            &["2100", "2110", "2120", "2130", "2140"],
            &["1350", "1360", "6130"],
        )
        .await;
    let by_code = |code: &str, place: &str| -> f64 {
        report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| {
                r["code"] == code
                    && format!(
                        "{}-{}",
                        r["country_code"].as_str().unwrap_or(""),
                        r["region"].as_str().unwrap_or("")
                    ) == place
            })
            .map(|r| dec(&r["collected"]))
            .sum()
    };
    assert!(close(by_code("CA-HST-ON", "CA-ON"), 13.0), "{report}");
    assert!(close(by_code("CA-GST", "CA-QC"), 5.0), "{report}");
    assert!(close(by_code("CA-PST-BC", "CA-BC"), 7.0), "{report}");
    t.stop().await;
}

#[tokio::test]
async fn eu_distance_sales_are_taxed_at_destination_past_the_threshold() {
    let t = TestApp::start().await;
    let de = set_up(&t, "de_skr03", "DEU").await;
    run(
        &t,
        "party.set",
        json!({ "party_type": "company", "party_id": de.company,
                "addresses": [{ "kind": "billing", "is_default": true, "city": "Berlin",
                                "postcode": "10115", "country_code": "DE" }] }),
    )
    .await;
    // At home: 19 % unless something says otherwise.
    t.create(
        "tax_rule",
        json!({ "company_id": de.company, "tax_code_id": tax_code(&t, "DE-USt19").await }),
    )
    .await;
    // Registered for the One-Stop Shop in Germany, EUR 10,000 a year.
    t.create(
        "tax_registration",
        json!({ "company_id": de.company, "country_code": "DE", "scheme": "oss",
                "threshold": "10000", "number": "DE123456789" }),
    )
    .await;
    // The standard rates of France and Austria, for the test.
    let imported = run(
        &t,
        "tax_area.import",
        json!({ "csv": "country,code,name,rate\nFR,FR-TVA-T,TVA France (test),20\nAT,AT-USt-T,USt Österreich (test),20\n",
                "company_id": de.company,
                "collected_account_id": de.account("1776"), "paid_account_id": de.account("1576") }),
    )
    .await;
    assert_eq!(imported["areas_created"], 2, "{imported}");

    let paris = party(&t, "customer", "PAR", ("FR", None, "75001", "Paris"), None).await;
    let vienna = party(&t, "customer", "VIE", ("AT", None, "1010", "Wien"), None).await;
    let business = party(
        &t,
        "customer",
        "LYO",
        ("FR", None, "69001", "Lyon"),
        Some("FR40303265045"),
    )
    .await;
    let de19 = json!(tax_code(&t, "DE-USt19").await);
    let fr20 = json!(tax_code(&t, "FR-TVA-T").await);

    // Below the threshold: German VAT.
    let sale = de.sell(&t, &paris, "2026-02-10", "6000.00").await;
    assert_eq!(sale["body"]["lines"][0]["tax_code_id"], de19, "{sale}");
    let sale = de.sell(&t, &vienna, "2026-03-10", "5000.00").await;
    assert_eq!(sale["body"]["lines"][0]["tax_code_id"], de19, "{sale}");
    // Past it: the buyer's country's VAT, for consumers only.
    let april = de.sell(&t, &paris, "2026-04-10", "1000.00").await;
    assert_eq!(april["body"]["lines"][0]["tax_code_id"], fr20, "{april}");
    assert!(close(line_tax(&april), 200.0), "{april}");
    let sale = de.sell(&t, &business, "2026-04-11", "1000.00").await;
    assert_eq!(
        sale["body"]["lines"][0]["tax_code_id"], de19,
        "a business is not taxed at destination"
    );
    let sale = de.sell(&t, &vienna, "2026-05-10", "500.00").await;
    assert!(close(line_tax(&sale), 100.0), "{sale}");
    // The April sale to Paris is reversed in July: a correction of Q2.
    run(
        &t,
        "sales_invoice.reverse",
        json!({ "id": id(&april), "posting_date": "2026-07-05" }),
    )
    .await;

    let q1 = run(
        &t,
        "tax.oss_report",
        json!({ "company_id": de.company, "year": 2026, "quarter": 1 }),
    )
    .await;
    assert_eq!(q1["rows"], json!([]), "{q1}");
    let q2 = run(
        &t,
        "tax.oss_report",
        json!({ "company_id": de.company, "year": 2026, "quarter": 2 }),
    )
    .await;
    let rows: Vec<(String, f64, f64)> = q2["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["country_code"].as_str().unwrap().to_owned(),
                dec(&r["base"]),
                dec(&r["vat"]),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![("AT".into(), 500.0, 100.0), ("FR".into(), 1000.0, 200.0)],
        "{q2}"
    );
    assert!(close(dec(&q2["total_vat"]), 300.0), "{q2}");
    let q3 = run(
        &t,
        "tax.oss_report",
        json!({ "company_id": de.company, "year": 2026, "quarter": 3 }),
    )
    .await;
    assert_eq!(q3["rows"], json!([]), "{q3}");
    let correction = &q3["corrections"][0];
    assert_eq!(
        (
            &correction["country_code"],
            &correction["year"],
            &correction["quarter"]
        ),
        (&json!("FR"), &json!(2026), &json!(2)),
        "{q3}"
    );
    assert!(close(dec(&correction["vat"]), -200.0), "{q3}");

    // The OSS VAT is what the destination codes took, and the report
    // equals the accounts.
    let report = de
        .report_ties(&t, &["1776", "1771"], &["1576", "1571"])
        .await;
    let abroad: f64 = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["code"] == "FR-TVA-T" || r["code"] == "AT-USt-T")
        .map(|r| dec(&r["collected"]))
        .sum();
    assert!(
        close(abroad, dec(&q2["total_vat"]) + dec(&q3["total_vat"])),
        "{report}"
    );
    t.stop().await;
}
