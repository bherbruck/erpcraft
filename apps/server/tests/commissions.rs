//! Salespeople on customers and documents, commission plans and runs.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.005
}

struct Shop {
    company: String,
    warehouse: Value,
    bank: Value,
    item: String,
    vendor: String,
}

impl Shop {
    async fn new(t: &TestApp) -> Shop {
        let created = run(
            t,
            "company.setup",
            json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                    "first_year": 2026, "years": 1 }),
        )
        .await;
        let company = created["company"]["id"].as_str().unwrap().to_owned();
        let item = t
            .create("item", json!({ "code": "W", "name": "Widget" }))
            .await;
        let vendor = t
            .create("vendor", json!({ "code": "PARTS", "name": "Parts Supply" }))
            .await;
        Shop {
            company,
            warehouse: created["warehouse_id"].clone(),
            bank: created["roles"]["bank"]["id"].clone(),
            item,
            vendor,
        }
    }

    async fn buy(&self, t: &TestApp, date: &str, price: &str) {
        let (status, order) = t
            .post(
                &t.admin,
                "/api/documents/purchase_order",
                json!({ "company_id": self.company, "doc_date": date, "body": {
                    "vendor_id": self.vendor, "lines": [{ "item_id": self.item,
                    "warehouse_id": self.warehouse, "qty": "10", "unit_price": price }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{order}");
        let order = run(t, "purchase_order.approve", json!({ "id": id(&order) })).await;
        let receipt = run(
            t,
            "purchase_order.receive",
            json!({ "id": id(&order), "doc_date": date }),
        )
        .await;
        run(t, "purchase_receipt.post", json!({ "id": id(&receipt) })).await;
    }

    async fn sell(
        &self,
        t: &TestApp,
        customer: &str,
        qty: &str,
        price: &str,
        date: &str,
    ) -> (Value, Value) {
        let (status, order) = t
            .post(
                &t.admin,
                "/api/documents/sales_order",
                json!({ "company_id": self.company, "doc_date": date, "body": {
                    "customer_id": customer, "lines": [{ "item_id": self.item,
                    "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{order}");
        let order = run(t, "sales_order.approve", json!({ "id": id(&order) })).await;
        let delivery = run(
            t,
            "sales_order.deliver",
            json!({ "id": id(&order), "doc_date": date }),
        )
        .await;
        run(t, "sales_delivery.post", json!({ "id": id(&delivery) })).await;
        let invoice = run(
            t,
            "sales_order.invoice",
            json!({ "id": id(&order), "doc_date": date }),
        )
        .await;
        let invoice = run(t, "sales_invoice.post", json!({ "id": id(&invoice) })).await;
        (order, invoice)
    }

    async fn commission_run(&self, t: &TestApp, from: &str, to: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/commission_run",
                json!({ "company_id": self.company, "doc_date": to,
                        "body": { "from_date": from, "to_date": to } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(t, "commission_run.propose", json!({ "id": id(&draft) })).await
    }
}

fn line<'a>(run: &'a Value, rep: &str) -> &'a Value {
    run["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["sales_rep_id"] == rep)
        .unwrap_or_else(|| panic!("no line for {rep}: {run}"))
}

#[tokio::test]
async fn salespeople_earn_commission_on_invoices_and_payments_and_give_it_back_on_credits() {
    let t = TestApp::start().await;
    let s = Shop::new(&t).await;
    let anna = t
        .create(
            "sales_rep",
            json!({ "code": "ANNA", "name": "Anna", "email": "anna@example.com" }),
        )
        .await;
    let bob = t
        .create("sales_rep", json!({ "code": "BOB", "name": "Bob Agency" }))
        .await;
    let agency = t
        .create(
            "vendor",
            json!({ "code": "BOBAG", "name": "Bob Agency LLC" }),
        )
        .await;
    t.create(
        "commission_payee",
        json!({ "sales_rep_id": bob, "vendor_id": agency }),
    )
    .await;
    let first = t
        .create(
            "customer",
            json!({ "code": "FIRST", "name": "First Co", "salesperson_id": anna }),
        )
        .await;
    let second = t
        .create(
            "customer",
            json!({ "code": "SECOND", "name": "Second Co", "salesperson_id": bob }),
        )
        .await;
    let expense = t
        .create(
            "account",
            json!({ "company_id": s.company, "code": "6900", "name": "Commissions",
                    "account_type": "expense" }),
        )
        .await;
    let payable = t
        .create(
            "account",
            json!({ "company_id": s.company, "code": "2150", "name": "Commissions payable",
                    "account_type": "liability" }),
        )
        .await;
    t.create(
        "commission_settings",
        json!({ "company_id": s.company, "expense_account_id": expense,
                "payable_account_id": payable }),
    )
    .await;
    // Everyone: 5 % of net on invoicing. Bob: 10 % of margin as he is paid.
    let all = t
        .create(
            "commission_plan",
            json!({ "company_id": s.company, "code": "ALL", "name": "Everyone" }),
        )
        .await;
    t.create(
        "commission_tier",
        json!({ "plan_id": all, "from_amount": "0", "percent": "5" }),
    )
    .await;
    let agent = t
        .create(
            "commission_plan",
            json!({ "company_id": s.company, "code": "BOB", "name": "Bob", "basis": "margin",
                    "trigger": "payment", "sales_rep_id": bob }),
        )
        .await;
    t.create(
        "commission_tier",
        json!({ "plan_id": agent, "from_amount": "0", "percent": "10" }),
    )
    .await;

    s.buy(&t, "2026-03-01", "5.00").await;
    let (order, anna_invoice) = s.sell(&t, &first, "4", "10.00", "2026-03-10").await;
    // The customer's salesperson, copied forward.
    assert_eq!(order["summary"]["salesperson_id"], anna, "{order}");
    assert_eq!(
        anna_invoice["summary"]["salesperson_id"], anna,
        "{anna_invoice}"
    );
    let (_, bob_invoice) = s.sell(&t, &second, "2", "20.00", "2026-03-12").await;
    assert_eq!(bob_invoice["summary"]["salesperson_id"], bob);

    // March, before Bob's customer pays: Anna earns 5 % of 40.
    let march = s.commission_run(&t, "2026-03-01", "2026-03-31").await;
    let a = line(&march, &anna);
    assert!(close(dec(&a["basis"]), 40.0), "{a}");
    assert!(close(dec(&a["commission"]), 2.0), "{a}");
    assert_eq!(
        march["body"]["lines"].as_array().unwrap().len(),
        1,
        "{march}"
    );
    run(&t, "commission_run.cancel", json!({ "id": id(&march) })).await;

    // Paid on March 20: now Bob earns 10 % of the margin (40 less 2 at 5).
    let item = bob_invoice["postings"][0]["entries"]["open_items"][0]["id"].clone();
    let payment = run(
        &t,
        "payment.draft",
        json!({ "company_id": s.company, "open_item_ids": [item], "bank_account_id": s.bank,
                "doc_date": "2026-03-20" }),
    )
    .await;
    run(&t, "payment.post", json!({ "id": payment["id"] })).await;
    let march = s.commission_run(&t, "2026-03-01", "2026-03-31").await;
    let b = line(&march, &bob);
    assert!(close(dec(&b["basis"]), 30.0), "{b}");
    assert!(close(dec(&b["commission"]), 3.0), "{b}");
    assert!(close(dec(&march["summary"]["total"]), 5.0), "{march}");
    let posted = run(&t, "commission_run.post", json!({ "id": id(&march) })).await;
    assert_eq!(posted["status"], "posted", "{posted}");
    // The accrual: 5 of expense, 3 owed to Bob's agency as a payable.
    let gl = &posted["postings"][0]["entries"]["gl"];
    let debit: f64 = gl
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["account_id"] == expense)
        .map(|e| dec(&e["debit"]))
        .sum();
    assert!(close(debit, 5.0), "{gl}");
    let owed = &posted["postings"][0]["entries"]["open_items"];
    assert_eq!(owed.as_array().unwrap().len(), 1, "{owed}");
    assert_eq!(owed[0]["party_id"], agency);
    assert!(close(dec(&owed[0]["amount"]), 3.0), "{owed}");
    // A statement each.
    let statement = line(&posted, &bob)["statement_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, statement) = t
        .get(
            &t.admin,
            &format!("/api/documents/commission_statement/{statement}"),
        )
        .await;
    assert_eq!(statement["status"], "issued", "{statement}");
    assert_eq!(statement["summary"]["party_id"], agency);
    assert!(close(dec(&statement["summary"]["total"]), 3.0));
    assert_eq!(statement["body"]["items"].as_array().unwrap().len(), 1);
    let (status, page) = t
        .command(
            &t.admin,
            "document.render",
            json!({ "doc_type": "commission_statement", "id": statement["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");

    // Nothing is earned twice.
    let again = s.commission_run(&t, "2026-03-01", "2026-03-31").await;
    assert_eq!(again["body"]["lines"], json!([]), "{again}");
    run(&t, "commission_run.cancel", json!({ "id": id(&again) })).await;

    // In April one of Anna's widgets comes back and is credited: she
    // gives back 5 % of 10.
    let back = run(
        &t,
        "sales_order.return",
        json!({ "id": id(&order), "doc_date": "2026-04-05" }),
    )
    .await;
    let mut body = back["body"].clone();
    body["lines"][0]["qty"] = json!("1");
    let (status, back) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_return/{}", id(&back)),
            json!({ "version": back["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{back}");
    run(&t, "sales_return.post", json!({ "id": id(&back) })).await;
    let credit = run(
        &t,
        "sales_invoice.credit",
        json!({ "id": id(&anna_invoice), "doc_date": "2026-04-06" }),
    )
    .await;
    let credit = run(&t, "sales_credit_note.post", json!({ "id": id(&credit) })).await;
    assert_eq!(credit["summary"]["salesperson_id"], anna, "{credit}");
    let april = s.commission_run(&t, "2026-04-01", "2026-04-30").await;
    let a = line(&april, &anna);
    assert!(close(dec(&a["commission"]), -0.5), "{a}");

    // Reversing March frees what it held.
    run(&t, "commission_run.reverse", json!({ "id": id(&posted) })).await;
    let (_, statement) = t
        .get(
            &t.admin,
            &format!(
                "/api/documents/commission_statement/{}",
                statement["id"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(statement["status"], "cancelled");
    let redo = s.commission_run(&t, "2026-03-01", "2026-03-31").await;
    assert!(close(dec(&redo["summary"]["total"]), 5.0), "{redo}");

    // Sales by salesperson.
    let report = run(
        &t,
        "analysis.sales",
        json!({ "company_id": s.company, "from": "2026-03-01", "to": "2026-03-31",
                "group_by": ["salesperson"] }),
    )
    .await;
    let by_anna = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["salesperson_code"] == "ANNA")
        .expect("a row for Anna");
    assert!(close(dec(&by_anna["net"]), 40.0), "{report}");
    t.stop().await;
}

#[tokio::test]
async fn a_salesperson_sees_only_their_customers_and_documents() {
    let t = TestApp::start().await;
    let s = Shop::new(&t).await;
    let anna = t
        .create("sales_rep", json!({ "code": "ANNA", "name": "Anna" }))
        .await;
    let bob = t
        .create("sales_rep", json!({ "code": "BOB", "name": "Bob" }))
        .await;
    let first = t
        .create(
            "customer",
            json!({ "code": "FIRST", "name": "First Co", "salesperson_id": anna }),
        )
        .await;
    let second = t
        .create(
            "customer",
            json!({ "code": "SECOND", "name": "Second Co", "salesperson_id": bob }),
        )
        .await;
    s.buy(&t, "2026-03-01", "5.00").await;
    s.sell(&t, &first, "1", "10.00", "2026-03-10").await;
    s.sell(&t, &second, "1", "10.00", "2026-03-11").await;
    let (token, user) = t
        .user_with_rules(
            "anna@example.com",
            json!([
                { "allow": "sales_invoice:read",
                  "where": "record.summary.salesperson_id == user.sales_rep_id" },
                { "allow": "customer:read", "where": "record.salesperson_id == user.sales_rep_id" }
            ]),
            Some(&s.company),
        )
        .await;
    t.set_attributes(&user, json!({ "sales_rep_id": anna }))
        .await;
    let (status, invoices) = t
        .get(
            &token,
            &format!("/api/documents/sales_invoice?company_id={}", s.company),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{invoices}");
    assert_eq!(invoices["total"], 1, "{invoices}");
    assert_eq!(invoices["items"][0]["summary"]["salesperson_id"], anna);
    let (_, customers) = t.get(&token, "/api/entities/customer").await;
    assert_eq!(customers["total"], 1, "{customers}");
    assert_eq!(customers["items"][0]["code"], "FIRST");
    // A draft's salesperson can be changed; the customer's is the default.
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            json!({ "company_id": s.company, "doc_date": "2026-03-15", "body": {
                "customer_id": first, "salesperson_id": bob, "lines": [] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    assert_eq!(draft["body"]["salesperson_id"], bob);
    t.stop().await;
}
