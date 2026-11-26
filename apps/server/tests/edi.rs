//! Electronic data interchange: receiving a partner's X12 850, the 997
//! back, the proposal and the sales order drafted from it.
//!
//! The fixture is the module's own made-up order (modules/edi/tests/
//! fixtures/po850.x12): a buyer that is not a real company orders three
//! tools from a seller that is not one either.

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

#[tokio::test]
async fn an_order_is_received_acknowledged_proposed_and_drafted() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    let first = received(&t, PO850).await;
    assert_eq!(first["interchange"]["state"], "read", "{first}");
    assert_eq!(first["interchange"]["partner_id"], json!(f.partner));
    assert_eq!(first["repeat"], false);
    let messages = first["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["state"], "proposed");
    assert_eq!(messages[0]["business_key"], "4500018842");
    let acks = first["acknowledgements"].as_array().unwrap();
    assert_eq!(acks.len(), 1);
    assert_eq!(
        (acks[0]["message"].as_str(), acks[0]["state"].as_str()),
        (Some("997"), Some("ready"))
    );

    // The 997 as the partner will read it: from us to them, accepting set
    // 0001 of group 417.
    let (status, ack_interchange) = t
        .get(
            &t.admin,
            &format!(
                "/api/entities/edi_interchange/{}",
                acks[0]["interchange_id"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{ack_interchange}");
    let bytes = content(&t, &ack_interchange).await;
    assert!(
        bytes.starts_with("ISA*00*          *00*          *ZZ*CEDARHOLLOW    *ZZ*NORTHFIELDMERC *"),
        "{bytes}"
    );
    let back = &erpcraft_edi::x12::read(bytes.as_bytes()).unwrap()[0];
    assert!(back.errors.is_empty());
    let ack = erpcraft_edi::x12::ack::FunctionalAck::read(&back.groups[0].sets[0]).unwrap();
    assert_eq!(
        (ack.functional_id.as_str(), ack.group_control.as_str()),
        ("PO", "417")
    );
    assert_eq!(ack.sets[0].control, "0001");
    assert_eq!(ack.sets[0].status, erpcraft_edi::x12::ack::Status::Accepted);
    assert_eq!(ack.status, erpcraft_edi::x12::ack::Status::Accepted);

    // The same bytes again: the same result, nothing more written.
    let interchanges = t.count("SELECT count(*) FROM edi_interchange").await;
    let again = received(&t, PO850).await;
    assert_eq!(again["repeat"], true);
    assert_eq!(again["interchange"]["id"], first["interchange"]["id"]);
    assert_eq!(again["messages"], first["messages"]);
    assert_eq!(again["acknowledgements"], first["acknowledgements"]);
    assert_eq!(
        t.count("SELECT count(*) FROM edi_interchange").await,
        interchanges
    );
    assert_eq!(
        t.count("SELECT next_value FROM edi_control WHERE kind = 'interchange'")
            .await,
        2
    );

    // The proposal: two lines found, the third not.
    let message = &first["messages"][0];
    let p = proposal(&t, &t.admin, message).await;
    assert_eq!(p["would_draft"], false, "{p}");
    assert_eq!(p["customer"]["code"], "NM");
    assert_eq!(p["ship_to"]["how"], "location");
    assert_eq!(p["ship_to"]["address_id"], json!(f.dc07));
    let unknown = findings(&p, "item_unknown");
    assert_eq!(unknown.len(), 1, "{p}");
    assert_eq!(unknown[0]["line"], "3");
    assert_eq!(unknown[0]["severity"], "blocking");
    assert_eq!(p["lines"][0]["how_matched"], "gtin 040000100010");
    assert_eq!(p["lines"][0]["item_id"], json!(f.hammer));
    // The case of six: 120 CA at 31.20 is 720 each at 5.20.
    let tape = &p["lines"][1];
    assert_eq!(tape["item_id"], json!(f.tape));
    assert_eq!(dec(&tape["units_per"]), 6.0);
    assert_eq!(dec(&tape["qty"]), 720.0);
    assert_eq!(tape["unit_price"], "5.2");
    // The routing segment has no field here: a note.
    assert_eq!(findings(&p, "unhandled").len(), 1);

    // Refused while a blocking finding stands, and while notes stand.
    let (status, body) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": message["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Line 3"),
        "{body}"
    );
    let (status, body) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": message["id"], "items": knife_choice(&f), "remember": true }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("accept_notes"),
        "{body}"
    );

    let (status, order) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": message["id"], "items": knife_choice(&f), "remember": true,
                    "accept_notes": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(order["doc_type"], "sales_order");
    assert_eq!(order["status"], "draft");
    assert_eq!(order["body"]["customer_id"], json!(f.customer));
    assert_eq!(order["body"]["shipping_address_id"], json!(f.dc07));
    let lines = order["body"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1]["item_id"], json!(f.tape));
    assert_eq!(dec(&lines[1]["qty"]), 720.0);
    assert_eq!(dec(&lines[1]["unit_price"]), 5.2);
    assert_eq!(lines[2]["item_id"], json!(f.knife));
    assert_eq!(lines[0]["warehouse_id"], json!(f.warehouse));
    // A distribution centre may ship from a warehouse of its own.
    let east = t
        .create(
            "warehouse",
            json!({ "company_id": f.c.id, "code": "EAST", "name": "East" }),
        )
        .await;
    let location: String = sqlx::query_scalar("SELECT id::text FROM edi_location_xref")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/edi_location_xref/{location}"),
            json!({ "warehouse_id": east }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let later = proposal(&t, &t.admin, message).await;
    assert_eq!(later["warehouse_id"], json!(east), "{later}");

    // To the cent what the order says: 48 x 12.50 + 120 x 31.20 + 24 x 7.95.
    assert_eq!(dec(&order["summary"]["total_net"]), 600.0 + 3744.0 + 190.8);
    // The partner's order number is the order's customer reference; the
    // order asks for no delivery or ship date, so its window stays in the
    // memo.
    assert_eq!(order["body"]["customer_reference"], "4500018842", "{order}");
    assert_eq!(
        order["summary"]["customer_reference"], "4500018842",
        "{order}"
    );
    assert_eq!(order["body"]["requested_date"], Value::Null, "{order}");
    let memo = order["memo"].as_str().unwrap();
    assert!(memo.starts_with("EDI order from partner"), "{memo}");
    assert!(!memo.contains("4500018842"), "{memo}");
    assert!(memo.contains("Ship not before: 2026-12-01."), "{memo}");

    // What the order said is kept as sent.
    let kept: (
        String,
        erpcraft_kernel::Decimal,
        erpcraft_kernel::Decimal,
        erpcraft_kernel::Decimal,
    ) = sqlx::query_as(
        "SELECT partner_uom, partner_qty, partner_price, units_per FROM edi_document_line
             WHERE document_id = $1::uuid AND partner_line_id = '2'",
    )
    .bind(order["id"].as_str().unwrap())
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(kept.0, "CA");
    assert_eq!(kept.1.normalize().to_string(), "120");
    assert_eq!(kept.2.normalize().to_string(), "31.2");
    assert_eq!(kept.3.normalize().to_string(), "6");
    let (_, state) = message_state(&t, message).await;
    assert_eq!(state, "drafted");

    // The draft is approved like any order.
    let (status, body) = t
        .command(
            &t.admin,
            "sales_order.approve",
            json!({ "id": order["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The knife was remembered: the next order of the partner, another
    // number, resolves by itself.
    let next = PO850.replace("BEG*00*SA*4500018842", "BEG*00*SA*4500018843");
    let next = received(&t, &renumbered(&next, 418)).await;
    let p = proposal(&t, &t.admin, &next["messages"][0]).await;
    assert!(findings(&p, "item_unknown").is_empty(), "{p}");
    assert_eq!(p["lines"][2]["how_matched"], "gtin 040000100034");
    assert_eq!(p["would_draft"], true, "{p}");

    // Two acknowledgements now, numbered one after the other.
    let controls: Vec<String> = sqlx::query_scalar(
        "SELECT control FROM edi_interchange WHERE direction = 'out' ORDER BY control",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(controls, ["000000001", "000000002"]);
    t.stop().await;
}

async fn message_state(t: &TestApp, message: &Value) -> (String, String) {
    sqlx::query_as("SELECT id::text, state FROM edi_message WHERE id = $1::uuid")
        .bind(message["id"].as_str().unwrap())
        .fetch_one(&t.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_reused_control_number_an_unknown_sender_and_test_traffic() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    received(&t, PO850).await;

    // The same control number with other bytes: kept, rejected, and a TA1
    // says why; no 997.
    let other = PO850.replace("BEG*00*SA*4500018842", "BEG*00*SA*4500019999");
    let dup = received(&t, &other).await;
    assert_eq!(dup["interchange"]["state"], "read");
    assert_eq!(dup["messages"][0]["state"], "rejected");
    assert_eq!(
        dup["messages"][0]["findings"][0]["code"],
        "duplicate_interchange"
    );
    let acks = dup["acknowledgements"].as_array().unwrap();
    assert_eq!(acks.len(), 1, "{dup}");
    assert_eq!(acks[0]["message"], "TA1");
    let (status, body) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": dup["messages"][0]["id"], "accept_notes": true }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // An unknown sender: kept as unroutable, nothing else.
    let before_messages = t.count("SELECT count(*) FROM edi_message").await;
    let before_interchanges = t.count("SELECT count(*) FROM edi_interchange").await;
    let stranger = PO850.replace(
        "*ZZ*NORTHFIELDMERC *ZZ*CEDARHOLLOW    *",
        "*ZZ*SOMEONEELSE    *ZZ*CEDARHOLLOW    *",
    );
    let lost = received(&t, &stranger).await;
    assert_eq!(lost["interchange"]["state"], "unroutable", "{lost}");
    assert_eq!(lost["interchange"]["partner_id"], Value::Null);
    assert!(lost["messages"].as_array().unwrap().is_empty());
    assert!(lost["acknowledgements"].as_array().unwrap().is_empty());
    assert_eq!(
        t.count("SELECT count(*) FROM edi_message").await,
        before_messages
    );
    assert_eq!(
        t.count("SELECT count(*) FROM edi_interchange").await,
        before_interchanges + 1
    );

    // A test interchange to a production partner: acknowledged, never a
    // document.
    let test = renumbered(&PO850.replace("*0*P*>~", "*0*T*>~"), 501);
    let test = received(&t, &test).await;
    assert_eq!(test["messages"][0]["state"], "test", "{test}");
    assert_eq!(test["acknowledgements"][0]["message"], "997");
    let ack = content(
        &t,
        &json!({ "id": test["acknowledgements"][0]["interchange_id"] }),
    )
    .await;
    assert!(
        ack.contains("*0*T*>~"),
        "the 997 of a test is a test: {ack}"
    );
    let (status, body) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": test["messages"][0]["id"], "accept_notes": true }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let p = proposal(&t, &t.admin, &test["messages"][0]).await;
    assert_eq!(findings(&p, "test").len(), 1);

    // Unreadable bytes are kept too, with the reason.
    let cut = renumbered(PO850, 502);
    let cut = &cut[..cut.len() - 20];
    let unread = received(&t, cut).await;
    assert_eq!(unread["interchange"]["state"], "unreadable");
    assert!(
        unread["interchange"]["error"]
            .as_str()
            .unwrap()
            .contains("cut off"),
        "{unread}"
    );
    t.stop().await;
}

#[tokio::test]
async fn a_broken_set_is_rejected_and_the_997_says_where() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let broken = PO850
        .replace("PO1*1*48*EA", "PO1*1*4>8*EA")
        .replace("DTM*038*20261205", "DTM*038*20261305");
    let got = received(&t, &broken).await;
    let message = &got["messages"][0];
    assert_eq!(message["state"], "rejected", "{got}");
    let words = message["findings"].to_string();
    assert!(words.contains("Segment DTM at position 9"), "{words}");
    let ack = content(
        &t,
        &json!({ "id": got["acknowledgements"][0]["interchange_id"] }),
    )
    .await;
    assert!(
        ack.contains("AK3*DTM*9**8~AK4*2**8*20261305~AK3*PO1*19**8~AK4*2**6*4~AK5*R*5~"),
        "{ack}"
    );
    assert!(ack.contains("AK9*R*1*1*0~"), "{ack}");
    t.stop().await;
}

#[tokio::test]
async fn prices_that_do_not_divide_and_orders_drafted_before() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // 31.25 a case of six is no price per each without rounding.
    let odd = renumbered(
        &PO850.replace("PO1*2*120*CA*31.2*", "PO1*2*120*CA*31.25*"),
        600,
    );
    let got = received(&t, &odd).await;
    let p = proposal(&t, &t.admin, &got["messages"][0]).await;
    let price = findings(&p, "price_does_not_divide");
    assert_eq!(price.len(), 1, "{p}");
    assert_eq!(price[0]["line"], "2");
    assert_eq!(p["lines"][1]["unit_price"], Value::Null);
    // A person may say what is meant, and the draft takes it.
    let (status, order) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": got["messages"][0]["id"], "items": knife_choice(&f),
                    "prices": { "2": "31.2" }, "accept_notes": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");

    // The same order number again, in other bytes: refused by the policy.
    let again = received(&t, &renumbered(PO850, 601)).await;
    let message = &again["messages"][0];
    let p = proposal(&t, &t.admin, message).await;
    assert_eq!(
        findings(&p, "duplicate_order")[0]["severity"],
        "blocking",
        "{p}"
    );
    let draft = json!({ "message_id": message["id"], "items": knife_choice(&f),
                        "accept_notes": true });
    let (status, body) = t.command(&t.admin, "edi.draft", draft.clone()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("drafted"),
        "{body}"
    );

    // A partner that reuses its numbers is allowed by a rule.
    let (status, rule) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": f.c.id, "point": "edi_order.duplicate", "effect": "allow",
                    "condition": "facts.partner_code == 'NM'" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{rule}");
    let p = proposal(&t, &t.admin, message).await;
    assert_eq!(
        findings(&p, "duplicate_order")[0]["severity"],
        "note",
        "{p}"
    );
    let (status, body) = t.command(&t.admin, "edi.draft", draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_proposal_is_read_with_the_callers_rights() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let got = received(&t, PO850).await;
    let reader = t
        .user_with_role(
            "reader@example.com",
            &["edi_message:read", "item:read"],
            &f.c.id,
        )
        .await;
    let p = proposal(&t, &reader, &got["messages"][0]).await;
    assert_eq!(p["customer"], Value::Null, "{p}");
    assert_eq!(p["lines"][0]["item_id"], json!(f.hammer));

    // Without edi:receive nothing is received, and without edi:draft
    // nothing is drafted.
    let (status, _) = receive(&t, &reader, &renumbered(PO850, 700)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .command(
            &reader,
            "edi.draft",
            json!({ "message_id": got["messages"][0]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    t.stop().await;
}

#[tokio::test]
async fn the_bytes_on_record_never_change() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let got = received(&t, PO850).await;
    let id = got["interchange"]["id"].as_str().unwrap().to_owned();
    for sql in [
        "UPDATE edi_interchange SET payload = 'x' WHERE id = $1::uuid",
        "UPDATE edi_interchange SET control = '000000999' WHERE id = $1::uuid",
        "DELETE FROM edi_interchange WHERE id = $1::uuid",
    ] {
        let refused = sqlx::query(sql).bind(&id).execute(&t.pool).await;
        assert!(refused.is_err(), "{sql} was let through");
    }
    assert!(
        sqlx::query("TRUNCATE edi_interchange CASCADE")
            .execute(&t.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM edi_message")
            .execute(&t.pool)
            .await
            .is_err(),
        "messages are never deleted"
    );
    // The state may move on; the bytes may not.
    sqlx::query("UPDATE edi_interchange SET error = 'noted' WHERE id = $1::uuid")
        .bind(&id)
        .execute(&t.pool)
        .await
        .unwrap();
    t.stop().await;
}

#[tokio::test]
async fn two_interchanges_at_once_get_two_control_numbers() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let a = renumbered(PO850, 801);
    let b = renumbered(PO850, 802).replace("BEG*00*SA*4500018842", "BEG*00*SA*4500018844");
    let (first, second) = tokio::join!(received(&t, &a), received(&t, &b));
    let one = first["acknowledgements"][0]["interchange_id"].clone();
    let two = second["acknowledgements"][0]["interchange_id"].clone();
    assert_ne!(one, two);
    let controls: Vec<String> = sqlx::query_scalar(
        "SELECT control FROM edi_interchange WHERE direction = 'out' ORDER BY control",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(controls, ["000000001", "000000002"]);
    let groups: Vec<String> = sqlx::query_scalar(
        "SELECT group_control FROM edi_message WHERE direction = 'out' ORDER BY group_control",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(groups, ["1", "2"]);
    t.stop().await;
}

#[tokio::test]
async fn a_message_is_explained_and_a_ready_interchange_marked_sent() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let got = received(&t, PO850).await;
    let (status, explained) = t
        .command(
            &t.admin,
            "edi.explain",
            json!({ "message_id": got["messages"][0]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{explained}");
    let segments = explained["segments"].as_array().unwrap();
    assert_eq!(segments[0]["tag"], "ST");
    assert_eq!(segments.len(), 28);
    let po1 = segments.iter().find(|s| s["tag"] == "PO1").unwrap();
    assert!(
        po1["says"]
            .as_str()
            .unwrap()
            .starts_with("Line 1: 48 EA at 12.5"),
        "{po1}"
    );
    assert_eq!(po1["position"], 19);

    let ack = &got["acknowledgements"][0];
    let (status, sent) = t
        .command(
            &t.admin,
            "edi_interchange.mark_sent",
            json!({ "id": ack["interchange_id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert_eq!(sent["state"], "sent");
    let (_, state) = message_state(&t, ack).await;
    assert_eq!(state, "sent");
    // What came in is never marked sent.
    let (status, _) = t
        .command(
            &t.admin,
            "edi_interchange.mark_sent",
            json!({ "id": got["interchange"]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let _ = id;
    t.stop().await;
}

/// What the plan asks to do by hand over MCP: upload the fixture, read the
/// proposal, draft, approve the order, download the 997.
#[tokio::test]
async fn an_agent_takes_an_order_over_mcp() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let call = |name: &'static str, arguments: Value| {
        let t = &t;
        async move {
            let result = t.tool(&t.admin, name, arguments).await;
            assert_eq!(result["isError"], false, "{name}: {result}");
            result["structuredContent"].clone()
        }
    };
    let got = call("edi_receive", json!({ "content": PO850 })).await;
    let message = got["messages"][0]["id"].clone();
    let p = call("edi_proposal", json!({ "message_id": message })).await;
    assert_eq!(findings(&p, "item_unknown")[0]["line"], "3");
    let order = call(
        "edi_draft",
        json!({ "message_id": message, "items": knife_choice(&f), "remember": true,
                "accept_notes": true }),
    )
    .await;
    let approved = call("sales_order_approve", json!({ "id": order["id"] })).await;
    assert_eq!(approved["status"], "open");
    let ack = call(
        "edi_interchange_content",
        json!({ "id": got["acknowledgements"][0]["interchange_id"] }),
    )
    .await;
    assert!(
        ack["content"]
            .as_str()
            .unwrap()
            .contains("AK1*PO*417~AK2*850*0001~AK5*A~"),
        "{ack}"
    );
    t.stop().await;
}

// ------------------------------------------------------------- phase 2

/// The rest an order-to-cash exchange needs: stock, a GS1 prefix, a tax
/// on every line, and the partner's automation running as the
/// administrator.
async fn answering(t: &TestApp, f: &Fixture) {
    let adjustments = t
        .create(
            "account",
            json!({ "company_id": f.c.id, "code": "5100", "name": "Adjustments",
                    "account_type": "expense" }),
        )
        .await;
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/stock_adjustment",
            json!({ "company_id": f.c.id, "doc_date": "2026-11-01", "body": {
                "offset_account_id": adjustments,
                "lines": [
                    { "item_id": f.hammer, "warehouse_id": f.warehouse, "qty": "500", "unit_cost": "6" },
                    { "item_id": f.tape, "warehouse_id": f.warehouse, "qty": "5000", "unit_cost": "2" },
                    { "item_id": f.knife, "warehouse_id": f.warehouse, "qty": "500", "unit_cost": "3" },
                ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, body) = t
        .command(
            &t.admin,
            "stock_adjustment.post",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.create(
        "edi_settings",
        json!({ "company_id": f.c.id, "gs1_company_prefix": "0400001" }),
    )
    .await;
    let tax_payable = t
        .create(
            "account",
            json!({ "company_id": f.c.id, "code": "2100", "name": "Sales tax",
                    "account_type": "liability" }),
        )
        .await;
    let tax = t
        .create(
            "tax_code",
            json!({ "code": "ST5", "name": "Sales tax 5 %", "rate": "5" }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({ "company_id": f.c.id, "tax_code_id": tax, "collected_account_id": tax_payable,
                "paid_account_id": tax_payable }),
    )
    .await;
    t.create(
        "tax_rule",
        json!({ "company_id": f.c.id, "tax_code_id": tax }),
    )
    .await;
    let admin: String = sqlx::query_scalar("SELECT id::text FROM app_user WHERE is_admin LIMIT 1")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/edi_partner/{}", f.partner),
            json!({ "run_as_user_id": admin }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Receives the fixture and drafts it; returns the draft.
async fn drafted(t: &TestApp, f: &Fixture, content: &str) -> Value {
    let got = received(t, content).await;
    let (status, order) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": got["messages"][0]["id"], "items": knife_choice(f),
                    "remember": true, "accept_notes": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    order
}

async fn run(t: &TestApp, name: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, name, input).await;
    assert_eq!(status, StatusCode::OK, "{name}: {body}");
    body
}

async fn deliver_events(t: &TestApp) {
    erpcraft_kernel::outbox::deliver_batch(&t.kernel, 100)
        .await
        .unwrap();
}

/// The newest message made for a document, and its bytes.
async fn made(t: &TestApp, document: &Value, message: &str) -> (Value, String) {
    let (id, interchange, state): (String, Option<String>, String) = sqlx::query_as(
        "SELECT id::text, interchange_id::text, state FROM edi_message
         WHERE document_id = $1::uuid AND message = $2 AND direction = 'out'
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(document["id"].as_str().unwrap())
    .bind(message)
    .fetch_one(&t.pool)
    .await
    .unwrap_or_else(|_| panic!("no {message} for {}", document["id"]));
    let bytes = match &interchange {
        Some(i) => content(t, &json!({ "id": i })).await,
        None => String::new(),
    };
    (
        json!({ "id": id, "interchange_id": interchange, "state": state }),
        bytes,
    )
}

/// The partner's 997 for one of our messages.
fn partner_997(control: u32, group: &str, functional_id: &str, set: &str, status: &str) -> String {
    let isa = format!(
        "ISA*00*          *00*          *ZZ*NORTHFIELDMERC *ZZ*CEDARHOLLOW    *261121*0800*U*00401*{control:09}*0*P*>~"
    );
    let body = if status == "A" {
        format!("AK1*{functional_id}*{group}~AK2*{set}*0001~AK5*A~AK9*A*1*1*1~")
    } else {
        format!(
            "AK1*{functional_id}*{group}~AK2*{set}*0001~AK3*IT1*5**8~AK4*2**6*X~AK5*R*5~AK9*R*1*1*0~"
        )
    };
    let segments = body.matches('~').count() + 2;
    format!(
        "{isa}GS*FA*NORTHFIELDMERC*CEDARHOLLOW*20261121*0800*{control}*X*004010~ST*997*0001~{body}SE*{segments}*0001~GE*1*{control}~IEA*1*{control:09}~"
    )
}

async fn group_of(t: &TestApp, message: &Value) -> String {
    sqlx::query_scalar("SELECT group_control FROM edi_message WHERE id = $1::uuid")
        .bind(message["id"].as_str().unwrap())
        .fetch_one(&t.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_order_to_cash_exchange_runs_on_fixtures() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    answering(&t, &f).await;
    let order = drafted(&t, &f, PO850).await;

    // The cross-reference changes after the order came: what is sent
    // still quotes the order.
    sqlx::query("UPDATE edi_item_xref SET value = '10400001000219' WHERE kind = 'gtin' AND value = '10400001000202'")
        .execute(&t.pool)
        .await
        .unwrap();

    // Approved: the 855 is made by itself, every line accepted.
    let order = run(&t, "sales_order.approve", json!({ "id": order["id"] })).await;
    deliver_events(&t).await;
    let (ack, bytes) = made(&t, &order, "855").await;
    assert_eq!(ack["state"], "ready", "{ack}");
    assert!(bytes.contains("BAK*00*AD*4500018842*20261118"), "{bytes}");
    assert_eq!(bytes.matches("ACK*IA*").count(), 3, "{bytes}");
    assert!(
        bytes.contains(
            "PO1*2*120*CA*31.2**UK*10400001000202*VN*CH-2040-C*IN*88213002~ACK*IA*120*CA~"
        ),
        "{bytes}"
    );
    assert!(
        bytes.contains("*0*P*>~"),
        "a production partner's message: {bytes}"
    );

    // Delivered: shipping one carton short is refused, and nothing posts.
    let delivery = run(&t, "sales_order.deliver", json!({ "id": order["id"] })).await;
    let lines = delivery["body"]["lines"].as_array().unwrap().clone();
    let short: Vec<Value> = lines
        .iter()
        .map(|l| {
            let qty = if l["order_line_id"] == order["body"]["lines"][1]["id"] {
                "714".to_owned()
            } else {
                l["qty"].as_str().unwrap().to_owned()
            };
            json!({ "kind": "carton", "lines": [{ "delivery_line_id": l["id"], "qty": qty }] })
        })
        .collect();
    let packed = run(
        &t,
        "edi_shipment.pack",
        json!({ "delivery_id": delivery["id"], "packs": short, "carrier_scac": "ABCD",
                "bill_of_lading": "BOL778" }),
    )
    .await;
    assert_eq!(packed["state"], "packing");
    assert_eq!(packed["missing"].as_array().unwrap().len(), 1, "{packed}");
    let (status, body) = t
        .command(
            &t.admin,
            "edi_shipment.ship",
            json!({ "delivery_id": delivery["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (_, still) = t
        .get(
            &t.admin,
            &format!("/api/documents/sales_delivery/{}", id(&delivery)),
        )
        .await;
    assert_eq!(still["status"], "draft");

    // Packed case by case, it ships: posted and noticed in one go.
    let packed = run(
        &t,
        "edi_shipment.auto_pack",
        json!({ "delivery_id": delivery["id"] }),
    )
    .await;
    assert_eq!(packed["state"], "packed", "{packed}");
    let cartons = packed["packs"].as_array().unwrap();
    assert_eq!(cartons.len(), 48 + 120 + 24);
    for carton in cartons {
        assert!(erpcraft_edi::gs1::is_valid(
            carton["sscc"].as_str().unwrap()
        ));
    }
    let shipped = run(
        &t,
        "edi_shipment.ship",
        json!({ "delivery_id": delivery["id"] }),
    )
    .await;
    assert_eq!(shipped["delivery"]["status"], "posted", "{shipped}");
    assert_eq!(shipped["notice"]["state"], "ready", "{shipped}");
    let (notice, bytes) = made(&t, &delivery, "856").await;
    // 720 each went as 120 cases of 6, one per carton, quoting the order.
    assert_eq!(
        bytes
            .matches("LIN*2*UK*10400001000202*VN*CH-2040-C*IN*88213002~SN1**1*CA**120*CA~")
            .count(),
        120,
        "{bytes}"
    );
    assert!(bytes.contains("PRF*4500018842***20261118~"), "{bytes}");
    assert!(bytes.contains("REF*BM*BOL778~"), "{bytes}");
    let label = run(&t, "edi.label", json!({ "pack_id": cartons[0]["id"] })).await;
    assert_eq!(label["sscc"], cartons[0]["sscc"]);
    assert!(
        label["zpl"]
            .as_str()
            .unwrap()
            .contains(&format!(">;>800{}", label["sscc"].as_str().unwrap()))
    );
    assert_eq!(
        (label["number"].as_i64(), label["of"].as_i64()),
        (Some(1), Some(192))
    );

    // Invoiced: the 810 comes to the invoice's total in cents, tax and all.
    let draft = run(&t, "sales_order.invoice", json!({ "id": order["id"] })).await;
    let invoice = run(&t, "sales_invoice.post", json!({ "id": draft["id"] })).await;
    deliver_events(&t).await;
    let (bill, bytes) = made(&t, &invoice, "810").await;
    assert_eq!(bill["state"], "ready", "{bill} {bytes}");
    let gross = dec(&invoice["summary"]["total_gross"]);
    assert_eq!(gross, (600.0 + 3744.0 + 190.8) * 1.05);
    assert!(
        bytes.contains(&format!("TDS*{}~", (gross * 100.0).round() as i64)),
        "{bytes}"
    );
    assert!(bytes.contains("TXI*ST*226.74*5~"), "{bytes}");
    assert!(
        bytes.contains("IT1*2*120*CA*31.2**UK*10400001000202*VN*CH-2040-C*IN*88213002~"),
        "{bytes}"
    );

    // Everything is handed over, and the partner acknowledges all three.
    let ready: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM edi_interchange WHERE direction = 'out' AND state = 'ready'",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    for id in ready {
        run(&t, "edi_interchange.mark_sent", json!({ "id": id })).await;
    }
    for (n, (message, functional_id, set)) in [
        (&ack, "PR", "855"),
        (&notice, "SH", "856"),
        (&bill, "IN", "810"),
    ]
    .into_iter()
    .enumerate()
    {
        let group = group_of(&t, message).await;
        let got = received(
            &t,
            &partner_997(900 + n as u32, &group, functional_id, set, "A"),
        )
        .await;
        assert!(
            got["acknowledgements"].as_array().unwrap().is_empty(),
            "never acknowledged"
        );
    }
    for message in [&ack, &notice, &bill] {
        let state: String = sqlx::query_scalar("SELECT state FROM edi_message WHERE id = $1::uuid")
            .bind(message["id"].as_str().unwrap())
            .fetch_one(&t.pool)
            .await
            .unwrap();
        assert_eq!(state, "acknowledged");
    }
    let pending = run(&t, "edi.pending", json!({ "company_id": f.c.id })).await;
    for list in [
        "proposals",
        "invalid",
        "rejected",
        "overdue",
        "ready",
        "deliveries_without_notice",
    ] {
        assert!(
            pending[list].as_array().unwrap().is_empty(),
            "{list}: {pending}"
        );
    }
    t.stop().await;
}

#[tokio::test]
async fn a_cut_quantity_a_rejection_a_resend_and_a_delivery_without_packing() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    answering(&t, &f).await;
    let order = drafted(&t, &f, PO850).await;

    // A quantity cut on the draft before approval: IQ with the new
    // quantity, and the acknowledgement says it changed.
    let mut lines = order["body"]["lines"].as_array().unwrap().clone();
    lines[1]["qty"] = json!("600");
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_order/{}", id(&order)),
            json!({ "version": order["version"], "body": {
                "customer_id": order["body"]["customer_id"],
                "shipping_address_id": order["body"]["shipping_address_id"],
                "lines": lines } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order = run(&t, "sales_order.approve", json!({ "id": order["id"] })).await;
    let generated = run(
        &t,
        "edi.generate",
        json!({ "doc_type": "sales_order", "id": order["id"], "message": "855" }),
    )
    .await;
    assert_eq!(generated["state"], "ready", "{generated}");
    let (ack, bytes) = made(&t, &order, "855").await;
    assert!(bytes.contains("BAK*00*AC*"), "{bytes}");
    assert!(
        bytes.contains(
            "PO1*2*120*CA*31.2**UK*10400001000202*VN*CH-2040-C*IN*88213002~ACK*IQ*100*CA~"
        ),
        "{bytes}"
    );
    // Made once: again is refused, and the subscriber's try changes nothing.
    let (status, _) = t
        .command(
            &t.admin,
            "edi.generate",
            json!({ "doc_type": "sales_order", "id": order["id"], "message": "855" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    deliver_events(&t).await;
    assert_eq!(
        t.count("SELECT count(*) FROM edi_message WHERE message = '855'")
            .await,
        1
    );

    // Sent, then sent again: the same bytes.
    run(
        &t,
        "edi_interchange.mark_sent",
        json!({ "id": ack["interchange_id"] }),
    )
    .await;
    let resent = run(&t, "edi_message.resend", json!({ "id": ack["id"] })).await;
    assert_eq!(resent["interchange"]["state"], "ready");
    assert_eq!(resent["interchange"]["id"], ack["interchange_id"]);
    assert_eq!(
        content(&t, &json!({ "id": ack["interchange_id"] })).await,
        bytes
    );

    // The partner rejects it: rejected, with the place in words; made
    // again, it supersedes the rejected one under a new control number.
    let group = group_of(&t, &ack).await;
    received(&t, &partner_997(950, &group, "PR", "855", "R")).await;
    let (state, findings): (String, Value) =
        sqlx::query_as("SELECT state, findings FROM edi_message WHERE id = $1::uuid")
            .bind(ack["id"].as_str().unwrap())
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(state, "rejected");
    assert!(
        findings.to_string().contains("Segment IT1 at position 5"),
        "{findings}"
    );
    let again = run(
        &t,
        "edi.generate",
        json!({ "doc_type": "sales_order", "id": order["id"], "message": "855" }),
    )
    .await;
    assert_eq!(again["supersedes"], ack["id"]);
    assert_ne!(
        again["message"]["group_control"].as_str(),
        Some(group.as_str())
    );

    // A delivery posted without packing makes no notice and waits.
    let delivery = run(&t, "sales_order.deliver", json!({ "id": order["id"] })).await;
    run(&t, "sales_delivery.post", json!({ "id": delivery["id"] })).await;
    deliver_events(&t).await;
    let pending = run(&t, "edi.pending", json!({ "company_id": f.c.id })).await;
    let waiting = pending["deliveries_without_notice"].as_array().unwrap();
    assert_eq!(waiting.len(), 1, "{pending}");
    assert_eq!(waiting[0]["delivery_id"], delivery["id"]);
    assert_eq!(waiting[0]["packed"], false);
    // Made by hand now, it is kept invalid with the reason.
    let invalid = run(
        &t,
        "edi.generate",
        json!({ "doc_type": "sales_delivery", "id": delivery["id"], "message": "856" }),
    )
    .await;
    assert_eq!(invalid["state"], "invalid");
    assert_eq!(invalid["findings"][0]["code"], "not_packed");
    t.stop().await;
}

#[tokio::test]
async fn ssccs_are_never_issued_twice_and_a_test_partner_is_told_so() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    answering(&t, &f).await;
    // A partner still in test: everything sent says T.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/edi_partner/{}", f.partner),
            json!({ "usage": "test" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let got = received(&t, PO850).await;
    assert_eq!(got["messages"][0]["state"], "test");
    let ack = content(
        &t,
        &json!({ "id": got["acknowledgements"][0]["interchange_id"] }),
    )
    .await;
    assert!(
        ack.contains("*0*P*>~"),
        "the 997 echoes the interchange's own usage: {ack}"
    );
    sqlx::query("UPDATE edi_partner SET usage = 'production'")
        .execute(&t.pool)
        .await
        .unwrap();
    let order = drafted(&t, &f, &renumbered(PO850, 431)).await;
    sqlx::query("UPDATE edi_partner SET usage = 'test'")
        .execute(&t.pool)
        .await
        .unwrap();
    let order = run(&t, "sales_order.approve", json!({ "id": order["id"] })).await;
    deliver_events(&t).await;
    let (_, bytes) = made(&t, &order, "855").await;
    assert!(bytes.contains("*0*T*>~"), "{bytes}");

    // Two packings at once take different serials.
    let first = run(&t, "sales_order.deliver", json!({ "id": order["id"] })).await;
    let line = &first["body"]["lines"][0];
    let pack = |delivery: &Value| {
        json!({ "delivery_id": delivery["id"], "packs": [
            { "kind": "pallet" },
            { "kind": "carton", "parent": 1,
              "lines": [{ "delivery_line_id": line["id"], "qty": "48" }] },
        ] })
    };
    let (a, b) = tokio::join!(
        t.command(&t.admin, "edi_shipment.pack", pack(&first)),
        t.command(&t.admin, "edi_shipment.pack", pack(&first)),
    );
    assert_eq!(
        (a.0, b.0),
        (StatusCode::OK, StatusCode::OK),
        "{} {}",
        a.1,
        b.1
    );
    let all: Vec<String> = sqlx::query_scalar("SELECT sscc FROM edi_pack ORDER BY sscc")
        .fetch_all(&t.pool)
        .await
        .unwrap();
    assert_eq!(all.len(), 4);
    let mut unique = all.clone();
    unique.dedup();
    assert_eq!(unique, all);
    // The database refuses a repeat, and a change of an SSCC.
    let repeat = sqlx::query(
        "INSERT INTO edi_pack (id, delivery_id, kind, sscc, position)
         SELECT gen_random_uuid(), delivery_id, 'pallet', sscc, 9 FROM edi_pack LIMIT 1",
    )
    .execute(&t.pool)
    .await;
    assert!(repeat.is_err());
    assert!(
        sqlx::query("UPDATE edi_pack SET sscc = '000400001999999995'")
            .execute(&t.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM edi_pack")
            .execute(&t.pool)
            .await
            .is_err()
    );
    t.stop().await;
}

#[tokio::test]
async fn automation_drafts_approves_and_invoices_with_a_tax_group_and_a_credit() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    answering(&t, &f).await;

    // Two taxes on every line: one TXI each.
    let tax_payable: String =
        sqlx::query_scalar("SELECT collected_account_id::text FROM tax_settings LIMIT 1")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    let state = t
        .create(
            "tax_code",
            json!({ "code": "STATE", "name": "State 4 %", "rate": "4" }),
        )
        .await;
    let county = t
        .create(
            "tax_code",
            json!({ "code": "COUNTY", "name": "County 1 %", "rate": "1" }),
        )
        .await;
    for code in [&state, &county] {
        t.create(
            "tax_settings",
            json!({ "company_id": f.c.id, "tax_code_id": code,
                    "collected_account_id": tax_payable, "paid_account_id": tax_payable }),
        )
        .await;
    }
    let group = t
        .create(
            "tax_code",
            json!({ "code": "BOTH", "name": "State and county", "rate": "5" }),
        )
        .await;
    for (position, component) in [(1, &state), (2, &county)] {
        t.create(
            "tax_code_component",
            json!({ "group_id": group, "component_id": component, "position": position }),
        )
        .await;
    }
    sqlx::query("UPDATE tax_rule SET tax_code_id = $1::uuid")
        .bind(&group)
        .execute(&t.pool)
        .await
        .unwrap();

    // The knife is known now, and the company lets the partner's orders
    // in without a person, and approved.
    t.create(
        "edi_item_xref",
        json!({ "company_id": f.c.id, "partner_id": f.partner, "item_id": f.knife, "kind": "seller_item",
                "value": "CH-3310" }),
    )
    .await;
    for point in ["edi.auto_draft", "edi.auto_approve"] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/entities/policy_rule",
                json!({ "company_id": f.c.id, "point": point, "effect": "allow",
                        "condition": "facts.partner_code == 'NM'" }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let got = received(&t, PO850).await;
    deliver_events(&t).await;
    let (state, order_id): (String, Option<String>) =
        sqlx::query_as("SELECT state, document_id::text FROM edi_message WHERE id = $1::uuid")
            .bind(got["messages"][0]["id"].as_str().unwrap())
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(state, "drafted");
    let (_, order) = t
        .get(
            &t.admin,
            &format!("/api/documents/sales_order/{}", order_id.unwrap()),
        )
        .await;
    assert_eq!(order["status"], "open", "approved by itself: {order}");
    let (ack, _) = made(&t, &order, "855").await;
    assert_eq!(ack["state"], "ready");

    let delivery = run(&t, "sales_order.deliver", json!({ "id": order["id"] })).await;
    run(&t, "sales_delivery.post", json!({ "id": delivery["id"] })).await;
    let draft = run(&t, "sales_order.invoice", json!({ "id": order["id"] })).await;
    let invoice = run(&t, "sales_invoice.post", json!({ "id": draft["id"] })).await;
    deliver_events(&t).await;
    let (bill, bytes) = made(&t, &invoice, "810").await;
    assert_eq!(bill["state"], "ready", "{bytes}");
    // Each component of the group on its own: 4 % and 1 % of 4,534.80.
    assert!(bytes.contains("TXI*ST*181.39*4~TXI*ST*45.35*1~"), "{bytes}");
    let gross = dec(&invoice["summary"]["total_gross"]);
    assert!(
        bytes.contains(&format!("TDS*{}~", (gross * 100.0).round() as i64)),
        "{bytes}"
    );

    // A credit for line 2's price: an 810 marked as a credit.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": f.c.id, "point": "sales_credit_note.without_return",
                    "effect": "allow", "condition": "true" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let line = invoice["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["order_line_id"] == order["body"]["lines"][1]["id"])
        .unwrap();
    let (status, credit) = t
        .post(
            &t.admin,
            "/api/documents/sales_credit_note",
            json!({ "company_id": f.c.id, "doc_date": "2026-11-25", "body": {
                "sales_invoice_id": invoice["id"],
                "lines": [{ "invoice_line_id": line["id"], "qty": "6", "unit_price": "5.2" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{credit}");
    let credit = run(&t, "sales_credit_note.post", json!({ "id": credit["id"] })).await;
    deliver_events(&t).await;
    let (note, bytes) = made(&t, &credit, "810").await;
    assert_eq!(note["state"], "ready", "{bytes}");
    assert!(bytes.contains("*CR~"), "{bytes}");
    assert!(
        bytes.contains("IT1*2*1*CA*31.2**UK*10400001000202"),
        "{bytes}"
    );
    t.stop().await;
}

// ------------------------------------------------------- review of phase 1

/// Review 7 (grid-builder): an ST01 the database's check refused failed
/// the whole receive, leaving the bytes stored with no 997.
#[tokio::test]
async fn review7_a_set_without_its_type_is_rejected_not_an_error() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let broken = PO850.replace("ST*850*0001~", "ST**0001~");
    let (status, body) = receive(&t, &t.admin, &broken).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["messages"][0]["state"], "rejected", "{body}");
    assert_eq!(body["acknowledgements"][0]["message"], "997", "{body}");
    assert_eq!(
        t.count("SELECT count(*) FROM edi_interchange WHERE state = 'stored'")
            .await,
        0
    );
    t.stop().await;
}

/// Review 7 (grid-builder): an interchange whose TA1 rejects it had its
/// order proposed and its sets accepted by a 997.
#[tokio::test]
async fn review7_an_interchange_the_ta1_rejects_proposes_no_order() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let broken = PO850.replace("IEA*1*000000417", "IEA*1*000000999");
    let got = received(&t, &broken).await;
    assert_eq!(got["messages"][0]["state"], "rejected", "{got}");
    let acks: Vec<&str> = got["acknowledgements"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["message"].as_str())
        .collect();
    assert_eq!(acks, ["TA1"], "{got}");
    let ta1 = content(
        &t,
        &json!({ "id": got["acknowledgements"][0]["interchange_id"] }),
    )
    .await;
    assert!(ta1.contains("*R*001~"), "{ta1}");
    t.stop().await;
}

/// Review 7 (grid-builder): a preview of edi.receive kept the bytes.
#[tokio::test]
async fn review7_a_preview_of_receive_keeps_nothing() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let before = t.count("SELECT count(*) FROM edi_interchange").await;
    let (status, body) = t
        .post(
            &t.admin,
            "/api/commands/edi.receive?preview=true",
            json!({ "content": renumbered(PO850, 9001) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        t.count("SELECT count(*) FROM edi_interchange").await,
        before
    );
    t.stop().await;
}

#[tokio::test]
async fn one_control_number_is_worked_on_once_even_at_once() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let other = PO850.replace("BEG*00*SA*4500018842", "BEG*00*SA*4500018845");
    let (a, b) = tokio::join!(received(&t, PO850), received(&t, &other));
    let mut states = vec![
        a["messages"][0]["state"].as_str().unwrap().to_owned(),
        b["messages"][0]["state"].as_str().unwrap().to_owned(),
    ];
    states.sort();
    assert_eq!(states, ["proposed", "rejected"]);
    t.stop().await;
}

#[tokio::test]
async fn what_a_partner_echoes_never_breaks_its_answer() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    // Bars between elements, so a star in the group's number is data to
    // the partner and a separator to us; and a usage that is none.
    let odd = PO850
        .replace('*', "|")
        .replace("|0|P|>~", "|0|#|>~")
        .replace("|417|X|004010~", "|4*17|X|004010~")
        .replace("GE|1|417~", "GE|1|4*17~");
    let got = received(&t, &odd).await;
    assert_eq!(got["messages"][0]["state"], "test", "{got}");
    let ack = content(
        &t,
        &json!({ "id": got["acknowledgements"][0]["interchange_id"] }),
    )
    .await;
    assert!(ack.contains("AK1*PO*417~"), "{ack}");
    assert!(ack.contains("*0*P*>~"), "{ack}");
    t.stop().await;
}

#[tokio::test]
async fn a_999_names_the_version_of_the_group_it_answers() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    sqlx::query("UPDATE edi_partner SET ack_form = '999', version = '005010' WHERE id = $1::uuid")
        .bind(&f.partner)
        .execute(&t.pool)
        .await
        .unwrap();
    let got = received(&t, PO850).await;
    let ack = content(
        &t,
        &json!({ "id": got["acknowledgements"][0]["interchange_id"] }),
    )
    .await;
    assert!(
        ack.contains("AK1*PO*417*004010~AK2*850*0001*004010~"),
        "{ack}"
    );
    t.stop().await;
}

#[tokio::test]
async fn an_answer_to_a_set_with_many_errors_stays_within_its_reader() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let bad: String = (0..300).map(|_| "DTM*038*20261399~\n").collect();
    let broken = PO850
        .replace("DTM*037*20261201~\n", &format!("DTM*037*20261201~\n{bad}"))
        .replace("SE*28*0001", "SE*328*0001");
    let got = received(&t, &broken).await;
    assert_eq!(got["messages"][0]["state"], "rejected", "{got}");
    let ack = content(
        &t,
        &json!({ "id": got["acknowledgements"][0]["interchange_id"] }),
    )
    .await;
    assert_eq!(
        ack.matches("AK3*").count(),
        100,
        "at most 100 segment errors a set"
    );
    assert!(ack.contains("AK5*R*5~"), "{ack}");
    t.stop().await;
}

#[tokio::test]
async fn a_free_line_and_a_quantity_too_large_are_for_a_person() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let odd = renumbered(
        &PO850
            .replace("PO1*1*48*EA*12.5*", "PO1*1*48*EA*0*")
            .replace("PO1*2*120*CA*31.2*", "PO1*2*123456789012345678*CA*31.2*"),
        620,
    );
    let got = received(&t, &odd).await;
    let p = proposal(&t, &t.admin, &got["messages"][0]).await;
    assert_eq!(findings(&p, "price_zero")[0]["line"], "1", "{p}");
    assert_eq!(findings(&p, "quantity_too_large")[0]["line"], "2", "{p}");
    let (status, body) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": got["messages"][0]["id"], "items": knife_choice(&f),
                    "prices": { "1": "0" }, "accept_notes": true }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Line 2 orders more"),
        "{body}"
    );
    t.stop().await;
}

/// Review 8: a policy may let a line priced at nothing be drafted free,
/// and a quantity with too many places says so, not that it is too large.
#[tokio::test]
async fn a_policy_lets_a_free_line_be_drafted_and_places_are_named() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let odd = renumbered(
        &PO850.replace("PO1*1*48*EA*12.5*", "PO1*1*48.1234567*EA*0*"),
        621,
    );
    let got = received(&t, &odd).await;
    let message = &got["messages"][0];
    let p = proposal(&t, &t.admin, message).await;
    assert_eq!(findings(&p, "price_zero")[0]["severity"], "blocking", "{p}");
    let places = findings(&p, "quantity_precision");
    assert_eq!(places[0]["line"], "1", "{p}");
    assert!(
        places[0]["message"]
            .as_str()
            .unwrap()
            .contains("decimal places"),
        "{p}"
    );
    assert!(findings(&p, "quantity_too_large").is_empty(), "{p}");

    let (status, rule) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": f.c.id, "point": "edi_order.price_zero", "effect": "allow",
                    "condition": "facts.partner_code == 'NM' && facts.line == '1'" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{rule}");
    let p = proposal(&t, &t.admin, message).await;
    assert_eq!(findings(&p, "price_zero")[0]["severity"], "note", "{p}");
    // Line 1 still has too many places; with that corrected by a new
    // file, the free line is drafted at zero without a price given.
    let fixed = renumbered(
        &PO850
            .replace("PO1*1*48*EA*12.5*", "PO1*1*48*EA*0*")
            .replace("BEG*00*SA*4500018842", "BEG*00*SA*4500018850"),
        622,
    );
    let got = received(&t, &fixed).await;
    let (status, order) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": got["messages"][0]["id"], "items": knife_choice(&f),
                    "accept_notes": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(
        dec(&order["body"]["lines"][0]["unit_price"]),
        0.0,
        "{order}"
    );
    t.stop().await;
}

#[tokio::test]
async fn partners_and_their_numbers_stay_inside_their_company() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // A second company trades with the same partner, under the same ids.
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
    let lost = received(&t, PO850).await;
    assert_eq!(lost["interchange"]["state"], "unroutable", "{lost}");
    assert!(
        lost["interchange"]["error"]
            .as_str()
            .unwrap()
            .contains("company_id")
    );
    let (status, got) = t
        .command(
            &t.admin,
            "edi.receive",
            json!({ "content": renumbered(PO850, 640), "company_id": other.id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["interchange"]["company_id"], json!(other.id));
    // The first company's numbers name nothing in the second.
    let p = proposal(&t, &t.admin, &got["messages"][0]).await;
    assert_eq!(findings(&p, "item_unknown").len(), 3, "{p}");
    assert_eq!(findings(&p, "ship_to_unknown").len(), 1, "{p}");

    // A caller of the second company cannot receive the first's
    // interchange, whether it came before or not, and learns nothing.
    let outsider = t
        .user_with_role("outsider@example.com", &["edi:receive"], &other.id)
        .await;
    let theirs = renumbered(PO850, 641);
    let (fresh, _) = t
        .command(
            &outsider,
            "edi.receive",
            json!({ "content": theirs, "company_id": f.c.id }),
        )
        .await;
    received_for(&t, &theirs, &f.c.id).await;
    let (again, _) = t
        .command(
            &outsider,
            "edi.receive",
            json!({ "content": theirs, "company_id": f.c.id }),
        )
        .await;
    assert_eq!(
        (fresh, again),
        (StatusCode::FORBIDDEN, StatusCode::FORBIDDEN)
    );
    // Bytes nobody could route are each caller's own.
    let garbage = "not an interchange at all";
    let (_, mine) = t
        .command(&t.admin, "edi.receive", json!({ "content": garbage }))
        .await;
    let (_, yours) = t
        .command(&outsider, "edi.receive", json!({ "content": garbage }))
        .await;
    assert_eq!(yours["repeat"], false, "{yours}");
    assert_ne!(yours["interchange"]["id"], mine["interchange"]["id"]);
    t.stop().await;
}

async fn received_for(t: &TestApp, content: &str, company: &str) -> Value {
    let (status, body) = t
        .command(
            &t.admin,
            "edi.receive",
            json!({ "content": content, "company_id": company }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn old_unread_files_can_be_purged_and_nothing_else() {
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    received(&t, "not an interchange").await;
    received(&t, PO850).await;
    sqlx::query("UPDATE edi_interchange SET created_at = now() - interval '90 days'")
        .execute(&t.pool)
        .await
        .unwrap();
    let (status, _) = t
        .command(
            &t.admin,
            "edi_interchange.purge",
            json!({ "older_than_days": 7 }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let purged = run(
        &t,
        "edi_interchange.purge",
        json!({ "older_than_days": 30 }),
    )
    .await;
    assert_eq!(purged["deleted"], 1);
    assert_eq!(
        t.count("SELECT count(*) FROM edi_interchange WHERE partner_id IS NOT NULL")
            .await,
        2,
        "the order and its 997 stay"
    );
    // Outside the command, the guard still holds.
    assert!(
        sqlx::query("DELETE FROM edi_interchange")
            .execute(&t.pool)
            .await
            .is_err()
    );
    t.stop().await;
}

#[tokio::test]
async fn one_order_number_is_drafted_once_even_at_once() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let a = received(&t, &renumbered(PO850, 660)).await;
    let b = received(&t, &renumbered(PO850, 661)).await;
    let draft = |m: &Value| {
        json!({ "message_id": m["messages"][0]["id"], "items": knife_choice(&f),
                "accept_notes": true })
    };
    let (one, two) = tokio::join!(
        t.command(&t.admin, "edi.draft", draft(&a)),
        t.command(&t.admin, "edi.draft", draft(&b)),
    );
    let mut statuses = [one.0, two.0];
    statuses.sort();
    assert_eq!(statuses[0], StatusCode::OK, "{} {}", one.1, two.1);
    assert_ne!(statuses[1], StatusCode::OK, "{} {}", one.1, two.1);
    assert_eq!(t.count("SELECT count(*) FROM edi_document").await, 1);
    t.stop().await;
}

/// edi.receive redacts the file: the audit log and the outbox keep its
/// SHA-256 and size, never the bytes, while an idempotency key still tells
/// one file from another.
#[tokio::test]
async fn the_audit_log_keeps_a_hash_of_the_file_not_the_file() {
    use sha2::{Digest, Sha256};
    let t = TestApp::start().await;
    let _f = setup(&t).await;
    let first = renumbered(PO850, 9101);
    let send = |content: String, key: &'static str| {
        let t = &t;
        async move {
            t.send(
                "POST",
                "/api/commands/edi.receive",
                Some(&t.admin),
                Some(json!({ "content": content })),
                &[("idempotency-key", key)],
            )
            .await
        }
    };
    let (status, body) = send(first.clone(), "edi-file-1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (input,): (Value,) = sqlx::query_as(
        "SELECT input FROM audit_log WHERE command = 'edi.receive' AND outcome = 'ok'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        input,
        json!({ "content": {
            "sha256": format!("{:x}", Sha256::digest(first.as_bytes())),
            "bytes": first.len(),
        } })
    );
    let leaked = |sql: &'static str| {
        let t = &t;
        async move {
            let rows: Vec<(String,)> = sqlx::query_as(sql).fetch_all(&t.pool).await.unwrap();
            rows.iter().any(|(text,)| text.contains("ISA*"))
        }
    };
    assert!(!leaked("SELECT input::text FROM audit_log WHERE input IS NOT NULL").await);
    assert!(!leaked("SELECT payload::text FROM outbox_event").await);
    // The same key with the same file returns the first result; with
    // another file it is refused, though both audit as a hash.
    let (status, again) = send(first.clone(), "edi-file-1").await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["id"], body["id"]);
    let (status, other) = send(renumbered(PO850, 9102), "edi-file-1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{other}");
    // A refused file is redacted too.
    let (status, _) = t
        .command(
            &t.admin,
            "edi.receive",
            json!({ "content_base64": "not base64!" }),
        )
        .await;
    assert!(status.is_client_error());
    let (input,): (Value,) = sqlx::query_as(
        "SELECT input FROM audit_log WHERE command = 'edi.receive' AND outcome = 'error'
         ORDER BY at DESC LIMIT 1",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(input["content_base64"]["bytes"], 11, "{input}");
    // The same file as base64 is audited with the file's own hash.
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(first.as_bytes());
    let (status, body) = t
        .command(
            &t.admin,
            "edi.receive",
            json!({ "content_base64": encoded }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (input,): (Value,) = sqlx::query_as(
        "SELECT input FROM audit_log WHERE command = 'edi.receive' AND outcome = 'ok'
         AND input ? 'content_base64'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        input["content_base64"],
        json!({ "sha256": format!("{:x}", Sha256::digest(first.as_bytes())), "bytes": first.len() })
    );
    t.stop().await;
}

#[tokio::test]
async fn a_partner_unit_that_is_another_unit_of_the_item_converts_by_its_factor() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Hammers are kept each and sold by the dozen too; the partner's DZ is
    // our dz.
    t.create(
        "item_uom",
        json!({ "item_id": f.hammer, "uom": "dz", "factor": "12", "sales": true }),
    )
    .await;
    t.create(
        "edi_uom_xref",
        json!({ "company_id": f.c.id, "partner_uom": "DZ", "uom": "dz" }),
    )
    .await;
    let dozens = renumbered(&PO850.replace("PO1*1*48*EA*12.5*", "PO1*1*4*DZ*150*"), 700);
    let got = received(&t, &dozens).await;
    let p = proposal(&t, &t.admin, &got["messages"][0]).await;
    assert!(findings(&p, "unit_unknown").is_empty(), "{p}");
    let line = &p["lines"][0];
    // Four dozen at 150.00 a dozen: 48 each at 12.50.
    assert_eq!(dec(&line["qty"]), 48.0, "{p}");
    assert_eq!(dec(&line["unit_price"]), 12.5, "{p}");
    t.stop().await;
}

/// A second partner of the same company, with its own identifiers, its
/// own number for the tape's case and the same distribution centre.
async fn second_partner(t: &TestApp, f: &Fixture) -> String {
    let admin: String = sqlx::query_scalar("SELECT id::text FROM app_user WHERE is_admin LIMIT 1")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let partner = t
        .create(
            "edi_partner",
            json!({
                "company_id": f.c.id, "code": "BX", "name": "Baxter Supply",
                "customer_id": f.customer, "warehouse_id": f.warehouse, "usage": "production",
                "our_qualifier": "ZZ", "our_id": "CEDARHOLLOW",
                "their_qualifier": "ZZ", "their_id": "BAXTERSUPPLY",
                "run_as_user_id": admin,
            }),
        )
        .await;
    t.create(
        "edi_item_xref",
        json!({ "company_id": f.c.id, "partner_id": partner, "item_id": f.tape, "kind": "gtin",
                "value": "10400001000202", "partner_uom": "CA", "units_per": "6" }),
    )
    .await;
    t.create(
        "edi_location_xref",
        json!({ "company_id": f.c.id, "partner_id": partner, "qualifier": "92", "code": "0007",
                "address_id": f.dc07 }),
    )
    .await;
    partner
}

/// The fixture as the second partner sends it.
fn from_baxter(content: &str) -> String {
    content
        .replace("NORTHFIELDMERC ", "BAXTERSUPPLY   ")
        .replace("NORTHFIELDMERC", "BAXTERSUPPLY")
}

async fn guide(t: &TestApp, f: &Fixture, code: &str, messages: Value) -> String {
    let id = t
        .create(
            "edi_guide",
            json!({ "company_id": f.c.id, "code": code, "name": format!("Guide {code}"),
                    "syntax": "x12", "version": "004010" }),
        )
        .await;
    for (message, direction, spec) in messages
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (&m[0], &m[1], &m[2]))
    {
        t.create(
            "edi_guide_message",
            json!({ "company_id": f.c.id, "guide_id": id, "message": message,
                    "direction": direction, "spec": spec }),
        )
        .await;
    }
    id
}

/// Approves, packs on one pallet, ships and invoices a drafted order;
/// the 855, 856 and 810 as written.
async fn order_to_cash(t: &TestApp, order: &Value) -> (String, String, String, Value) {
    let order = run(t, "sales_order.approve", json!({ "id": order["id"] })).await;
    deliver_events(t).await;
    let (_, ack) = made(t, &order, "855").await;
    let delivery = run(t, "sales_order.deliver", json!({ "id": order["id"] })).await;
    let mut packs = vec![json!({ "kind": "pallet" })];
    for line in delivery["body"]["lines"].as_array().unwrap() {
        packs.push(json!({ "kind": "carton", "parent": 1,
                           "lines": [{ "delivery_line_id": line["id"], "qty": line["qty"] }] }));
    }
    run(
        t,
        "edi_shipment.pack",
        json!({ "delivery_id": delivery["id"], "packs": packs }),
    )
    .await;
    let shipped = run(
        t,
        "edi_shipment.ship",
        json!({ "delivery_id": delivery["id"] }),
    )
    .await;
    assert_eq!(shipped["notice"]["state"], "ready", "{shipped}");
    let (_, notice) = made(t, &delivery, "856").await;
    let draft = run(t, "sales_order.invoice", json!({ "id": order["id"] })).await;
    let invoice = run(t, "sales_invoice.post", json!({ "id": draft["id"] })).await;
    deliver_events(t).await;
    let (bill, bytes) = made(t, &invoice, "810").await;
    assert_eq!(bill["state"], "ready", "{bill} {bytes}");
    (ack, notice, bytes, invoice)
}

#[tokio::test]
async fn two_partners_with_two_guides_trade_the_same_order_differently() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    answering(&t, &f).await;
    let baxter = second_partner(&t, &f).await;

    // Northfield's guide: pallets as packed, our vendor number (as it
    // named it in the order) at the head of the invoice and the case lines marked.
    let north = guide(
        &t,
        &f,
        "NORTHFIELD",
        json!([
            ["856", "out", { "options": { "hierarchy": "SOTPI" } }],
            ["810", "out", { "extra": [
                { "segment": "REF", "after": "BIG",
                  "elements": [{ "value": "VR" }, { "expr": "order.reference.IA" }] },
                { "segment": "REF", "after": "IT1",
                  "elements": [{ "value": "PK" }, { "expr": "'CASE OF ' + line.product.UK" }],
                  "condition": "line.uom == 'CA'" },
            ] }],
        ]),
    )
    .await;
    // Baxter's: a header-only acknowledgement, no pallets and its own
    // packaging code, units written as it wants them, and an order must
    // carry a contract number.
    let bax = guide(
        &t,
        &f,
        "BAXTER",
        json!([
            ["850", "in", { "constraints": [{ "segment": "REF", "element": 1,
                                              "codes": ["DP", "IA", "CT"] },
                                            { "segment": "TD1", "required": true }] }],
            ["855", "out", { "options": { "ack_detail": "header" } }],
            ["856", "out", { "options": { "hierarchy": "SOPI", "pack_qualifier": "PLT94" } }],
            ["810", "out", { "options": { "uom": { "CA": "CS" } },
                             "constraints": [{ "segment": "CUR", "not_used": true }] }],
        ]),
    )
    .await;
    for (partner, guide) in [(&f.partner, &north), (&baxter, &bax)] {
        let (status, body) = t
            .patch(
                &t.admin,
                &format!("/api/entities/edi_partner/{partner}"),
                json!({ "guide_id": guide }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // The same order from each. Baxter's guide wants a TD1 the order
    // does not have: a note on the proposal, not a rejection.
    let got = received(&t, &from_baxter(PO850)).await;
    assert_eq!(got["messages"][0]["state"], "proposed", "{got}");
    let p = proposal(&t, &t.admin, &got["messages"][0]).await;
    let broken = findings(&p, "guide");
    assert_eq!(broken.len(), 1, "{p}");
    assert!(
        broken[0]["message"]
            .as_str()
            .unwrap()
            .contains("segment TD1 is required"),
        "{p}"
    );
    assert_eq!(broken[0]["severity"], "note");
    let (status, baxter_order) = t
        .command(
            &t.admin,
            "edi.draft",
            json!({ "message_id": got["messages"][0]["id"], "items": knife_choice(&f),
                    "remember": true, "accept_notes": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{baxter_order}");
    let north_order = drafted(&t, &f, PO850).await;

    let (north_ack, north_notice, north_bill, _) = order_to_cash(&t, &north_order).await;
    let (bax_ack, bax_notice, bax_bill, bax_invoice) = order_to_cash(&t, &baxter_order).await;

    // The acknowledgement: every line for one, the header alone for the
    // other.
    assert_eq!(north_ack.matches("PO1*").count(), 3, "{north_ack}");
    assert_eq!(bax_ack.matches("PO1*").count(), 0, "{bax_ack}");
    assert!(bax_ack.contains("BAK*00*AD*4500018842"), "{bax_ack}");

    // The notice: a pallet for one, cartons alone for the other.
    assert!(north_notice.contains("*2*T*1~"), "{north_notice}");
    assert!(!bax_notice.contains("*T*"), "{bax_notice}");
    assert!(north_notice.contains("TD1*CTN25*3"), "{north_notice}");
    assert!(bax_notice.contains("TD1*PLT94*3"), "{bax_notice}");

    // The invoice: Northfield's vendor number after the BIG and a mark on
    // the case line; Baxter's cases written CS. Both come to the books.
    assert!(
        north_bill.contains("*DI~REF*VR*0071934~REF*DP*042~"),
        "{north_bill}"
    );
    assert!(
        north_bill.contains("~REF*PK*CASE OF 10400001000202~"),
        "{north_bill}"
    );
    assert_eq!(north_bill.matches("REF*PK*").count(), 1, "{north_bill}");
    assert!(
        bax_bill.contains("IT1*2*120*CS*31.2**UK*10400001000202"),
        "{bax_bill}"
    );
    assert!(!bax_bill.contains("REF*VR*"), "{bax_bill}");
    let gross = dec(&bax_invoice["summary"]["total_gross"]);
    assert!(
        bax_bill.contains(&format!("TDS*{}~", (gross * 100.0).round() as i64)),
        "{bax_bill}"
    );

    // An expression that cannot be worked out: the message is invalid,
    // never written with an empty value.
    let id: String = sqlx::query_scalar(
        "SELECT id::text FROM edi_guide_message WHERE guide_id = $1::uuid AND message = '810'",
    )
    .bind(&bax)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/edi_guide_message/{id}"),
            json!({ "message": "810", "direction": "out", "spec": { "extra": [
                { "segment": "REF", "after": "BIG",
                  "elements": [{ "value": "ZZ" }, { "expr": "order.reference.ZZ" }] }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let preview = run(
        &t,
        "edi.generate",
        json!({ "doc_type": "sales_invoice", "id": bax_invoice["id"], "message": "810",
                "preview": true }),
    )
    .await;
    assert_eq!(preview["state"], "invalid", "{preview}");
    assert!(
        preview["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "guide_extra"),
        "{preview}"
    );

    // A spec that names a segment the message has not is refused on save.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/edi_guide_message",
            json!({ "company_id": f.c.id, "guide_id": north, "message": "855",
                    "direction": "out", "spec": { "extra": [
                        { "segment": "REF", "after": "XYZ", "elements": [{ "value": "A" }] }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("XYZ"), "{body}");

    // Exported, imported into another company, exported again: the same.
    let file = run(&t, "edi_guide.export", json!({ "guide_id": north })).await;
    let other = t.company("OT", "USD").await;
    let imported = run(
        &t,
        "edi_guide.import",
        json!({ "company_id": other.id, "file": file }),
    )
    .await;
    assert_eq!(imported["created"], true);
    let again = run(
        &t,
        "edi_guide.export",
        json!({ "guide_id": imported["guide_id"] }),
    )
    .await;
    assert_eq!(again, file);
    let (status, _) = t
        .command(
            &t.admin,
            "edi_guide.import",
            json!({ "company_id": other.id, "file": file }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A sample checked under a guide, and what samples suggest.
    let checked = run(
        &t,
        "edi_guide.check",
        json!({ "guide_id": bax, "content": PO850 }),
    )
    .await;
    assert_eq!(checked["messages"][0]["message"], "850", "{checked}");
    assert_eq!(
        checked["messages"][0]["violations"],
        json!(["segment TD1 is required by the guide"]),
        "{checked}"
    );
    let derived = run(&t, "edi_guide.derive", json!({ "content": [PO850] })).await;
    let spec = &derived["messages"][0]["spec"];
    assert_eq!(derived["messages"][0]["message"], "850", "{derived}");
    assert_eq!(
        spec["options"]["item_match"][0],
        json!({ "kind": "gtin", "qualifiers": ["UP", "UK"] }),
        "{derived}"
    );
    assert_eq!(
        spec["options"]["ship_to"],
        json!({ "role": "ST", "qualifier": "92" }),
        "{derived}"
    );
    t.stop().await;
}

#[tokio::test]
async fn an_inventory_advice_says_what_stock_availability_says_in_the_partners_units() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    answering(&t, &f).await;
    // An order of 120 cases of tape takes 720 of the 5000 on hand.
    let order = drafted(&t, &f, PO850).await;
    run(&t, "sales_order.approve", json!({ "id": order["id"] })).await;

    let availability = run(
        &t,
        "stock.availability",
        json!({ "company_id": f.c.id, "warehouse_id": f.warehouse }),
    )
    .await;
    let available = |item: &str| {
        availability["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["item_id"] == item)
            .map(|r| dec(&r["available"]))
            .unwrap()
    };
    assert_eq!(available(&f.tape), 5000.0 - 720.0);

    let preview = run(
        &t,
        "edi.inventory_advice",
        json!({ "partner_id": f.partner, "preview": true }),
    )
    .await;
    assert_eq!(preview["state"], "ready", "{preview}");
    assert!(preview["message"].is_null());
    let made = run(
        &t,
        "edi.inventory_advice",
        json!({ "partner_id": f.partner }),
    )
    .await;
    assert_eq!(made["state"], "ready", "{made}");
    let bytes = content(&t, &json!({ "id": made["message"]["interchange_id"] })).await;
    assert!(
        bytes.contains("GS*IB*CEDARHOLLOW*NORTHFIELDMERC*"),
        "{bytes}"
    );
    assert!(bytes.contains("ST*846*0001~BIA*00*SI*"), "{bytes}");
    // The hammer by its GTIN, in each; the tape in whole cases of six:
    // 4280 each is 713 cases and 2 over, which cannot be promised.
    let hammer = available(&f.hammer) as i64;
    assert!(
        bytes.contains(&format!("LIN**UP*040000100010~QTY*33*{hammer}*EA~")),
        "{bytes}"
    );
    assert!(
        bytes.contains("LIN**UK*10400001000202~QTY*33*713*CA~"),
        "{bytes}"
    );
    // The knife has no number for the partner: not advised.
    assert_eq!(
        bytes.matches("LIN*").count(),
        3,
        "the knife was remembered: {bytes}"
    );
    t.stop().await;
}
