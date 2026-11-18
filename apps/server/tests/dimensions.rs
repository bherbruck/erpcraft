//! Dimensions on the general ledger: every entry carries a dimension set,
//! from the values a line states and the defaults of what it is about;
//! policies decide what combinations are allowed; reversals copy the set;
//! reports filter and group by it.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    expense: String,
    inventory: String,
    retained: String,
    site: String,
    cost_center: String,
    ohio: String,
    texas: String,
    assembly: String,
    sales: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let c = t.company("ACME", "USD").await;
    let account = |code: &'static str, kind: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": code, "account_type": kind }),
            )
            .await
        }
    };
    let expense = account("6000", "expense").await;
    let inventory = account("1300", "asset").await;
    let retained = account("3000", "equity").await;
    t.inventory_rule(&c.id, &inventory).await;
    let dimension = |code: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "dimension",
                json!({ "company_id": company, "code": code, "name": code }),
            )
            .await
        }
    };
    let site = dimension("SITE").await;
    let cost_center = dimension("COSTCENTER").await;
    let value = |dimension: String, code: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "dimension_value",
                json!({ "company_id": company, "dimension_id": dimension, "code": code, "name": code }),
            )
            .await
        }
    };
    let ohio = value(site.clone(), "OHIO").await;
    let texas = value(site.clone(), "TEXAS").await;
    let assembly = value(cost_center.clone(), "ASSY").await;
    let sales = value(cost_center.clone(), "SALES").await;
    Fixture {
        c,
        expense,
        inventory,
        retained,
        site,
        cost_center,
        ohio,
        texas,
        assembly,
        sales,
    }
}

impl Fixture {
    async fn rule(&self, t: &TestApp, scope: Option<(&str, &str)>, value: &str) -> String {
        let (scope_type, scope_id) = scope.unzip();
        t.create(
            "dimension_rule",
            json!({ "company_id": self.c.id, "scope_type": scope_type, "scope_id": scope_id,
                    "value_id": value }),
        )
        .await
    }

    /// Draft and post a journal entry of these lines.
    async fn journal(&self, t: &TestApp, lines: Value) -> (StatusCode, Value) {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/journal_entry",
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15",
                        "body": { "lines": lines } }),
            )
            .await;
        if status != StatusCode::CREATED {
            return (status, draft);
        }
        t.command(&t.admin, "journal_entry.post", json!({ "id": id(&draft) }))
            .await
    }

    async fn posted(&self, t: &TestApp, lines: Value) -> Value {
        let (status, doc) = self.journal(t, lines).await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }

    async fn report(&self, t: &TestApp, name: &str, input: Value) -> Value {
        let mut input = input;
        input["company_id"] = json!(self.c.id);
        let (status, body) = t.command(&t.admin, name, input).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }
}

/// The dimensions of each general ledger entry of a document's first
/// posting, by line.
fn dims(doc: &Value) -> Vec<Value> {
    doc["postings"][0]["entries"]["gl"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["dimensions"].clone())
        .collect()
}

fn sets(doc: &Value, posting: usize) -> Vec<Value> {
    doc["postings"][posting]["entries"]["gl"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["dimension_set_id"].clone())
        .collect()
}

#[tokio::test]
async fn lines_take_stated_values_then_the_defaults_of_what_they_are_about() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // No rule and nothing stated: no dimensions at all.
    let doc = f
        .posted(
            &t,
            json!([{ "account_id": f.expense, "debit": "10" },
                   { "account_id": f.c.cash, "credit": "10" }]),
        )
        .await;
    assert_eq!(dims(&doc), vec![Value::Null, Value::Null]);

    // The company's default, then a rule for an account, which is more
    // specific; a stated value wins over both.
    f.rule(&t, None, &f.ohio).await;
    f.rule(&t, Some(("account", &f.c.cash)), &f.texas).await;
    f.rule(&t, Some(("account", &f.expense)), &f.sales).await;
    let doc = f
        .posted(
            &t,
            json!([{ "account_id": f.expense, "debit": "10",
                     "dimension_value_ids": [f.assembly] },
                   { "account_id": f.c.cash, "credit": "10" },
                   { "account_id": f.expense, "debit": "5" },
                   { "account_id": f.c.cash, "credit": "5",
                     "dimension_value_ids": [f.ohio] }]),
        )
        .await;
    assert_eq!(
        dims(&doc),
        vec![
            json!("COSTCENTER:ASSY, SITE:OHIO"),
            json!("SITE:TEXAS"),
            json!("COSTCENTER:SALES, SITE:OHIO"),
            json!("SITE:OHIO"),
        ]
    );
    // The same combination is the same set.
    let again = f
        .posted(
            &t,
            json!([{ "account_id": f.expense, "debit": "1",
                     "dimension_value_ids": [f.assembly] },
                   { "account_id": f.c.cash, "credit": "1" }]),
        )
        .await;
    assert_eq!(sets(&again, 0)[0], sets(&doc, 0)[0]);
    // OHIO+ASSY, TEXAS, OHIO+SALES and OHIO.
    assert_eq!(t.count("SELECT count(*) FROM dimension_set").await, 4);

    // Two values of one dimension, a value of another company and an
    // inactive value are refused when the draft is saved.
    let (status, body) = f
        .journal(
            &t,
            json!([{ "account_id": f.expense, "debit": "1",
                     "dimension_value_ids": [f.ohio, f.texas] },
                   { "account_id": f.c.cash, "credit": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("more than one value for the dimension SITE"),
        "{body}"
    );
    assert_eq!(body["error"]["line"], 1);
    let other = t.company("OTHER", "USD").await;
    let foreign_dimension = t
        .create(
            "dimension",
            json!({ "company_id": other.id, "code": "SITE", "name": "Site" }),
        )
        .await;
    let foreign = t
        .create(
            "dimension_value",
            json!({ "company_id": other.id, "dimension_id": foreign_dimension,
                    "code": "OHIO", "name": "Ohio" }),
        )
        .await;
    let (status, body) = f
        .journal(
            &t,
            json!([{ "account_id": f.expense, "debit": "1", "dimension_value_ids": [foreign] },
                   { "account_id": f.c.cash, "credit": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/dimension_value/{}", f.sales),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f
        .journal(
            &t,
            json!([{ "account_id": f.expense, "debit": "1", "dimension_value_ids": [f.sales] },
                   { "account_id": f.c.cash, "credit": "1" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("inactive")
    );
    // A default that points to an inactive value gives nothing.
    let doc = f
        .posted(
            &t,
            json!([{ "account_id": f.expense, "debit": "2" },
                   { "account_id": f.c.cash, "credit": "2" }]),
        )
        .await;
    assert_eq!(dims(&doc)[0], json!("SITE:OHIO"));
    // A rule's value must be the rule's company's, and a code does not
    // change once made.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/dimension_rule",
            json!({ "company_id": f.c.id, "value_id": foreign }),
        )
        .await;
    assert_ne!(status, StatusCode::CREATED);
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/dimension/{}", f.site),
            json!({ "code": "PLANT" }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);
    t.stop().await;
}

#[tokio::test]
async fn a_warehouse_rule_beats_the_account_and_reversals_copy_the_set() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let houston = t
        .create(
            "warehouse",
            json!({ "company_id": f.c.id, "code": "HOU", "name": "Houston" }),
        )
        .await;
    f.rule(&t, None, &f.ohio).await;
    f.rule(&t, Some(("account", &f.inventory)), &f.ohio).await;
    f.rule(&t, Some(("warehouse", &houston)), &f.texas).await;
    f.rule(&t, Some(("item", &item)), &f.assembly).await;

    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/stock_adjustment",
            json!({ "company_id": f.c.id, "doc_date": "2026-03-15", "body": {
                "offset_account_id": f.expense,
                "lines": [{ "item_id": item, "warehouse_id": houston, "qty": "10", "unit_cost": "3" }],
            }}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, doc) = t
        .command(
            &t.admin,
            "stock_adjustment.post",
            json!({ "id": id(&draft) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    // Both sides of the movement are about the warehouse and the item.
    assert_eq!(
        dims(&doc),
        vec![
            json!("COSTCENTER:ASSY, SITE:TEXAS"),
            json!("COSTCENTER:ASSY, SITE:TEXAS")
        ]
    );
    let set = sets(&doc, 0);

    let (status, reversed) = t
        .command(
            &t.admin,
            "stock_adjustment.reverse",
            json!({ "id": id(&draft) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reversed}");
    let postings = reversed["postings"].as_array().unwrap();
    assert_eq!(postings.len(), 2);
    let reversal = postings
        .iter()
        .position(|p| !p["reverses_id"].is_null())
        .unwrap();
    assert_eq!(sets(&reversed, reversal), set);
    t.stop().await;
}

#[tokio::test]
async fn policies_require_allow_and_refuse_combinations() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // A cost center on every expense.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "gl.dimensions", "effect": "refuse",
                "condition": "facts.account_type == 'expense' && !('COSTCENTER' in facts.dimensions)",
                "message": "an expense needs a cost center", "priority": 10 }),
    )
    .await;
    // Sales is not a cost center of Texas.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "gl.dimensions", "effect": "refuse",
                "condition": "'SITE:TEXAS' in facts.values && 'COSTCENTER:SALES' in facts.values",
                "priority": 20 }),
    )
    .await;
    let (status, body) = f
        .journal(
            &t,
            json!([{ "account_id": f.c.cash, "credit": "4" },
                   { "account_id": f.expense, "debit": "4" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("an expense needs a cost center"),
        "{message}"
    );
    assert_eq!(body["error"]["line"], 2, "{body}");

    let (status, body) = f
        .journal(
            &t,
            json!([{ "account_id": f.expense, "debit": "4",
                     "dimension_value_ids": [f.texas, f.sales] },
                   { "account_id": f.c.cash, "credit": "4" }]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("COSTCENTER:SALES, SITE:TEXAS"),
        "{body}"
    );

    let doc = f
        .posted(
            &t,
            json!([{ "account_id": f.expense, "debit": "4",
                     "dimension_value_ids": [f.ohio, f.sales] },
                   { "account_id": f.c.cash, "credit": "4" }]),
        )
        .await;
    // A reversal is not asked: it takes back what was allowed.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "gl.dimensions", "effect": "refuse",
                "condition": "'SITE:OHIO' in facts.values", "priority": 5 }),
    )
    .await;
    let (status, body) = t
        .command(&t.admin, "journal_entry.reverse", json!({ "id": id(&doc) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn reports_filter_and_group_by_dimension() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Ohio sells for 100 and spends 30; Texas sells for 50 and spends 45.
    for (site, sold, spent) in [(&f.ohio, "100", "30"), (&f.texas, "50", "45")] {
        f.posted(
            &t,
            json!([{ "account_id": f.c.cash, "debit": sold, "dimension_value_ids": [site] },
                   { "account_id": f.c.revenue, "credit": sold, "dimension_value_ids": [site] },
                   { "account_id": f.expense, "debit": spent,
                     "dimension_value_ids": [site, f.assembly] },
                   { "account_id": f.c.cash, "credit": spent, "dimension_value_ids": [site] }]),
        )
        .await;
    }
    // One entry without dimensions.
    f.posted(
        &t,
        json!([{ "account_id": f.c.cash, "debit": "7" },
               { "account_id": f.retained, "credit": "7" }]),
    )
    .await;

    let texas = f
        .report(
            &t,
            "gl.income_statement",
            json!({ "from": "2026-01-01", "to": "2026-12-31", "dimension_value_ids": [f.texas] }),
        )
        .await;
    assert_eq!(dec(&texas["net_income"]), 5.0);

    let by_site = f
        .report(
            &t,
            "gl.income_statement",
            json!({ "from": "2026-01-01", "to": "2026-12-31", "group_by_dimension_id": f.site }),
        )
        .await;
    assert_eq!(dec(&by_site["net_income"]), 75.0);
    let groups = by_site["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{by_site}");
    assert_eq!(groups[0]["dimension_value_code"], "OHIO");
    assert_eq!(dec(&groups[0]["net_income"]), 70.0);
    assert_eq!(dec(&groups[1]["total_expenses"]), 45.0);
    assert_eq!(by_site["income"]["rows"].as_array().unwrap().len(), 2);

    // Two dimensions: Ohio and the assembly cost center.
    let narrow = f
        .report(
            &t,
            "gl.trial_balance",
            json!({ "dimension_value_ids": [f.ohio, f.assembly] }),
        )
        .await;
    assert_eq!(dec(&narrow["total_debit"]), 30.0);
    // Two values of one dimension: either.
    let both = f
        .report(
            &t,
            "gl.trial_balance",
            json!({ "dimension_value_ids": [f.ohio, f.texas] }),
        )
        .await;
    assert_eq!(dec(&both["total_debit"]), 225.0);
    let grouped = f
        .report(
            &t,
            "gl.trial_balance",
            json!({ "group_by_dimension_id": f.cost_center }),
        )
        .await;
    let expense_rows: Vec<&Value> = grouped["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["code"] == "6000")
        .collect();
    assert_eq!(expense_rows.len(), 1);
    assert_eq!(expense_rows[0]["dimension_value_code"], "ASSY");
    // The whole ledger still balances, and so do the grouped totals.
    assert_eq!(grouped["total_debit"], grouped["total_credit"]);

    let sheet = f
        .report(
            &t,
            "gl.balance_sheet",
            json!({ "group_by_dimension_id": f.site }),
        )
        .await;
    assert_eq!(sheet["total_assets"], sheet["total_liabilities_and_equity"]);
    let groups = sheet["groups"].as_array().unwrap();
    // Entries without a site come first.
    assert!(groups[0]["dimension_value_id"].is_null(), "{sheet}");
    assert_eq!(dec(&groups[1]["total_assets"]), 70.0);
    assert_eq!(dec(&groups[2]["net_income"]), 5.0);

    let ledger = f
        .report(
            &t,
            "gl.account_ledger",
            json!({ "account_id": f.c.cash, "dimension_value_ids": [f.texas],
                    "group_by_dimension_id": f.site }),
        )
        .await;
    assert_eq!(ledger["total"], 2);
    assert_eq!(dec(&ledger["closing_balance"]), 5.0);
    assert_eq!(ledger["entries"][0]["dimensions"], "SITE:TEXAS");
    assert_eq!(ledger["groups"].as_array().unwrap().len(), 1);

    // A value of another company narrows nothing: it is refused.
    let (status, _) = t
        .command(
            &t.admin,
            "gl.trial_balance",
            json!({ "company_id": f.c.id, "dimension_value_ids": [uuid::Uuid::now_v7()] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The year-end close keeps each site's result under its site.
    let (status, close) = t
        .post(
            &t.admin,
            "/api/documents/year_end_close",
            json!({ "company_id": f.c.id, "doc_date": "2026-12-31", "body": {
                "fiscal_period_id": f.c.period, "retained_earnings_account_id": f.retained } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{close}");
    let (status, close) = t
        .command(&t.admin, "year_end_close.post", json!({ "id": id(&close) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{close}");
    let sheet = f
        .report(
            &t,
            "gl.balance_sheet",
            json!({ "dimension_value_ids": [f.ohio] }),
        )
        .await;
    assert_eq!(dec(&sheet["net_income"]), 0.0);
    assert_eq!(dec(&sheet["equity"]["total"]), 70.0);
    assert_eq!(sheet["total_assets"], sheet["total_liabilities_and_equity"]);
    t.stop().await;
}
