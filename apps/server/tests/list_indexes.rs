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

/// The plan of `sql` with sequential scans off: whether an index can answer
/// the query at all. For trigram indexes the planner's choice depends on how
/// varied the text is, which test data says nothing about; on a copy of
/// the AWC data it chose them (95 to 17 ms for a name filter on 57k
/// customers).
async fn usable(t: &TestApp, sql: &str, bind: &str) -> String {
    let mut tx = t.pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(&format!("EXPLAIN (COSTS OFF) {sql}"))
        .bind(bind)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    plan.join("\n")
}

#[tokio::test]
async fn an_item_list_can_find_items_by_part_of_their_name_through_an_index() {
    let t = TestApp::start().await;
    // `record.name.containsIgnoreCase(...)` as erpcraft-rules compiles it,
    // and `q` as the entity list writes it over code and name.
    let p = usable(
        &t,
        "SELECT * FROM item AS t WHERE (t.name ILIKE $1::text)",
        "%bolt%",
    )
    .await;
    assert!(p.contains("item_name_trgm"), "{p}");
    let p = usable(
        &t,
        "SELECT * FROM item AS t WHERE (t.code::text ILIKE $1 OR t.name::text ILIKE $1)",
        "%bolt%",
    )
    .await;
    assert!(
        p.contains("item_code_trgm") && p.contains("item_name_trgm"),
        "{p}"
    );
}

#[tokio::test]
async fn party_lists_can_find_parties_by_part_of_their_name_through_an_index() {
    let t = TestApp::start().await;
    for table in ["customer", "vendor"] {
        let p = usable(
            &t,
            &format!("SELECT * FROM {table} AS t WHERE (t.name ILIKE $1::text)"),
            "%son%",
        )
        .await;
        assert!(p.contains(&format!("{table}_name_trgm")), "{table}: {p}");
        let p = usable(
            &t,
            &format!(
                "SELECT * FROM {table} AS t WHERE (t.code::text ILIKE $1 OR t.name::text ILIKE $1)"
            ),
            "%son%",
        )
        .await;
        assert!(p.contains(&format!("{table}_code_trgm")), "{table}: {p}");
    }
}

#[tokio::test]
async fn party_and_summary_sorts_use_their_indexes() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    for table in ["customer", "vendor"] {
        sqlx::query(&format!(
            "INSERT INTO {table} (id, code, name)
             SELECT gen_random_uuid(), 'P' || n, 'Party ' || n FROM generate_series(1, 5000) n"
        ))
        .execute(&t.pool)
        .await
        .unwrap();
    }
    for (doc_type, field) in [
        ("sales_order", "customer_name"),
        ("purchase_order", "vendor_name"),
    ] {
        sqlx::query(
            "INSERT INTO document (id, company_id, doc_type, number, status, doc_date, created_by, summary)
             SELECT gen_random_uuid(), $1::uuid, $2, 'D-' || n, 'closed', date '2022-01-01' + (n % 1800),
                    (SELECT id FROM app_user LIMIT 1), jsonb_build_object($3::text, 'Party ' || (n % 900))
             FROM generate_series(1, 5000) n",
        )
        .bind(&c.id)
        .bind(doc_type)
        .bind(field)
        .execute(&t.pool)
        .await
        .unwrap();
    }
    sqlx::query("ANALYZE").execute(&t.pool).await.unwrap();

    // Paging deep into a party list by name.
    for table in ["customer", "vendor"] {
        let p = plan(
            &t,
            &format!("SELECT * FROM {table} AS t ORDER BY t.name ASC NULLS LAST, t.id LIMIT 50 OFFSET 2000"),
            &[],
        )
        .await;
        assert!(p.contains(&format!("{table}_name")), "{table}: {p}");
    }
    // A summary's link sorted by the name the summary copies, as
    // document.list writes it: the same expression, no cast, ascending,
    // nulls last.
    for (doc_type, field, index) in [
        ("sales_order", "customer_name", "document_customer_name"),
        ("purchase_order", "vendor_name", "document_vendor_name"),
    ] {
        let sql = format!(
            "SELECT t.* FROM document AS t WHERE t.doc_type = $1::text AND t.company_id = $2::uuid \
             ORDER BY (t.summary->>'{field}') ASC NULLS LAST, t.id DESC LIMIT 50 OFFSET 2000"
        );
        let p = plan(&t, &sql, &[doc_type, &c.id]).await;
        assert!(p.contains(index), "{doc_type}: {p}");
    }
}
