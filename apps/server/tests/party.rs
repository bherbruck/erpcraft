//! Party data: the particulars, addresses, tax identifiers, electronic
//! addresses and contacts of the company, customers and vendors, kept in
//! one place, read and written with the rights of the record they belong
//! to, gone with it, and read by printing and e-invoices.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};

async fn set(t: &TestApp, token: &str, input: Value) -> (StatusCode, Value) {
    t.command(token, "party.set", input).await
}

#[tokio::test]
async fn a_partys_records_hold_together() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let customer = t
        .create("customer", json!({ "code": "C1", "name": "Buyer LLC" }))
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V1", "name": "Parts Inc" }))
        .await;

    // Addresses: several of each kind, one default of each.
    let (status, body) = set(
        &t,
        &t.admin,
        json!({ "party_type": "customer", "party_id": customer, "addresses": [
            { "kind": "billing", "street": "1 Main St", "city": "Columbus", "region": "OH",
              "postcode": "43004", "country_code": "us", "is_default": true },
            { "kind": "shipping", "street": "9 Dock Rd", "country_code": "US", "is_default": true },
            { "kind": "shipping", "street": "10 Dock Rd", "country_code": "US" },
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["addresses"].as_array().unwrap().len(), 3);
    assert_eq!(body["addresses"][0]["country_code"], "US");
    let (status, body) = set(
        &t,
        &t.admin,
        json!({ "party_type": "customer", "party_id": customer, "addresses": [
            { "kind": "billing", "street": "1 Main St", "country_code": "US", "is_default": true },
            { "kind": "billing", "street": "2 Main St", "country_code": "US", "is_default": true },
        ] }),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "two default billing addresses: {body}"
    );
    // Refused whole: the three addresses before stay.
    let (_, got) = t
        .command(
            &t.admin,
            "party.get",
            json!({ "party_type": "customer", "party_id": customer }),
        )
        .await;
    assert_eq!(got["addresses"].as_array().unwrap().len(), 3);

    // A party is one that exists, of the kind named.
    let (status, _) = set(
        &t,
        &t.admin,
        json!({ "party_type": "vendor", "party_id": customer,
                "addresses": [{ "country_code": "US" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Tax identifiers by scheme, one of each.
    let (status, body) = set(
        &t,
        &t.admin,
        json!({ "party_type": "vendor", "party_id": vendor, "tax_ids": [
            { "scheme": "us_ein", "value": "12-3456789", "country_code": "US" },
            { "scheme": "us_ein", "value": "98-7654321" } ] }),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "{body}");

    // The account the company is paid into is a bank account of its own;
    // only a customer has an invoice reference.
    let (status, _) = set(
        &t,
        &t.admin,
        json!({ "party_type": "company", "party_id": c.id,
                "profile": { "payment_account_id": c.revenue } }),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    let (status, body) = set(
        &t,
        &t.admin,
        json!({ "party_type": "company", "party_id": c.id,
                "profile": { "legal_name": "ACME Holdings Inc", "payment_account_id": c.cash } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = set(
        &t,
        &t.admin,
        json!({ "party_type": "vendor", "party_id": vendor,
                "profile": { "invoice_reference": "PO-1" } }),
    )
    .await;
    assert_ne!(status, StatusCode::OK);

    // The read: the default billing address, tax identifiers by scheme.
    let (status, _) = set(
        &t,
        &t.admin,
        json!({ "party_type": "vendor", "party_id": vendor,
                "tax_ids": [{ "scheme": "us_ein", "value": "12-3456789" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut conn = t.pool.acquire().await.unwrap();
    let id = |s: &str| s.parse().unwrap();
    let data = erpcraft_party::load(&mut conn, "customer", id(&customer))
        .await
        .unwrap();
    assert_eq!(data.name.as_deref(), Some("Buyer LLC"));
    assert_eq!(data.address.unwrap().street.as_deref(), Some("1 Main St"));
    let data = erpcraft_party::load(&mut conn, "vendor", id(&vendor))
        .await
        .unwrap();
    assert_eq!(data.tax_id("us_ein").as_deref(), Some("12-3456789"));
    let data = erpcraft_party::load(&mut conn, "company", id(&c.id))
        .await
        .unwrap();
    assert_eq!(data.legal_name.as_deref(), Some("ACME Holdings Inc"));
    drop(conn);
    t.stop().await;
}

#[tokio::test]
async fn rights_follow_the_owner() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let other = t.company("OTHER", "USD").await;
    let customer = t
        .create("customer", json!({ "code": "C1", "name": "Buyer LLC" }))
        .await;
    for (kind, id) in [
        ("customer", customer.as_str()),
        ("company", other.id.as_str()),
    ] {
        let (status, body) = set(
            &t,
            &t.admin,
            json!({ "party_type": kind, "party_id": id,
                    "addresses": [{ "city": "Columbus", "country_code": "US", "is_default": true }] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let get = |token: String, kind: &'static str, id: String| {
        let t = &t;
        async move {
            t.command(
                &token,
                "party.get",
                json!({ "party_type": kind, "party_id": id }),
            )
            .await
        }
    };

    // Who may read customers reads their party data, and may not change it.
    let (reader, _) = t
        .user_with_rules("reader@example.com", json!(["customer:read"]), Some(&c.id))
        .await;
    let (status, body) = get(reader.clone(), "customer", customer.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["addresses"][0]["city"], "Columbus");
    let (status, _) = set(
        &t,
        &reader,
        json!({ "party_type": "customer", "party_id": customer, "addresses": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A customer is shared by all companies: changing it, and its party
    // data, takes the grant for all companies.
    let (one_company, _) = t
        .user_with_rules(
            "one@example.com",
            json!(["customer:read", "customer:update"]),
            Some(&c.id),
        )
        .await;
    let (status, _) = set(
        &t,
        &one_company,
        json!({ "party_type": "customer", "party_id": customer, "addresses": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (all, _) = t
        .user_with_rules(
            "all@example.com",
            json!(["customer:read", "customer:update"]),
            None,
        )
        .await;
    let (status, body) = set(
        &t,
        &all,
        json!({ "party_type": "customer", "party_id": customer,
                "contacts": [{ "name": "Pat", "email": "pat@buyer.example", "is_default": true }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Without the right to read customers, a customer's data is not there.
    let (stranger, _) = t
        .user_with_rules("stranger@example.com", json!(["vendor:read"]), Some(&c.id))
        .await;
    let (status, _) = get(stranger, "customer", customer.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A company's party data follows the company's row filter: who may read
    // one company does not read another's.
    let (accountant, _) = t
        .user_with_rules("acct@example.com", json!(["company:read"]), Some(&c.id))
        .await;
    let (status, _) = get(accountant.clone(), "company", c.id.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(accountant, "company", other.id.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A user with sales rights only (customers and sales documents, for all
    // companies) changes no vendor's and no company's data.
    let vendor = t
        .create("vendor", json!({ "code": "V1", "name": "Parts Inc" }))
        .await;
    let (sales, _) = t
        .user_with_rules(
            "sales@example.com",
            json!(["customer:*", "sales_order:*", "sales_invoice:*"]),
            None,
        )
        .await;
    for (kind, id) in [("vendor", vendor.as_str()), ("company", c.id.as_str())] {
        let (status, body) = set(
            &t,
            &sales,
            json!({ "party_type": kind, "party_id": id,
                    "contacts": [{ "name": "Mallory" }] }),
        )
        .await;
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
            "{kind}: {status} {body}"
        );
    }
    assert_eq!(
        t.count("SELECT count(*) FROM party_contact WHERE name = 'Mallory'")
            .await,
        0
    );
    // No permission of the party module's own exists to get around this.
    let (status, _) = t.get(&t.admin, "/api/entities/party_address").await;
    assert_ne!(status, StatusCode::OK);
    t.stop().await;
}

#[tokio::test]
async fn a_partys_rows_go_with_it() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let gone = t
        .create(
            "customer",
            json!({ "code": "C1", "name": "Short-lived Ltd" }),
        )
        .await;
    let (status, _) = set(
        &t,
        &t.admin,
        json!({ "party_type": "customer", "party_id": gone,
                "addresses": [{ "country_code": "US", "is_default": true }],
                "tax_ids": [{ "scheme": "us_ein", "value": "11-1111111" }],
                "contacts": [{ "name": "Lee" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = t
        .send(
            "DELETE",
            &format!("/api/entities/customer/{gone}"),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    assert!(status.is_success(), "{status}: {body}");
    for table in ["party_address", "party_tax_id", "party_contact"] {
        assert_eq!(
            t.count(&format!(
                "SELECT count(*) FROM {table} WHERE party_id = '{gone}'"
            ))
            .await,
            0,
            "{table}"
        );
    }

    // A customer that documents name cannot be deleted, as before; its
    // party rows neither block nor change that, and stay with it.
    let item = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let kept = t
        .create("customer", json!({ "code": "C2", "name": "Kept Ltd" }))
        .await;
    set(
        &t,
        &t.admin,
        json!({ "party_type": "customer", "party_id": kept,
                "addresses": [{ "country_code": "US", "is_default": true }] }),
    )
    .await;
    let (status, order) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            json!({ "company_id": c.id, "doc_date": "2026-03-01", "body": {
                "customer_id": kept,
                "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "1",
                            "unit_price": "1.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let (status, _) = t
        .send(
            "DELETE",
            &format!("/api/entities/customer/{kept}"),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    assert!(!status.is_success());
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM party_address WHERE party_id = '{kept}'"
        ))
        .await,
        1
    );
    t.stop().await;
}

#[tokio::test]
async fn a_purchase_document_names_the_vendor_as_seller() {
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
        .create("vendor", json!({ "code": "V1", "name": "Parts Inc" }))
        .await;
    for (kind, id, street, city) in [
        ("vendor", vendor.as_str(), "5 Mill Ln", "Toledo"),
        ("company", c.id.as_str(), "1 HQ Plaza", "Cleveland"),
    ] {
        let (status, body) = set(
            &t,
            &t.admin,
            json!({ "party_type": kind, "party_id": id, "addresses": [
                { "street": street, "city": city, "country_code": "US", "is_default": true }] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, order) = t
        .post(
            &t.admin,
            "/api/documents/purchase_order",
            json!({ "company_id": c.id, "doc_date": "2026-03-01", "body": {
                "vendor_id": vendor,
                "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "1",
                            "unit_price": "1.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let (status, parties) = t
        .command(
            &t.admin,
            "document.parties",
            json!({ "doc_type": "purchase_order", "id": order["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{parties}");
    assert_eq!(parties["seller"]["kind"], "vendor");
    assert_eq!(parties["seller"]["city"], "Toledo");
    assert_eq!(parties["buyer"]["kind"], "company");
    assert_eq!(parties["buyer"]["street"], "1 HQ Plaza");
    t.stop().await;
}

/// The e-invoicing module kept these before the party module did: its
/// migration moves them across. The old tables are made again here, with
/// rows, and the migration's SQL is run on them.
#[tokio::test]
async fn e_invoice_party_rows_move_across() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "EUR").await;
    let customer = t
        .create("customer", json!({ "code": "C1", "name": "Kunde AG" }))
        .await;
    // The company already has a billing address of its own: it stays.
    let (status, _) = set(
        &t,
        &t.admin,
        json!({ "party_type": "company", "party_id": c.id, "addresses": [
            { "street": "Kept 1", "city": "Berlin", "country_code": "DE", "is_default": true }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let old = format!(
        r#"
        CREATE TABLE einvoice_profile (
            id uuid PRIMARY KEY, company_id uuid, legal_name text, trading_name text,
            legal_registration text, legal_info text, vat_id text, tax_number text,
            street text, additional_street text, city text, postcode text, country_code text,
            contact_name text, contact_phone text, contact_email text,
            electronic_address text, electronic_address_scheme text, payment_account_id uuid);
        CREATE TABLE einvoice_party (
            id uuid PRIMARY KEY, party_type text, party_id uuid, legal_name text, vat_id text,
            tax_number text, legal_registration text, street text, additional_street text,
            city text, postcode text, country_code text, buyer_reference text,
            electronic_address text, electronic_address_scheme text, contact_name text,
            contact_phone text, contact_email text);
        CREATE FUNCTION einvoice_profile_check() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
        CREATE FUNCTION einvoice_party_check() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
        CREATE FUNCTION einvoice_bank_check(uuid, uuid) RETURNS void LANGUAGE plpgsql AS $$ BEGIN END $$;
        INSERT INTO einvoice_profile VALUES (gen_random_uuid(), '{company}', 'ACME GmbH', NULL,
            'HRB 1', NULL, 'DE123456789', '12/345/67890', 'Moved 2', NULL, 'Hamburg', '20095',
            'DE', 'Buchhaltung', '+49 40 1', 'buch@acme.example', 'buch@acme.example', 'EM',
            '{cash}');
        INSERT INTO einvoice_party VALUES (gen_random_uuid(), 'customer', '{customer}', NULL,
            'DE987654321', NULL, NULL, 'Markt 5', NULL, 'Köln', '50667', 'DE',
            '04011000-12345-03', 'einkauf@kunde.example', 'EM', NULL, NULL, NULL);
        "#,
        company = c.id,
        cash = c.cash,
    );
    sqlx::raw_sql(&old).execute(&t.pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../modules/einvoice/migrations/0003_party_data_in_party.sql"
    ))
    .execute(&t.pool)
    .await
    .unwrap();

    let mut conn = t.pool.acquire().await.unwrap();
    let us = erpcraft_party::load(&mut conn, "company", c.id.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(us.legal_name.as_deref(), Some("ACME GmbH"));
    assert_eq!(us.legal_registration.as_deref(), Some("HRB 1"));
    assert_eq!(us.payment_account_id.unwrap().to_string(), c.cash);
    assert_eq!(us.tax_id("eu_vat").as_deref(), Some("DE123456789"));
    assert_eq!(us.tax_id("tax_number").as_deref(), Some("12/345/67890"));
    assert_eq!(
        us.address.as_ref().unwrap().street.as_deref(),
        Some("Kept 1")
    );
    assert_eq!(us.contact_email.as_deref(), Some("buch@acme.example"));
    assert_eq!(
        us.electronic_address,
        Some(("EM".to_owned(), "buch@acme.example".to_owned()))
    );
    let them = erpcraft_party::load(&mut conn, "customer", customer.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(them.invoice_reference.as_deref(), Some("04011000-12345-03"));
    assert_eq!(them.address.as_ref().unwrap().city.as_deref(), Some("Köln"));
    assert_eq!(them.tax_id("eu_vat").as_deref(), Some("DE987654321"));
    drop(conn);
    let gone: Option<String> = sqlx::query_scalar("SELECT to_regclass('einvoice_profile')::text")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(gone, None);
    t.stop().await;
}

/// Every command a field looks its choices up with accepts what the
/// generic lookup sends (`limit`), given the field's params: here filled
/// from a company, a customer and a vendor that exist.
#[tokio::test]
async fn every_lookup_command_takes_the_standard_lookup_inputs() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let customer = t
        .create("customer", json!({ "code": "C1", "name": "Buyer LLC" }))
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V1", "name": "Parts Inc" }))
        .await;
    // An asset, whose capitalisation looks up the lines it can come from.
    let cost = t
        .create(
            "account",
            json!({ "company_id": c.id, "code": "1500", "name": "Equipment",
                    "account_type": "asset" }),
        )
        .await;
    let class = t
        .create(
            "asset_class",
            json!({ "company_id": c.id, "code": "EQ", "name": "Equipment",
                    "method": "straight_line", "useful_life_months": 12 }),
        )
        .await;
    t.create(
        "account_rule",
        json!({ "company_id": c.id, "purpose": "asset_cost", "scope_type": "asset_class",
                "scope_id": class, "account_id": cost }),
    )
    .await;
    let asset = t
        .create(
            "asset",
            json!({ "company_id": c.id, "class_id": class, "code": "A1", "name": "Laptop" }),
        )
        .await;
    let (status, meta) = t.get(&t.admin, "/api/meta").await;
    assert_eq!(status, StatusCode::OK);
    // Every lookup anywhere in the description.
    fn walk(value: &Value, found: &mut Vec<Value>) {
        match value {
            Value::Object(map) => {
                if map.get("command").is_some_and(Value::is_string)
                    && map.get("rows").is_some_and(Value::is_string)
                    && map.contains_key("params")
                {
                    found.push(value.clone());
                }
                map.values().for_each(|v| walk(v, found));
            }
            Value::Array(list) => list.iter().for_each(|v| walk(v, found)),
            _ => {}
        }
    }
    let mut lookups = vec![];
    walk(&meta, &mut lookups);
    assert!(
        lookups.iter().any(|l| l["command"] == "party.get"),
        "{lookups:?}"
    );
    let mut seen = std::collections::BTreeSet::new();
    for lookup in &lookups {
        let mut input = json!({ "limit": 100 });
        for (param, source) in lookup["params"].as_object().unwrap() {
            input[param] = match source["from"].as_str() {
                Some("value") => source["value"].clone(),
                _ => {
                    let field = source["field"].as_str().unwrap_or_default();
                    match (source["map"].as_object(), field) {
                        (Some(map), _) => map.values().next().unwrap().clone(),
                        (None, "company_id") => json!(c.id),
                        (None, "customer_id") => json!(customer),
                        (None, "vendor_id") => json!(vendor),
                        // The record the field belongs to: here an asset.
                        (None, "id") => json!(asset),
                        (None, other) => panic!("no value for lookup field {other}: {lookup}"),
                    }
                }
            };
        }
        let command = lookup["command"].as_str().unwrap();
        if !seen.insert((command.to_owned(), input.to_string())) {
            continue;
        }
        let (status, out) = t.command(&t.admin, command, input.clone()).await;
        assert_eq!(status, StatusCode::OK, "{command} {input}: {out}");
        let rows = lookup["rows"].as_str().unwrap();
        assert!(out[rows].is_array(), "{command}: {out}");
    }
    t.stop().await;
}
