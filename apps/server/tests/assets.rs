//! Fixed assets: capitalised from the ledger, depreciated month by month,
//! disposed of, and a register that ties to the ledger.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{Company, TestApp, dec};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    account: BTreeMap<&'static str, String>,
    /// Office equipment: straight line over 12 months.
    equipment: String,
    /// Machinery: 25 % declining balance over 10 years.
    machinery: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("ACME", "USD").await;
    let mut account = BTreeMap::new();
    for (code, kind) in [
        ("1500", "asset"),
        ("1510", "asset"),
        ("1600", "asset"),
        ("1610", "asset"),
        ("3000", "equity"),
        ("4900", "income"),
        ("6030", "expense"),
        ("6900", "expense"),
    ] {
        let id = t
            .create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind }),
            )
            .await;
        account.insert(code, id);
    }
    let equipment = t
        .create(
            "asset_class",
            json!({ "company_id": c.id, "code": "EQ", "name": "Office equipment",
                    "method": "straight_line", "useful_life_months": 12 }),
        )
        .await;
    let machinery = t
        .create(
            "asset_class",
            json!({ "company_id": c.id, "code": "MACH", "name": "Machinery",
                    "method": "declining_balance", "declining_rate": "25",
                    "useful_life_months": 120 }),
        )
        .await;
    // Each class holds its cost and depreciation on its own accounts; both
    // share the expense, gain and loss accounts, which are the company's
    // defaults.
    for (class, cost, accumulated) in [(&equipment, "1500", "1510"), (&machinery, "1600", "1610")] {
        for (purpose, code) in [
            ("asset_cost", cost),
            ("accumulated_depreciation", accumulated),
        ] {
            t.create(
                "account_rule",
                json!({ "company_id": c.id, "purpose": purpose, "scope_type": "asset_class",
                        "scope_id": class, "account_id": account[code] }),
            )
            .await;
        }
    }
    for (purpose, code) in [
        ("depreciation_expense", "6030"),
        ("asset_disposal_gain", "4900"),
        ("asset_disposal_loss", "6900"),
    ] {
        t.create(
            "account_rule",
            json!({ "company_id": c.id, "purpose": purpose, "account_id": account[code] }),
        )
        .await;
    }
    Fixture {
        c,
        account,
        equipment,
        machinery,
    }
}

impl Fixture {
    /// A posted journal entry debiting `debit` and crediting cash; returns
    /// it with its lines.
    async fn bought(&self, t: &TestApp, debit: &str, amount: &str, date: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/journal_entry",
                json!({ "company_id": self.c.id, "doc_date": date, "body": { "lines": [
                    { "account_id": self.account[debit], "debit": amount },
                    { "account_id": self.c.cash, "credit": amount },
                ] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, posted) = t
            .command(&t.admin, "journal_entry.post", json!({ "id": draft["id"] }))
            .await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        posted
    }

    /// An asset of `class`, bought for `amount` on `date` and in service
    /// from `in_service`.
    async fn asset(
        &self,
        t: &TestApp,
        code: &str,
        class: &str,
        amount: &str,
        date: &str,
        in_service: &str,
    ) -> String {
        let id = t
            .create(
                "asset",
                json!({ "company_id": self.c.id, "class_id": class, "code": code, "name": code }),
            )
            .await;
        let cost_account = if class == self.equipment {
            "1500"
        } else {
            "1600"
        };
        let entry = self.bought(t, cost_account, amount, date).await;
        let (status, asset) = t
            .command(
                &t.admin,
                "asset.capitalise",
                json!({ "id": id, "document_id": entry["id"],
                        "line_id": entry["body"]["lines"][0]["id"],
                        "in_service_date": in_service }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{asset}");
        assert_eq!(asset["status"], "in_service");
        assert_eq!(dec(&asset["cost"]), amount.parse::<f64>().unwrap());
        id
    }

    /// Draft a depreciation run up to `period_end` and post it.
    async fn run(&self, t: &TestApp, period_end: &str) -> (StatusCode, Value) {
        let draft = self.draft(t, period_end).await;
        t.command(
            &t.admin,
            "depreciation_run.post",
            json!({ "id": draft["id"] }),
        )
        .await
    }

    async fn draft(&self, t: &TestApp, period_end: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/depreciation_run",
                json!({ "company_id": self.c.id, "doc_date": period_end, "body": {} }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        draft
    }

    /// Debit less credit of an account in the ledger.
    async fn balance(&self, t: &TestApp, code: &str) -> f64 {
        let sql = format!(
            "SELECT COALESCE(sum(debit - credit), 0)::float8 FROM gl_entry
             WHERE account_id = '{}'",
            self.account[code]
        );
        sqlx::query_scalar(&sql).fetch_one(&t.pool).await.unwrap()
    }

    async fn register(&self, t: &TestApp, as_of: &str) -> Value {
        let (status, register) = t
            .command(
                &t.admin,
                "asset.register",
                json!({ "company_id": self.c.id, "as_of": as_of }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{register}");
        register
    }

    async fn dispose(
        &self,
        t: &TestApp,
        asset: &str,
        date: &str,
        proceeds: &str,
    ) -> (StatusCode, Value) {
        let mut body = json!({ "asset_id": asset, "proceeds": proceeds });
        if proceeds != "0" {
            body["proceeds_account_id"] = json!(self.c.cash);
        }
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/asset_disposal",
                json!({ "company_id": self.c.id, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        t.command(
            &t.admin,
            "asset_disposal.post",
            json!({ "id": draft["id"] }),
        )
        .await
    }
}

async fn schedule(t: &TestApp, asset: &str) -> Vec<Value> {
    let (status, schedule) = t
        .command(&t.admin, "asset.schedule", json!({ "id": asset }))
        .await;
    assert_eq!(status, StatusCode::OK, "{schedule}");
    schedule["months"].as_array().unwrap().clone()
}

async fn status(t: &TestApp, asset: &str) -> String {
    let (_, asset) = t
        .get(&t.admin, &format!("/api/entities/asset/{asset}"))
        .await;
    asset["status"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn straight_line_and_declining_balance_over_a_year() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // In service mid-January: by the company's convention (full month by
    // default) January counts in full.
    let desk = f
        .asset(
            &t,
            "DESK",
            &f.equipment,
            "1000.00",
            "2026-01-10",
            "2026-01-15",
        )
        .await;
    // From April: nine months of the first year.
    let press = f
        .asset(
            &t,
            "PRESS",
            &f.machinery,
            "10000.00",
            "2026-03-20",
            "2026-04-01",
        )
        .await;

    // The schedule: 83.33 a month, December takes the remainder.
    let months = schedule(&t, &desk).await;
    assert_eq!(months.len(), 12);
    assert_eq!(months[0]["month"], "2026-01-01");
    assert!(months[..11].iter().all(|m| dec(&m["amount"]) == 83.33));
    assert_eq!(dec(&months[11]["amount"]), 83.37);
    assert_eq!(dec(&months[11]["book_value"]), 0.0);
    assert!(months.iter().all(|m| m["posted"] == false));

    // A draft shows what it would post.
    let draft = f.draft(&t, "2026-01-31").await;
    assert_eq!(draft["body"]["lines"].as_array().unwrap().len(), 1);
    assert_eq!(dec(&draft["body"]["lines"][0]["amount"]), 83.33);
    let (status, january) = t
        .command(
            &t.admin,
            "depreciation_run.post",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{january}");
    assert_eq!(january["number"], "DEP-2026-00001");
    assert_eq!(f.balance(&t, "6030").await, 83.33);
    assert_eq!(f.balance(&t, "1510").await, -83.33);

    // The rest of the year at once: each asset's months since its last run.
    let (status, december) = f.run(&t, "2026-12-31").await;
    assert_eq!(status, StatusCode::OK, "{december}");
    let lines = december["body"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2, "{december}");
    assert_eq!(lines[0]["asset_id"], desk.as_str());
    assert_eq!(lines[0]["months"], 11);
    assert_eq!(dec(&lines[0]["amount"]), 916.67);
    assert_eq!(lines[1]["asset_id"], press.as_str());
    assert_eq!(lines[1]["months"], 9);
    // 25 % of 10,000 for nine months; December takes the remainder.
    assert_eq!(dec(&lines[1]["amount"]), 1875.0);
    let press_months = schedule(&t, &press).await;
    assert_eq!(dec(&press_months[0]["amount"]), 208.33);
    assert_eq!(press_months[8]["month"], "2026-12-01");
    assert_eq!(dec(&press_months[8]["amount"]), 208.36);
    assert!(press_months[..9].iter().all(|m| m["posted"] == true));
    assert_eq!(press_months[9]["posted"], false);
    // 2027: 25 % of the 8,125 left.
    let next_year: f64 = press_months[9..21].iter().map(|m| dec(&m["amount"])).sum();
    assert!((next_year - 2031.25).abs() < 0.001, "{next_year}");

    assert_eq!(f.balance(&t, "6030").await, 2875.0);
    assert_eq!(f.balance(&t, "1510").await, -1000.0);
    assert_eq!(f.balance(&t, "1610").await, -1875.0);
    assert_eq!(status_of(&t, &desk).await, "fully_depreciated");
    assert_eq!(status_of(&t, &press).await, "in_service");

    // Each asset is a line of its own in the ledger, with the run's line as
    // its source.
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM gl_entry g JOIN posting p ON p.id = g.posting_id
             WHERE p.document_id = '{}' AND g.source_line_id IS NOT NULL",
            december["id"].as_str().unwrap()
        ))
        .await,
        4
    );
    t.stop().await;
}

async fn status_of(t: &TestApp, asset: &str) -> String {
    status(t, asset).await
}

#[tokio::test]
async fn runs_go_forward_once_and_reverse_exactly() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let desk = f
        .asset(
            &t,
            "DESK",
            &f.equipment,
            "1200.00",
            "2026-01-05",
            "2026-01-05",
        )
        .await;

    // A run depreciates whole months.
    let (status, body) = f.run(&t, "2026-02-15").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "doc_date");

    let (status, body) = f.run(&t, "2026-03-31").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(dec(&body["body"]["lines"][0]["amount"]), 300.0);
    let march = body;

    // A second run for the same period posts nothing more.
    let entries = t.count("SELECT count(*) FROM gl_entry").await;
    let (status, body) = f.run(&t, "2026-03-31").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has depreciated 2026-03-31 already"),
        "{body}"
    );
    // Nor one before it.
    let (status, body) = f.run(&t, "2026-02-28").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("a run goes after the last one"),
        "{body}"
    );
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, entries);
    assert_eq!(f.balance(&t, "6030").await, 300.0);

    // The database holds to it whatever the code does.
    let doc: String = sqlx::query_scalar(
        "SELECT id::text FROM document WHERE doc_type = 'depreciation_run' AND status = 'draft' LIMIT 1",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let refused = sqlx::query(&format!(
        "INSERT INTO depreciation_period (company_id, period_end, document_id)
         VALUES ('{}', '2026-01-31', '{doc}')",
        f.c.id
    ))
    .execute(&t.pool)
    .await;
    assert!(refused.is_err());
    let refused = sqlx::query(&format!(
        "INSERT INTO asset_depreciation (id, company_id, asset_id, document_id, month, posting_date, amount)
         VALUES (gen_random_uuid(), '{}', '{desk}', '{}', '2026-02-01', '2026-02-28', 1)",
        f.c.id,
        march["id"].as_str().unwrap()
    ))
    .execute(&t.pool)
    .await;
    assert!(refused.is_err(), "a month is depreciated once");

    // Once depreciated, what the plan was made of no longer changes.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/asset/{desk}"),
            json!({ "useful_life_months": 24 }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // April, then reversing March is refused: April is later.
    let (status, april) = f.run(&t, "2026-04-30").await;
    assert_eq!(status, StatusCode::OK, "{april}");
    let (status, body) = t
        .command(
            &t.admin,
            "depreciation_run.reverse",
            json!({ "id": march["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Reversing April undoes it exactly; the next run depreciates April
    // again.
    let (status, body) = t
        .command(
            &t.admin,
            "depreciation_run.reverse",
            json!({ "id": april["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "reversed");
    assert_eq!(f.balance(&t, "6030").await, 300.0);
    assert_eq!(f.balance(&t, "1510").await, -300.0);
    let months = schedule(&t, &desk).await;
    assert_eq!(months[3]["posted"], false);
    let (status, again) = f.run(&t, "2026-04-30").await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(dec(&again["body"]["lines"][0]["amount"]), 100.0);
    assert_eq!(f.balance(&t, "6030").await, 400.0);
    t.stop().await;
}

#[tokio::test]
async fn the_month_of_service_follows_the_company_convention() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Depreciation starts the month after the asset goes into service.
    let settings = t
        .create(
            "asset_settings",
            json!({ "company_id": f.c.id, "convention": "next_month" }),
        )
        .await;
    let desk = f
        .asset(
            &t,
            "DESK",
            &f.equipment,
            "1200.00",
            "2026-01-15",
            "2026-01-15",
        )
        .await;
    let (status, january) = f.run(&t, "2026-01-31").await;
    assert_eq!(status, StatusCode::OK, "{january}");
    assert!(january["body"]["lines"].as_array().unwrap().is_empty());
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM posting WHERE document_id = '{}'",
            january["id"].as_str().unwrap()
        ))
        .await,
        0,
        "a run with nothing to depreciate posts nothing"
    );
    let months = schedule(&t, &desk).await;
    assert_eq!(months[0]["month"], "2026-02-01");
    assert_eq!(months[11]["month"], "2027-01-01");
    let (status, february) = f.run(&t, "2026-02-28").await;
    assert_eq!(status, StatusCode::OK, "{february}");
    assert_eq!(february["body"]["lines"][0]["months"], 1);
    assert_eq!(dec(&february["body"]["lines"][0]["amount"]), 100.0);

    // Once the company has depreciated, its convention stays.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/asset_settings/{settings}"),
            json!({ "convention": "full_month" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn disposal_with_a_gain_and_with_a_loss() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let sold = f
        .asset(
            &t,
            "SOLD",
            &f.equipment,
            "1200.00",
            "2026-01-02",
            "2026-01-02",
        )
        .await;
    let scrapped = f
        .asset(
            &t,
            "SCRAP",
            &f.equipment,
            "600.00",
            "2026-01-02",
            "2026-01-02",
        )
        .await;
    let (status, body) = f.run(&t, "2026-03-31").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Not before what has been depreciated.
    let (status, body) = f.dispose(&t, &sold, "2026-03-15", "1000.00").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Sold at the end of April for 1,000: April is depreciated first (100),
    // leaving a book value of 800 and a gain of 200.
    let cash_before = sqlx::query_scalar::<_, f64>(&format!(
        "SELECT COALESCE(sum(debit - credit), 0)::float8 FROM gl_entry WHERE account_id = '{}'",
        f.c.cash
    ))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let (status, sale) = f.dispose(&t, &sold, "2026-04-30", "1000.00").await;
    assert_eq!(status, StatusCode::OK, "{sale}");
    let body = &sale["body"];
    assert_eq!(dec(&body["depreciation"]), 100.0);
    assert_eq!(dec(&body["accumulated"]), 400.0);
    assert_eq!(dec(&body["book_value"]), 800.0);
    assert_eq!(dec(&body["gain"]), 200.0);
    assert_eq!(f.balance(&t, "4900").await, -200.0);
    assert_eq!(status_of(&t, &sold).await, "disposed");
    let cash_after = sqlx::query_scalar::<_, f64>(&format!(
        "SELECT COALESCE(sum(debit - credit), 0)::float8 FROM gl_entry WHERE account_id = '{}'",
        f.c.cash
    ))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(cash_after - cash_before, 1000.0);

    // Scrapped for nothing in mid-April: April has not ended, so the book
    // value after March (450) is the loss.
    let (status, scrap) = f.dispose(&t, &scrapped, "2026-04-15", "0").await;
    assert_eq!(status, StatusCode::OK, "{scrap}");
    assert_eq!(dec(&scrap["body"]["depreciation"]), 0.0);
    assert_eq!(dec(&scrap["body"]["gain"]), -450.0);
    assert_eq!(f.balance(&t, "6900").await, 450.0);

    // Both are out of the books: cost and depreciation accounts are empty.
    assert_eq!(f.balance(&t, "1500").await, 0.0);
    assert_eq!(f.balance(&t, "1510").await, 0.0);
    // A disposed asset is not depreciated again, nor disposed of twice.
    let (status, may) = f.run(&t, "2026-05-31").await;
    assert_eq!(status, StatusCode::OK, "{may}");
    assert!(may["body"]["lines"].as_array().unwrap().is_empty());
    let (status, body) = f.dispose(&t, &sold, "2026-05-31", "0").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Reversing the sale brings the asset back as it was.
    let (status, body) = t
        .command(
            &t.admin,
            "asset_disposal.reverse",
            json!({ "id": sale["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(status_of(&t, &sold).await, "in_service");
    assert_eq!(f.balance(&t, "1500").await, 1200.0);
    assert_eq!(f.balance(&t, "1510").await, -300.0);
    assert_eq!(f.balance(&t, "4900").await, 0.0);
    // The next run catches up April and May.
    let (status, june) = f.run(&t, "2026-06-30").await;
    assert_eq!(status, StatusCode::OK, "{june}");
    assert_eq!(june["body"]["lines"][0]["months"], 3);
    t.stop().await;
}

#[tokio::test]
async fn the_register_ties_to_the_ledger() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.asset(
        &t,
        "DESK",
        &f.equipment,
        "1200.00",
        "2026-01-05",
        "2026-01-05",
    )
    .await;
    // A machine taken over from the old system: bought in 2024 for 3,600,
    // depreciated by 2,400 up to the end of 2025, three years' life.
    let press = t
        .create(
            "asset",
            json!({ "company_id": f.c.id, "class_id": f.machinery, "code": "PRESS",
                    "name": "Press", "method": "straight_line", "useful_life_months": 36 }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "asset.take_over",
            json!({ "id": press, "cost": "3600.00", "accumulated_depreciation": "2400.00",
                    "acquisition_date": "2023-12-20", "in_service_date": "2024-01-01",
                    "depreciated_through": "2025-12-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["recorded_on"], "2026-01-01");
    // Taking over posts nothing, so until the opening entry the ledger lacks it.
    let register = f.register(&t, "2026-01-31").await;
    assert_eq!(register["ties"], false);
    let press_cost = register["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["code"] == "1600")
        .unwrap()
        .clone();
    assert_eq!(dec(&press_cost["register"]), 3600.0);
    assert_eq!(dec(&press_cost["ledger"]), 0.0);
    assert_eq!(dec(&press_cost["difference"]), -3600.0);

    // The opening entry.
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({ "company_id": f.c.id, "doc_date": "2026-01-01", "body": { "lines": [
                { "account_id": f.account["1600"], "debit": "3600.00" },
                { "account_id": f.account["1610"], "credit": "2400.00" },
                { "account_id": f.account["3000"], "credit": "1200.00" },
            ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, _) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": draft["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = f.run(&t, "2026-06-30").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The taken over machine goes on at 100 a month over the year left.
    let lines = body["body"]["lines"].as_array().unwrap();
    let press_line = lines
        .iter()
        .find(|l| l["asset_id"] == press.as_str())
        .unwrap();
    assert_eq!(dec(&press_line["amount"]), 600.0);

    let register = f.register(&t, "2026-06-30").await;
    assert_eq!(register["ties"], true, "{register}");
    assert_eq!(dec(&register["total_cost"]), 4800.0);
    assert_eq!(dec(&register["total_accumulated"]), 600.0 + 3000.0);
    assert_eq!(dec(&register["total_book_value"]), 1200.0);
    assert_eq!(dec(&register["total_depreciation_this_year"]), 1200.0);
    let rows = register["assets"].as_array().unwrap();
    assert_eq!(rows[0]["code"], "DESK");
    assert_eq!(dec(&rows[0]["book_value"]), 600.0);
    assert_eq!(rows[1]["code"], "PRESS");
    assert_eq!(dec(&rows[1]["accumulated"]), 3000.0);
    for account in register["accounts"].as_array().unwrap() {
        assert_eq!(dec(&account["difference"]), 0.0, "{account}");
    }
    // As of an earlier day, before the run: it ties as well.
    let earlier = f.register(&t, "2026-03-31").await;
    assert_eq!(earlier["ties"], true, "{earlier}");
    assert_eq!(dec(&earlier["total_accumulated"]), 2400.0);
    // And the register is a report.
    let (_, meta) = t.get(&t.admin, "/api/meta").await;
    let command = meta["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "asset.register")
        .unwrap()
        .clone();
    assert_eq!(command["report"]["title"], "Fixed asset register");
    t.stop().await;
}

#[tokio::test]
async fn capitalising_is_checked() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let chair = t
        .create(
            "asset",
            json!({ "company_id": f.c.id, "class_id": f.equipment, "code": "CHAIR", "name": "Chair" }),
        )
        .await;
    let (_, read) = t
        .get(&t.admin, &format!("/api/entities/asset/{chair}"))
        .await;
    // The class's method and life unless the asset says otherwise.
    assert_eq!(read["method"], "straight_line");
    assert_eq!(read["useful_life_months"], 12);
    assert_eq!(read["status"], "planned");

    // A line that did not post to the class's cost account.
    let wrong = f.bought(&t, "1600", "300.00", "2026-02-01").await;
    let (status, body) = t
        .command(
            &t.admin,
            "asset.capitalise",
            json!({ "id": chair, "document_id": wrong["id"],
                    "line_id": wrong["body"]["lines"][0]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "line_id");

    // Below the company's minimum: refused unless the policy allows it.
    t.create(
        "asset_settings",
        json!({ "company_id": f.c.id, "minimum_cost": "500.00" }),
    )
    .await;
    let entry = f.bought(&t, "1500", "300.00", "2026-02-01").await;
    let input = json!({ "id": chair, "document_id": entry["id"],
                        "line_id": entry["body"]["lines"][0]["id"] });
    let (status, body) = t.command(&t.admin, "asset.capitalise", input.clone()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["message"],
        "the asset costs less than the company capitalises"
    );
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "asset.below_minimum", "effect": "allow",
                "condition": "facts.class_code == 'EQ'", "priority": 10 }),
    )
    .await;
    let (status, body) = t.command(&t.admin, "asset.capitalise", input.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Without an in-service date it waits.
    assert_eq!(body["status"], "planned");
    assert_eq!(body["acquisition_date"], "2026-02-01");

    // A line is capitalised once.
    let other = t
        .create(
            "asset",
            json!({ "company_id": f.c.id, "class_id": f.equipment, "code": "CHAIR2", "name": "Chair" }),
        )
        .await;
    let mut twice = input.clone();
    twice["id"] = json!(other);
    let (status, body) = t.command(&t.admin, "asset.capitalise", twice).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Not before it was bought; then in service.
    let (status, _) = t
        .command(
            &t.admin,
            "asset.put_in_service",
            json!({ "id": chair, "in_service_date": "2026-01-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = t
        .command(
            &t.admin,
            "asset.put_in_service",
            json!({ "id": chair, "in_service_date": "2026-02-03" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "in_service");

    // A capitalised asset is disposed of, not deleted; its cost stays.
    let (status, _) = t
        .send(
            "DELETE",
            &format!("/api/entities/asset/{chair}"),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    assert_ne!(status, StatusCode::OK);
    t.stop().await;
}

#[tokio::test]
async fn fixed_assets_take_their_permissions() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let desk = f
        .asset(
            &t,
            "DESK",
            &f.equipment,
            "1200.00",
            "2026-01-05",
            "2026-01-05",
        )
        .await;
    let (auditor, _) = t
        .user_with_rules(
            "auditor@example.com",
            json!(["asset:read", "asset_class:read", "depreciation_run:read"]),
            Some(&f.c.id),
        )
        .await;
    let (status, body) = t
        .command(
            &auditor,
            "asset.register",
            json!({ "company_id": f.c.id, "as_of": "2026-12-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["assets"].as_array().unwrap().len(), 1);
    let (status, _) = t
        .command(&auditor, "asset.schedule", json!({ "id": desk }))
        .await;
    assert_eq!(status, StatusCode::OK);
    // Reading is not doing.
    let draft = f.draft(&t, "2026-01-31").await;
    let (status, body) = t
        .command(
            &auditor,
            "depreciation_run.post",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let chair = t
        .create(
            "asset",
            json!({ "company_id": f.c.id, "class_id": f.equipment, "code": "CHAIR", "name": "Chair" }),
        )
        .await;
    let (status, _) = t
        .command(
            &auditor,
            "asset.take_over",
            json!({ "id": chair, "cost": "100", "accumulated_depreciation": "0",
                    "acquisition_date": "2025-01-01", "in_service_date": "2025-01-01",
                    "depreciated_through": "2025-12-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Someone of another company sees nothing of this one.
    let other = t.company("OTHER", "USD").await;
    let (stranger, _) = t
        .user_with_rules(
            "stranger@example.com",
            json!(["asset:read", "asset:capitalise"]),
            Some(&other.id),
        )
        .await;
    let (status, _) = t
        .command(&stranger, "asset.register", json!({ "company_id": f.c.id }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .command(&stranger, "asset.schedule", json!({ "id": desk }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    t.stop().await;
}

#[tokio::test]
async fn the_setup_templates_bring_asset_classes() {
    let t = TestApp::start().await;
    for (template, code, classes) in [("us_small_business", "US", 5), ("de_skr03", "DE", 7)] {
        let (status, body) = t
            .command(
                &t.admin,
                "company.setup",
                json!({ "template": template, "code": code, "name": code, "first_year": 2026 }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["asset_classes"].as_array().unwrap().len(),
            classes,
            "{body}"
        );
    }
    // In SKR 03 depreciation is written off the asset account directly:
    // the register still ties, account by account.
    let company: String = sqlx::query_scalar("SELECT id::text FROM company WHERE code = 'DE'")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let account = |code: &'static str| {
        let company = company.clone();
        let pool = t.pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT id::text FROM account WHERE company_id = '{company}' AND code = '{code}'"
            ))
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let class: String = sqlx::query_scalar(&format!(
        "SELECT id::text FROM asset_class WHERE company_id = '{company}' AND code = 'EDV'"
    ))
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({ "company_id": company, "doc_date": "2026-01-10", "body": { "lines": [
                { "account_id": account("0420").await, "debit": "3600.00" },
                { "account_id": account("1200").await, "credit": "3600.00" },
            ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, entry) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": draft["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{entry}");
    let laptop = t
        .create(
            "asset",
            json!({ "company_id": company, "class_id": class, "code": "NB-1", "name": "Notebook" }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "asset.capitalise",
            json!({ "id": laptop, "document_id": entry["id"],
                    "line_id": entry["body"]["lines"][0]["id"], "in_service_date": "2026-01-10" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, run) = t
        .post(
            &t.admin,
            "/api/documents/depreciation_run",
            json!({ "company_id": company, "doc_date": "2026-06-30", "body": {} }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let (status, body) = t
        .command(
            &t.admin,
            "depreciation_run.post",
            json!({ "id": run["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(dec(&body["body"]["lines"][0]["amount"]), 600.0);
    let (status, register) = t
        .command(
            &t.admin,
            "asset.register",
            json!({ "company_id": company, "as_of": "2026-06-30" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{register}");
    assert_eq!(register["ties"], true, "{register}");
    let office = register["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["code"] == "0420")
        .unwrap()
        .clone();
    assert_eq!(dec(&office["ledger"]), 3000.0);
    assert_eq!(dec(&office["register"]), 3000.0);
    t.stop().await;
}

/// The command's metadata from `/api/meta`.
async fn command_meta(t: &TestApp, name: &str) -> Value {
    let (_, meta) = t.get(&t.admin, "/api/meta").await;
    meta["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no command {name}"))
        .clone()
}

#[tokio::test]
async fn a_dialog_finds_the_lines_an_asset_is_capitalised_from() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Inputs are asked in the order they are declared.
    let take_over = command_meta(&t, "asset.take_over").await;
    let order: Vec<&String> = take_over["input_schema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(
        order,
        [
            "id",
            "cost",
            "accumulated_depreciation",
            "acquisition_date",
            "in_service_date",
            "depreciated_through",
            "opening_date",
            "document_id",
            "line_id"
        ]
    );
    // The line is looked up among the asset's cost lines.
    let capitalise = command_meta(&t, "asset.capitalise").await;
    let line = &capitalise["input_schema"]["properties"]["line_id"]["x-field-type"];
    assert_eq!(line["kind"], "lookup");
    assert_eq!(line["command"], "asset.cost_lines");
    assert_eq!(line["params"]["id"]["field"], "id");
    let required = capitalise["input_schema"]["required"].as_array().unwrap();
    assert!(!required.contains(&json!("document_id")), "{required:?}");

    let chair = t
        .create(
            "asset",
            json!({ "company_id": f.c.id, "class_id": f.equipment, "code": "CHAIR", "name": "Chair" }),
        )
        .await;
    let bought = f.bought(&t, "1500", "800.00", "2026-02-01").await;
    // A line on another class's cost account is not offered.
    f.bought(&t, "1600", "900.00", "2026-02-02").await;
    let (status, found) = t
        .command(&t.admin, "asset.cost_lines", json!({ "id": chair }))
        .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let items = found["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{found}");
    let line_id = bought["body"]["lines"][0]["id"].clone();
    assert_eq!(items[0]["line_id"], line_id);
    assert_eq!(items[0]["document_id"], bought["id"]);
    assert_eq!(items[0]["doc_type"], "journal_entry");
    assert_eq!(dec(&items[0]["amount"]), 800.0);

    // The line alone is enough.
    let (status, body) = t
        .command(
            &t.admin,
            "asset.capitalise",
            json!({ "id": chair, "line_id": line_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(dec(&body["cost"]), 800.0);
    // Once capitalised, it is no longer offered, but can still be shown.
    let (_, found) = t
        .command(&t.admin, "asset.cost_lines", json!({ "id": chair }))
        .await;
    assert_eq!(found["items"], json!([]));
    let (_, found) = t
        .command(
            &t.admin,
            "asset.cost_lines",
            json!({ "id": chair, "line_ids": [line_id] }),
        )
        .await;
    assert_eq!(found["items"].as_array().unwrap().len(), 1, "{found}");

    // The ledger entry it came from is shown by its day, amount and memo.
    let (_, meta) = t.get(&t.admin, "/api/meta").await;
    let gl_entry = meta["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "gl_entry")
        .unwrap()
        .clone();
    assert_eq!(
        gl_entry["display"],
        json!(["posting_date", "amount", "memo"])
    );
    let (_, chair_record) = t
        .get(&t.admin, &format!("/api/entities/asset/{chair}"))
        .await;
    let (status, entry) = t
        .get(
            &t.admin,
            &format!(
                "/api/entities/gl_entry/{}",
                chair_record["source_entry_id"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{entry}");
    assert_eq!(dec(&entry["amount"]), 800.0);
    t.stop().await;
}

#[tokio::test]
async fn a_schedule_names_the_type_of_what_posted_it() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let desk = f
        .asset(
            &t,
            "DESK",
            &f.equipment,
            "1200.00",
            "2026-01-05",
            "2026-01-05",
        )
        .await;
    let (status, body) = f.run(&t, "2026-01-31").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let months = schedule(&t, &desk).await;
    assert_eq!(months[0]["posted"], true);
    assert_eq!(months[0]["doc_type"], "depreciation_run");
    assert_eq!(months[0]["document_id"], body["id"]);
    assert_eq!(months[1]["posted"], false);
    assert_eq!(months[1]["doc_type"], Value::Null);
    t.stop().await;
}

#[tokio::test]
async fn a_taken_over_asset_is_deleted_until_it_is_depreciated() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let take_over = |code: &'static str| {
        let t = &t;
        let f = &f;
        async move {
            let id = t
                .create(
                    "asset",
                    json!({ "company_id": f.c.id, "class_id": f.machinery, "code": code,
                            "name": code, "method": "straight_line", "useful_life_months": 36 }),
                )
                .await;
            let (status, body) = t
                .command(
                    &t.admin,
                    "asset.take_over",
                    json!({ "id": id, "cost": "3600.00", "accumulated_depreciation": "2400.00",
                            "acquisition_date": "2023-12-20", "in_service_date": "2024-01-01",
                            "depreciated_through": "2025-12-31" }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            id
        }
    };
    let delete = |id: String| {
        let t = &t;
        async move {
            t.send(
                "DELETE",
                &format!("/api/entities/asset/{id}"),
                Some(&t.admin),
                None,
                &[],
            )
            .await
        }
    };

    // Taken over by mistake: nothing posted, so it goes.
    let wrong = take_over("WRONG").await;
    let (status, body) = delete(wrong.clone()).await;
    assert!(status.is_success(), "{status} {body}");
    let (status, _) = t
        .get(&t.admin, &format!("/api/entities/asset/{wrong}"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Once a run has depreciated it, it is disposed of instead.
    let press = take_over("PRESS").await;
    let (status, body) = f.run(&t, "2026-01-31").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = delete(press).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has been depreciated"),
        "{body}"
    );
    t.stop().await;
}

#[tokio::test]
async fn the_opening_entry_of_assets_taken_over_is_not_offered_for_capitalising() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let asset = |code: &'static str| {
        let t = &t;
        let f = &f;
        async move {
            t.create(
                "asset",
                json!({ "company_id": f.c.id, "class_id": f.machinery, "code": code,
                        "name": code, "method": "straight_line", "useful_life_months": 36 }),
            )
            .await
        }
    };
    let take_over = |id: String, cost: &'static str, line: Option<(Value, Value)>| {
        let t = &t;
        async move {
            let mut input = json!({ "id": id, "cost": cost, "accumulated_depreciation": "0",
                "acquisition_date": "2025-12-01", "in_service_date": "2025-12-01",
                "depreciated_through": "2025-12-31" });
            if let Some((document, line)) = line {
                input["document_id"] = document;
                input["line_id"] = line;
            }
            t.command(&t.admin, "asset.take_over", input).await
        }
    };
    // One opening line of 5,000.00 for two machines taken over.
    let opening = f.bought(&t, "1600", "5000.00", "2026-01-01").await;
    let line = opening["body"]["lines"][0]["id"].clone();
    let press = asset("PRESS").await;
    let (status, body) = take_over(
        press.clone(),
        "3000.00",
        Some((opening["id"].clone(), line.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["opening_entry_id"].is_string(), "{body}");
    // The second is taken over first and linked later; more than the line
    // debited is refused.
    let lathe = asset("LATHE").await;
    let (status, body) = take_over(lathe.clone(), "2500.00", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let link = json!({ "id": lathe, "document_id": opening["id"], "line_id": line });
    let (status, body) = t
        .command(&t.admin, "asset.link_opening", link.clone())
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "line_id", "{body}");
    let drill = asset("DRILL").await;
    let (status, body) = take_over(drill.clone(), "2000.00", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .command(
            &t.admin,
            "asset.link_opening",
            json!({ "id": drill, "document_id": opening["id"], "line_id": line }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A machine bought later is offered its own line, not the opening one;
    // and the opening line cannot be capitalised.
    let bought = f.bought(&t, "1600", "800.00", "2026-02-01").await;
    let saw = asset("SAW").await;
    let (_, lines) = t
        .command(&t.admin, "asset.cost_lines", json!({ "id": saw }))
        .await;
    let offered: Vec<&Value> = lines["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| &l["line_id"])
        .collect();
    assert_eq!(offered, [&bought["body"]["lines"][0]["id"]], "{lines}");
    let (status, body) = t
        .command(
            &t.admin,
            "asset.capitalise",
            json!({ "id": saw, "document_id": opening["id"], "line_id": line }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // While the machines are in the register, their opening entry stays.
    let (status, body) = t
        .command(
            &t.admin,
            "journal_entry.reverse",
            json!({ "id": opening["id"] }),
        )
        .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("opened by it"),
        "{body}"
    );
    t.stop().await;
}

#[tokio::test]
async fn the_posting_an_asset_came_from_is_not_reversed_under_it() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // From a journal entry.
    let desk = t
        .create(
            "asset",
            json!({ "company_id": f.c.id, "class_id": f.equipment, "code": "DESK", "name": "Desk" }),
        )
        .await;
    let entry = f.bought(&t, "1500", "1200.00", "2026-01-05").await;
    let (status, body) = t
        .command(
            &t.admin,
            "asset.capitalise",
            json!({ "id": desk, "line_id": entry["body"]["lines"][0]["id"],
                    "in_service_date": "2026-01-05" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .command(
            &t.admin,
            "journal_entry.reverse",
            json!({ "id": entry["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("DESK (Desk)"), "{body}");
    assert!(message.contains("dispose of the asset first"), "{body}");
    assert_eq!(f.balance(&t, "1500").await, 1200.0);
    // An entry no asset came from is reversed as ever.
    let other = f.bought(&t, "1500", "50.00", "2026-01-06").await;
    let (status, body) = t
        .command(
            &t.admin,
            "journal_entry.reverse",
            json!({ "id": other["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // From a vendor invoice without an order.
    let mut account = f.account.clone();
    for (code, kind) in [("2000", "liability"), ("2100", "liability")] {
        let id = t
            .create(
                "account",
                json!({ "company_id": f.c.id, "code": code, "name": code, "account_type": kind }),
            )
            .await;
        account.insert(code, id);
    }
    t.create(
        "purchasing_settings",
        json!({ "company_id": f.c.id, "grni_account_id": account["2100"],
                "payable_account_id": account["2000"],
                "price_variance_account_id": account["6900"] }),
    )
    .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Machines Inc" }))
        .await;
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/vendor_invoice",
            json!({ "company_id": f.c.id, "doc_date": "2026-02-01", "body": {
                "vendor_id": vendor, "lines": [
                    { "account_id": account["1600"], "description": "Press", "qty": "1",
                      "unit_price": "9000.00" } ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, invoice) = t
        .command(
            &t.admin,
            "vendor_invoice.post",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{invoice}");
    let press = t
        .create(
            "asset",
            json!({ "company_id": f.c.id, "class_id": f.machinery, "code": "PRESS", "name": "Press" }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "asset.capitalise",
            json!({ "id": press, "line_id": invoice["body"]["lines"][0]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .command(
            &t.admin,
            "vendor_invoice.reverse",
            json!({ "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("PRESS (Press)"),
        "{body}"
    );

    // Once the desk is disposed of, its entry may be reversed.
    let (status, body) = f.dispose(&t, &desk, "2026-03-31", "0").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .command(
            &t.admin,
            "journal_entry.reverse",
            json!({ "id": entry["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}
