//! Electronic data interchange in EDIFACT: the order-to-cash exchange of
//! tests/edi.rs on ORDERS, ORDRSP, DESADV and INVOIC, with a CONTRL each
//! way.
//!
//! The parties are made up: a retailer that is not a real company orders
//! from a toolmaker that is not one either, with identifiers that are not
//! real GLNs. The ORDERS is written here in this module's own words.

mod common;

use axum::http::StatusCode;
use base64::Engine;
use common::{Company, TestApp, dec};
use serde_json::{Value, json};

/// The buyer's and the seller's identifiers (UNB, qualifier 14).
const BUYER: &str = "4012345000016";
const SELLER: &str = "4098765000010";

/// The buyer's order: a decimal comma, a name in ISO 8859-1, a line in
/// pieces and a line in cartons of six.
fn orders() -> Vec<u8> {
    let text = format!(
        "UNA:+,? '\
UNB+UNOC:3+{BUYER}:14+{SELLER}:14+261118:0930+EU0417'\
UNH+1+ORDERS:D:96A:UN:EAN008'\
BGM+220+PO-88001+9'\
DTM+137:20261118:102'\
DTM+2:20261202:102'\
RFF+CT:K-2026-17'\
NAD+BY+{BUYER}::9++Lindenhof Märkte GmbH'\
NAD+SU+{SELLER}::9'\
NAD+DP+4012345000092::9++Zentrallager Ost+Lagerweg 3+Erfurt++99084+DE'\
CUX+2:EUR:9'\
LIN+1++4098765100017:EN'\
PIA+1+HM-100:SA'\
IMD+F++:::Hammer 500 g'\
QTY+21:48:PCE'\
PRI+AAA:12,5'\
LIN+2++4098765100024:EN'\
QTY+21:20:CT'\
PRI+AAA:31,2'\
UNS+S'\
CNT+2:2'\
UNT+20+1'\
UNZ+1+EU0417'"
    );
    text.chars().map(|c| c as u32 as u8).collect()
}

/// The buyer's CONTRL for our interchange `interchange`, acknowledging
/// its message `kind`.
fn partner_contrl(control: &str, interchange: &str, kind: &str) -> Vec<u8> {
    format!(
        "UNA:+.? '\
UNB+UNOC:3+{BUYER}:14+{SELLER}:14+261121:0800+{control}'\
UNH+1+CONTRL:2:2:UN'\
UCI+{interchange}+{SELLER}:14+{BUYER}:14+7'\
UCM+1+{kind}:D:96A:UN+7'\
UNT+4+1'\
UNZ+1+{control}'"
    )
    .into_bytes()
}

struct Fixture {
    c: Company,
    hammer: String,
    tape: String,
    dc: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("EU", "EUR").await;
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
    let adjustments = account("5100", "Adjustments", "expense").await;
    let vat = account("2100", "VAT", "liability").await;
    t.create(
        "sales_settings",
        json!({ "company_id": c.id, "receivable_account_id": receivable,
                "revenue_account_id": c.revenue, "cogs_account_id": cogs }),
    )
    .await;
    t.inventory_rule(&c.id, &inventory).await;
    let customer = t
        .create(
            "customer",
            json!({ "code": "LH", "name": "Lindenhof Märkte" }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "customer", "party_id": customer, "addresses": [
                { "kind": "billing", "street": "Marktplatz 1", "city": "Weimar",
                  "postcode": "99423", "country_code": "DE", "is_default": true },
                { "kind": "shipping", "street": "Lagerweg 3", "city": "Erfurt",
                  "postcode": "99084", "country_code": "DE", "is_default": true },
            ] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let dc: String = sqlx::query_scalar(
        "SELECT id::text FROM party_address WHERE party_id = $1::uuid AND kind = 'shipping'",
    )
    .bind(&customer)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let hammer = t
        .create(
            "item",
            json!({ "code": "HM-100", "name": "Hammer", "uom": "each" }),
        )
        .await;
    let tape = t
        .create(
            "item",
            json!({ "code": "TM-200", "name": "Tape measure", "uom": "each" }),
        )
        .await;
    let admin: String = sqlx::query_scalar("SELECT id::text FROM app_user WHERE is_admin LIMIT 1")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let partner = t
        .create(
            "edi_partner",
            json!({
                "company_id": c.id, "code": "LH", "name": "Lindenhof Märkte",
                "customer_id": customer, "warehouse_id": warehouse, "usage": "production",
                "syntax": "edifact", "version": "D96A", "ack_form": "contrl",
                "decimal_mark": ",", "charset": "UNOC",
                "our_qualifier": "14", "our_id": SELLER,
                "their_qualifier": "14", "their_id": BUYER,
                "run_as_user_id": admin,
            }),
        )
        .await;
    t.create(
        "edi_uom_xref",
        json!({ "company_id": c.id, "partner_uom": "PCE", "uom": "each" }),
    )
    .await;
    t.create(
        "edi_item_xref",
        json!({ "company_id": c.id, "item_id": hammer, "kind": "gtin", "value": "4098765100017" }),
    )
    .await;
    t.create(
        "edi_item_xref",
        json!({ "company_id": c.id, "partner_id": partner, "item_id": tape, "kind": "gtin",
                "value": "4098765100024", "partner_uom": "CT", "units_per": "6" }),
    )
    .await;
    t.create(
        "edi_location_xref",
        json!({ "company_id": c.id, "partner_id": partner, "qualifier": "UL",
                "code": "4012345000092", "address_id": dc }),
    )
    .await;

    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/stock_adjustment",
            json!({ "company_id": c.id, "doc_date": "2026-11-01", "body": {
                "offset_account_id": adjustments,
                "lines": [
                    { "item_id": hammer, "warehouse_id": warehouse, "qty": "500", "unit_cost": "6" },
                    { "item_id": tape, "warehouse_id": warehouse, "qty": "1000", "unit_cost": "2" },
                ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(t, "stock_adjustment.post", json!({ "id": draft["id"] })).await;
    t.create(
        "edi_settings",
        json!({ "company_id": c.id, "gs1_company_prefix": "4098765" }),
    )
    .await;
    let tax = t
        .create(
            "tax_code",
            json!({ "code": "VAT19", "name": "VAT 19 %", "rate": "19" }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({ "company_id": c.id, "tax_code_id": tax, "collected_account_id": vat,
                "paid_account_id": vat }),
    )
    .await;
    t.create(
        "tax_rule",
        json!({ "company_id": c.id, "tax_code_id": tax }),
    )
    .await;
    Fixture {
        c,
        hammer,
        tape,
        dc,
    }
}

async fn run(t: &TestApp, name: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, name, input).await;
    assert_eq!(status, StatusCode::OK, "{name}: {body}");
    body
}

async fn receive(t: &TestApp, bytes: &[u8]) -> Value {
    run(
        t,
        "edi.receive",
        json!({ "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes) }),
    )
    .await
}

async fn content(t: &TestApp, interchange: &str) -> String {
    let body = run(t, "edi_interchange.content", json!({ "id": interchange })).await;
    body["content"].as_str().unwrap().to_owned()
}

async fn deliver_events(t: &TestApp) {
    erpcraft_kernel::outbox::deliver_batch(&t.kernel, 100)
        .await
        .unwrap();
}

/// The newest message made for a document: its id, state, interchange
/// control, and the interchange as text.
async fn made(t: &TestApp, document: &Value, message: &str) -> (String, String, String, String) {
    let (id, interchange, state, group): (String, String, String, String) = sqlx::query_as(
        "SELECT id::text, interchange_id::text, state, group_control FROM edi_message
         WHERE document_id = $1::uuid AND message = $2 AND direction = 'out'
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(document["id"].as_str().unwrap())
    .bind(message)
    .fetch_one(&t.pool)
    .await
    .unwrap_or_else(|_| panic!("no {message} for {}", document["id"]));
    let bytes = content(t, &interchange).await;
    (id, state, group, bytes)
}

#[tokio::test]
async fn the_order_to_cash_exchange_runs_on_edifact() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // The ORDERS: proposed, and a CONTRL back acknowledging it.
    let got = receive(&t, &orders()).await;
    assert_eq!(got["interchange"]["state"], "read", "{got}");
    let message = &got["messages"][0];
    assert_eq!(message["state"], "proposed", "{got}");
    assert_eq!(message["business_key"], "PO-88001");
    let acks = got["acknowledgements"].as_array().unwrap();
    assert_eq!(acks.len(), 1, "{got}");
    assert_eq!(acks[0]["message"], "CONTRL");
    let contrl = content(&t, acks[0]["interchange_id"].as_str().unwrap()).await;
    assert!(
        contrl.starts_with(&format!("UNA:+,? 'UNB+UNOC:3+{SELLER}:14+{BUYER}:14+")),
        "{contrl}"
    );
    assert!(
        contrl.contains(&format!("UCI+EU0417+{BUYER}:14+{SELLER}:14+7'")),
        "{contrl}"
    );
    assert!(contrl.contains("UCM+1+ORDERS:D:96A:UN+7'"), "{contrl}");

    // The proposal: both lines found, the cartons in pieces, the place by
    // its GLN.
    let p = run(&t, "edi.proposal", json!({ "message_id": message["id"] })).await;
    assert_eq!(p["customer"]["code"], "LH", "{p}");
    assert_eq!(p["ship_to"]["address_id"], json!(f.dc), "{p}");
    assert_eq!(p["lines"][0]["item_id"], json!(f.hammer), "{p}");
    assert_eq!(dec(&p["lines"][0]["qty"]), 48.0);
    assert_eq!(p["lines"][1]["item_id"], json!(f.tape), "{p}");
    assert_eq!(dec(&p["lines"][1]["qty"]), 120.0);
    assert_eq!(p["lines"][1]["unit_price"], "5.2");
    let order = run(
        &t,
        "edi.draft",
        json!({ "message_id": message["id"], "accept_notes": true }),
    )
    .await;
    assert_eq!(order["body"]["shipping_address_id"], json!(f.dc));

    // Approved: the ORDRSP, every line accepted as ordered, in the
    // partner's units and with its decimal comma.
    let order = run(&t, "sales_order.approve", json!({ "id": order["id"] })).await;
    deliver_events(&t).await;
    let (ordrsp, state, ordrsp_group, bytes) = made(&t, &order, "ORDRSP").await;
    assert_eq!(state, "ready", "{bytes}");
    assert!(bytes.contains("BGM+231+"), "{bytes}");
    assert!(bytes.contains("+29'"), "{bytes}");
    assert!(
        bytes.contains("RFF+ON:PO-88001'DTM+171:20261118:102'"),
        "{bytes}"
    );
    assert_eq!(bytes.matches("LIN+").count(), 2, "{bytes}");
    assert!(
        bytes.contains("LIN+2+5+4098765100024:EN'QTY+21:20:CT'QTY+113:20:CT'PRI+AAA:31,2'"),
        "{bytes}"
    );

    // Packed and shipped: the DESADV, one carton of six per pack.
    let delivery = run(&t, "sales_order.deliver", json!({ "id": order["id"] })).await;
    let packed = run(
        &t,
        "edi_shipment.auto_pack",
        json!({ "delivery_id": delivery["id"] }),
    )
    .await;
    assert_eq!(packed["state"], "packed", "{packed}");
    let shipped = run(
        &t,
        "edi_shipment.ship",
        json!({ "delivery_id": delivery["id"] }),
    )
    .await;
    assert_eq!(shipped["delivery"]["status"], "posted", "{shipped}");
    let (desadv, state, desadv_group, bytes) = made(&t, &delivery, "DESADV").await;
    assert_eq!(state, "ready", "{bytes}");
    assert!(bytes.contains("BGM+351+"), "{bytes}");
    assert!(bytes.contains("RFF+ON:PO-88001'"), "{bytes}");
    assert!(bytes.contains("GIN+BJ+"), "{bytes}");
    let packs = packed["packs"].as_array().unwrap().len();
    assert_eq!(bytes.matches("GIN+BJ+").count(), packs, "{bytes}");

    // Invoiced: the INVOIC comes to the invoice's total, VAT and all.
    let draft = run(&t, "sales_order.invoice", json!({ "id": order["id"] })).await;
    let invoice = run(&t, "sales_invoice.post", json!({ "id": draft["id"] })).await;
    deliver_events(&t).await;
    let (invoic, state, invoic_group, bytes) = made(&t, &invoice, "INVOIC").await;
    assert_eq!(state, "ready", "{bytes}");
    let gross = dec(&invoice["summary"]["total_gross"]);
    assert_eq!(gross, (600.0 + 624.0) * 1.19);
    let figure = format!("{gross:.2}").replace('.', ",");
    assert!(bytes.contains(&format!("MOA+77:{figure}'")), "{bytes}");
    assert!(bytes.contains("MOA+79:1224'"), "{bytes}");
    assert!(bytes.contains("MOA+124:232,56'"), "{bytes}");
    assert!(bytes.contains("CUX+2:EUR:4'"), "{bytes}");
    assert!(bytes.contains("TAX+7+VAT+++:::19'"), "{bytes}");
    assert!(
        bytes.contains(&format!(
            "NAD+BY+{BUYER}::9++Lindenhof Märkte GmbH'NAD+SU+{SELLER}::9'"
        )),
        "{bytes}"
    );

    // Handed over, and the buyer's CONTRLs acknowledge all three.
    let ready: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM edi_interchange WHERE direction = 'out' AND state = 'ready'",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    for id in ready {
        run(&t, "edi_interchange.mark_sent", json!({ "id": id })).await;
    }
    for (n, (group, kind)) in [
        (&ordrsp_group, "ORDRSP"),
        (&desadv_group, "DESADV"),
        (&invoic_group, "INVOIC"),
    ]
    .into_iter()
    .enumerate()
    {
        let got = receive(&t, &partner_contrl(&format!("C90{n}"), group, kind)).await;
        assert!(
            got["acknowledgements"].as_array().unwrap().is_empty(),
            "a CONTRL is never answered: {got}"
        );
    }
    for id in [&ordrsp, &desadv, &invoic] {
        let state: String = sqlx::query_scalar("SELECT state FROM edi_message WHERE id = $1::uuid")
            .bind(id)
            .fetch_one(&t.pool)
            .await
            .unwrap();
        assert_eq!(state, "acknowledged");
    }
    let pending = run(&t, "edi.pending", json!({ "company_id": f.c.id })).await;
    for list in ["proposals", "invalid", "rejected", "overdue", "ready"] {
        assert!(
            pending[list].as_array().unwrap().is_empty(),
            "{list}: {pending}"
        );
    }
    t.stop().await;
}

#[tokio::test]
async fn a_broken_orders_is_rejected_and_the_contrl_says_so() {
    let t = TestApp::start().await;
    setup(&t).await;

    // The trailer counts one segment too many: the message is rejected,
    // and the CONTRL names it with code 29.
    let broken: Vec<u8> = {
        let text: String = orders().iter().map(|b| *b as char).collect();
        text.replace("UNT+20+1", "UNT+21+1")
            .replace("EU0417", "EU0418")
            .chars()
            .map(|c| c as u32 as u8)
            .collect()
    };
    let got = receive(&t, &broken).await;
    assert_eq!(got["messages"][0]["state"], "rejected", "{got}");
    let acks = got["acknowledgements"].as_array().unwrap();
    let contrl = content(&t, acks[0]["interchange_id"].as_str().unwrap()).await;
    assert!(contrl.contains("UCM+1+ORDERS:D:96A:UN+4+29'"), "{contrl}");

    // The same control reference again with other content: rejected as a
    // duplicate, and not answered.
    let first = receive(&t, &orders()).await;
    assert_eq!(first["messages"][0]["state"], "proposed", "{first}");
    let again: Vec<u8> = {
        let text: String = orders().iter().map(|b| *b as char).collect();
        text.replace("PO-88001", "PO-88002")
            .chars()
            .map(|c| c as u32 as u8)
            .collect()
    };
    let got = receive(&t, &again).await;
    assert_eq!(got["messages"][0]["state"], "rejected", "{got}");
    assert!(
        got["acknowledgements"].as_array().unwrap().is_empty(),
        "{got}"
    );

    // An ORDRSP the buyer rejects is marked so, with the reason.
    let order = run(
        &t,
        "edi.draft",
        json!({ "message_id": first["messages"][0]["id"], "accept_notes": true }),
    )
    .await;
    let order = run(&t, "sales_order.approve", json!({ "id": order["id"] })).await;
    deliver_events(&t).await;
    let (ordrsp, _, group, _) = made(&t, &order, "ORDRSP").await;
    let rejecting = String::from_utf8(partner_contrl("C910", &group, "ORDRSP"))
        .unwrap()
        .replace("UCM+1+ORDRSP:D:96A:UN+7'", "UCM+1+ORDRSP:D:96A:UN+4+13'");
    receive(&t, rejecting.as_bytes()).await;
    let (state, findings): (String, Value) =
        sqlx::query_as("SELECT state, findings FROM edi_message WHERE id = $1::uuid")
            .bind(&ordrsp)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(state, "rejected");
    assert_eq!(findings[0]["code"], "partner_rejected", "{findings}");
    t.stop().await;
}

#[tokio::test]
async fn an_invrpt_advises_stock_in_the_partners_units() {
    let t = TestApp::start().await;
    setup(&t).await;
    let partner: String = sqlx::query_scalar("SELECT id::text FROM edi_partner WHERE code = 'LH'")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let made = run(&t, "edi.inventory_advice", json!({ "partner_id": partner })).await;
    assert_eq!(made["state"], "ready", "{made}");
    assert_eq!(made["message"]["message"], "INVRPT");
    let bytes = content(&t, made["message"]["interchange_id"].as_str().unwrap()).await;
    assert!(bytes.contains("UNH+1+INVRPT:D:96A:UN'BGM+35+"), "{bytes}");
    // 500 hammers in pieces; 1000 tapes as 166 cartons of six.
    assert!(
        bytes.contains("LIN+1++4098765100017:EN'QTY+145:500:PCE'"),
        "{bytes}"
    );
    assert!(
        bytes.contains("LIN+2++4098765100024:EN'QTY+145:166:CT'"),
        "{bytes}"
    );
    assert!(bytes.contains("CNT+2:2'"), "{bytes}");
    t.stop().await;
}

#[tokio::test]
async fn an_ordchg_changes_the_draft_its_orders_became() {
    let t = TestApp::start().await;
    setup(&t).await;
    let got = receive(&t, &orders()).await;
    let order = run(
        &t,
        "edi.draft",
        json!({ "message_id": got["messages"][0]["id"], "accept_notes": true }),
    )
    .await;
    // Fifteen cartons in place of twenty, and the hammers' price lowered.
    let change = format!(
        "UNA:+,? '\
UNB+UNOC:3+{BUYER}:14+{SELLER}:14+261119:1000+EU0420'\
UNH+1+ORDCHG:D:96A:UN'\
BGM+230+CH-1+4'\
RFF+ON:PO-88001'\
DTM+171:20261118:102'\
LIN+1+3+4098765100017:EN'\
QTY+21:48:PCE'\
PRI+AAA:12'\
LIN+2+3+4098765100024:EN'\
QTY+21:15:CT'\
UNS+S'\
CNT+2:2'\
UNT+12+1'\
UNZ+1+EU0420'"
    );
    let got = receive(&t, change.as_bytes()).await;
    let message = &got["messages"][0];
    assert_eq!(message["message"], "ORDCHG");
    assert_eq!(message["state"], "proposed", "{got}");
    let acks = got["acknowledgements"].as_array().unwrap();
    let contrl = content(&t, acks[0]["interchange_id"].as_str().unwrap()).await;
    assert!(contrl.contains("UCM+1+ORDCHG:D:96A:UN+7'"), "{contrl}");

    let p = run(
        &t,
        "edi.change_proposal",
        json!({ "message_id": message["id"] }),
    )
    .await;
    assert_eq!(p["would_apply"], true, "{p}");
    let changed = run(
        &t,
        "edi.apply_change",
        json!({ "message_id": message["id"] }),
    )
    .await;
    let lines = changed["body"]["lines"].as_array().unwrap();
    assert_eq!(dec(&lines[0]["unit_price"]), 12.0, "{changed}");
    assert_eq!(dec(&lines[1]["qty"]), 90.0, "{changed}");
    assert_eq!(changed["id"], order["id"]);
    t.stop().await;
}
