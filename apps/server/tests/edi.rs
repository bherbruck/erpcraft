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
        json!({ "partner_uom": "EA", "uom": "each" }),
    )
    .await;
    t.create(
        "edi_item_xref",
        json!({ "item_id": hammer, "kind": "gtin", "value": "040000100010" }),
    )
    .await;
    t.create(
        "edi_item_xref",
        json!({ "partner_id": partner, "item_id": tape, "kind": "gtin",
                "value": "10400001000202", "partner_uom": "CA", "units_per": "6" }),
    )
    .await;
    t.create(
        "edi_location_xref",
        json!({ "partner_id": partner, "qualifier": "92", "code": "0007", "address_id": dc07 }),
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
    assert!(
        order["memo"]
            .as_str()
            .unwrap()
            .contains("EDI order 4500018842"),
        "{order}"
    );

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
