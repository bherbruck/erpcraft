//! A benchmark, not a test: how long a journal entry takes to save and to
//! post, by number of lines. Ignored by default. Run it with
//!
//! ```text
//! cargo test -p erpcraft-server --test bench -- --ignored --nocapture
//! ```
//!
//! The times include the HTTP layer and the command executor, in process.

mod common;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

const ROUNDS: usize = 40;

fn median(mut times: Vec<Duration>) -> f64 {
    times.sort();
    times[times.len() / 2].as_secs_f64() * 1000.0
}

#[tokio::test]
#[ignore = "a benchmark: run it with --ignored --nocapture"]
async fn journal_entries_by_number_of_lines() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    println!("lines   save ms   post ms   post ms per line   (median of {ROUNDS})");
    let mut results = vec![];
    // The first size is run once more beforehand, to warm up connections
    // and caches.
    for (round, lines) in [2usize, 2, 20, 100].into_iter().enumerate() {
        let body: Vec<Value> = (0..lines)
            .map(|n| {
                if n % 2 == 0 {
                    json!({ "account_id": c.cash, "debit": "1.25" })
                } else {
                    json!({ "account_id": c.revenue, "credit": "1.25" })
                }
            })
            .collect();
        let (mut saves, mut posts) = (vec![], vec![]);
        for _ in 0..ROUNDS {
            let started = Instant::now();
            let (status, draft) = t
                .post(
                    &t.admin,
                    "/api/documents/journal_entry",
                    json!({
                        "company_id": c.id,
                        "doc_date": "2026-03-15",
                        "body": { "lines": body },
                    }),
                )
                .await;
            saves.push(started.elapsed());
            assert_eq!(status, StatusCode::CREATED, "{draft}");

            let started = Instant::now();
            let (status, posted) = t
                .command(&t.admin, "journal_entry.post", json!({ "id": id(&draft) }))
                .await;
            posts.push(started.elapsed());
            assert_eq!(status, StatusCode::OK, "{posted}");
        }
        if round > 0 {
            results.push((lines, median(saves), median(posts)));
        }
    }
    let base = results[0].2;
    for (lines, save, post) in results {
        let per_line = if lines > 2 {
            format!("{:.3}", (post - base) / (lines - 2) as f64)
        } else {
            "-".into()
        };
        println!("{lines:>5} {save:>9.2} {post:>9.2} {per_line:>18}");
    }

    t.stop().await;
}

/// Where the time of one ledger entry goes inside the database: the plan
/// of a 100-entry insert with the time spent in each trigger, then each of
/// the lookups the rules of an entry need, timed alone. A trigger made them
/// for every row until `gl_entry_check` took over, once per statement.
#[tokio::test]
#[ignore = "a benchmark: run it with --ignored --nocapture"]
async fn where_a_ledger_entry_spends_its_time() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let draft = t.draft(&t.admin, &c, "10.00").await;
    let (status, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");

    let mut tx = t.pool.begin().await.unwrap();
    // A second posting for the same document, to hang 100 entries on.
    let posting: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO posting (id, company_id, document_id, command, posting_date, created_by)
         SELECT gen_random_uuid(), company_id, document_id, command, posting_date, created_by
         FROM posting LIMIT 1 RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN (ANALYZE, BUFFERS OFF)
         INSERT INTO gl_entry (id, company_id, posting_id, line_no, account_id, posting_date,
             debit, credit, currency, txn_debit, txn_credit, fx_rate)
         SELECT gen_random_uuid(), p.company_id, p.id, n,
             CASE WHEN n % 2 = 1 THEN $2::uuid ELSE $3::uuid END, p.posting_date,
             CASE WHEN n % 2 = 1 THEN 1.25 ELSE 0 END, CASE WHEN n % 2 = 1 THEN 0 ELSE 1.25 END,
             'USD',
             CASE WHEN n % 2 = 1 THEN 1.25 ELSE 0 END, CASE WHEN n % 2 = 1 THEN 0 ELSE 1.25 END, 1
         FROM posting p, generate_series(1, 100) AS n WHERE p.id = $1 ORDER BY n",
    )
    .bind(posting)
    .bind(uuid::Uuid::parse_str(&c.cash).unwrap())
    .bind(uuid::Uuid::parse_str(&c.revenue).unwrap())
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    println!("--- 100 entries in one statement");
    for line in plan {
        println!("{line}");
    }

    // The trigger's lookups, 1,000 times each, in microseconds per call.
    let lookups = [
        ("posting by id", "PERFORM 1 FROM posting WHERE id = $1"),
        ("account by id", "PERFORM 1 FROM account WHERE id = $2"),
        (
            "open fiscal period, FOR SHARE",
            "PERFORM 1 FROM fiscal_period WHERE company_id = $3 AND status = 'open'
                 AND DATE '2026-03-15' BETWEEN start_date AND end_date FOR SHARE",
        ),
        (
            "open fiscal period, no lock",
            "PERFORM 1 FROM fiscal_period WHERE company_id = $3 AND status = 'open'
                 AND DATE '2026-03-15' BETWEEN start_date AND end_date",
        ),
        (
            "company and currency",
            "PERFORM 1 FROM company c JOIN currency cur ON cur.code = c.base_currency WHERE c.id = $3",
        ),
        (
            "currency by code",
            "PERFORM 1 FROM currency WHERE code = 'USD'",
        ),
        ("nothing (loop overhead)", "NULL"),
    ];
    sqlx::query("CREATE TEMP TABLE bench_time (label text, micros numeric)")
        .execute(&mut *tx)
        .await
        .unwrap();
    // Twice over: the second pass shows how steady the numbers are.
    for (label, statement) in lookups.iter().chain(lookups.iter()) {
        let statement = statement
            .replace("$1", &format!("'{posting}'::uuid"))
            .replace("$2", &format!("'{}'::uuid", c.cash))
            .replace("$3", &format!("'{}'::uuid", c.id));
        sqlx::query(&format!(
            "DO $bench$
             DECLARE started timestamptz := clock_timestamp();
             BEGIN
                 FOR i IN 1..1000 LOOP {statement}; END LOOP;
                 INSERT INTO bench_time VALUES ('{label}',
                     extract(epoch FROM clock_timestamp() - started) * 1000);
             END $bench$"
        ))
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    let times: Vec<(String, sqlx::types::Decimal)> =
        sqlx::query_as("SELECT label, round(micros, 1) FROM bench_time")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    println!("--- lookups of the trigger, microseconds per call");
    for (label, micros) in times {
        println!("{micros:>8}  {label}");
    }
    let definition: String =
        sqlx::query_scalar("SELECT pg_get_functiondef('gl_entry_check'::regproc)")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    println!("--- the trigger as installed\n{definition}");
    tx.rollback().await.unwrap();

    t.stop().await;
}

/// What remembering parsed rules saves on an ordinary request: a list
/// read by a user whose role has 20 rules, 5 of them with a condition,
/// one of which is a row rule on the entity listed, with a `where` of the
/// caller's own. Run it twice to compare:
///
/// ```text
/// cargo test -p erpcraft-server --test bench a_list_for_a_user -- --ignored --nocapture
/// ERPCRAFT_RULE_CACHE=off cargo test -p erpcraft-server --test bench a_list_for_a_user -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "a benchmark: run it with --ignored --nocapture"]
async fn a_list_for_a_user_with_a_role_of_twenty_rules() {
    const REQUESTS: usize = 300;
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let mut rules = vec![
        json!({ "allow": "account:read", "where": "record.active || user.is_manager" }),
        json!({ "allow": "item:read", "where": "record.active && user.region in ['EU', 'US']" }),
        json!({ "deny": "journal_entry:post", "where": "record.created_by == user.id" }),
        json!({ "allow": "journal_entry:read", "where": "record.doc_date >= '2026-01-01'" }),
        json!({ "allow": "warehouse:read", "where": "record.code in user.warehouses" }),
    ];
    for n in 0..15 {
        rules.push(json!(format!("resource_{n}:read")));
    }
    let (token, user) = t
        .user_with_rules("reader@example.com", json!(rules), Some(&c.id))
        .await;
    t.set_attributes(
        &user,
        json!({ "is_manager": false, "region": "EU", "warehouses": ["MAIN"] }),
    )
    .await;

    let path = "/api/entities/account?where=record.account_type%20%3D%3D%20%27asset%27%20%26%26%20!record.is_group";
    let mut times = vec![];
    // The first requests warm up connections, and the cache when it is on.
    for round in 0..REQUESTS + 20 {
        let started = Instant::now();
        let (status, body) = t.get(&token, path).await;
        if round >= 20 {
            times.push(started.elapsed());
        }
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 1, "{body}");
    }
    let cache = match std::env::var("ERPCRAFT_RULE_CACHE").as_deref() {
        Ok("off") => "off",
        _ => "on",
    };
    times.sort();
    println!(
        "rule cache {cache}: median {:.2} ms, 90th percentile {:.2} ms per list request (of {REQUESTS})",
        median(times.clone()),
        times[times.len() * 9 / 10].as_secs_f64() * 1000.0,
    );

    t.stop().await;
}

// ------------------------------------------------- statements per command

/// What one statement cost, as sqlx reports it.
struct Statement {
    sql: String,
    secs: f64,
}

/// Collects the statements sqlx runs while `ON` is set. sqlx reports each
/// statement as a `sqlx::query` event; asking for them costs a little, so
/// timed rounds run with collection off.
mod capture {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tracing::field::{Field, Visit};
    use tracing::subscriber::Interest;
    use tracing::{Event, Metadata, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};

    use super::Statement;

    pub static ON: AtomicBool = AtomicBool::new(false);
    pub static SEEN: Mutex<Vec<Statement>> = Mutex::new(Vec::new());

    pub struct Collect;

    #[derive(Default)]
    struct Fields {
        summary: String,
        statement: String,
        secs: f64,
    }

    impl Visit for Fields {
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "summary" => self.summary = value.to_owned(),
                "db.statement" => self.statement = value.trim().to_owned(),
                _ => {}
            }
        }
        fn record_f64(&mut self, field: &Field, value: f64) {
            if field.name() == "elapsed_secs" {
                self.secs = value;
            }
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "summary" {
                self.summary = format!("{value:?}").trim_matches('"').to_owned();
            }
        }
    }

    impl<S: Subscriber> Layer<S> for Collect {
        fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
            if metadata.target() == "sqlx::query" {
                Interest::sometimes()
            } else {
                Interest::never()
            }
        }

        fn enabled(&self, metadata: &Metadata<'_>, _: Context<'_, S>) -> bool {
            metadata.target() == "sqlx::query" && ON.load(Ordering::Relaxed)
        }

        fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            let sql = if fields.statement.is_empty() {
                fields.summary
            } else {
                fields.statement
            };
            SEEN.lock().unwrap().push(Statement {
                sql,
                secs: fields.secs,
            });
        }
    }
}

/// Statements of one command, grouped by their text with whitespace
/// folded: how many, and how long they took together.
fn grouped(statements: &[Statement]) -> Vec<(String, usize, f64)> {
    let mut groups: std::collections::BTreeMap<String, (usize, f64)> = Default::default();
    for statement in statements {
        let text: String = statement
            .sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let entry = groups.entry(text).or_default();
        entry.0 += 1;
        entry.1 += statement.secs * 1000.0;
    }
    let mut groups: Vec<_> = groups.into_iter().map(|(k, (n, ms))| (k, n, ms)).collect();
    groups.sort_by(|a, b| b.2.total_cmp(&a.2));
    groups
}

/// The posting commands of a purchase, one at a time: how long each takes
/// at the median, how many statements it sends and how long those take.
/// What is not spent in statements is spent in the server's own code.
///
/// ```text
/// cargo test -p erpcraft-server --test bench a_purchase_posted_and_paid -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a benchmark: run it with --ignored --nocapture"]
async fn a_purchase_posted_and_paid() {
    use tracing_subscriber::layer::SubscriberExt;
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(capture::Collect),
    );

    let t = TestApp::start().await;
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
    let mut account = std::collections::BTreeMap::new();
    for (code, kind) in [
        ("1300", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
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
    t.inventory_rule(&c.id, &account["1300"]).await;

    let draft = |doc_type: &'static str, body: Value| {
        let company = c.id.clone();
        let t = &t;
        async move {
            let (status, doc) = t
                .post(
                    &t.admin,
                    &format!("/api/documents/{doc_type}"),
                    json!({ "company_id": company, "doc_date": "2026-03-15", "body": body }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{doc}");
            doc
        }
    };
    const COMMANDS: [&str; 4] = [
        "purchase_order.approve",
        "purchase_receipt.post",
        "vendor_invoice.post",
        "payment.post",
    ];
    // Run `command` on `doc`, timed, and with its statements collected in
    // a capturing round.
    let post = |command: &'static str, doc: Value, capturing: bool| {
        let t = &t;
        async move {
            if capturing {
                capture::SEEN.lock().unwrap().clear();
                capture::ON.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let started = Instant::now();
            let (status, done) = t
                .command(&t.admin, command, json!({ "id": id(&doc) }))
                .await;
            let took = started.elapsed();
            capture::ON.store(false, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(status, StatusCode::OK, "{command}: {done}");
            let statements = std::mem::take(&mut *capture::SEEN.lock().unwrap());
            (done, took, statements)
        }
    };

    const CAPTURED: usize = 5;
    let mut times: Vec<Vec<Duration>> = vec![vec![]; COMMANDS.len()];
    let mut statements: Vec<Vec<Vec<Statement>>> = (0..COMMANDS.len()).map(|_| vec![]).collect();
    // Five rounds to warm up, then the timed ones, then the captured ones.
    for round in 0..5 + ROUNDS + CAPTURED {
        let capturing = round >= 5 + ROUNDS;
        let order = draft(
            "purchase_order",
            json!({ "vendor_id": vendor, "lines": [
                { "item_id": item, "warehouse_id": warehouse, "qty": "4", "unit_price": "2.50" },
                { "item_id": item, "warehouse_id": warehouse, "qty": "1", "unit_price": "7.00" },
            ] }),
        )
        .await;
        let (order, took, seen) = post(COMMANDS[0], order, capturing).await;
        let mut record = |n: usize, took: Duration, seen: Vec<Statement>| {
            if capturing {
                statements[n].push(seen);
            } else if round >= 5 {
                times[n].push(took);
            }
        };
        record(0, took, seen);
        let lines: Vec<Value> = order["body"]["lines"].as_array().unwrap().to_vec();
        let receipt = draft(
            "purchase_receipt",
            json!({ "purchase_order_id": order["id"], "lines": lines.iter().map(|l| json!({ "order_line_id": l["id"], "qty": l["qty"] })).collect::<Vec<_>>() }),
        )
        .await;
        let (_, took, seen) = post(COMMANDS[1], receipt, capturing).await;
        record(1, took, seen);
        let invoice = draft(
            "vendor_invoice",
            json!({ "purchase_order_id": order["id"], "lines": lines.iter().map(|l| json!({ "order_line_id": l["id"], "qty": l["qty"], "unit_price": l["unit_price"] })).collect::<Vec<_>>() }),
        )
        .await;
        let (invoice, took, seen) = post(COMMANDS[2], invoice, capturing).await;
        record(2, took, seen);
        let (_, open) = t
            .command(
                &t.admin,
                "open_item.list",
                json!({ "company_id": c.id, "kind": "payable" }),
            )
            .await;
        let open_item = open["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["document_id"] == invoice["id"])
            .unwrap()
            .clone();
        let payment = draft(
            "payment",
            json!({ "direction": "outgoing", "bank_account_id": c.cash,
                    "allocations": [{ "open_item_id": open_item["id"], "amount": open_item["open"] }] }),
        )
        .await;
        let (_, took, seen) = post(COMMANDS[3], payment, capturing).await;
        record(3, took, seen);
    }

    println!(
        "command                    median ms   statements   in statements ms   (median of {ROUNDS}; statements from {CAPTURED} more)"
    );
    for (n, command) in COMMANDS.iter().enumerate() {
        let runs = &statements[n];
        let count = runs.iter().map(Vec::len).sum::<usize>() as f64 / runs.len() as f64;
        let in_db = runs
            .iter()
            .map(|r| r.iter().map(|s| s.secs).sum::<f64>())
            .sum::<f64>()
            * 1000.0
            / runs.len() as f64;
        println!(
            "{command:<26} {:>9.2} {count:>12.1} {in_db:>18.2}",
            median(times[n].clone())
        );
    }
    let detail = std::env::var("BENCH_DETAIL").is_ok();
    for (n, command) in COMMANDS.iter().enumerate() {
        let all: Vec<Statement> = std::mem::take(&mut statements[n])
            .into_iter()
            .flatten()
            .collect();
        println!("--- {command}: statements per run, by total time");
        for (sql, count, ms) in grouped(&all)
            .into_iter()
            .take(if detail { 200 } else { 25 })
        {
            let shown: String = sql.chars().take(150).collect();
            println!(
                "{:>5.1} x {:>7.3} ms  {shown}",
                count as f64 / CAPTURED as f64,
                ms / CAPTURED as f64
            );
        }
    }

    t.stop().await;
}
