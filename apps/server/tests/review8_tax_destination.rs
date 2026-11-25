//! Review 8: tax by destination. Helpers copied from tax_destination.rs.
#![allow(dead_code)]

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

async fn canada(t: &TestApp) -> Books {
    let ca = set_up(t, "ca_small_business", "CAN").await;
    run(
        t,
        "party.set",
        json!({ "party_type": "company", "party_id": ca.company,
                "addresses": [{ "kind": "billing", "is_default": true, "city": "Vancouver",
                                "postcode": "V6B 1A1", "region": "BC", "country_code": "CA" }] }),
    )
    .await;
    ca
}

/// The vendor invoice records its tax before it keeps its parties
/// (purchasing/src/lib.rs: erpcraft_tax::record before snapshot_purchase),
/// so its entries, the self-assessed PST among them, have no place:
/// tax.report by_place cannot say where the use tax / PST was owed.
#[tokio::test]
async fn review8_tax_purchase_entries_keep_no_place() {
    let t = TestApp::start().await;
    let ca = canada(&t).await;
    let away = party(
        &t,
        "vendor",
        "TOR-SUP",
        ("CA", Some("ON"), "M5V 2T6", "Toronto"),
        None,
    )
    .await;
    ca.buy(&t, &away, "6050", "100.00").await;
    let report = run(
        &t,
        "tax.report",
        json!({ "company_id": ca.company, "from": "2026-01-01", "to": "2026-12-31",
                "by_place": true }),
    )
    .await;
    let rows = report["rows"].as_array().unwrap();
    let sa: Vec<&Value> = rows
        .iter()
        .filter(|r| r["code"] == "CA-PST-BC-SA")
        .collect();
    assert!(!sa.is_empty(), "{report}");
    for row in sa {
        assert_eq!(
            (row["country_code"].as_str(), row["region"].as_str()),
            (Some("CA"), Some("BC")),
            "the PST self-assessed on a purchase received in British Columbia has no place: {report}"
        );
    }
    t.stop().await;
}

/// A credit not tied to an invoice (sales_credit, a rebate) still proposes
/// its lines' codes with the old proposed_code: no destination. The
/// invoice to a Toronto customer is HST 13 %; the rebate to the same
/// customer is proposed something else (or nothing), while its entries
/// record the place CA-ON.
#[tokio::test]
async fn review8_tax_standalone_credit_ignores_destination() {
    let t = TestApp::start().await;
    let ca = canada(&t).await;
    let toronto = party(
        &t,
        "customer",
        "TOR",
        ("CA", Some("ON"), "M5V 2T6", "Toronto"),
        None,
    )
    .await;
    let sale = ca.sell(&t, &toronto, "2026-03-10", "100.00").await;
    let hst = json!(tax_code(&t, "CA-HST-ON").await);
    assert_eq!(sale["body"]["lines"][0]["tax_code_id"], hst, "{sale}");
    let (status, credit) = t
        .post(
            &t.admin,
            "/api/documents/sales_credit",
            json!({ "company_id": ca.company, "doc_date": "2026-03-20", "body": {
                "customer_id": toronto,
                "lines": [{ "account_id": ca.revenue, "description": "Rebate",
                            "qty": "1", "unit_price": "10.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{credit}");
    assert_eq!(
        credit["body"]["lines"][0]["tax_code_id"], hst,
        "a rebate to the Ontario customer is proposed a code other than HST Ontario: {credit}"
    );
    t.stop().await;
}

/// tax_area is shared and the purchase side ignores registrations: a rate
/// table imported by (and given accounts for) company A makes company B's
/// vendor invoices start with codes B has no accounts for.
#[tokio::test]
async fn review8_tax_shared_area_reaches_another_companys_purchases() {
    let t = TestApp::start().await;
    let a = set_up(&t, "us_small_business", "USA").await;
    let csv = "country,region,code,name,rate\nUS,OH,US-OH-T,Ohio (test),5.75\n";
    run(
        &t,
        "tax_area.import",
        json!({ "csv": csv, "company_id": a.company,
                "collected_account_id": a.account("2100"), "paid_account_id": a.account("6130") }),
    )
    .await;
    let b = set_up(&t, "us_small_business", "USB").await;
    run(
        &t,
        "party.set",
        json!({ "party_type": "company", "party_id": b.company,
                "addresses": [{ "kind": "billing", "is_default": true, "city": "Columbus",
                                "postcode": "43215", "region": "OH", "country_code": "US" }] }),
    )
    .await;
    let vendor = party(
        &t,
        "vendor",
        "OHV",
        ("US", Some("OH"), "43215", "Columbus"),
        None,
    )
    .await;
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/vendor_invoice",
            json!({ "company_id": b.company, "doc_date": "2026-03-12", "body": {
                "vendor_id": vendor,
                "lines": [{ "account_id": b.account("6050"), "description": "Supplies",
                            "qty": "1", "unit_price": "100.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let proposed = draft["body"]["lines"][0]["tax_code_id"].clone();
    let (status, body) = t
        .command(&t.admin, "vendor_invoice.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "company B's vendor invoice, proposed {proposed} from company A's rate table, is refused: {body}"
    );
    t.stop().await;
}

/// The sale that takes the year's distance sales past the OSS threshold is
/// taxed at origin: beyond_threshold compares the sales before it.
/// Article 59c: the supply by which the threshold is exceeded is taxed at
/// destination.
#[tokio::test]
async fn review8_tax_oss_sale_crossing_the_threshold() {
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
    t.create(
        "tax_rule",
        json!({ "company_id": de.company, "tax_code_id": tax_code(&t, "DE-USt19").await }),
    )
    .await;
    t.create(
        "tax_registration",
        json!({ "company_id": de.company, "country_code": "DE", "scheme": "oss",
                "threshold": "10000", "number": "DE123456789" }),
    )
    .await;
    run(
        &t,
        "tax_area.import",
        json!({ "csv": "country,code,name,rate\nFR,FR-TVA-T,TVA France (test),20\n",
                "company_id": de.company,
                "collected_account_id": de.account("1776"), "paid_account_id": de.account("1576") }),
    )
    .await;
    let paris = party(&t, "customer", "PAR", ("FR", None, "75001", "Paris"), None).await;
    let fr20 = json!(tax_code(&t, "FR-TVA-T").await);
    let sale = de.sell(&t, &paris, "2026-02-10", "12000.00").await;
    assert_eq!(
        sale["body"]["lines"][0]["tax_code_id"], fr20,
        "the 12,000 sale that exceeds the 10,000 threshold is taxed in Germany: {sale}"
    );
    t.stop().await;
}
