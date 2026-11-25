//! Review 8: EDI receive across companies (setup copied from edi.rs).
#![allow(dead_code, unused_imports)]

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

const PO850: &str = include_str!("../../../modules/edi/tests/fixtures/po850.x12");

struct Fixture {
    c: Company,
    partner: String,
    customer: String,
    warehouse: String,
    hammer: String,
    tape: String,
    knife: String,
    dc07: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("CH", "USD").await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let account = |code: &'static str, name: &'static str, kind: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": name, "account_type": kind }),
            )
            .await
        }
    };
    let receivable = account("1200", "Accounts receivable", "asset").await;
    let inventory = account("1300", "Inventory", "asset").await;
    let cogs = account("5000", "Cost of goods sold", "expense").await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id,
            "receivable_account_id": receivable,
            "revenue_account_id": c.revenue,
            "cogs_account_id": cogs,
        }),
    )
    .await;
    t.inventory_rule(&c.id, &inventory).await;
    let customer = t
        .create(
            "customer",
            json!({ "code": "NM", "name": "Northfield Mercantile" }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "customer", "party_id": customer, "addresses": [
                { "kind": "billing", "street": "PO Box 4410", "city": "Dayton", "region": "OH",
                  "postcode": "45401", "country_code": "US", "is_default": true },
                { "kind": "shipping", "street": "2200 Logistics Pkwy", "city": "Columbus",
                  "region": "OH", "postcode": "43217", "country_code": "US", "is_default": true },
            ] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let dc07: String = sqlx::query_scalar(
        "SELECT id::text FROM party_address WHERE party_id = $1::uuid AND kind = 'shipping'",
    )
    .bind(&customer)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let item = |code: &'static str, name: &'static str| async move {
        t.create("item", json!({ "code": code, "name": name, "uom": "each" }))
            .await
    };
    let hammer = item("CH-1001", "Claw hammer").await;
    let tape = item("CH-2040", "Tape measure").await;
    let knife = item("CH-3310", "Utility knife").await;
    let partner = t
        .create(
            "edi_partner",
            json!({
                "company_id": c.id, "code": "NM", "name": "Northfield Mercantile",
                "customer_id": customer, "warehouse_id": warehouse, "usage": "production",
                "our_qualifier": "ZZ", "our_id": "CEDARHOLLOW",
                "their_qualifier": "ZZ", "their_id": "NORTHFIELDMERC",
            }),
        )
        .await;
    t.create(
        "edi_uom_xref",
        json!({ "company_id": c.id, "partner_uom": "EA", "uom": "each" }),
    )
    .await;
    t.create(
        "edi_item_xref",
        json!({ "company_id": c.id, "item_id": hammer, "kind": "gtin", "value": "040000100010" }),
    )
    .await;
    t.create(
        "edi_item_xref",
        json!({ "company_id": c.id, "partner_id": partner, "item_id": tape, "kind": "gtin",
                "value": "10400001000202", "partner_uom": "CA", "units_per": "6" }),
    )
    .await;
    t.create(
        "edi_location_xref",
        json!({ "company_id": c.id, "partner_id": partner, "qualifier": "92", "code": "0007", "address_id": dc07 }),
    )
    .await;
    Fixture {
        c,
        partner,
        customer,
        warehouse,
        hammer,
        tape,
        knife,
        dc07,
    }
}

async fn receive(t: &TestApp, token: &str, content: &str) -> (StatusCode, Value) {
    t.command(token, "edi.receive", json!({ "content": content }))
        .await
}

async fn received(t: &TestApp, content: &str) -> Value {
    let (status, body) = receive(t, &t.admin, content).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// The order with another interchange control number (ISA13 and IEA02),
/// so it is new bytes from the partner.
fn renumbered(content: &str, control: u32) -> String {
    content
        .replace("*000000417*", &format!("*{control:09}*"))
        .replace("IEA*1*000000417", &format!("IEA*1*{control:09}"))
}

async fn content(t: &TestApp, interchange: &Value) -> String {
    let (status, body) = t
        .command(
            &t.admin,
            "edi_interchange.content",
            json!({ "id": interchange["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["content"].as_str().unwrap().to_owned()
}

async fn proposal(t: &TestApp, token: &str, message: &Value) -> Value {
    let (status, body) = t
        .command(
            token,
            "edi.proposal",
            json!({ "message_id": message["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn findings<'a>(proposal: &'a Value, code: &str) -> Vec<&'a Value> {
    proposal["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == code)
        .collect()
}

/// What the third line needs: it names no item known here.
fn knife_choice(f: &Fixture) -> Value {
    json!({ "3": f.knife })
}

/// Two companies trade with the same partner under the same ids (as
/// 0003 allows). The file is received for the first company. A user who
/// holds edi:receive only in the second company receives the same bytes
/// for its own company: the route finds its own partner, the permission
/// check passes, the global sha256 unique index makes it a "repeat", and
/// the output is the FIRST company's interchange and messages.
#[tokio::test]
async fn review8_a_repeat_for_another_company_returns_the_first_companys_interchange() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let other = t.company("OT", "USD").await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": other.id, "code": "W", "name": "W" }),
        )
        .await;
    t.create(
        "edi_partner",
        json!({
            "company_id": other.id, "code": "NM", "name": "Northfield Mercantile",
            "customer_id": f.customer, "warehouse_id": warehouse, "usage": "production",
            "our_qualifier": "ZZ", "our_id": "CEDARHOLLOW",
            "their_qualifier": "ZZ", "their_id": "NORTHFIELDMERC",
        }),
    )
    .await;
    let theirs = renumbered(PO850, 777);
    let (status, first) = t
        .command(
            &t.admin,
            "edi.receive",
            json!({ "content": theirs, "company_id": f.c.id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let outsider = t
        .user_with_role("outsider@example.com", &["edi:receive"], &other.id)
        .await;
    let (status, got) = t
        .command(
            &outsider,
            "edi.receive",
            json!({ "content": theirs, "company_id": other.id }),
        )
        .await;
    t.stop().await;
    assert_ne!(
        got["interchange"]["company_id"],
        json!(f.c.id),
        "a user of company OT was handed company CH's interchange ({status}): {got}"
    );
}

/// edi.receive promises files up to 4 MiB, but the HTTP body (axum's
/// default 2 MB limit on `Bytes`) refuses a 3 MB file before the command
/// sees it.
#[tokio::test]
async fn review8_a_three_megabyte_file_reaches_edi_receive() {
    let t = TestApp::start().await;
    let content = "X".repeat(3 * 1024 * 1024);
    let (status, body) = t
        .send(
            "POST",
            "/api/commands/edi.receive",
            Some(&t.admin),
            Some(json!({ "content": content })),
            &[],
        )
        .await;
    t.stop().await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
