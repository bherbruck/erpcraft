//! Printable documents: the built-in templates, templates of one's own,
//! and what a template can and cannot do.

mod common;

use std::time::Instant;

use axum::http::StatusCode;
use common::{Company, TestApp, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    item: String,
    warehouse: String,
    vendor: String,
    customer: String,
    vat: String,
}

async fn setup(t: &TestApp, customer_name: &str) -> Fixture {
    let c = t.company("ACME", "USD").await;
    let item = t
        .create(
            "item",
            json!({ "code": "WIDGET", "name": "Widget", "uom": "pcs" }),
        )
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main store" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C001", "name": customer_name }))
        .await;
    let mut account = std::collections::BTreeMap::new();
    for (code, kind) in [
        ("1200", "asset"),
        ("1300", "asset"),
        ("1400", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
        ("2300", "liability"),
        ("5000", "expense"),
        ("5100", "expense"),
        ("5200", "expense"),
    ] {
        let created = t
            .create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind }),
            )
            .await;
        account.insert(code, created);
    }
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id,
            "grni_account_id": account["2100"],
            "payable_account_id": account["2000"],
            "price_variance_account_id": account["5200"],
        }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id,
            "receivable_account_id": account["1200"],
            "revenue_account_id": c.revenue,
            "cogs_account_id": account["5000"],
        }),
    )
    .await;
    t.inventory_rule(&c.id, &account["1300"]).await;
    let vat = t
        .create(
            "tax_code",
            json!({ "code": "VAT19", "name": "VAT 19%", "rate": "19" }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({
            "company_id": c.id,
            "tax_code_id": vat,
            "collected_account_id": account["2300"],
            "paid_account_id": account["1400"],
        }),
    )
    .await;
    let f = Fixture {
        c,
        item,
        warehouse,
        vendor,
        customer,
        vat,
    };
    let stock = f
        .draft(
            t,
            "stock_adjustment",
            json!({
                "offset_account_id": account["5100"],
                "lines": [{ "item_id": f.item, "warehouse_id": f.warehouse, "qty": "100", "unit_cost": "2.00" }],
            }),
            None,
        )
        .await;
    f.run(t, "stock_adjustment.post", &stock).await;
    f
}

impl Fixture {
    async fn draft(&self, t: &TestApp, doc_type: &str, body: Value, memo: Option<&str>) -> Value {
        let (status, doc) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "memo": memo, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{doc}");
        doc
    }

    async fn run(&self, t: &TestApp, command: &str, doc: &Value) -> Value {
        let (status, body) = t.command(&t.admin, command, json!({ "id": id(doc) })).await;
        assert_eq!(status, StatusCode::OK, "{command}: {body}");
        body
    }

    async fn sales_order(&self, t: &TestApp, memo: Option<&str>) -> Value {
        let draft = self
            .draft(
                t,
                "sales_order",
                json!({
                    "customer_id": self.customer,
                    "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": "10", "unit_price": "5.00" }],
                }),
                memo,
            )
            .await;
        self.run(t, "sales_order.approve", &draft).await
    }
}

async fn render(
    t: &TestApp,
    token: &str,
    doc: &Value,
    template: Option<&str>,
) -> (StatusCode, Value) {
    let mut input = json!({ "doc_type": doc["doc_type"], "id": doc["id"] });
    if let Some(template) = template {
        input["template_id"] = json!(template);
    }
    t.command(token, "document.render", input).await
}

async fn html(t: &TestApp, doc: &Value) -> String {
    let (status, page) = render(t, &t.admin, doc, None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["html"].as_str().unwrap().to_owned()
}

/// What the template wrote: the body of the page the server wraps it in.
fn body_of(page: &Value) -> String {
    let html = page["html"].as_str().unwrap();
    let start = html.find("<body>\n").unwrap() + 7;
    let end = html.rfind("\n</body>").unwrap();
    html[start..end].to_owned()
}

/// With PRINT_DUMP set to a directory, keep a page there to look at.
fn dump(name: &str, page: &str) {
    if let Ok(dir) = std::env::var("PRINT_DUMP") {
        std::fs::write(format!("{dir}/{name}.html"), page).unwrap();
    }
}

fn has(page: &str, texts: &[&str]) {
    for text in texts {
        assert!(page.contains(text), "missing {text:?} in\n{page}");
    }
}

async fn template(t: &TestApp, input: Value) -> (StatusCode, Value) {
    t.post(&t.admin, "/api/entities/print_template", input)
        .await
}

#[tokio::test]
async fn each_built_in_template_prints_a_posted_document() {
    let t = TestApp::start().await;
    let f = setup(&t, "Buyer LLC").await;

    // A purchase order, with an amount no float survives.
    let draft = f
        .draft(
            &t,
            "purchase_order",
            json!({
                "vendor_id": f.vendor,
                "lines": [
                    { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "1", "unit_price": "123456789012.34" },
                    { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "2.5", "unit_price": "4.10" },
                ],
            }),
            Some("Deliver to the back door"),
        )
        .await;
    let order = f.run(&t, "purchase_order.approve", &draft).await;
    let (status, page) = render(&t, &t.admin, &order, None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["title"], "Purchase order PO-2026-00001");
    assert_eq!(page["template_id"], Value::Null);
    let po = page["html"].as_str().unwrap();
    has(
        po,
        &[
            "<!doctype html>",
            "@page",
            "ACME Inc",
            "PO-2026-00001",
            // A date reads the same in every country.
            "15 Mar 2026",
            "V001 Parts Inc",
            "WIDGET Widget",
            "MAIN Main store",
            "pcs",
            "<td class=\"num\">2.5</td>",
            "<td class=\"num\">4.10</td>",
            "<td class=\"num\">10.25</td>",
            "<td class=\"num\">123,456,789,012.34</td>",
            // The total is the order's summary: exact to the cent.
            "123,456,789,022.59",
            "Deliver to the back door",
        ],
    );

    // Its vendor invoice prints through the generic template: the order
    // line in full, the tax code by its code, so that it does not wrap.
    let lines: Vec<Value> = order["body"]["lines"].as_array().unwrap().to_vec();
    let receipt = f
        .draft(
            &t,
            "purchase_receipt",
            json!({ "purchase_order_id": order["id"], "lines": lines.iter().map(|l| json!({ "order_line_id": l["id"], "qty": l["qty"] })).collect::<Vec<_>>() }),
            None,
        )
        .await;
    f.run(&t, "purchase_receipt.post", &receipt).await;
    let bill = f
        .draft(
            &t,
            "vendor_invoice",
            json!({ "purchase_order_id": order["id"], "lines": lines.iter().map(|l| json!({ "order_line_id": l["id"], "qty": l["qty"], "unit_price": l["unit_price"], "tax_code_id": f.vat })).collect::<Vec<_>>() }),
            None,
        )
        .await;
    let bill = f.run(&t, "vendor_invoice.post", &bill).await;
    let vi = html(&t, &bill).await;
    has(
        &vi,
        &[
            "Vendor invoice",
            "15 Mar 2026",
            "<td>WIDGET Widget</td>",
            "<td class=\"short\">VAT19</td>",
        ],
    );
    assert!(!vi.contains("VAT 19%</td>"), "{vi}");
    dump("vendor_invoice", &vi);
    dump("purchase_order", po);

    // A sales order, its delivery and its invoice.
    let order = f.sales_order(&t, None).await;
    let so = html(&t, &order).await;
    has(
        &so,
        &[
            "SO-2026-00001",
            "C001 Buyer LLC",
            "WIDGET Widget",
            "<td class=\"num\">10</td>",
            "<td class=\"num\">5.00</td>",
            "<td class=\"num\">50.00</td>",
        ],
    );

    let line = &order["body"]["lines"][0]["id"];
    let delivery = f
        .draft(
            &t,
            "sales_delivery",
            json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "4" }] }),
            None,
        )
        .await;
    let delivery = f.run(&t, "sales_delivery.post", &delivery).await;
    let dn = html(&t, &delivery).await;
    has(
        &dn,
        &[
            "Sales delivery",
            "DN-2026-00001",
            "C001 Buyer LLC",
            "SO-2026-00001",
            // The line is shown by the item of the order line it delivers.
            "WIDGET Widget",
            "<td class=\"num\">4</td>",
            "pcs",
            "Received by",
        ],
    );
    dump("sales_order", &so);
    dump("sales_delivery", &dn);
    // What the goods cost is not for the customer.
    assert!(!dn.contains("8.00"), "{dn}");

    let invoice = f
        .draft(
            &t,
            "sales_invoice",
            json!({
                "sales_order_id": order["id"],
                "lines": [{ "order_line_id": line, "qty": "4", "unit_price": "5.00", "tax_code_id": f.vat }],
            }),
            None,
        )
        .await;
    let invoice = f.run(&t, "sales_invoice.post", &invoice).await;
    let si = html(&t, &invoice).await;
    has(
        &si,
        &[
            "Sales invoice",
            invoice["number"].as_str().unwrap(),
            "C001 Buyer LLC",
            "WIDGET Widget",
            // A narrow column shows the code, not the code and its name.
            "<td class=\"short\">VAT19</td>",
            "<td class=\"num\">20.00</td>",
            "<td class=\"num\">3.80</td>",
            "<td class=\"num\">23.80</td>",
            "Total USD",
        ],
    );

    dump("sales_invoice", &si);

    // Any other document prints from its type's description.
    let entry = t.draft(&t.admin, &f.c, "1234.5").await;
    let (status, page) = render(&t, &t.admin, &entry, None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["title"], "Journal entry (draft)");
    let je = page["html"].as_str().unwrap();
    dump("journal_entry", je);
    has(
        je,
        &[
            "Journal entry",
            "Draft",
            "1000 Cash",
            "4000 Revenue",
            "1,234.50",
            "Cash sale",
        ],
    );

    // The built-in source is there to start from.
    let (status, builtin) = t
        .command(
            &t.admin,
            "print_template.builtin",
            json!({ "doc_type": "sales_invoice" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{builtin}");
    assert_eq!(builtin["specific"], true);
    assert!(
        builtin["body"]
            .as_str()
            .unwrap()
            .contains("doc.summary.total_gross")
    );

    t.stop().await;
}

#[tokio::test]
async fn stored_templates_override_the_built_in_company_first() {
    let t = TestApp::start().await;
    let f = setup(&t, "Buyer LLC").await;
    let order = f.sales_order(&t, None).await;
    let other = t.company("OTHER", "USD").await;

    let (status, global) = template(
        &t,
        json!({
            "doc_type": "sales_order", "name": "Plain", "is_default": true, "active": true,
            "body": "<p>GLOBAL {{ doc.number }} {{ doc.summary.total_net | money }}</p>",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{global}");
    let (status, page) = render(&t, &t.admin, &order, None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(body_of(&page), "<p>GLOBAL SO-2026-00001 50.00</p>");
    assert_eq!(page["template_id"], global["id"]);

    // The company's own default beats the one for all companies.
    let (status, own) = template(
        &t,
        json!({
            "doc_type": "sales_order", "name": "ACME", "company_id": f.c.id,
            "is_default": true, "active": true,
            "body": "<p>ACME {{ company.name }}</p>",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{own}");
    let (_, page) = render(&t, &t.admin, &order, None).await;
    assert_eq!(body_of(&page), "<p>ACME ACME Inc</p>");
    assert_eq!(page["template_id"], own["id"]);

    // One default per document type and scope, enforced by the database.
    for company in [Value::Null, json!(f.c.id)] {
        let (status, body) = template(
            &t,
            json!({
                "doc_type": "sales_order", "name": "Second", "company_id": company,
                "is_default": true, "body": "<p>x</p>",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }
    // Another company's default is no business of this one.
    let (status, theirs) = template(
        &t,
        json!({
            "doc_type": "sales_order", "name": "Other", "company_id": other.id,
            "is_default": true, "body": "<p>OTHER</p>",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{theirs}");
    let (_, page) = render(&t, &t.admin, &order, None).await;
    assert_eq!(page["template_id"], own["id"]);
    let (status, body) = render(&t, &t.admin, &order, theirs["id"].as_str()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "template_id");

    // A template asked for by id, default or not.
    let (status, extra) = template(
        &t,
        json!({ "doc_type": "sales_order", "name": "Short", "body": "<p>SHORT</p>" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{extra}");
    let (_, page) = render(&t, &t.admin, &order, extra["id"].as_str()).await;
    assert_eq!(body_of(&page), "<p>SHORT</p>");
    // Not one of another document type.
    let (status, _) = template(
        &t,
        json!({ "doc_type": "purchase_order", "name": "PO", "body": "<p>PO</p>" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // An inactive default is passed over.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/print_template/{}", id(&own)),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, page) = render(&t, &t.admin, &order, None).await;
    assert_eq!(page["template_id"], global["id"]);

    t.stop().await;
}

#[tokio::test]
async fn a_template_is_checked_when_saved_and_bounded_when_rendered() {
    let t = TestApp::start().await;
    let f = setup(&t, "Buyer LLC").await;
    let order = f.sales_order(&t, None).await;

    let refused = |body: &str| json!({ "doc_type": "sales_order", "name": "Bad", "body": body });
    // Does not compile: the message says what and where.
    let (status, body) = template(&t, refused("<p>\n{% for x in %}\n</p>")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "body");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("line 2"), "{message}");
    let (status, body) = template(&t, refused("{% if x %}open")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Nothing that runs in the page, and no way out of escaping or into a file.
    for active in [
        "<script>alert(1)</script>",
        "<p onclick=\"alert(1)\">x</p>",
        "<a href=\"javascript:alert(1)\">x</a>",
        "<iframe src=\"https://example.com\"></iframe>",
        "<meta http-equiv=\"refresh\" content=\"0\">",
        "{% autoescape false %}{{ doc.memo }}{% endautoescape %}",
        "{% include 'other' %}",
    ] {
        let (status, body) = template(&t, refused(active)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{active}: {body}");
        assert_eq!(body["error"]["field"], "body", "{active}");
    }
    // Nor for a document type that does not exist.
    let (status, body) = template(
        &t,
        json!({ "doc_type": "nonsense", "name": "X", "body": "<p>x</p>" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "doc_type");
    // A change is checked as a creation is.
    let (_, good) = template(&t, refused("<p>ok</p>")).await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/print_template/{}", id(&good)),
            json!({ "body": "<script>x</script>" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(t.count("SELECT count(*) FROM print_template").await, 1);

    // A template that would run for ever is stopped, and soon.
    let (status, runaway) = template(
        &t,
        refused(
            "{% for i in range(100000) %}{% for j in range(100000) %}{{ j }}{% endfor %}{% endfor %}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{runaway}");
    let started = Instant::now();
    let (status, body) = render(&t, &t.admin, &order, runaway["id"].as_str()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("too much work"),
        "{body}"
    );
    assert!(started.elapsed().as_secs() < 10);
    // And so is one that would make a page without end.
    let (_, huge) = template(
        &t,
        refused("{% set s = 'x' * 100000 %}{% for i in range(100000) %}{{ s }}{% endfor %}"),
    )
    .await;
    let (status, body) = render(&t, &t.admin, &order, huge["id"].as_str()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // A link made from data loses its target when rendered.
    let (_, sneaky) = template(&t, refused("<a href=\"{{ doc.memo }}\">x</a>")).await;
    let draft = f
        .draft(
            &t,
            "sales_order",
            json!({
                "customer_id": f.customer,
                "lines": [{ "item_id": f.item, "warehouse_id": f.warehouse, "qty": "1", "unit_price": "1" }],
            }),
            Some("javascript:alert(1)"),
        )
        .await;
    let (status, page) = render(&t, &t.admin, &draft, sneaky["id"].as_str()).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(body_of(&page), "<a rel=\"noopener noreferrer\">x</a>");

    t.stop().await;
}

#[tokio::test]
async fn a_page_shows_only_what_the_caller_may_read_and_escapes_it() {
    let t = TestApp::start().await;
    let f = setup(&t, "<img src=x onerror=alert(1)>Evil & Co").await;
    let order = f
        .sales_order(&t, Some("<script>alert('memo')</script>"))
        .await;

    // Markup in a memo or a name is text on the page.
    let page = html(&t, &order).await;
    assert!(!page.contains("<script>alert"), "{page}");
    assert!(!page.contains("<img src=x"), "{page}");
    has(
        &page,
        &[
            "&lt;script&gt;alert(",
            "&lt;img src=x onerror=alert(1)&gt;Evil &amp; Co",
        ],
    );

    // Somebody who may not read the order cannot print it.
    let (outsider, _) = t
        .user_with_rules("outsider@example.com", json!(["item:read"]), Some(&f.c.id))
        .await;
    let (status, body) = render(&t, &outsider, &order, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Somebody who may read the order but not the item's name sees the
    // order without it.
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!([
                "sales_order:read",
                "item:read",
                { "deny": "item:read", "fields": ["name"] },
            ]),
            Some(&f.c.id),
        )
        .await;
    let (status, page) = render(&t, &clerk, &order, None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let page = page["html"].as_str().unwrap();
    has(page, &["SO-2026-00001", "<td>WIDGET</td>", "50.00"]);
    assert!(!page.contains("Widget"), "{page}");
    // The warehouse is not theirs to read, the customer neither: what the
    // order itself says of the customer is all there is.
    assert!(!page.contains("Main store"), "{page}");
    assert!(!page.contains("C001"), "{page}");

    // A template that goes looking for more finds nothing more.
    let (status, nosy) = template(
        &t,
        json!({
            "doc_type": "sales_order", "name": "Nosy",
            "body": "{% for k, r in refs | items %}[{{ r.record.name }}]{% endfor %}",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{nosy}");
    let (status, page) = render(&t, &clerk, &order, nosy["id"].as_str()).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(body_of(&page), "[]");

    t.stop().await;
}

#[tokio::test]
async fn an_unsaved_template_is_previewed_with_the_checks_of_saving() {
    let t = TestApp::start().await;
    let f = setup(&t, "Buyer LLC").await;
    let order = f.sales_order(&t, None).await;
    let preview = |body: &str| {
        let t = &t;
        let input = json!({ "doc_type": "sales_order", "id": order["id"], "body": body });
        async move { t.command(&t.admin, "print_template.preview", input).await }
    };

    let (status, page) = preview("<p>{{ doc.number }} on {{ doc.doc_date | date }}</p>").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(body_of(&page), "<p>SO-2026-00001 on 15 Mar 2026</p>");
    assert_eq!(page["template_id"], Value::Null);
    assert_eq!(t.count("SELECT count(*) FROM print_template").await, 0);

    for refused in [
        "<script>alert(1)</script>",
        "<p onclick=\"x()\">x</p>",
        "<a href=\"javascript:x\">x</a>",
        "{% for x in %}",
    ] {
        let (status, body) = preview(refused).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{refused}: {body}"
        );
        assert_eq!(body["error"]["field"], "body", "{refused}");
    }
    let started = Instant::now();
    let (status, body) = preview(
        "{% for i in range(100000) %}{% for j in range(100000) %}{{ j }}{% endfor %}{% endfor %}",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(started.elapsed().as_secs() < 10);

    // Previewing is writing templates without saving them.
    let (reader, _) = t
        .user_with_rules(
            "reader@example.com",
            json!(["sales_order:read", "print_template:read"]),
            None,
        )
        .await;
    let (status, _) = t
        .command(
            &reader,
            "print_template.preview",
            json!({ "doc_type": "sales_order", "id": order["id"], "body": "<p>x</p>" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    t.stop().await;
}

#[tokio::test]
async fn a_page_is_sanitized_whatever_the_template_or_the_data() {
    let t = TestApp::start().await;
    let f = setup(&t, "Buyer LLC").await;
    let order = f
        .sales_order(&t, Some("img src=x onerror=alert('memo')>"))
        .await;

    // Templates as they might be stored behind the save checks' back, each
    // a way past the old hand-written check.
    let attacks = [
        "<textarea><p title=\"</textarea><img src=x onerror=alert(1)>\"></textarea>",
        "<?x><img src=x onerror=alert(1)>",
        "</ x><img src=x onerror=alert(1)>",
        "<!x><img src=x onerror=alert(1)>",
        "<!-- a --!><img src=x onerror=alert(1)>",
        "<a href=\"&#106avascript:alert(1)\">x</a>",
        "<a href=\"java&#9script:alert(1)\">x</a>",
        "<svg><animate attributeName=\"href\" values=\"#;javascript:alert(1)\"/></svg>",
        "<style>body{background:url(https://evil.example/x)}</style>",
        "<meta http-equiv=\"refresh\" content=\"0;url=https://evil.example\">",
        // Escaping undone in the template, then data made into a tag.
        "{% set lt %}<{% endset %}{{ lt }}{{ doc.memo }}",
    ];
    for attack in attacks {
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO print_template (id, doc_type, name, body) VALUES ($1, 'sales_order', 'x', $2)",
        )
        .bind(id)
        .bind(attack)
        .execute(&t.pool)
        .await
        .unwrap();
        let (status, page) = render(&t, &t.admin, &order, Some(&id.to_string())).await;
        assert_eq!(status, StatusCode::OK, "{attack}: {page}");
        let body = body_of(&page).to_ascii_lowercase();
        for bad in [
            "onerror",
            "javascript",
            "<svg",
            "<style",
            "<meta",
            "<textarea",
            "evil.example",
        ] {
            assert!(!body.contains(bad), "{attack}\n -> {body}");
        }
        // The head is the server's: one stylesheet, no script, no meta
        // but the charset.
        let html = page["html"].as_str().unwrap();
        assert_eq!(html.matches("<style>").count(), 1);
        assert!(!html.contains("<script"));
        assert!(!html.contains("http-equiv"));
    }
    t.stop().await;
}

#[tokio::test]
async fn a_template_id_says_nothing_about_templates_one_may_not_use() {
    let t = TestApp::start().await;
    let f = setup(&t, "Buyer LLC").await;
    let order = f.sales_order(&t, None).await;
    let other = t.company("OTHER", "USD").await;
    let make = |input: Value| {
        let t = &t;
        async move {
            let (status, body) = template(t, input).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            body["id"].as_str().unwrap().to_owned()
        }
    };
    let theirs = make(json!({ "doc_type": "sales_order", "name": "Theirs", "company_id": other.id, "body": "<p>x</p>" })).await;
    let po = make(json!({ "doc_type": "purchase_order", "name": "PO", "body": "<p>x</p>" })).await;
    let off = make(
        json!({ "doc_type": "sales_order", "name": "Off", "active": false, "body": "<p>x</p>" }),
    )
    .await;
    let missing = uuid::Uuid::new_v4().to_string();
    let mut messages = vec![];
    for id in [&theirs, &po, &off, &missing] {
        let (status, body) = render(&t, &t.admin, &order, Some(id)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["field"], "template_id");
        messages.push(body["error"]["message"].as_str().unwrap().to_owned());
    }
    messages.dedup();
    assert_eq!(messages.len(), 1, "{messages:?}");
    t.stop().await;
}
