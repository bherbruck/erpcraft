//! Regression tests for the findings of the second code review: conditions
//! that could overflow the stack, upgrade migrations that changed accounts
//! silently, a payment through its own control account, fiscal period
//! dates changed after the fact, postings dated in a closed period, and
//! reads that told more than the reader may see.

mod common;

use std::str::FromStr;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use erpcraft_db::MigrationSet;
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

async fn run(t: &TestApp, command: &str, doc: &Value) -> (StatusCode, Value) {
    t.command(&t.admin, command, json!({ "id": id(doc) })).await
}

// ------------------------------------------------- conditions (items 1, 8)

/// A query string value, percent-encoded.
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// A source that would parse into a tree as deep as it has terms.
fn chain(terms: usize) -> String {
    format!("{} > 0", vec!["1"; terms].join("+"))
}

fn nested(levels: usize) -> String {
    format!(
        "{}record.code == 'A'{}",
        "(".repeat(levels),
        ")".repeat(levels)
    )
}

#[tokio::test]
async fn a_condition_that_is_too_long_or_too_deep_is_refused_and_the_server_goes_on() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    t.create("item", json!({ "code": "A", "name": "An item" }))
        .await;
    let listed = |condition: String| {
        let t = &t;
        async move {
            t.command(
                &t.admin,
                "entity.list",
                json!({ "entity": "item", "where": condition }),
            )
            .await
        }
    };
    let answering = || async {
        let (status, body) = t
            .get(
                &t.admin,
                &format!("/api/entities/item?where={}", encoded(&nested(3))),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 1, "the server still answers: {body}");
    };

    // The `where` of a list. Any of these used to end the process, not
    // the request: a stack overflow cannot be caught.
    let (status, body) = listed(chain(50_000)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "a rule can be at most 4000 characters long; this one has 100003"
    );
    answering().await;
    for (condition, why) in [
        // In the address itself, where a signed-in user would put it.
        (chain(3_000), "at most 4000 characters"),
        (nested(3_000), "at most 4000 characters"),
        // Short enough, and still far too deep.
        (chain(1_900), "nested more than 64 levels deep"),
        (
            format!("record.code{} == 'A'", ".a".repeat(1_900)),
            "nested more than 64 levels deep",
        ),
        (nested(1_900), "brackets are nested too deeply"),
        (
            format!("{}1{} == [1]", "[".repeat(1_900), "]".repeat(1_900)),
            "brackets are nested too deeply",
        ),
    ] {
        let (status, body) = t
            .get(
                &t.admin,
                &format!("/api/entities/item?where={}", encoded(&condition)),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(message(&body).contains(why), "{body}");
        answering().await;
    }
    // Parentheses a person could plausibly write are simply accepted. In
    // an unoptimized build 11 levels were enough to overflow before.
    let (status, body) = listed(nested(40)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1);

    // A policy rule and a role are stored, so they are refused when saved.
    for condition in [
        chain(50_000),
        format!("facts.over_by + {}", chain(1_900)),
        format!(
            "{}facts.over_by > 0{}",
            "(".repeat(1_900),
            ")".repeat(1_900)
        ),
    ] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/entities/policy_rule",
                json!({
                    "company_id": c.id, "point": "purchase_receipt.over_receipt",
                    "effect": "allow", "condition": condition, "priority": 10,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        let why = message(&body);
        assert!(
            why.contains("at most 4000 characters")
                || why.contains("nested more than 64 levels deep")
                || why.contains("brackets are nested too deeply"),
            "{body}"
        );
        let (status, body) = t
            .post(
                &t.admin,
                "/api/entities/role",
                json!({
                    "name": "too deep",
                    "permissions": [{ "allow": "item:read", "where": condition.replace("facts.over_by", "1") }],
                }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(t.count("SELECT count(*) FROM policy_rule").await, 0);
    assert_eq!(
        t.count("SELECT count(*) FROM role WHERE name = 'too deep'")
            .await,
        0
    );
    answering().await;

    t.stop().await;
}

#[tokio::test]
async fn a_condition_that_cannot_be_read_is_told_in_a_sentence() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({
                "company_id": c.id, "point": "purchase_receipt.over_receipt",
                "effect": "allow", "condition": "(facts.over_percent <= 5", "priority": 10,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let why = message(&body);
    assert!(
        why.ends_with("it cannot be read: `)` is missing, at line 1, column 25"),
        "{body}"
    );
    for word in ["ParseError", "{", "Syntax error", "EOF"] {
        assert!(!why.contains(word), "{body}");
    }

    // The same sentence for a list and for a role.
    let (status, body) = t
        .get(
            &t.admin,
            &format!(
                "/api/entities/account?where={}",
                encoded("record.code == '1000' &&")
            ),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        message(&body).ends_with("it cannot be read: it ends too early, at line 1, column 25"),
        "{body}"
    );
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/role",
            json!({
                "name": "broken",
                "permissions": [{ "allow": "account:read", "where": "record.code = '1000'" }],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        message(&body).contains("`=` cannot be used; write `==` to compare"),
        "{body}"
    );
    assert!(!message(&body).contains("ParseError"), "{body}");

    t.stop().await;
}

// ------------------------------------------------ null attributes (item 5)

#[tokio::test]
async fn a_null_attribute_allows_nothing_in_a_record_or_in_a_list() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    // Everyone who is not restricted may read accounts.
    let (token, user) = t
        .user_with_rules(
            "clerk@example.com",
            json!([{ "allow": "account:read", "where": "user.restricted != true" }]),
            Some(&c.id),
        )
        .await;
    let reads = |token: &str| {
        let (t, token, cash) = (&t, token.to_owned(), c.cash.clone());
        async move {
            let (one, _) = t
                .get(&token, &format!("/api/entities/account/{cash}"))
                .await;
            let (status, list) = t.get(&token, "/api/entities/account").await;
            let listed = match status {
                StatusCode::OK => list["total"].as_i64().unwrap(),
                _ => 0,
            };
            (one, listed)
        }
    };

    t.set_attributes(&user, json!({ "restricted": false }))
        .await;
    assert_eq!(reads(&token).await, (StatusCode::OK, 3));
    t.set_attributes(&user, json!({ "restricted": true })).await;
    assert_eq!(reads(&token).await.1, 0);
    // Null used to compare as "differs from true", and so allowed.
    t.set_attributes(&user, json!({ "restricted": null })).await;
    let (one, listed) = reads(&token).await;
    assert_ne!(one, StatusCode::OK);
    assert_eq!(listed, 0);
    // As without the attribute at all.
    t.set_attributes(&user, json!({})).await;
    let (one, listed) = reads(&token).await;
    assert_ne!(one, StatusCode::OK);
    assert_eq!(listed, 0);

    t.stop().await;
}

// ---------------------------------------------- upgrade migrations (item 2)

/// A database migrated up to, and not including, the migrations that move
/// the inventory account out of the module settings: what an installation
/// of the version before them has. This is the real upgrade path, with the
/// real migration runner; nothing is rebuilt by hand.
struct Upgrade {
    root: sqlx::PgPool,
    pool: sqlx::PgPool,
    database: String,
    all: Vec<MigrationSet>,
}

impl Upgrade {
    async fn start() -> Upgrade {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://erpcraft:erpcraft@127.0.0.1:54320/erpcraft".into());
        let options = PgConnectOptions::from_str(&url).unwrap();
        let root = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        let database = format!("erpcraft_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&root)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.database(&database))
            .await
            .unwrap();
        let kernel = erpcraft_server::modules(pool.clone()).build().unwrap();
        let all = kernel.registry().migration_sets().to_vec();
        let before: Vec<MigrationSet> = all
            .iter()
            .cloned()
            .map(|mut set| {
                let last = match set.module.as_str() {
                    "purchasing" | "sales" => 2,
                    "manufacturing" => 1,
                    _ => i32::MAX,
                };
                set.migrations.retain(|m| m.version <= last);
                set
            })
            .collect();
        erpcraft_db::migrate(&pool, &before).await.unwrap();
        Upgrade {
            root,
            pool,
            database,
            all,
        }
    }

    async fn sql(&self, sql: &str) {
        sqlx::raw_sql(sql)
            .execute(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(&self.pool).await.unwrap()
    }

    /// Run the upgrade; the error it stops with, if it stops.
    async fn upgrade(&self) -> Option<String> {
        erpcraft_db::migrate(&self.pool, &self.all)
            .await
            .err()
            .map(|e| e.to_string())
    }

    async fn stop(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.root)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn an_upgrade_stops_rather_than_change_where_a_company_posts() {
    let u = Upgrade::start().await;
    // Two companies as the version before kept them. BETA uses one
    // inventory account everywhere. ACME buys into 1300, sells out of
    // 1310, takes components from 1320 and makes products into 1330.
    u.sql(
        "INSERT INTO company (id, code, name, base_currency)
            SELECT gen_random_uuid(), code, code, 'USD' FROM unnest(ARRAY['ACME', 'BETA']) AS code;
         INSERT INTO account (id, company_id, code, name, account_type)
            SELECT gen_random_uuid(), c.id, a.code, a.code, 'asset' FROM company c
            CROSS JOIN unnest(ARRAY['1300', '1310', '1320', '1330', '1340', '2000']) AS a (code);
         CREATE FUNCTION acct(company text, account text) RETURNS uuid LANGUAGE sql AS $$
            SELECT a.id FROM account a JOIN company c ON c.id = a.company_id
            WHERE c.code = company AND a.code = account $$;
         INSERT INTO purchasing_settings (id, company_id, inventory_account_id, grni_account_id,
                payable_account_id, price_variance_account_id)
            SELECT gen_random_uuid(), c.id, acct(c.code, '1300'), acct(c.code, '2000'),
                   acct(c.code, '2000'), acct(c.code, '2000') FROM company c;
         INSERT INTO sales_settings (id, company_id, receivable_account_id, revenue_account_id,
                cogs_account_id, inventory_account_id)
            SELECT gen_random_uuid(), c.id, acct(c.code, '2000'), acct(c.code, '2000'), acct(c.code, '2000'),
                   acct(c.code, CASE c.code WHEN 'ACME' THEN '1310' ELSE '1300' END) FROM company c;
         INSERT INTO manufacturing_settings (id, company_id, materials_account_id, finished_goods_account_id)
            SELECT gen_random_uuid(), c.id,
                   acct(c.code, CASE c.code WHEN 'ACME' THEN '1320' ELSE '1300' END),
                   acct(c.code, CASE c.code WHEN 'ACME' THEN '1330' ELSE '1300' END) FROM company c;
         INSERT INTO item (id, code, name) VALUES (gen_random_uuid(), 'CAKE', 'Cake');
         INSERT INTO bom (id, item_id, code, name) SELECT gen_random_uuid(), id, 'CAKE-1', 'Cake' FROM item;",
    )
    .await;
    let default_rule = "SELECT count(*) FROM account_rule r JOIN company c ON c.id = r.company_id
                        JOIN account a ON a.id = r.account_id
                        WHERE r.purpose = 'inventory' AND r.scope_type IS NULL";

    // Sales disagrees with purchasing: the upgrade stops there and says
    // which company, which accounts, and what to do.
    let stopped = u.upgrade().await.expect("the upgrade must stop");
    assert!(
        stopped.contains(
            "company ACME: sales posts stock to inventory account 1310, but the inventory \
             account of the company is 1300."
        ),
        "{stopped}"
    );
    assert!(
        stopped.contains(
            "Before upgrading, make the two agree and move the balance with a journal entry"
        ),
        "{stopped}"
    );
    // Nothing of the sales migration was applied, for either company, and
    // running the upgrade again stops at the same place.
    assert_eq!(
        u.count("SELECT count(*) FROM sales_settings WHERE inventory_account_id IS NOT NULL")
            .await,
        2
    );
    assert_eq!(
        u.count("SELECT count(*) FROM erpcraft_migration WHERE module = 'sales' AND version >= 3")
            .await,
        0
    );
    assert!(u.upgrade().await.unwrap().contains("company ACME: sales"));

    // Sales made to agree; now manufacturing's materials account does not.
    u.sql(
        "UPDATE sales_settings s SET inventory_account_id = acct('ACME', '1300')
         FROM company c WHERE c.id = s.company_id AND c.code = 'ACME'",
    )
    .await;
    let stopped = u.upgrade().await.expect("the upgrade must stop");
    assert!(
        stopped.contains(
            "company ACME: manufacturing takes components from materials account 1320, but the \
             inventory account of the company is 1300."
        ),
        "{stopped}"
    );
    assert_eq!(
        u.count("SELECT count(*) FROM manufacturing_settings").await,
        2
    );

    // Materials made to agree; but someone gave the product an account
    // rule that is not the finished goods account.
    u.sql(
        "UPDATE manufacturing_settings s SET materials_account_id = acct('ACME', '1300')
         FROM company c WHERE c.id = s.company_id AND c.code = 'ACME';
         INSERT INTO account_rule (id, company_id, purpose, scope_type, scope_id, account_id)
         SELECT gen_random_uuid(), c.id, 'inventory', 'item', i.id, acct('ACME', '1340')
         FROM company c, item i WHERE c.code = 'ACME'",
    )
    .await;
    let stopped = u.upgrade().await.expect("the upgrade must stop");
    assert!(
        stopped.contains(
            "company ACME: manufacturing posts product CAKE to finished goods account 1330, but \
             the account rule for that item names account 1340."
        ),
        "{stopped}"
    );

    // With that rule gone the upgrade goes through, and every account is
    // what it was: 1300 for both companies, and 1330 for ACME's product.
    u.sql("DELETE FROM account_rule WHERE scope_type = 'item'")
        .await;
    assert_eq!(u.upgrade().await, None);
    assert_eq!(u.count(default_rule).await, 2);
    assert_eq!(
        u.count(&format!("{default_rule} AND a.code = '1300'"))
            .await,
        2
    );
    assert_eq!(
        u.count(
            "SELECT count(*) FROM account_rule r JOIN company c ON c.id = r.company_id
             JOIN account a ON a.id = r.account_id JOIN item i ON i.id = r.scope_id
             WHERE r.scope_type = 'item' AND c.code = 'ACME' AND a.code = '1330' AND i.code = 'CAKE'"
        )
        .await,
        1
    );
    // BETA made everything into its one account and needs no item rule.
    assert_eq!(
        u.count("SELECT count(*) FROM account_rule WHERE scope_type = 'item'")
            .await,
        1
    );
    assert_eq!(
        u.count(
            "SELECT count(*) FROM information_schema.columns
             WHERE column_name = 'inventory_account_id'
               AND table_name IN ('purchasing_settings', 'sales_settings')"
        )
        .await
            + u.count(
                "SELECT count(*) FROM information_schema.tables
                 WHERE table_name = 'manufacturing_settings'"
            )
            .await,
        0
    );
    assert_eq!(u.upgrade().await, None, "nothing is left to apply");

    u.stop().await;
}

// ------------------------------------------------- buying and selling setup

struct Fixture {
    c: Company,
    item: String,
    warehouse: String,
    vendor: String,
    customer: String,
    payable: String,
    receivable: String,
}

/// A company set up to buy, stock and sell widgets.
async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("ACME", "USD").await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C001", "name": "Buyer LLC" }))
        .await;
    let mut account = std::collections::BTreeMap::new();
    for (code, kind) in [
        ("1200", "asset"),
        ("1300", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
        ("5000", "expense"),
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
    Fixture {
        c,
        item,
        warehouse,
        vendor,
        customer,
        payable: account["2000"].clone(),
        receivable: account["1200"].clone(),
    }
}

impl Fixture {
    async fn create(&self, t: &TestApp, doc_type: &str, body: Value) -> (StatusCode, Value) {
        t.post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "memo": "Spring order", "body": body }),
        )
        .await
    }

    /// Draft a document and run `command` on it; both must succeed.
    async fn done(&self, t: &TestApp, doc_type: &str, command: &str, body: Value) -> Value {
        let (status, draft) = self.create(t, doc_type, body).await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, doc) = run(t, command, &draft).await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }

    /// Order, move and invoice `qty` widgets at `price`, buying or selling.
    /// Returns the posted invoice.
    async fn trade(&self, t: &TestApp, buying: bool, qty: &str, price: &str) -> Value {
        let (order_type, party, movement, invoice) = if buying {
            (
                "purchase_order",
                json!({ "vendor_id": self.vendor }),
                "purchase_receipt",
                "vendor_invoice",
            )
        } else {
            (
                "sales_order",
                json!({ "customer_id": self.customer }),
                "sales_delivery",
                "sales_invoice",
            )
        };
        let mut body = party;
        body["lines"] = json!([{
            "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price,
        }]);
        let order = self
            .done(t, order_type, &format!("{order_type}.approve"), body)
            .await;
        let line = &order["body"]["lines"][0]["id"];
        let order_ref = format!("{order_type}_id");
        self.done(
            t,
            movement,
            &format!("{movement}.post"),
            json!({ &order_ref: order["id"], "lines": [{ "order_line_id": line, "qty": qty }] }),
        )
        .await;
        self.done(
            t,
            invoice,
            &format!("{invoice}.post"),
            json!({
                &order_ref: order["id"],
                "lines": [{ "order_line_id": line, "qty": qty, "unit_price": price }],
            }),
        )
        .await
    }

    async fn open_items(&self, t: &TestApp, token: &str, kind: &str) -> Value {
        let (status, list) = t
            .command(
                token,
                "open_item.list",
                json!({ "company_id": self.c.id, "kind": kind }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{list}");
        list
    }

    /// Balance of one account, debit minus credit.
    async fn balance(&self, t: &TestApp, code: &str) -> f64 {
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["code"] == code)
            .map(|r| dec(&r["balance"]))
            .unwrap_or(0.0)
    }
}

// ------------------------------------------------- bank account (item 3)

#[tokio::test]
async fn a_payment_cannot_go_through_the_account_of_the_items_it_settles() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.trade(&t, true, "10", "2.00").await;
    f.trade(&t, false, "4", "6.00").await;
    let owed = f.open_items(&t, &t.admin, "payable").await;
    let due = f.open_items(&t, &t.admin, "receivable").await;
    let payment = |direction: &str, bank: &str, item: &Value| {
        json!({
            "direction": direction, "bank_account_id": bank,
            "allocations": [{ "open_item_id": item["id"], "amount": item["open"] }],
        })
    };
    let consistent = || async {
        let owed = f.open_items(&t, &t.admin, "payable").await;
        let due = f.open_items(&t, &t.admin, "receivable").await;
        assert_eq!(dec(&owed["total_open"]), 20.0);
        assert_eq!(dec(&due["total_open"]), 24.0);
        assert_eq!(-f.balance(&t, "2000").await, 20.0, "payables");
        assert_eq!(f.balance(&t, "1200").await, 24.0, "receivables");
    };

    // Refused as a draft, in both directions, with the field named.
    for (direction, bank, item, account) in [
        ("outgoing", &f.payable, &owed["items"][0], "payables"),
        ("incoming", &f.receivable, &due["items"][0], "receivables"),
    ] {
        let (status, body) = f
            .create(&t, "payment", payment(direction, bank, item))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(
            message(&body),
            format!(
                "the bank or cash account cannot be the {account} account of the items being paid"
            )
        );
        assert_eq!(body["error"]["field"], "bank_account_id", "{body}");
    }
    // A group account holds no money either.
    let (status, body) = f
        .create(
            &t,
            "payment",
            payment("outgoing", &f.c.group, &owed["items"][0]),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("is a group account"), "{body}");
    // The other control account is an odd bank account, but it moves the
    // debt somewhere else and does not make it vanish; it is not refused.
    consistent().await;

    // And refused again when posted: a draft written before the rule, or
    // changed behind the document's back, settles nothing.
    for (direction, bank, item) in [
        ("outgoing", &f.payable, &owed["items"][0]),
        ("incoming", &f.receivable, &due["items"][0]),
    ] {
        let (status, draft) = f
            .create(&t, "payment", payment(direction, &f.c.cash, item))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        sqlx::query("UPDATE payment SET bank_account_id = $1::uuid WHERE document_id = $2::uuid")
            .bind(bank)
            .bind(id(&draft))
            .execute(&t.pool)
            .await
            .unwrap();
        let (status, body) = run(&t, "payment.post", &draft).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(message(&body).contains("cannot be the"), "{body}");
    }
    assert_eq!(t.count("SELECT count(*) FROM settlement").await, 0);
    consistent().await;

    // Through the bank, the same payment settles the item.
    f.done(
        &t,
        "payment",
        "payment.post",
        payment("outgoing", &f.c.cash, &owed["items"][0]),
    )
    .await;
    assert_eq!(
        dec(&f.open_items(&t, &t.admin, "payable").await["total_open"]),
        0.0
    );
    assert_eq!(f.balance(&t, "2000").await, 0.0);

    t.stop().await;
}

// ------------------------------------------- fiscal period dates (item 4)

#[tokio::test]
async fn the_dates_of_a_fiscal_period_are_fixed_once_it_is_used_or_closed() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let period = |name: &'static str, start: &'static str, end: &'static str| {
        let (t, company) = (&t, c.id.clone());
        async move {
            t.create(
                "fiscal_period",
                json!({ "company_id": company, "name": name, "start_date": start, "end_date": end }),
            )
            .await
        }
    };
    let change = |id: &str, dates: Value| {
        let (t, path) = (&t, format!("/api/entities/fiscal_period/{id}"));
        async move { t.patch(&t.admin, &path, dates).await }
    };
    let delete = |id: &str| {
        let (t, path) = (&t, format!("/api/entities/fiscal_period/{id}"));
        async move { t.send("DELETE", &path, Some(&t.admin), None, &[]).await }
    };

    // An open period nothing was posted in can be corrected, and removed.
    let next = period("2027", "2027-01-01", "2027-11-30").await;
    let (status, body) = change(&next, json!({ "end_date": "2027-12-31" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["end_date"], "2027-12-31");
    let spare = period("2028", "2028-01-01", "2028-12-31").await;
    let (status, body) = delete(&spare).await;
    assert!(status.is_success(), "{body}");

    // 2026 has an entry in March. Its dates no longer change, even while
    // it is open, and it cannot be deleted.
    let draft = t.draft(&t.admin, &c, "100.00").await;
    let (status, posted) = run(&t, "journal_entry.post", &draft).await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    for dates in [
        json!({ "end_date": "2026-02-28" }),
        json!({ "end_date": "2026-06-30" }),
        json!({ "start_date": "2026-02-01" }),
    ] {
        let (status, body) = change(&c.period, dates).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(
            message(&body),
            "fiscal period 2026 already has postings: its dates cannot change"
        );
    }
    let (status, body) = delete(&c.period).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "fiscal period 2026 already has postings: it cannot be deleted"
    );
    // Its name is not a date.
    let (status, body) = change(&c.period, json!({ "name": "FY 2026" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The attack: close the year, shorten it, open a new period over the
    // freed months, and post into the closed year without reopening it.
    let (status, _) = t
        .command(&t.admin, "fiscal_period.close", json!({ "id": c.period }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = change(&c.period, json!({ "end_date": "2026-06-30" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "fiscal period FY 2026 is closed: its dates cannot change"
    );
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/fiscal_period",
            json!({ "company_id": c.id, "name": "2026 H2", "start_date": "2026-07-01", "end_date": "2026-12-31" }),
        )
        .await;
    assert!(status.is_client_error(), "{body}");
    let (status, body) = delete(&c.period).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "fiscal period FY 2026 is closed: it cannot be deleted"
    );
    // An empty period that is closed is fixed too, until it is reopened.
    let (status, _) = t
        .command(&t.admin, "fiscal_period.close", json!({ "id": next }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = change(&next, json!({ "start_date": "2027-02-01" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, _) = t
        .command(&t.admin, "fiscal_period.reopen", json!({ "id": next }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = change(&next, json!({ "start_date": "2027-02-01" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Not through SQL either.
    for sql in [
        "UPDATE fiscal_period SET end_date = '2026-06-30' WHERE name = 'FY 2026'",
        "DELETE FROM fiscal_period WHERE name = 'FY 2026'",
    ] {
        assert!(sqlx::query(sql).execute(&t.pool).await.is_err(), "{sql}");
    }
    assert_eq!(
        t.count(
            "SELECT count(*) FROM fiscal_period
             WHERE start_date = '2026-01-01' AND end_date = '2026-12-31' AND status = 'closed'"
        )
        .await,
        1
    );

    t.stop().await;
}

// -------------------------------- postings in a closed period (item 6)

#[tokio::test]
async fn nothing_is_posted_in_a_closed_period_even_without_ledger_entries() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let closed = t
        .create(
            "fiscal_period",
            json!({ "company_id": c.id, "name": "2025", "start_date": "2025-01-01", "end_date": "2025-12-31" }),
        )
        .await;
    let (status, _) = t
        .command(&t.admin, "fiscal_period.close", json!({ "id": closed }))
        .await;
    assert_eq!(status, StatusCode::OK);

    let item = |code: &'static str| {
        let t = &t;
        async move {
            t.create("item", json!({ "code": code, "name": code }))
                .await
        }
    };
    let (flour, cake) = (item("FLOUR").await, item("CAKE").await);
    let warehouse = |code: &'static str| {
        let (t, company) = (&t, c.id.clone());
        async move {
            t.create(
                "warehouse",
                json!({ "company_id": company, "code": code, "name": code }),
            )
            .await
        }
    };
    let (main, dock) = (warehouse("MAIN").await, warehouse("DOCK").await);
    let inventory = t
        .create(
            "account",
            json!({ "company_id": c.id, "code": "1300", "name": "Inventory", "account_type": "asset" }),
        )
        .await;
    // One inventory account for components and products: production
    // moves stock and posts nothing to the general ledger.
    t.inventory_rule(&c.id, &inventory).await;
    let document = |doc_type: &'static str, doc_date: &'static str, body: Value| {
        let (t, company) = (&t, c.id.clone());
        async move {
            let (status, draft) = t
                .post(
                    &t.admin,
                    &format!("/api/documents/{doc_type}"),
                    json!({ "company_id": company, "doc_date": doc_date, "body": body }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{draft}");
            draft
        }
    };
    let stocked = document(
        "stock_adjustment",
        "2026-03-15",
        json!({
            "offset_account_id": c.revenue,
            "lines": [{ "item_id": flour, "warehouse_id": main, "qty": "10", "unit_cost": "2.00" }],
        }),
    )
    .await;
    let (status, body) = run(&t, "stock_adjustment.post", &stocked).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before = (
        t.count("SELECT count(*) FROM stock_entry").await,
        t.count("SELECT count(*) FROM posting").await,
    );

    // A production order completed with a date in the closed year.
    let bom = t
        .create(
            "bom",
            json!({ "item_id": cake, "code": "CAKE-1", "name": "Cake" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": bom, "item_id": flour, "qty": "2" }),
    )
    .await;
    let order = document(
        "production_order",
        "2026-03-15",
        json!({ "bom_id": bom, "qty": "1", "warehouse_id": main }),
    )
    .await;
    let (status, body) = run(&t, "production_order.release", &order).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .command(
            &t.admin,
            "production_order.complete",
            json!({ "id": id(&order), "posting_date": "2025-06-30" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "no open fiscal period contains 2025-06-30");

    // A stock transfer never has general ledger entries.
    let transfer = |doc_date: &'static str| {
        document(
            "stock_transfer",
            doc_date,
            json!({
                "from_warehouse_id": main, "to_warehouse_id": dock,
                "lines": [{ "item_id": flour, "qty": "1" }],
            }),
        )
    };
    let back_dated = transfer("2025-06-30").await;
    let (status, body) = run(&t, "stock_transfer.post", &back_dated).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "no open fiscal period contains 2025-06-30");
    assert_eq!(
        (
            t.count("SELECT count(*) FROM stock_entry").await,
            t.count("SELECT count(*) FROM posting").await,
        ),
        before,
        "nothing moved"
    );

    // In the open year both go through, with no general ledger entry.
    let entries = t.count("SELECT count(*) FROM gl_entry").await;
    let (status, body) = t
        .command(
            &t.admin,
            "production_order.complete",
            json!({ "id": id(&order), "posting_date": "2026-06-30" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let moved = transfer("2026-06-30").await;
    let (status, body) = run(&t, "stock_transfer.post", &moved).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, entries);
    // Where the company has no period at all, stock can still move: such
    // a posting could not have general ledger entries anyway.
    let no_period = transfer("2030-01-15").await;
    let (status, body) = run(&t, "stock_transfer.post", &no_period).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Once 2026 is closed, the transfer cannot be reversed into it
    // either, until the period is reopened.
    let (status, _) = t
        .command(&t.admin, "fiscal_period.close", json!({ "id": c.period }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = run(&t, "stock_transfer.reverse", &moved).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "no open fiscal period contains 2026-06-30");
    let (status, _) = t
        .command(&t.admin, "fiscal_period.reopen", json!({ "id": c.period }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = run(&t, "stock_transfer.reverse", &moved).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    t.stop().await;
}

// ------------------------------------------------------ read leaks (item 7)

#[tokio::test]
async fn a_reader_learns_nothing_about_documents_and_items_out_of_reach() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let invoice = f.trade(&t, true, "10", "2.00").await;
    let owed = f.open_items(&t, &t.admin, "payable").await;
    let item = owed["items"][0].clone();
    assert_eq!(item["document_id"], invoice["id"]);
    assert_eq!(item["doc_type"], "vendor_invoice");
    assert_eq!(item["number"], invoice["number"]);

    // Someone who posts payments and may not read open items is told that
    // a payment is too much, and not how much is open.
    let cashier = t
        .user_with_role(
            "cashier@example.com",
            &["payment:read", "payment:create", "payment:post"],
            &f.c.id,
        )
        .await;
    let too_much = json!({
        "company_id": f.c.id, "doc_date": "2026-03-20",
        "body": {
            "direction": "outgoing", "bank_account_id": f.c.cash,
            "allocations": [{ "open_item_id": item["id"], "amount": "25.00" }],
        },
    });
    let (status, draft) = t
        .post(&cashier, "/api/documents/payment", too_much.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, body) = t
        .command(&cashier, "payment.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 25 is more than is still open on this item"
    );
    assert!(!body.to_string().contains("20"), "{body}");
    // Someone who may read open items is told the amount, as before.
    let (status, body) = run(&t, "payment.post", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: only 20 is still open on this item, not 25"
    );

    // Open items: what is owed, without the invoice behind it for a
    // reader who may not read vendor invoices.
    let creditor = |email: &'static str, rules: Value| {
        let (t, company) = (&t, f.c.id.clone());
        async move { t.user_with_rules(email, rules, Some(&company)).await.0 }
    };
    let blind = creditor("blind@example.com", json!(["open_item:read"])).await;
    let sighted = creditor(
        "sighted@example.com",
        json!(["open_item:read", "vendor_invoice:read"]),
    )
    .await;
    // A rule on the document decides too: only one's own invoices.
    let own_only = creditor(
        "own@example.com",
        json!([
            "open_item:read",
            { "allow": "vendor_invoice:read", "where": "record.created_by == user.id" },
        ]),
    )
    .await;
    for (token, sees) in [(&blind, false), (&sighted, true), (&own_only, false)] {
        let listed = f.open_items(&t, token, "payable").await;
        let row = &listed["items"][0];
        assert_eq!(row["id"], item["id"]);
        assert_eq!(dec(&row["open"]), 20.0);
        if sees {
            assert_eq!(row["document_id"], invoice["id"]);
            assert_eq!(row["doc_type"], "vendor_invoice");
            assert_eq!(row["number"], invoice["number"]);
        } else {
            for field in ["document_id", "doc_type", "number"] {
                assert_eq!(row[field], Value::Null, "{field}: {row}");
            }
        }
    }

    // The account ledger: the entries of the payables account, without
    // the memo and the document of a posting out of reach.
    let ledger = |token: &str| {
        let (t, token) = (&t, token.to_owned());
        let input = json!({ "company_id": f.c.id, "account_id": f.payable });
        async move {
            let (status, ledger) = t.command(&token, "gl.account_ledger", input).await;
            assert_eq!(status, StatusCode::OK, "{ledger}");
            ledger["entries"][0].clone()
        }
    };
    let bookkeeper = creditor("books@example.com", json!(["gl_entry:read"])).await;
    let auditor = creditor(
        "audit@example.com",
        json!(["gl_entry:read", "vendor_invoice:read"]),
    )
    .await;
    let seen = ledger(&auditor).await;
    assert_eq!(seen["document_id"], invoice["id"]);
    assert_eq!(seen["document_type"], "vendor_invoice");
    assert_eq!(seen["document_number"], invoice["number"]);
    assert_eq!(seen["posting_memo"], "Spring order");
    let hidden = ledger(&bookkeeper).await;
    assert_eq!(dec(&hidden["credit"]), 20.0);
    assert_eq!(hidden["posting_id"], seen["posting_id"]);
    for field in [
        "document_id",
        "document_type",
        "document_number",
        "posting_memo",
    ] {
        assert_eq!(hidden[field], Value::Null, "{field}: {hidden}");
    }

    // A payment shows which invoice each settlement is for only to a
    // reader of that invoice.
    let paid = f
        .done(
            &t,
            "payment",
            "payment.post",
            json!({
                "direction": "outgoing", "bank_account_id": f.c.cash,
                "allocations": [{ "open_item_id": item["id"], "amount": "5.00" }],
            }),
        )
        .await;
    let settlement = |token: &str| {
        let (t, token) = (&t, token.to_owned());
        let path = format!("/api/documents/payment/{}", id(&paid));
        async move {
            let (status, payment) = t.get(&token, &path).await;
            assert_eq!(status, StatusCode::OK, "{payment}");
            payment["postings"][0]["entries"]["open_items"][0].clone()
        }
    };
    let payer = creditor(
        "payer@example.com",
        json!(["payment:read", "open_item:read"]),
    )
    .await;
    let seen = settlement(&t.admin).await;
    assert_eq!(seen["document_id"], invoice["id"]);
    assert_eq!(seen["number"], invoice["number"]);
    let hidden = settlement(&payer).await;
    assert_eq!(hidden["open_item_id"], item["id"]);
    assert_eq!(dec(&hidden["amount"]), 5.0);
    for field in ["document_id", "doc_type", "number"] {
        assert_eq!(hidden[field], Value::Null, "{field}: {hidden}");
    }

    t.stop().await;
}

// ------------------------------------------------------- parsed rule cache

#[tokio::test]
async fn remembered_rules_answer_as_parsed_ones_whatever_else_is_sent() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    // A row rule that every request of this user needs.
    let (token, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!([{ "allow": "account:read", "where": "record.account_type == 'asset'" }]),
            Some(&c.id),
        )
        .await;
    let listed = |condition: String| {
        let (t, token) = (&t, token.clone());
        async move {
            t.get(
                &token,
                &format!("/api/entities/account?where={}", encoded(&condition)),
            )
            .await
        }
    };
    let (status, first) = listed("!record.is_group".into()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["total"], 1, "cash: the one asset that is not a group");

    // More different conditions than the cache for requests holds, good
    // and broken, each asked twice: the remembered answer is the same.
    for n in 0..300 {
        let good = format!("record.code == 'X{n}' || record.code == '1000'");
        let (status, body) = listed(good).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 1, "{body}");
        if n % 10 == 0 {
            let broken = format!("record.code == '{n}' &&");
            let (status, once) = listed(broken.clone()).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{once}");
            let (_, again) = listed(broken).await;
            assert_eq!(message(&once), message(&again));
            assert!(message(&once).contains("it ends too early"), "{once}");
        }
    }
    // The user's own rule still decides, and the first answer still holds.
    let (status, again) = listed("!record.is_group".into()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["items"], first["items"]);
    let (status, body) = t
        .get(&token, &format!("/api/entities/account/{}", c.revenue))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    t.stop().await;
}
