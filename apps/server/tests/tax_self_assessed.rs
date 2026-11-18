//! Tax the buyer accounts for and tax adjustments, across currencies,
//! credits and reversals: the general ledger, the open items and the tax
//! report stay in step. `Fixture::books` checks that at every step.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    vendor: String,
    customer: String,
    account: BTreeMap<&'static str, String>,
    vat: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("ACME", "USD").await;
    let net30 = t
        .create(
            "payment_terms",
            json!({ "code": "NET30", "name": "Net 30", "net_days": 30 }),
        )
        .await;
    let vendor = t
        .create(
            "vendor",
            json!({ "code": "V1", "name": "Parts Inc", "payment_terms_id": net30 }),
        )
        .await;
    let customer = t
        .create(
            "customer",
            json!({ "code": "C1", "name": "Buyer Ltd", "payment_terms_id": net30 }),
        )
        .await;
    let mut account = BTreeMap::new();
    for (code, kind, currency) in [
        ("1010", "asset", Some("EUR")),
        ("1200", "asset", None),
        ("1300", "asset", None),
        ("1400", "asset", None),
        ("2000", "liability", None),
        ("2100", "liability", None),
        ("2300", "liability", None),
        ("4030", "income", None),
        ("5000", "expense", None),
        ("5040", "expense", None),
        ("5200", "expense", None),
        ("7100", "income", None),
        ("7200", "expense", None),
    ] {
        let mut data =
            json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind });
        if let Some(currency) = currency {
            data["currency"] = json!(currency);
            data["is_bank"] = json!(true);
        }
        account.insert(code, t.create("account", data).await);
    }
    t.create(
        "purchasing_settings",
        json!({ "company_id": c.id, "grni_account_id": account["2100"],
                "payable_account_id": account["2000"], "price_variance_account_id": account["5200"] }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({ "company_id": c.id, "receivable_account_id": account["1200"],
                "revenue_account_id": c.revenue, "cogs_account_id": account["5000"] }),
    )
    .await;
    for (purpose, code) in [
        ("fx_gain", "7100"),
        ("fx_loss", "7200"),
        ("discount_received", "5040"),
        ("discount_given", "4030"),
    ] {
        t.create(
            "account_rule",
            json!({ "company_id": c.id, "purpose": purpose, "account_id": account[code] }),
        )
        .await;
    }
    let vat = t
        .create(
            "tax_code",
            json!({ "code": "VAT19", "name": "VAT 19%", "rate": "19" }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({ "company_id": c.id, "tax_code_id": vat,
                "collected_account_id": account["2300"], "paid_account_id": account["1400"] }),
    )
    .await;
    for (date, rate) in [("2026-01-01", "1.10"), ("2026-04-01", "1.20")] {
        t.create(
            "exchange_rate",
            json!({ "from_currency": "EUR", "to_currency": "USD", "rate_date": date, "rate": rate }),
        )
        .await;
    }
    Fixture {
        c,
        vendor,
        customer,
        account,
        vat,
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

impl Fixture {
    async fn draft(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
        draft
    }

    async fn post(&self, t: &TestApp, doc_type: &str, draft: &Value) -> (StatusCode, Value) {
        t.command(
            &t.admin,
            &format!("{doc_type}.post"),
            json!({ "id": id(draft) }),
        )
        .await
    }

    async fn posted(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let draft = self.draft(t, doc_type, date, body).await;
        let (status, done) = self.post(t, doc_type, &draft).await;
        assert_eq!(status, StatusCode::OK, "{doc_type}: {done}");
        done
    }

    async fn item_of(&self, t: &TestApp, kind: &str, invoice: &Value) -> Value {
        let (_, items) = t
            .command(
                &t.admin,
                "open_item.list",
                json!({ "company_id": self.c.id, "kind": kind, "include_settled": true, "limit": 500 }),
            )
            .await;
        items["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["document_id"] == invoice["id"])
            .cloned()
            .unwrap_or_else(|| panic!("no item for {invoice}"))
    }

    /// Balances by account code, as of a day; on the way the books are
    /// checked against open items and the tax report.
    async fn books(&self, t: &TestApp) -> BTreeMap<String, f64> {
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        let balances: BTreeMap<String, f64> = tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["code"].as_str().unwrap().to_owned(), dec(&r["balance"])))
            .collect();
        let at = |code: &str| balances.get(code).copied().unwrap_or(0.0);
        for (kind, account, sign) in [("payable", "2000", -1.0), ("receivable", "1200", 1.0)] {
            let (_, items) = t
                .command(
                    &t.admin,
                    "open_item.list",
                    json!({ "company_id": self.c.id, "kind": kind }),
                )
                .await;
            assert!(
                close(dec(&items["total_open"]), sign * at(account)),
                "open {kind}s {} equal account {account} {}",
                items["total_open"],
                at(account)
            );
        }
        let (_, report) = t
            .command(
                &t.admin,
                "tax.report",
                json!({ "company_id": self.c.id, "from": "2026-01-01", "to": "2026-12-31" }),
            )
            .await;
        assert!(close(dec(&report["total_paid"]), at("1400")), "{report}");
        assert!(
            close(dec(&report["total_collected"]), -at("2300")),
            "{report}"
        );
        balances
    }
}

async fn self_assessed(t: &TestApp, f: &Fixture) -> String {
    let code = t
        .create(
            "tax_code",
            json!({ "code": "RC19", "name": "Reverse charge 19 %", "rate": "19",
                    "self_assessed": true }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({ "company_id": f.c.id, "tax_code_id": code,
                "collected_account_id": f.account["2300"], "paid_account_id": f.account["1400"] }),
    )
    .await;
    code
}

/// A customer credit in euros: GL, open items and the tax report agree.
#[tokio::test]
async fn foreign_customer_credit_keeps_ledgers_together() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let credit = f
        .posted(
            &t,
            "sales_credit",
            "2026-02-10",
            json!({ "customer_id": f.customer, "currency": "EUR", "lines": [
                { "account_id": f.c.revenue, "qty": "3", "unit_price": "33.33", "tax_code_id": f.vat },
                { "account_id": f.c.revenue, "qty": "1", "unit_price": "0.07", "tax_code_id": f.vat },
                { "account_id": f.c.revenue, "qty": "7", "unit_price": "1.11", "tax_code_id": f.vat } ] }),
        )
        .await;
    f.books(&t).await;
    f.item_of(&t, "receivable", &credit).await;
    // Reversed: everything back to zero.
    let (status, body) = t
        .command(
            &t.admin,
            "sales_credit.reverse",
            json!({ "id": credit["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let books = f.books(&t).await;
    for code in ["1200", "2300", "4000"] {
        assert!(
            close(books.get(code).copied().unwrap_or(0.0), 0.0),
            "{code}: {books:?}"
        );
    }
    // The reversal of an adjustment entry is marked as one too.
    let flags: Vec<(bool, bool)> = sqlx::query_as(
        "SELECT reverses_id IS NOT NULL, adjustment FROM tax_entry ORDER BY created_at, line_no",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert!(
        flags.iter().all(|(_, adj)| *adj),
        "reversal entries lose `adjustment`: {flags:?}"
    );
    t.stop().await;
}

/// Reverse charge on a vendor invoice in euros, credited in two parts at
/// another rate, one credit note reversed and credited again.
#[tokio::test]
async fn foreign_self_assessed_credits_keep_ledgers_together() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let rc = self_assessed(&t, &f).await;
    let invoice = f
        .posted(
            &t,
            "vendor_invoice",
            "2026-03-01",
            json!({ "vendor_id": f.vendor, "currency": "EUR", "lines": [
                { "account_id": f.account["5200"], "description": "Services",
                  "qty": "3", "unit_price": "333.33", "tax_code_id": rc } ] }),
        )
        .await;
    f.books(&t).await;
    let line = invoice["body"]["lines"][0]["id"].clone();
    let first = f
        .posted(
            &t,
            "vendor_credit_note",
            "2026-04-05",
            json!({ "vendor_invoice_id": invoice["id"],
                    "lines": [{ "invoice_line_id": line, "qty": "1", "unit_price": "333.33" }] }),
        )
        .await;
    f.books(&t).await;
    let (status, body) = t
        .command(
            &t.admin,
            "vendor_credit_note.reverse",
            json!({ "id": first["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    f.books(&t).await;
    f.posted(
        &t,
        "vendor_credit_note",
        "2026-04-06",
        json!({ "vendor_invoice_id": invoice["id"],
                "lines": [{ "invoice_line_id": line, "qty": "3", "unit_price": "333.33" }] }),
    )
    .await;
    let books = f.books(&t).await;
    for code in ["1400", "2300", "2000"] {
        assert!(
            close(books.get(code).copied().unwrap_or(0.0), 0.0),
            "{code}: {books:?}"
        );
    }
    t.stop().await;
}

/// A customer credit with a self-assessed code charges nothing and leaves
/// the books in step.
#[tokio::test]
async fn customer_credit_with_self_assessed_code() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let rc = self_assessed(&t, &f).await;
    let credit = f
        .posted(
            &t,
            "sales_credit",
            "2026-02-10",
            json!({ "customer_id": f.customer, "lines": [
                { "account_id": f.c.revenue, "qty": "1", "unit_price": "100.00", "tax_code_id": rc } ] }),
        )
        .await;
    assert_eq!(dec(&credit["summary"]["total_gross"]), 100.0, "{credit}");
    f.books(&t).await;
    t.stop().await;
}

/// A group marked as accounted for by the buyer makes its components so,
/// though they are ordinary rates: the vendor charges nothing, and the
/// company assesses the tax and reclaims it. A sale under it charges none.
#[tokio::test]
async fn a_group_the_buyer_accounts_for_makes_its_components_so() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let group = t
        .create(
            "tax_code",
            json!({ "code": "RCG", "name": "Reverse charge group", "rate": "19",
                    "self_assessed": true }),
        )
        .await;
    t.create(
        "tax_code_component",
        json!({ "group_id": group, "component_id": f.vat, "position": 1 }),
    )
    .await;
    let invoice = f
        .posted(
            &t,
            "vendor_invoice",
            "2026-03-01",
            json!({ "vendor_id": f.vendor, "lines": [
                { "account_id": f.account["5200"], "description": "Services",
                  "qty": "1", "unit_price": "100.00", "tax_code_id": group } ] }),
        )
        .await;
    assert_eq!(
        dec(&invoice["summary"]["total_gross"]),
        100.0,
        "a group marked self-assessed is not owed to the vendor: {}",
        invoice["summary"]
    );
    let entries: Vec<(String, bool, String)> = sqlx::query_as(
        "SELECT direction, self_assessed, amount::text FROM tax_entry ORDER BY direction",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        entries,
        vec![
            ("collected".into(), true, "19.0000".into()),
            ("paid".into(), true, "19.0000".into()),
        ]
    );
    f.books(&t).await;
    let sale = f
        .posted(
            &t,
            "sales_credit",
            "2026-03-02",
            json!({ "customer_id": f.customer, "lines": [
                { "account_id": f.c.revenue, "qty": "1", "unit_price": "100.00",
                  "tax_code_id": group } ] }),
        )
        .await;
    assert_eq!(dec(&sale["summary"]["total_gross"]), 100.0, "{sale}");
    f.books(&t).await;
    t.stop().await;
}

/// A group that is not marked, of a component that is, is accounted for
/// by the buyer as before: the component's own flag counts too.
#[tokio::test]
async fn a_component_the_buyer_accounts_for_stays_so_in_a_plain_group() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let rc = self_assessed(&t, &f).await;
    let group = t
        .create(
            "tax_code",
            json!({ "code": "G-RC", "name": "Group of the reverse charge", "rate": "19" }),
        )
        .await;
    t.create(
        "tax_code_component",
        json!({ "group_id": group, "component_id": rc, "position": 1 }),
    )
    .await;
    let invoice = f
        .posted(
            &t,
            "vendor_invoice",
            "2026-03-01",
            json!({ "vendor_id": f.vendor, "lines": [
                { "account_id": f.account["5200"], "description": "Services",
                  "qty": "1", "unit_price": "100.00", "tax_code_id": group } ] }),
        )
        .await;
    assert_eq!(dec(&invoice["summary"]["total_gross"]), 100.0, "{invoice}");
    f.books(&t).await;
    t.stop().await;
}
