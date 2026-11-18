//! The indexes lists rely on match the SQL the lists write, expression for
//! expression: with sequential scans off, the planner uses each of them.
//! Tables in a test are tiny, so this checks the match, not the speed.

mod common;

use common::TestApp;

/// The plan of `sql` as the planner chooses it.
async fn plan(t: &TestApp, sql: &str, binds: &[&str]) -> String {
    let explain = format!("EXPLAIN (COSTS OFF) {sql}");
    let mut query = sqlx::query_scalar::<_, String>(&explain);
    for bind in binds {
        query = query.bind(*bind);
    }
    query.fetch_all(&t.pool).await.unwrap().join("\n")
}

#[tokio::test]
async fn document_lists_and_the_summary_refresh_use_the_kernels_indexes() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    // Five years of sales orders, every summary up to date, as a company
    // has after a while; the planner picks indexes by such statistics.
    let version: i32 = t
        .kernel
        .registry()
        .doc_type("sales_order")
        .unwrap()
        .meta
        .summary_version;
    sqlx::query(
        "INSERT INTO document (id, company_id, doc_type, number, status, doc_date, created_by, summary, summary_version)
         SELECT gen_random_uuid(), $1::uuid, 'sales_order', 'SO-' || n, 'closed',
                date '2022-01-01' + (n % 1800), (SELECT id FROM app_user LIMIT 1),
                jsonb_build_object('customer_name', 'Customer ' || (n % 300)), $2
         FROM generate_series(1, 5000) n",
    )
    .bind(&c.id)
    .bind(version)
    .execute(&t.pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE document")
        .execute(&t.pool)
        .await
        .unwrap();

    // A month, as `record.doc_date >= ... && record.doc_date <= ...` compiles.
    let p = plan(
        &t,
        "SELECT count(*) FROM document AS t WHERE t.doc_type = $1::text AND t.company_id = $2::uuid \
         AND ((t.doc_date >= $3::date) AND (t.doc_date <= $4::date))",
        &["sales_order", &c.id, "2024-03-01", "2024-03-31"],
    )
    .await;
    assert!(p.contains("document_type_company_date"), "{p}");

    // The documents whose summary is stale, as the server asks at every
    // start: none are, and finding that reads indexes only.
    let sql = format!(
        "SELECT count(*) {}",
        erpcraft_kernel::document::STALE_SUMMARIES
    )
    .replace("$2::int4", &version.to_string());
    let p = plan(&t, &sql, &["sales_order"]).await;
    assert!(
        p.contains("document_summary_version") && p.contains("document_summary_missing"),
        "{p}"
    );
}
