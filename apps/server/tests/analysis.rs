//! Business analysis: sales and purchase analysis, stock aging, slow
//! movers, turnover and the key figures, all from what was posted.

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
    item: String,
    first: String,
    second: String,
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
        let first = t
            .create("customer", json!({ "code": "FIRST", "name": "First Co" }))
            .await;
        let second = t
            .create("customer", json!({ "code": "SECOND", "name": "Second Co" }))
            .await;
        let vendor = t
            .create("vendor", json!({ "code": "PARTS", "name": "Parts Supply" }))
            .await;
        Shop {
            company,
            warehouse: created["warehouse_id"].clone(),
            item,
            first,
            second,
            vendor,
        }
    }

    /// Ten bought at `price`, received and invoiced on `date`.
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
        let invoice = run(
            t,
            "purchase_order.invoice",
            json!({ "id": id(&order), "doc_date": date }),
        )
        .await;
        run(t, "vendor_invoice.post", json!({ "id": id(&invoice) })).await;
    }

    /// `qty` sold at `price`: ordered and delivered on `delivered`,
    /// invoiced on `invoiced`.
    async fn sell(
        &self,
        t: &TestApp,
        customer: &str,
        qty: &str,
        price: &str,
        delivered: &str,
        invoiced: &str,
    ) -> (Value, Value) {
        let (status, order) = t
            .post(
                &t.admin,
                "/api/documents/sales_order",
                json!({ "company_id": self.company, "doc_date": delivered, "body": {
                    "customer_id": customer, "lines": [{ "item_id": self.item,
                    "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{order}");
        let order = run(t, "sales_order.approve", json!({ "id": id(&order) })).await;
        let delivery = run(
            t,
            "sales_order.deliver",
            json!({ "id": id(&order), "doc_date": delivered }),
        )
        .await;
        run(t, "sales_delivery.post", json!({ "id": id(&delivery) })).await;
        let invoice = run(
            t,
            "sales_order.invoice",
            json!({ "id": id(&order), "doc_date": invoiced }),
        )
        .await;
        let invoice = run(t, "sales_invoice.post", json!({ "id": id(&invoice) })).await;
        (order, invoice)
    }

    async fn sales(&self, t: &TestApp, token: &str, input: Value) -> Value {
        let mut input = input;
        input["company_id"] = json!(self.company);
        let (status, body) = t.command(token, "analysis.sales", input).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }
}

fn row<'a>(report: &'a Value, field: &str, value: &str) -> &'a Value {
    report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r[field] == value)
        .unwrap_or_else(|| panic!("no row with {field} {value}: {report}"))
}

/// What the stock ledger valued the goods of an order's deliveries at,
/// less its returns: the cost the margin must use.
async fn posted_cost(t: &TestApp, order: &Value) -> f64 {
    let cost: Option<erpcraft_kernel::Decimal> = sqlx::query_scalar(
        "SELECT -sum(se.value) FROM stock_entry se
         WHERE se.source_line_id IN (
             SELECT dl.id FROM sales_delivery_line dl JOIN sales_order_line l ON l.id = dl.order_line_id
             WHERE l.document_id = $1::uuid
             UNION ALL
             SELECT rl.id FROM sales_return_line rl JOIN sales_order_line l ON l.id = rl.order_line_id
             WHERE l.document_id = $1::uuid)",
    )
    .bind(id(order))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    cost.map(|c| c.to_string().parse().unwrap()).unwrap_or(0.0)
}

#[tokio::test]
async fn sales_margins_use_the_cost_the_stock_ledger_posted() {
    let t = TestApp::start().await;
    let s = Shop::new(&t).await;
    s.buy(&t, "2026-01-10", "5.00").await;
    // First: 5 at 10, delivered at 5.00 each.
    let (first, first_invoice) = s
        .sell(&t, &s.first, "5", "10.00", "2026-01-20", "2026-01-25")
        .await;
    s.buy(&t, "2026-02-10", "7.00").await;
    // Second: 6 at 12, delivered at the average then (5 at 5, 10 at 7).
    let (second, _) = s
        .sell(&t, &s.second, "6", "12.00", "2026-02-15", "2026-02-20")
        .await;
    // A later purchase at another price does not change what was posted.
    s.buy(&t, "2026-03-20", "20.00").await;

    let q1 = s
        .sales(
            &t,
            &t.admin,
            json!({ "from": "2026-01-01", "to": "2026-03-31" }),
        )
        .await;
    let a = row(&q1, "party_code", "FIRST");
    assert!(close(dec(&a["net"]), 50.0), "{a}");
    assert!(close(dec(&a["qty"]), 5.0), "{a}");
    assert!(close(dec(&a["cost"]), 25.0), "{a}");
    assert!(close(dec(&a["margin"]), 25.0), "{a}");
    assert!(close(dec(&a["margin_percent"]), 50.0), "{a}");
    let b = row(&q1, "party_code", "SECOND");
    assert!(close(dec(&b["net"]), 72.0), "{b}");
    let cost = posted_cost(&t, &second).await;
    assert!(close(dec(&b["cost"]), cost), "{b} against {cost}");
    assert!(cost > 37.0 && cost < 39.0, "{cost}");
    assert!(close(dec(&q1["totals"]["net"]), 122.0), "{q1}");
    assert_eq!(q1["currency"], "USD");
    // Largest first.
    assert_eq!(q1["rows"][0]["party_code"], "SECOND", "{q1}");

    // Two of the second come back and are credited: revenue and cost
    // taken back on the credit's date.
    let back = run(
        &t,
        "sales_order.return",
        json!({ "id": id(&second), "doc_date": "2026-03-01" }),
    )
    .await;
    let mut body = back["body"].clone();
    body["lines"][0]["qty"] = json!("2");
    let (status, back) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_return/{}", id(&back)),
            json!({ "version": back["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{back}");
    run(&t, "sales_return.post", json!({ "id": id(&back) })).await;
    let invoice_of_second: String = sqlx::query_scalar(
        "SELECT document_id::text FROM sales_invoice WHERE sales_order_id = $1::uuid",
    )
    .bind(id(&second))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let credit = run(
        &t,
        "sales_invoice.credit",
        json!({ "id": invoice_of_second, "doc_date": "2026-03-02" }),
    )
    .await;
    run(&t, "sales_credit_note.post", json!({ "id": id(&credit) })).await;
    let march = s
        .sales(
            &t,
            &t.admin,
            json!({ "from": "2026-03-01", "to": "2026-03-31" }),
        )
        .await;
    let b = row(&march, "party_code", "SECOND");
    assert!(close(dec(&b["net"]), -24.0), "{b}");
    assert!(close(dec(&b["qty"]), -2.0), "{b}");
    assert!(dec(&b["cost"]) < -12.0 && dec(&b["cost"]) > -13.5, "{b}");
    // Over the quarter the second's cost is what its order's goods cost,
    // net of the return.
    let q1 = s
        .sales(
            &t,
            &t.admin,
            json!({ "from": "2026-01-01", "to": "2026-03-31" }),
        )
        .await;
    let b = row(&q1, "party_code", "SECOND");
    assert!(close(dec(&b["net"]), 48.0), "{b}");
    assert!(
        close(dec(&b["cost"]), posted_cost(&t, &second).await),
        "{b}"
    );

    // A reversal counts on its own date.
    run(
        &t,
        "sales_invoice.reverse",
        json!({ "id": id(&first_invoice), "posting_date": "2026-04-05" }),
    )
    .await;
    let april = s
        .sales(
            &t,
            &t.admin,
            json!({ "from": "2026-04-01", "to": "2026-04-30" }),
        )
        .await;
    let a = row(&april, "party_code", "FIRST");
    assert!(close(dec(&a["net"]), -50.0), "{a}");
    assert!(close(dec(&a["cost"]), -25.0), "{a}");
    let january = s
        .sales(
            &t,
            &t.admin,
            json!({ "from": "2026-01-01", "to": "2026-01-31" }),
        )
        .await;
    assert!(close(
        dec(&row(&january, "party_code", "FIRST")["net"]),
        50.0
    ));
    let _ = first;

    // By item and month, and against the period before.
    let by_month = s
        .sales(
            &t,
            &t.admin,
            json!({ "from": "2026-02-01", "to": "2026-02-28", "group_by": ["item", "period"],
                    "compare": "prior_period" }),
        )
        .await;
    assert_eq!(by_month["compare_from"], "2026-01-04", "{by_month}");
    assert_eq!(by_month["compare_to"], "2026-01-31");
    let feb = row(&by_month, "period", "2026-02-01");
    assert_eq!(feb["item_code"], "W");
    assert!(close(dec(&feb["net"]), 72.0), "{feb}");
    assert!(
        close(dec(&by_month["totals"]["compare_net"]), 50.0),
        "{by_month}"
    );
    assert!(
        close(dec(&by_month["totals"]["net_change_percent"]), 44.0),
        "{by_month}"
    );

    // Top customers by margin.
    let top = run(
        &t,
        "analysis.top",
        json!({ "company_id": s.company, "from": "2026-01-01", "to": "2026-03-31",
                "of": "customers", "by": "margin", "n": 1 }),
    )
    .await;
    assert_eq!(top["rows"].as_array().unwrap().len(), 1, "{top}");
    assert!(close(dec(&top["totals"]["net"]), 98.0), "{top}");

    // Only in the document currency: none of these were in euros.
    let eur = s
        .sales(
            &t,
            &t.admin,
            json!({ "from": "2026-01-01", "to": "2026-03-31", "currency": "EUR" }),
        )
        .await;
    assert_eq!(eur["rows"], json!([]), "{eur}");
    let usd = s
        .sales(
            &t,
            &t.admin,
            json!({ "from": "2026-01-01", "to": "2026-03-31", "currency": "USD" }),
        )
        .await;
    assert!(close(dec(&usd["totals"]["net_document"]), 98.0), "{usd}");

    // Who may read invoices but not the stock ledger sees no cost.
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!(["sales_invoice:read", "customer:read"]),
            Some(&s.company),
        )
        .await;
    let seen = s
        .sales(
            &t,
            &clerk,
            json!({ "from": "2026-01-01", "to": "2026-03-31" }),
        )
        .await;
    let a = row(&seen, "party_code", "SECOND");
    assert!(a["cost"].is_null() && a["margin"].is_null(), "{a}");
    // Nor credit notes, which this clerk may not read.
    assert!(close(dec(&seen["totals"]["net"]), 122.0), "{seen}");
    // Who may not read invoices gets nothing.
    let (nobody, _) = t
        .user_with_rules(
            "nobody@example.com",
            json!(["customer:read"]),
            Some(&s.company),
        )
        .await;
    let (status, _) = t
        .command(
            &nobody,
            "analysis.sales",
            json!({ "company_id": s.company, "from": "2026-01-01", "to": "2026-03-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Purchases: thirty bought, at 5, 7 and 20.
    let bought = run(
        &t,
        "analysis.purchases",
        json!({ "company_id": s.company, "from": "2026-01-01", "to": "2026-03-31",
                "group_by": ["vendor", "item"] }),
    )
    .await;
    let v = row(&bought, "party_code", "PARTS");
    assert!(close(dec(&v["qty"]), 30.0), "{v}");
    assert!(close(dec(&v["net"]), 320.0), "{v}");
    assert!(close(dec(&v["average_price"]), 10.6667), "{v}");
    assert_eq!(v["item_code"], "W");

    // Stock: 30 in, 11 out, 2 back: 21 on hand; the newest first in.
    let aging = run(
        &t,
        "analysis.inventory_aging",
        json!({ "company_id": s.company }),
    )
    .await;
    let w = &aging["rows"][0];
    assert!(close(dec(&w["qty"]), 21.0), "{aging}");
    assert_eq!(w["oldest_receipt"], "2026-02-10", "{w}");
    let buckets: f64 = [
        "value_0_30",
        "value_31_60",
        "value_61_90",
        "value_91_180",
        "value_over_180",
    ]
    .iter()
    .map(|b| dec(&w[*b]))
    .sum();
    assert!(close(buckets, dec(&w["value"])), "{w}");

    // Nothing was issued since February: dead stock.
    let slow = run(
        &t,
        "analysis.slow_movers",
        json!({ "company_id": s.company, "days": 90 }),
    )
    .await;
    assert_eq!(slow["rows"][0]["movement"], "dead", "{slow}");
    assert_eq!(slow["rows"][0]["last_issue"], "2026-02-15", "{slow}");
    assert!(close(dec(&slow["dead_value"]), dec(&w["value"])), "{slow}");

    // Turnover over the quarter: what left stock over the average value.
    let turnover = run(
        &t,
        "analysis.inventory_turnover",
        json!({ "company_id": s.company, "from": "2026-01-01", "to": "2026-03-31" }),
    )
    .await;
    let r = &turnover["rows"][0];
    assert!(
        close(
            dec(&r["cost_of_issues"]),
            25.0 + posted_cost(&t, &second).await + posted_returned(&t, &second).await
        ),
        "{r}"
    );
    assert!(close(dec(&r["opening_value"]), 0.0), "{r}");
    assert!(r["turnover"].as_str().is_some(), "{r}");

    // Key figures at the end of March.
    let kpis = run(
        &t,
        "analysis.kpis",
        json!({ "company_id": s.company, "as_of": "2026-03-31", "days": 90 }),
    )
    .await;
    assert!(close(dec(&kpis["revenue"]), 98.0), "{kpis}");
    assert!(dec(&kpis["receivables"]) > 0.0, "{kpis}");
    assert!(kpis["dso"].as_str().is_some(), "{kpis}");
    assert!(kpis["dio"].as_str().is_some(), "{kpis}");
    assert!(dec(&kpis["inventory"]) > 0.0, "{kpis}");
    assert!(kpis["current_ratio"].as_str().is_some(), "{kpis}");
    // The backlog is as the orders stand now: the first order's invoice was
    // reversed in April, so its goods are to invoice again.
    assert_eq!(kpis["open_orders"], 1, "{kpis}");
    assert!(close(dec(&kpis["backlog_to_invoice"]), 50.0), "{kpis}");
    t.stop().await;
}

/// The value the returns of an order put back into stock.
async fn posted_returned(t: &TestApp, order: &Value) -> f64 {
    let value: Option<erpcraft_kernel::Decimal> = sqlx::query_scalar(
        "SELECT sum(se.value) FROM stock_entry se
         WHERE se.source_line_id IN (
             SELECT rl.id FROM sales_return_line rl JOIN sales_order_line l ON l.id = rl.order_line_id
             WHERE l.document_id = $1::uuid)",
    )
    .bind(id(order))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    value.map(|c| c.to_string().parse().unwrap()).unwrap_or(0.0)
}

#[tokio::test]
async fn the_backlog_is_what_open_orders_have_still_to_deliver_and_invoice() {
    let t = TestApp::start().await;
    let s = Shop::new(&t).await;
    s.buy(&t, "2026-01-10", "5.00").await;
    let (status, order) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            json!({ "company_id": s.company, "doc_date": "2026-02-01", "body": {
                "customer_id": s.first, "lines": [{ "item_id": s.item,
                "warehouse_id": s.warehouse, "qty": "8", "unit_price": "10.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let order = run(&t, "sales_order.approve", json!({ "id": id(&order) })).await;
    let mut delivery = run(
        &t,
        "sales_order.deliver",
        json!({ "id": id(&order), "doc_date": "2026-02-05" }),
    )
    .await;
    delivery["body"]["lines"][0]["qty"] = json!("3");
    let (status, delivery) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_delivery/{}", id(&delivery)),
            json!({ "version": delivery["version"], "body": delivery["body"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{delivery}");
    run(&t, "sales_delivery.post", json!({ "id": id(&delivery) })).await;
    let kpis = run(
        &t,
        "analysis.kpis",
        json!({ "company_id": s.company, "as_of": "2026-02-28" }),
    )
    .await;
    assert_eq!(kpis["open_orders"], 1, "{kpis}");
    assert!(close(dec(&kpis["backlog_to_deliver"]), 50.0), "{kpis}");
    assert!(close(dec(&kpis["backlog_to_invoice"]), 30.0), "{kpis}");
    t.stop().await;
}

#[tokio::test]
async fn sales_by_site_item_group_and_currency_within_the_readers_rules() {
    let t = TestApp::start().await;
    let s = Shop::new(&t).await;
    // Each customer is served by its own site.
    let site = t
        .create(
            "dimension",
            json!({ "company_id": s.company, "code": "SITE", "name": "Site" }),
        )
        .await;
    let mut values = vec![];
    for (code, customer) in [("OHIO", &s.first), ("TEXAS", &s.second)] {
        let value = t
            .create(
                "dimension_value",
                json!({ "company_id": s.company, "dimension_id": site, "code": code,
                        "name": code }),
            )
            .await;
        t.create(
            "dimension_rule",
            json!({ "company_id": s.company, "scope_type": "customer", "scope_id": customer,
                    "value_id": value }),
        )
        .await;
        values.push(value);
    }
    let gadgets = t
        .create(
            "price_group",
            json!({ "kind": "item", "code": "GADGETS", "name": "Gadgets" }),
        )
        .await;
    t.create(
        "price_group_member",
        json!({ "group_id": gadgets, "member_id": s.item }),
    )
    .await;
    t.create(
        "exchange_rate",
        json!({ "from_currency": "CAD", "to_currency": "USD", "rate_date": "2026-01-01",
                "rate": "0.75" }),
    )
    .await;
    s.buy(&t, "2026-01-10", "5.00").await;
    s.sell(&t, &s.first, "2", "10.00", "2026-01-20", "2026-01-25")
        .await;
    // Four to the second in Canadian dollars: 40 CAD, 30 USD.
    let (status, order) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            json!({ "company_id": s.company, "doc_date": "2026-01-20", "body": {
                "customer_id": s.second, "currency": "CAD", "lines": [{ "item_id": s.item,
                "warehouse_id": s.warehouse, "qty": "4", "unit_price": "10.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let order = run(&t, "sales_order.approve", json!({ "id": id(&order) })).await;
    let delivery = run(
        &t,
        "sales_order.deliver",
        json!({ "id": id(&order), "doc_date": "2026-01-21" }),
    )
    .await;
    run(&t, "sales_delivery.post", json!({ "id": id(&delivery) })).await;
    let invoice = run(
        &t,
        "sales_order.invoice",
        json!({ "id": id(&order), "doc_date": "2026-01-26" }),
    )
    .await;
    run(&t, "sales_invoice.post", json!({ "id": id(&invoice) })).await;

    let range = json!({ "from": "2026-01-01", "to": "2026-01-31" });
    let mut by_site = range.clone();
    by_site["group_by"] = json!(["dimension"]);
    by_site["group_by_dimension_id"] = json!(site);
    let report = s.sales(&t, &t.admin, by_site).await;
    let ohio = row(&report, "dimension_value_code", "OHIO");
    assert!(close(dec(&ohio["net"]), 20.0), "{report}");
    let texas = row(&report, "dimension_value_code", "TEXAS");
    assert!(close(dec(&texas["net"]), 30.0), "{report}");
    assert!(close(dec(&texas["cost"]), 20.0), "{report}");

    let mut only_ohio = range.clone();
    only_ohio["dimension_value_ids"] = json!([values[0]]);
    let report = s.sales(&t, &t.admin, only_ohio).await;
    assert!(close(dec(&report["totals"]["net"]), 20.0), "{report}");

    let mut by_group = range.clone();
    by_group["group_by"] = json!(["item_group"]);
    let report = s.sales(&t, &t.admin, by_group).await;
    let g = row(&report, "item_group_code", "GADGETS");
    assert!(close(dec(&g["net"]), 50.0), "{report}");
    assert!(close(dec(&g["qty"]), 6.0), "{report}");

    let mut in_cad = range.clone();
    in_cad["currency"] = json!("CAD");
    let report = s.sales(&t, &t.admin, in_cad).await;
    assert_eq!(report["document_currency"], "CAD");
    assert!(close(dec(&report["totals"]["net"]), 30.0), "{report}");
    assert!(
        close(dec(&report["totals"]["net_document"]), 40.0),
        "{report}"
    );

    // A reader whose rules reach only the first customer's invoices.
    let (rep, _) = t
        .user_with_rules(
            "rep@example.com",
            json!([{ "allow": "sales_invoice:read",
                     "where": format!("record.summary.customer_id == '{}'", s.first) },
                   "customer:read", "stock_entry:read"]),
            Some(&s.company),
        )
        .await;
    let report = s.sales(&t, &rep, range.clone()).await;
    assert_eq!(report["rows"].as_array().unwrap().len(), 1, "{report}");
    assert!(close(dec(&report["totals"]["net"]), 20.0), "{report}");
    t.stop().await;
}
