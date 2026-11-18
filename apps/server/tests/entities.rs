//! Plain records: what the generic entity layer refuses, and how it says so.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

fn rate(value: Value) -> Value {
    json!({ "from_currency": "EUR", "to_currency": "USD", "rate_date": "2026-03-01", "rate": value })
}

#[tokio::test]
async fn a_decimal_finer_than_its_column_is_refused_not_rounded() {
    let t = TestApp::start().await;
    let path = "/api/entities/exchange_rate";

    // The column keeps 18 decimal places. A nineteenth would be rounded
    // away, so it is refused, by name.
    let (status, body) = t
        .post(&t.admin, path, rate(json!("1.1234567890123456789")))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "field 'rate' has at most 18 decimal places");
    // So is a number too fine for any parser to hold exactly.
    let fine = format!("1.{}1", "0".repeat(40));
    let (status, body) = t.post(&t.admin, path, rate(json!(fine))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "field 'rate' has at most 18 decimal places");
    let (status, body) = t.post(&t.admin, path, rate(json!(1e-19))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "field 'rate' has at most 18 decimal places");

    // What is not a number is refused in the kernel's words, not Postgres's.
    for bad in [
        json!("lots"),
        json!("1,5"),
        json!(""),
        json!(true),
        json!(["1"]),
    ] {
        let (status, body) = t.post(&t.admin, path, rate(bad)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(
            message(&body).starts_with("field 'rate' must be a number"),
            "{body}"
        );
    }
    assert_eq!(t.count("SELECT count(*) FROM exchange_rate").await, 0);

    // Zeros beyond the eighteenth place change nothing, so they are accepted.
    let (status, created) = t
        .post(&t.admin, path, rate(json!("1.10000000000000000000")))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["rate"], "1.100000000000000000");
    let one = format!("{path}/{}", created["id"].as_str().unwrap());

    // The same on update.
    let (status, body) = t
        .patch(&t.admin, &one, json!({ "rate": "1.1000000000000000001" }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "field 'rate' has at most 18 decimal places");
    let (status, body) = t.patch(&t.admin, &one, json!({ "rate": "x" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).starts_with("field 'rate' must be a number"));
    let (_, read) = t.get(&t.admin, &one).await;
    assert_eq!(
        read["rate"], "1.100000000000000000",
        "nothing was rounded in"
    );

    let (status, body) = t
        .patch(&t.admin, &one, json!({ "rate": "1.25000000000000000000" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rate"], "1.250000000000000000");
    // A JSON number and an exponent are numbers too.
    let (status, body) = t.patch(&t.admin, &one, json!({ "rate": 1.5 })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rate"], "1.500000000000000000");
    let (status, body) = t.patch(&t.admin, &one, json!({ "rate": "125e-2" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rate"], "1.250000000000000000");

    // Another entity, another scale: each column says what it keeps.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/bom",
            json!({ "item_id": t.create("item", json!({ "code": "CAKE", "name": "Cake" })).await,
                    "code": "CAKE-1", "name": "Cake", "output_qty": "1.0000001" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "field 'output_qty' has at most 6 decimal places"
    );

    t.stop().await;
}

#[tokio::test]
async fn a_broken_constraint_names_the_fields_involved() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // A duplicate says what is duplicated, in the labels a person sees.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/account",
            json!({ "company_id": c.id, "code": "1000", "name": "Cash again", "account_type": "asset" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "conflict");
    assert_eq!(
        message(&body),
        "an account with this Company and Code already exists"
    );
    // The same when an update runs into another record.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account/{}", c.revenue),
            json!({ "code": "1000" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        message(&body),
        "an account with this Company and Code already exists"
    );
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/company",
            json!({ "code": "ACME", "name": "Acme again", "base_currency": "USD" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(message(&body), "a company with this Code already exists");

    let path = "/api/entities/exchange_rate";
    let (status, body) = t.post(&t.admin, path, rate(json!("1.1"))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let one = format!("{path}/{}", body["id"].as_str().unwrap());
    let (status, body) = t.post(&t.admin, path, rate(json!("1.2"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        message(&body),
        "an exchange rate with this From, To and Applies from already exists"
    );

    // A value the table's rules refuse names its field and the rule, and
    // no constraint.
    for zero in ["0", "-1.5"] {
        let (status, body) = t
            .post(&t.admin, path, {
                let mut other = rate(json!(zero));
                other["rate_date"] = json!("2026-04-01");
                other
            })
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["code"], "validation");
        assert_eq!(
            message(&body),
            "field 'rate' was refused: it must satisfy rate > 0"
        );
    }
    let (status, body) = t.patch(&t.admin, &one, json!({ "rate": "0" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "field 'rate' was refused: it must satisfy rate > 0"
    );
    // A rule over two fields names both.
    let (status, body) = t
        .post(
            &t.admin,
            path,
            json!({ "from_currency": "EUR", "to_currency": "EUR", "rate_date": "2026-03-01", "rate": "1" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "fields 'from_currency' and 'to_currency' were refused: \
         they must satisfy from_currency <> to_currency"
    );
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/account",
            json!({ "company_id": c.id, "code": "", "name": "Nameless", "account_type": "asset" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "field 'code' was refused: it must satisfy code <> ''"
    );

    // A reference to a record that is not there names the field, by its
    // label, and no constraint.
    let nowhere = uuid::Uuid::new_v4();
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/account",
            json!({ "company_id": nowhere, "code": "1010", "name": "Till", "account_type": "asset" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "Company refers to a record that does not exist"
    );
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/company",
            json!({ "code": "NOPE", "name": "No such currency", "base_currency": "XXX" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "Accounting currency refers to a record that does not exist"
    );

    // A sentence raised by one of the project's own triggers arrives as it
    // was written.
    let other = t.company("OTHER", "USD").await;
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/account",
            json!({ "company_id": other.id, "code": "1010", "name": "Till", "account_type": "asset", "parent_id": c.group }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "the parent account must belong to the same company"
    );

    t.stop().await;
}
