//! A month in the life of Ohio Widgets Inc.: a widget maker with a plant in
//! Ohio and a warehouse in Germany, driven through March 2026 the way it
//! would run, and then every ledger reconciled against every other.
//!
//! The company is set up from the US manufacturer template. It buys steel at home and
//! bolts in the EU (reverse charge), pays freight that lands on the goods,
//! lets planning draft its purchase and production orders, makes widgets on
//! a routed line with recorded time, takes an order by EDI, sells in USD and
//! EUR with reservations, sends goods to Germany in two steps, invoices and
//! emails PDFs, is paid with a discount and a write-off, credits a
//! customer, revalues its euros, depreciates a machine, and checks its books.
//!
//! What could not be done without a workaround was not worked around: each
//! such gap became a test of its own, and each is now closed.

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

const COMPANY: &str = "OWI";

/// The company, its roles and the ids the month needs.
struct Owi {
    c: String,
    /// Account ids of the template's roles, and their codes.
    role: HashMap<String, (String, String)>,
    ohio: String,
    germany: String,
    ohio_value: String,
    germany_value: String,
    plant: String,
    hamburg: String,
    transit: String,
    steel: String,
    bolt: String,
    widget: String,
    gadget: String,
    sales_tax: String,
    reverse_charge: String,
}

impl Owi {
    fn role(&self, name: &str) -> &str {
        &self.role[name].0
    }

    fn code(&self, name: &str) -> &str {
        match self.role.get(name) {
            Some((_, code)) => code,
            None => panic!(
                "no role {name}; the template's roles are {:?}",
                self.role.keys()
            ),
        }
    }
}

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn draft(t: &TestApp, c: &str, doc_type: &str, date: &str, body: Value) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": c, "doc_date": date, "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
    draft
}

/// Draft a document dated `date` and run `command` on it.
async fn done(
    t: &TestApp,
    c: &str,
    doc_type: &str,
    command: &str,
    date: &str,
    body: Value,
) -> Value {
    let draft = draft(t, c, doc_type, date, body).await;
    run(t, command, json!({ "id": id(&draft) })).await
}

async fn account(t: &TestApp, c: &str, code: &str, name: &str, kind: &str) -> String {
    t.create(
        "account",
        json!({ "company_id": c, "code": code, "name": name, "account_type": kind }),
    )
    .await
}

/// Record a party's address, tax id and email, as party.set takes them.
async fn party(
    t: &TestApp,
    kind: &str,
    id: &str,
    address: Value,
    tax_id: Option<(&str, &str)>,
    email: &str,
) {
    let mut input = json!({ "party_type": kind, "party_id": id,
                            "addresses": [ { "kind": "billing", "is_default": true } ],
                            "electronic_addresses": [{ "value": email, "is_default": true }] });
    for (key, value) in address.as_object().unwrap() {
        input["addresses"][0][key] = value.clone();
    }
    if let Some((scheme, value)) = tax_id {
        input["tax_ids"] = json!([{ "scheme": scheme, "value": value }]);
    }
    run(t, "party.set", input).await;
}

/// The company as March 2026 begins: set up from the US manufacturer
/// template, with two sites and a SITE dimension, warehouses, items, a bill
/// with a routing, reorder rules, and taxes, EU VAT on accounts of its own.
async fn set_up(t: &TestApp) -> Owi {
    let created = run(
        t,
        "company.setup",
        json!({ "template": "us_manufacturer", "code": COMPANY, "name": "Ohio Widgets Inc",
                "first_year": 2026, "years": 1 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let mut role = HashMap::new();
    for (name, value) in created["roles"].as_object().unwrap() {
        role.insert(
            name.clone(),
            (
                value["id"].as_str().unwrap().to_owned(),
                value["code"].as_str().unwrap().to_owned(),
            ),
        );
    }

    // The template has every account making needs; EU VAT is the company's
    // own business.
    let mut account_ids = HashMap::new();
    for (code, name, kind) in [
        ("1410", "EU VAT reclaimable", "asset"),
        ("2310", "EU VAT owed", "liability"),
    ] {
        account_ids.insert(code, account(t, &c, code, name, kind).await);
    }

    // Two sites, each with its value of the SITE dimension.
    let site = |code: &'static str, name: &'static str| {
        let c = c.clone();
        async move {
            t.create(
                "site",
                json!({ "company_id": c, "code": code, "name": name }),
            )
            .await
        }
    };
    let ohio = site("OH", "Ohio plant").await;
    let germany = site("DE", "Hamburg warehouse").await;
    let dimension = t
        .create(
            "dimension",
            json!({ "company_id": c, "code": "SITE", "name": "Site" }),
        )
        .await;
    let value = |code: &'static str| {
        let (c, dimension) = (c.clone(), dimension.clone());
        async move {
            t.create(
                "dimension_value",
                json!({ "company_id": c, "dimension_id": dimension, "code": code, "name": code }),
            )
            .await
        }
    };
    let ohio_value = value("OHIO").await;
    let germany_value = value("GERMANY").await;
    for (scope, value) in [(&ohio, &ohio_value), (&germany, &germany_value)] {
        t.create(
            "dimension_rule",
            json!({ "company_id": c, "scope_type": "site", "scope_id": scope, "value_id": value }),
        )
        .await;
    }
    let warehouse = |code: &'static str, name: &'static str, site: &String, transit: bool| {
        let (c, site) = (c.clone(), site.clone());
        async move {
            t.create(
                "warehouse",
                json!({ "company_id": c, "code": code, "name": name, "site_id": site, "transit": transit }),
            )
            .await
        }
    };
    let plant = warehouse("OH-MAIN", "Ohio plant stores", &ohio, false).await;
    let hamburg = warehouse("DE-HAM", "Hamburg", &germany, false).await;
    let transit = warehouse("DE-TRANSIT", "On the way to Hamburg", &germany, true).await;

    // Items: steel and bolts are bought, widgets made, gadgets bought for resale.
    let item = |code: &'static str, name: &'static str, uom: &'static str| async move {
        t.create("item", json!({ "code": code, "name": name, "uom": uom }))
            .await
    };
    let steel = item("STEEL", "Steel sheet", "kg").await;
    let bolt = item("BOLT", "Bolt M6", "each").await;
    let widget = item("WIDGET", "Widget", "each").await;
    let gadget = item("GADGET", "Gadget", "each").await;
    // Widgets are carried on finished goods.
    t.create(
        "account_rule",
        json!({ "company_id": c, "purpose": "inventory", "scope_type": "item", "scope_id": widget,
                "account_id": role["finished_goods"].0 }),
    )
    .await;

    // A widget: 2 kg of steel and 4 bolts, on line 1: half an hour to set
    // up, six minutes a widget, at 40.00 an hour.
    let bom = t
        .create(
            "bom",
            json!({ "item_id": widget, "code": "WIDGET-1", "name": "Widget", "output_qty": "1" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": bom, "item_id": steel, "qty": "2" }),
    )
    .await;
    t.create(
        "bom_line",
        json!({ "bom_id": bom, "item_id": bolt, "qty": "4" }),
    )
    .await;
    let assembly = t
        .create(
            "cost_activity",
            json!({ "code": "ASSY", "name": "Assembly", "kind": "labor" }),
        )
        .await;
    t.create(
        "cost_activity_rate",
        json!({ "company_id": c, "activity_id": assembly, "rate": "40.00" }),
    )
    .await;
    t.create(
        "work_center",
        json!({ "company_id": c, "code": "LINE1", "name": "Line 1", "activity_id": assembly,
                "hours_per_day": "8" }),
    )
    .await;
    t.create(
        "bom_operation",
        json!({ "bom_id": bom, "seq": 10, "work_center_code": "LINE1", "setup_hours": "0.5",
                "run_hours": "0.1", "description": "Assemble" }),
    )
    .await;

    // Taxes: a state and county sales tax from the template (4 % + 1.5 %, on
    // 2100 and 2110), and the EU reverse charge. (The template's own
    // US-SALES-TAX is 0 %: a placeholder to be set.)
    let (_, list) = t
        .get(
            &t.admin,
            "/api/entities/tax_code?code=US-EXAMPLE-STATE-COUNTY",
        )
        .await;
    let sales_tax = list["items"][0]["id"].as_str().unwrap().to_owned();
    let reverse_charge = t
        .create(
            "tax_code",
            json!({ "code": "EU-RC19", "name": "EU reverse charge 19 %", "rate": "19", "self_assessed": true }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({ "company_id": c, "tax_code_id": reverse_charge,
                "collected_account_id": account_ids["2310"], "paid_account_id": account_ids["1410"] }),
    )
    .await;

    // Euros against the dollar through the month.
    for (date, rate) in [
        ("2026-03-01", "1.08"),
        ("2026-03-16", "1.09"),
        ("2026-03-31", "1.10"),
    ] {
        t.create(
            "exchange_rate",
            json!({ "from_currency": "EUR", "to_currency": "USD", "rate_date": date, "rate": rate }),
        )
        .await;
    }

    Owi {
        c,
        role,
        ohio,
        germany,
        ohio_value,
        germany_value,
        plant,
        hamburg,
        transit,
        steel,
        bolt,
        widget,
        gadget,
        sales_tax,
        reverse_charge,
    }
}

/// A tiny SMTP server that keeps every message (as apps/server/tests/einvoice.rs has).
async fn smtp_server() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let kept = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let store = kept.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let store = store.clone();
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read);
                write.write_all(b"220 test ESMTP\r\n").await.unwrap();
                let mut line = String::new();
                loop {
                    line.clear();
                    if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let command = line.trim_end().to_ascii_uppercase();
                    if command.starts_with("EHLO") {
                        write.write_all(b"250-test\r\n250 OK\r\n").await.unwrap();
                    } else if command.starts_with("DATA") {
                        write.write_all(b"354 go on\r\n").await.unwrap();
                        let mut data = String::new();
                        loop {
                            line.clear();
                            lines.read_line(&mut line).await.unwrap();
                            if line == ".\r\n" {
                                break;
                            }
                            data.push_str(&line);
                        }
                        store.lock().unwrap().push(data);
                        write.write_all(b"250 queued\r\n").await.unwrap();
                    } else if command.starts_with("QUIT") {
                        write.write_all(b"221 bye\r\n").await.unwrap();
                        return;
                    } else {
                        write.write_all(b"250 OK\r\n").await.unwrap();
                    }
                }
            });
        }
    });
    (port, kept)
}

/// The retailer's purchase order, as it arrives by EDI: 48 widgets at 12.50.
fn retailer_850() -> String {
    [
        "ISA*00*          *00*          *ZZ*NORTHFIELDMERC *ZZ*OHIOWIDGETS    *260302*0930*U*00401*000000501*0*P*>~",
        "GS*PO*NORTHFIELDMERC*OHIOWIDGETS*20260302*0930*501*X*004010~",
        "ST*850*0001~",
        "BEG*00*SA*4500020001**20260302~",
        "CUR*BY*USD~",
        "DTM*002*20260320~",
        "N1*ST*NORTHFIELD DC 07*92*0007~",
        "N3*2200 LOGISTICS PKWY~",
        "N4*COLUMBUS*OH*43217*US~",
        "PO1*1*48*EA*12.5**UP*040000100096*VN*WIDGET~",
        "PID*F****WIDGET~",
        "CTT*1*48~",
        "SE*11*0001~",
        "GE*1*501~",
        "IEA*1*000000501~",
    ]
    .join("\n")
        + "\n"
}

/// The balance of accounts by code, in the trial balance `tb`.
fn balance(tb: &Value, code: &str) -> f64 {
    tb["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["code"] == code)
        .map(|r| dec(&r["balance"]))
        .sum()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.005
}

#[tokio::test]
async fn a_month_in_the_life_of_ohio_widgets() {
    // The invoices go out through an SMTP server on this host, which a
    // server sends to only when it allows private addresses.
    // SAFETY: set before this test's server starts; only the mail module
    // reads it, as a server starts.
    unsafe { std::env::set_var(erpcraft_mail::ALLOW_PRIVATE, "1") };
    let t = TestApp::start().await;
    let o = set_up(&t).await;
    let c = o.c.clone();
    let bank = o.role("bank").to_owned();
    // Every document of the month, to be printed at the end.
    let mut documents: Vec<Value> = vec![];

    // ------------------------------------------------------------ parties
    let net30 = run(
        &t,
        "entity.list",
        json!({ "entity": "payment_terms", "filters": { "code": "NET30" } }),
    )
    .await["items"][0]["id"]
        .clone();
    let two_ten = run(
        &t,
        "entity.list",
        json!({ "entity": "payment_terms", "filters": { "code": "2-10-NET30" } }),
    )
    .await["items"][0]["id"]
        .clone();
    // 2 % in 10 days, 1 % in 20: a second step on the template's terms.
    t.create(
        "payment_terms_step",
        json!({ "terms_id": two_ten, "discount_percent": "1", "discount_days": 20 }),
    )
    .await;
    // Half now, half in 30 days: for the German customer.
    let halves = t
        .create(
            "payment_terms",
            json!({ "code": "50-50", "name": "Half now, half in 30 days", "net_days": 30 }),
        )
        .await;
    t.create(
        "payment_terms_instalment",
        json!({ "terms_id": halves, "percent": "50", "days": 0 }),
    )
    .await;

    let steelco = t
        .create(
            "vendor",
            json!({ "code": "STEELCO", "name": "SteelCo Inc", "payment_terms_id": net30 }),
        )
        .await;
    party(
        &t,
        "vendor",
        &steelco,
        json!({ "street": "1 Mill Rd", "city": "Pittsburgh", "region": "PA",
        "postcode": "15201", "country_code": "US" }),
        Some(("us_ein", "25-1234567")),
        "ap@steelco.example",
    )
    .await;
    let boltwerk = t
        .create(
            "vendor",
            json!({ "code": "BOLTWERK", "name": "Boltwerk GmbH", "payment_terms_id": net30 }),
        )
        .await;
    party(
        &t,
        "vendor",
        &boltwerk,
        json!({ "street": "Schraubenweg 4", "city": "Stuttgart",
        "postcode": "70173", "country_code": "DE" }),
        Some(("eu_vat", "DE123456789")),
        "rechnung@boltwerk.example",
    )
    .await;
    let carrier = t
        .create(
            "vendor",
            json!({ "code": "TRUCKS", "name": "Midwest Trucking", "payment_terms_id": net30 }),
        )
        .await;
    let machines = t
        .create(
            "vendor",
            json!({ "code": "MACHCO", "name": "Machine Tools Co" }),
        )
        .await;
    let acme = t
        .create(
            "customer",
            json!({ "code": "ACME", "name": "Acme Retail", "payment_terms_id": two_ten }),
        )
        .await;
    party(
        &t,
        "customer",
        &acme,
        json!({ "street": "500 Main St", "city": "Columbus", "region": "OH",
        "postcode": "43215", "country_code": "US" }),
        None,
        "ap@acme.example",
    )
    .await;
    let muller = t
        .create(
            "customer",
            json!({ "code": "MUELLER", "name": "Müller Handel GmbH", "payment_terms_id": halves }),
        )
        .await;
    party(
        &t,
        "customer",
        &muller,
        json!({ "street": "Hafenstraße 1", "city": "Hamburg",
        "postcode": "20457", "country_code": "DE" }),
        Some(("eu_vat", "DE987654321")),
        "einkauf@mueller.example",
    )
    .await;
    let northfield = t
        .create("customer", json!({ "code": "NORTHFIELD", "name": "Northfield Mercantile", "payment_terms_id": net30 }))
        .await;
    run(&t, "party.set", json!({ "party_type": "customer", "party_id": northfield, "addresses": [
        { "kind": "billing", "street": "PO Box 4410", "city": "Dayton", "region": "OH", "postcode": "45401",
          "country_code": "US", "is_default": true },
        { "kind": "shipping", "street": "2200 Logistics Pkwy", "city": "Columbus", "region": "OH",
          "postcode": "43217", "country_code": "US", "is_default": true } ],
        "electronic_addresses": [{ "value": "ap@northfield.example", "is_default": true }] })).await;

    // Planning: steel is kept at 1,500 kg at least, bought from SteelCo at
    // 2.00; widgets at 100 at least, made.
    t.create(
        "reorder_rule",
        json!({ "company_id": c, "item_id": o.steel, "warehouse_id": o.plant,
        "min_qty": "1500", "vendor_id": steelco, "unit_price": "2.00" }),
    )
    .await;
    t.create(
        "reorder_rule",
        json!({ "company_id": c, "item_id": o.widget, "warehouse_id": o.plant,
        "min_qty": "100" }),
    )
    .await;

    // ------------------------------------------------------------ March 2: buying
    // 1,000 kg of steel at 2.00 from SteelCo, and 2,000 bolts at 0.05 EUR
    // from Boltwerk in Stuttgart; 100 gadgets at 6.00 for resale.
    let steel_order = done(&t, &c, "purchase_order", "purchase_order.approve", "2026-03-02", json!({
        "vendor_id": steelco, "lines": [
            { "item_id": o.steel, "warehouse_id": o.plant, "qty": "1000", "unit_price": "2.00" },
            { "item_id": o.gadget, "warehouse_id": o.plant, "qty": "100", "unit_price": "6.00" } ] })).await;
    let bolt_order = done(&t, &c, "purchase_order", "purchase_order.approve", "2026-03-02", json!({
        "vendor_id": boltwerk, "currency": "EUR", "lines": [
            { "item_id": o.bolt, "warehouse_id": o.plant, "qty": "2000", "unit_price": "0.05" } ] })).await;
    documents.extend([steel_order.clone(), bolt_order.clone()]);

    // March 4: both arrive.
    let steel_receipt = done(
        &t,
        &c,
        "purchase_receipt",
        "purchase_receipt.post",
        "2026-03-04",
        json!({
        "purchase_order_id": steel_order["id"], "lines": [
            { "order_line_id": steel_order["body"]["lines"][0]["id"], "qty": "1000" },
            { "order_line_id": steel_order["body"]["lines"][1]["id"], "qty": "100" } ] }),
    )
    .await;
    let bolt_receipt = done(
        &t,
        &c,
        "purchase_receipt",
        "purchase_receipt.post",
        "2026-03-04",
        json!({
        "purchase_order_id": bolt_order["id"], "lines": [
            { "order_line_id": bolt_order["body"]["lines"][0]["id"], "qty": "2000" } ] }),
    )
    .await;
    documents.extend([steel_receipt.clone(), bolt_receipt.clone()]);

    // March 5: the trucker's freight of 110.00 lands on the steel and the
    // gadgets, by value.
    let freight = t
        .create("charge", json!({ "code": "INBOUND", "name": "Inbound freight", "kind": "freight", "landed_cost": true }))
        .await;
    let freight_bill = done(&t, &c, "vendor_invoice", "vendor_invoice.post", "2026-03-05", json!({
        "vendor_id": carrier, "lines": [
            { "charge_id": freight, "qty": "1", "unit_price": "110.00", "receipt_ids": [steel_receipt["id"]] } ] })).await;
    // SteelCo's invoice, and Boltwerk's under the reverse charge: Boltwerk
    // charges no VAT, the company owes and reclaims it.
    let steel_bill = done(&t, &c, "vendor_invoice", "vendor_invoice.post", "2026-03-05", json!({
        "purchase_order_id": steel_order["id"], "lines": [
            { "order_line_id": steel_order["body"]["lines"][0]["id"], "qty": "1000", "unit_price": "2.00" },
            { "order_line_id": steel_order["body"]["lines"][1]["id"], "qty": "100", "unit_price": "6.00" } ] })).await;
    let bolt_bill = done(&t, &c, "vendor_invoice", "vendor_invoice.post", "2026-03-05", json!({
        "purchase_order_id": bolt_order["id"], "currency": "EUR", "lines": [
            { "order_line_id": bolt_order["body"]["lines"][0]["id"], "qty": "2000", "unit_price": "0.05",
              "tax_code_id": o.reverse_charge } ] })).await;
    assert_eq!(
        dec(&bolt_bill["summary"]["total_gross"]),
        100.0,
        "{bolt_bill}"
    );
    documents.extend([freight_bill.clone(), steel_bill.clone(), bolt_bill.clone()]);

    // A machine for line 1, capitalised and depreciated from March.
    let mach = run(
        &t,
        "entity.list",
        json!({ "entity": "asset_class", "filters": { "company_id": c, "code": "MACH" } }),
    )
    .await["items"][0]["id"]
        .clone();
    let mach_cost = run(
        &t,
        "entity.list",
        json!({ "entity": "account", "filters": { "company_id": c, "code": "1540" } }),
    )
    .await["items"][0]["id"]
        .clone();
    let machine_bill = done(&t, &c, "vendor_invoice", "vendor_invoice.post", "2026-03-01", json!({
        "vendor_id": machines, "lines": [
            { "account_id": mach_cost, "description": "Press for line 1", "qty": "1", "unit_price": "8400.00" } ] })).await;
    documents.push(machine_bill.clone());
    let press = t
        .create(
            "asset",
            json!({ "company_id": c, "class_id": mach, "code": "PRESS-1", "name": "Press" }),
        )
        .await;
    run(
        &t,
        "asset.capitalise",
        json!({ "id": press, "document_id": machine_bill["id"],
        "line_id": machine_bill["body"]["lines"][0]["id"], "in_service_date": "2026-03-01" }),
    )
    .await;

    // ------------------------------------------------------------ March 6: planning
    // Steel is at 1,000 of 1,500 and no widget is in stock: planning drafts
    // a purchase order for steel and a production order for widgets.
    let suggestions = run(&t, "planning.suggestions", json!({ "company_id": c })).await;
    assert!(
        !suggestions["suggestions"].as_array().unwrap().is_empty(),
        "{suggestions}"
    );
    let bought = run(
        &t,
        "planning.draft_purchase_orders",
        json!({ "company_id": c, "doc_date": "2026-03-06" }),
    )
    .await;
    let made = run(
        &t,
        "planning.draft_production_orders",
        json!({ "company_id": c, "doc_date": "2026-03-06" }),
    )
    .await;
    assert_eq!(
        bought["purchase_order_ids"].as_array().unwrap().len(),
        1,
        "{bought}"
    );
    assert_eq!(
        made["production_order_ids"].as_array().unwrap().len(),
        1,
        "{made}"
    );
    let planned_po = bought["purchase_order_ids"][0].as_str().unwrap().to_owned();
    run(&t, "purchase_order.approve", json!({ "id": planned_po })).await;
    let production = made["production_order_ids"][0].as_str().unwrap().to_owned();

    // ------------------------------------------------------------ March 9-11: making
    let released = run(&t, "production_order.release", json!({ "id": production })).await;
    let qty = dec(&released["body"]["qty"]);
    assert!(qty >= 100.0, "{released}");
    // The line took an hour longer than standard.
    let standard: f64 = released["body"]["operations"][0]["hours"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    run(
        &t,
        "production_order.record_time",
        json!({ "id": production, "line_no": 1,
        "hours": format!("{}", standard + 1.0), "worked_on": "2026-03-10" }),
    )
    .await;
    let completed = run(&t, "production_order.complete", json!({ "id": production })).await;
    documents.push(completed.clone());

    // ------------------------------------------------------------ March 2-12: selling
    // The retailer's order comes in by EDI and becomes a sales order.
    let partner = t
        .create("edi_partner", json!({ "company_id": c, "code": "NORTHFIELD", "name": "Northfield Mercantile",
            "customer_id": northfield, "warehouse_id": o.plant, "usage": "production",
            "our_qualifier": "ZZ", "our_id": "OHIOWIDGETS", "their_qualifier": "ZZ", "their_id": "NORTHFIELDMERC" }))
        .await;
    t.create(
        "edi_uom_xref",
        json!({ "partner_uom": "EA", "uom": "each" }),
    )
    .await;
    t.create(
        "edi_item_xref",
        json!({ "item_id": o.widget, "kind": "gtin", "value": "040000100096" }),
    )
    .await;
    // The retailer's distribution centre 07 is its shipping address.
    let records = run(
        &t,
        "party.get",
        json!({ "party_type": "customer", "party_id": northfield }),
    )
    .await;
    let dc07 = records["addresses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "shipping")
        .unwrap()["id"]
        .clone();
    t.create(
        "edi_location_xref",
        json!({ "partner_id": partner, "qualifier": "92", "code": "0007", "address_id": dc07 }),
    )
    .await;
    let received = run(&t, "edi.receive", json!({ "content": retailer_850() })).await;
    let message = received["messages"][0].clone();
    assert_eq!(message["state"], "proposed", "{received}");
    let edi_order = run(
        &t,
        "edi.draft",
        json!({ "message_id": message["id"], "accept_notes": true }),
    )
    .await;
    assert_eq!(
        dec(&edi_order["body"]["lines"][0]["qty"]),
        48.0,
        "{edi_order}"
    );
    let edi_order = run(&t, "sales_order.approve", json!({ "id": edi_order["id"] })).await;

    // Acme orders 30 widgets (taxed when invoiced); Müller in Hamburg 40
    // gadgets in euros, to come from the Hamburg warehouse.
    let acme_order = done(&t, &c, "sales_order", "sales_order.approve", "2026-03-11", json!({
        "customer_id": acme, "lines": [
            { "item_id": o.widget, "warehouse_id": o.plant, "qty": "30", "unit_price": "15.00" } ] })).await;
    let muller_order = done(&t, &c, "sales_order", "sales_order.approve", "2026-03-11", json!({
        "customer_id": muller, "currency": "EUR", "lines": [
            { "item_id": o.gadget, "warehouse_id": o.hamburg, "qty": "40", "unit_price": "11.00" } ] })).await;
    documents.extend([edi_order.clone(), acme_order.clone(), muller_order.clone()]);
    // The approved orders hold their widgets.
    let reserved = run(&t, "stock.reservations", json!({ "company_id": c })).await;
    let held: f64 = reserved["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["item_id"] == json!(o.widget))
        .map(|r| dec(&r["qty"]))
        .sum();
    assert!(
        close(held, 78.0),
        "48 for the retailer and 30 for Acme: {reserved}"
    );

    // ------------------------------------------------------------ March 12-16: Hamburg
    // 60 gadgets go to Hamburg in two steps; 50 arrive by the 16th, 10 are
    // still at sea at the month's end.
    let shipped = done(&t, &c, "stock_transfer", "stock_transfer.post", "2026-03-12", json!({
        "from_warehouse_id": o.plant, "to_warehouse_id": o.hamburg, "transit_warehouse_id": o.transit,
        "lines": [{ "item_id": o.gadget, "qty": "60" }] })).await;
    assert_eq!(shipped["status"], "in_transit", "{shipped}");
    let receipt = run(&t, "stock_transfer.receive", json!({ "id": shipped["id"] })).await;
    let mut body = receipt["body"].clone();
    body["lines"][0]["qty"] = json!("50");
    let (status, changed) = t
        .patch(
            &t.admin,
            &format!("/api/documents/stock_transfer_receipt/{}", id(&receipt)),
            json!({ "version": receipt["version"], "doc_date": "2026-03-16", "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let arrived = run(
        &t,
        "stock_transfer_receipt.post",
        json!({ "id": receipt["id"] }),
    )
    .await;
    documents.extend([shipped.clone(), arrived.clone()]);

    // ------------------------------------------------------------ March 17-20: shipping and billing
    let deliver = |order: Value, qty: &'static str, date: &'static str| {
        let (t, c) = (&t, c.clone());
        async move {
            done(
                t,
                &c,
                "sales_delivery",
                "sales_delivery.post",
                date,
                json!({
                "sales_order_id": order["id"], "lines": [
                    { "order_line_id": order["body"]["lines"][0]["id"], "qty": qty } ] }),
            )
            .await
        }
    };
    let bill = |order: Value,
                qty: &'static str,
                price: &'static str,
                tax: Option<String>,
                date: &'static str| {
        let (t, c) = (&t, c.clone());
        async move {
            let mut line = json!({ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty, "unit_price": price });
            if let Some(tax) = tax {
                line["tax_code_id"] = json!(tax);
            }
            done(
                t,
                &c,
                "sales_invoice",
                "sales_invoice.post",
                date,
                json!({
                "sales_order_id": order["id"], "lines": [line] }),
            )
            .await
        }
    };
    let edi_delivery = deliver(edi_order.clone(), "48", "2026-03-17").await;
    let acme_delivery = deliver(acme_order.clone(), "30", "2026-03-17").await;
    let muller_delivery = deliver(muller_order.clone(), "40", "2026-03-18").await;
    let edi_invoice = bill(edi_order.clone(), "48", "12.50", None, "2026-03-17").await;
    let acme_invoice = bill(
        acme_order.clone(),
        "30",
        "15.00",
        Some(o.sales_tax.clone()),
        "2026-03-17",
    )
    .await;
    let muller_invoice = bill(muller_order.clone(), "40", "11.00", None, "2026-03-18").await;
    documents.extend([
        edi_delivery,
        acme_delivery,
        muller_delivery,
        edi_invoice.clone(),
        acme_invoice.clone(),
        muller_invoice.clone(),
    ]);

    // The invoices go out by email as PDFs.
    let (port, kept) = smtp_server().await;
    run(
        &t,
        "smtp_settings.set",
        json!({ "company_id": c, "host": "127.0.0.1", "port": port, "security": "none",
        "from_address": "billing@ohiowidgets.example", "from_name": "Ohio Widgets billing" }),
    )
    .await;
    for invoice in [&edi_invoice, &acme_invoice, &muller_invoice] {
        let sent = run(
            &t,
            "document.send",
            json!({ "doc_type": "sales_invoice", "id": invoice["id"] }),
        )
        .await;
        assert_eq!(sent["status"], "queued", "{sent}");
    }
    // The month has made more events than one batch takes: deliver until the outbox is empty.
    for _ in 0..50 {
        let delivered = erpcraft_kernel::outbox::deliver_batch(&t.kernel, 100)
            .await
            .unwrap();
        assert_eq!(delivered.failed, 0);
        if delivered.total() == 0 {
            break;
        }
    }
    let mail = kept.lock().unwrap().clone();
    assert_eq!(mail.len(), 3);
    assert!(mail.iter().all(|m| m.contains("application/pdf")));

    // Two of Acme's widgets were scratched: Acme keeps them and is credited.
    // A credit without a return is the company's call (refused by default).
    t.create(
        "policy_rule",
        json!({ "company_id": c, "point": "sales_credit_note.without_return",
        "effect": "allow", "condition": "facts.without_return <= 5", "priority": 10 }),
    )
    .await;
    let credit = done(
        &t,
        &c,
        "sales_credit_note",
        "sales_credit_note.post",
        "2026-03-20",
        json!({
        "sales_invoice_id": acme_invoice["id"], "lines": [
            { "invoice_line_id": acme_invoice["body"]["lines"][0]["id"], "qty": "2" } ] }),
    )
    .await;
    documents.push(credit.clone());

    // ------------------------------------------------------------ March 24-27: money
    let open_item = |kind: &'static str, document: Value| {
        let (t, c) = (&t, c.clone());
        async move {
            let open = run(
                t,
                "open_item.list",
                json!({ "company_id": c, "kind": kind }),
            )
            .await;
            open["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["document_id"] == document["id"])
                .unwrap_or_else(|| panic!("no open item for {}: {open}", document["id"]))
                .clone()
        }
    };
    // Acme pays within the second discount step, net of its credit.
    let acme_item = open_item("receivable", acme_invoice.clone()).await;
    let credit_item = open_item("receivable", credit.clone()).await;
    let acme_pays = run(
        &t,
        "payment.draft",
        json!({ "company_id": c, "open_item_ids": [acme_item["id"], credit_item["id"]],
        "bank_account_id": bank, "doc_date": "2026-03-24" }),
    )
    .await;
    assert!(
        dec(&acme_pays["body"]["allocations"][0]["discount"]) > 0.0,
        "a discount is offered: {acme_pays}"
    );
    let acme_paid = run(&t, "payment.post", json!({ "id": acme_pays["id"] })).await;
    // Müller pays its first half in euros, 0.40 short: written off.
    let muller_item = open_item("receivable", muller_invoice.clone()).await;
    let first_half = dec(&muller_item["instalments"][0]["open"]);
    t.create(
        "policy_rule",
        json!({ "company_id": c, "point": "payment.write_off", "effect": "allow",
        "condition": "facts.amount <= 1", "priority": 10 }),
    )
    .await;
    let muller_paid = done(
        &t,
        &c,
        "payment",
        "payment.post",
        "2026-03-25",
        json!({
        "direction": "incoming", "bank_account_id": bank, "currency": "EUR", "allocations": [
            { "open_item_id": muller_item["id"], "amount": format!("{:.2}", first_half - 0.40),
              "write_off": "0.40", "instalment": 1 } ] }),
    )
    .await;
    // SteelCo and the trucker are paid; Boltwerk, in euros, at the month's end is not.
    let mut vendor_payments = vec![];
    for bill in [&steel_bill, &freight_bill] {
        let item = open_item("payable", (*bill).clone()).await;
        vendor_payments.push(
            done(
                &t,
                &c,
                "payment",
                "payment.post",
                "2026-03-27",
                json!({
            "direction": "outgoing", "bank_account_id": bank, "allocations": [
                { "open_item_id": item["id"], "amount": item["open"] } ] }),
            )
            .await,
        );
    }
    documents.extend([acme_paid, muller_paid]);
    documents.extend(vendor_payments);

    // ------------------------------------------------------------ March 31: month end
    let revalued = done(
        &t,
        &c,
        "fx_revaluation",
        "fx_revaluation.post",
        "2026-03-31",
        json!({}),
    )
    .await;
    let depreciated = done(
        &t,
        &c,
        "depreciation_run",
        "depreciation_run.post",
        "2026-03-31",
        json!({}),
    )
    .await;
    documents.extend([revalued, depreciated]);

    // ------------------------------------------------------------ the books
    let tb = run(&t, "gl.trial_balance", json!({ "company_id": c })).await;
    assert_eq!(tb["total_debit"], tb["total_credit"], "{tb}");
    // The press: 8,400.00 over 84 months, 100.00 for March.
    assert!(
        close(balance(&tb, "6030"), 100.0),
        "depreciation {}",
        balance(&tb, "6030")
    );
    let figures: Vec<(&str, f64)> = [
        ("inventory", o.code("inventory")),
        ("finished", o.code("finished_goods")),
        ("in transit", o.code("inventory_in_transit")),
        ("receivable", o.code("receivable")),
        ("payable", o.code("payable")),
        ("grni", o.code("grni")),
        ("revenue", o.code("revenue")),
        ("cogs", o.code("cogs")),
        ("price variance", o.code("price_variance")),
        ("absorbed", o.code("absorbed_cost")),
        ("production variance", o.code("production_variance")),
        ("write-off", o.code("write_off")),
        ("discount given", o.code("discount_given")),
        ("fx gain", o.code("fx_gain")),
        ("fx loss", o.code("fx_loss")),
        ("unrealized gain", o.code("fx_unrealized_gain")),
        ("unrealized loss", o.code("fx_unrealized_loss")),
        ("EU VAT reclaim", "1410"),
        ("EU VAT owed", "2310"),
        ("bank", o.code("bank")),
    ]
    .into_iter()
    .map(|(name, code)| (name, balance(&tb, code)))
    .collect();
    println!("March 2026 books of Ohio Widgets: {figures:?}");
    let figure = |name: &str| figures.iter().find(|(n, _)| *n == name).unwrap().1;
    // An hour over standard at 40.00.
    assert!(
        close(figure("production variance"), 40.0),
        "variance {}",
        figure("production variance")
    );
    // 0.40 EUR written off at the 16th's rate, 1.09.
    assert!(
        close(figure("write-off"), 0.44),
        "write-off {}",
        figure("write-off")
    );
    // The revaluation is reversed on April 1 (the next day is in an open
    // period), so it shows in the books as of March 31 only.
    assert!(close(figure("unrealized loss"), 0.0) && close(figure("unrealized gain"), 0.0));
    let month_end = run(
        &t,
        "gl.trial_balance",
        json!({ "company_id": c, "as_of": "2026-03-31" }),
    )
    .await;
    assert_eq!(
        month_end["total_debit"], month_end["total_credit"],
        "{month_end}"
    );
    // Boltwerk's 100.00 EUR, booked at 1.08, is 110.00 at 1.10: 2.00 lost.
    assert!(
        close(balance(&month_end, o.code("fx_unrealized_loss")), 2.0),
        "unrealized loss {}",
        balance(&month_end, o.code("fx_unrealized_loss"))
    );
    // Müller's open half, 220.00 EUR booked at 1.09, is 242.00 at 1.10: 2.20 gained.
    assert!(
        close(balance(&month_end, o.code("fx_unrealized_gain")), -2.2),
        "unrealized gain {}",
        balance(&month_end, o.code("fx_unrealized_gain"))
    );
    assert!(figure("discount given") > 0.0, "Acme took a discount");
    assert!(
        figure("receivable") > 0.0 && figure("payable") < 0.0,
        "something is still owed both ways"
    );

    // Inventory in the ledger is the stock's value: in all, and at each site.
    let stock = run(
        &t,
        "stock.on_hand",
        json!({ "company_id": c, "transit": false }),
    )
    .await;
    let in_transit_stock = run(
        &t,
        "stock.on_hand",
        json!({ "company_id": c, "transit": true }),
    )
    .await;
    let inventory = balance(&tb, o.code("inventory")) + balance(&tb, o.code("finished_goods"));
    assert!(
        close(inventory, dec(&stock["total_value"])),
        "inventory {inventory} vs stock {stock}"
    );
    assert!(
        close(
            balance(&tb, o.code("inventory_in_transit")),
            dec(&in_transit_stock["total_value"])
        ),
        "in transit {} vs {in_transit_stock}",
        balance(&tb, o.code("inventory_in_transit"))
    );
    assert!(
        dec(&in_transit_stock["total_value"]) > 0.0,
        "ten gadgets are still at sea"
    );
    let transit = run(&t, "stock_transfer.in_transit", json!({ "company_id": c })).await;
    assert!(!transit["rows"].as_array().unwrap().is_empty(), "{transit}");
    for (site, value) in [(&o.ohio, &o.ohio_value), (&o.germany, &o.germany_value)] {
        let gl = run(
            &t,
            "gl.trial_balance",
            json!({ "company_id": c, "dimension_value_ids": [value] }),
        )
        .await;
        let at_site = balance(&gl, o.code("inventory"))
            + balance(&gl, o.code("finished_goods"))
            + balance(&gl, o.code("inventory_in_transit"));
        let mut stock_at_site = 0.0;
        for transit in [false, true] {
            let s = run(
                &t,
                "stock.on_hand",
                json!({ "company_id": c, "site_id": site, "transit": transit }),
            )
            .await;
            stock_at_site += dec(&s["total_value"]);
        }
        assert!(
            close(at_site, stock_at_site),
            "at site {site}: ledger {at_site} vs stock {stock_at_site}"
        );
    }
    assert!(
        close(balance(&tb, o.code("grni")), 0.0),
        "everything received is invoiced: {}",
        balance(&tb, o.code("grni"))
    );

    // Receivables and payables are what is open, at the rates booked; at the
    // month's end, what the open items were carried at that day, with the
    // month-end revaluation (which April 1 reverses).
    for (kind, control, sign, revalued) in [
        ("receivable", "receivable", 1.0, 2.2),
        ("payable", "payable", -1.0, 2.0),
    ] {
        let open = run(
            &t,
            "open_item.list",
            json!({ "company_id": c, "kind": kind }),
        )
        .await;
        let open = dec(&open["total_open"]);
        let ledger = sign * balance(&tb, o.code(control));
        assert!(
            close(open, ledger),
            "{kind}: open {open} vs ledger {ledger}"
        );
        let march = run(
            &t,
            "open_item.list",
            json!({ "company_id": c, "kind": kind, "as_of": "2026-03-31" }),
        )
        .await;
        let carried = dec(&march["total_carried"]);
        let at_month_end = sign * balance(&month_end, o.code(control));
        assert!(
            close(at_month_end, carried),
            "{kind} at March 31: ledger {at_month_end} vs open items carried at {carried}"
        );
        assert!(
            close(carried, dec(&march["total_open"]) + revalued),
            "{kind}: {march}"
        );
    }

    // March is closed: nothing more is posted into it, April goes on.
    let periods = run(
        &t,
        "entity.list",
        json!({ "entity": "fiscal_period", "filters": { "company_id": c, "name": "2026-03" } }),
    )
    .await;
    run(
        &t,
        "fiscal_period.close",
        json!({ "id": periods["items"][0]["id"] }),
    )
    .await;
    let (status, late) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({ "company_id": c, "doc_date": "2026-03-31", "body": { "lines": [
                { "account_id": o.role("bank"), "debit": "1.00" },
                { "account_id": o.role("revenue"), "credit": "1.00" } ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{late}");
    let (status, refused) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": late["id"] }))
        .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "posted into a closed March: {refused}"
    );

    // The tax accounts are the tax report.
    let tax = run(
        &t,
        "tax.report",
        json!({ "company_id": c, "from": "2026-03-01", "to": "2026-03-31" }),
    )
    .await;
    let rc = tax["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"] == "EU-RC19")
        .unwrap()
        .clone();
    assert!(
        close(dec(&rc["paid"]), balance(&tb, "1410")),
        "reclaimed {rc}"
    );
    assert!(
        close(dec(&rc["collected"]), -balance(&tb, "2310")),
        "owed {rc}"
    );
    let collected: f64 = tax["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["code"] != "EU-RC19")
        .map(|r| dec(&r["collected"]))
        .sum();
    let us_tax_accounts = balance(&tb, "2100") + balance(&tb, "2110");
    assert!(collected > 0.0, "sales tax was charged: {tax}");
    assert!(
        close(collected, -us_tax_accounts),
        "sales tax {collected} vs accounts {us_tax_accounts}: {tax}"
    );

    // Every document of the month prints, on screen and as a PDF.
    for doc in &documents {
        let page = run(
            &t,
            "document.render",
            json!({ "doc_type": doc["doc_type"], "id": doc["id"] }),
        )
        .await;
        assert!(
            page["html"].as_str().is_some_and(|h| !h.is_empty()),
            "{}",
            doc["doc_type"]
        );
        let pdf = run(
            &t,
            "document.render_pdf",
            json!({ "doc_type": doc["doc_type"], "id": doc["id"] }),
        )
        .await;
        assert_eq!(
            pdf["content_type"], "application/pdf",
            "{}",
            doc["doc_type"]
        );
    }
    // For a demo: keep the month's database (scripts/scenario-month.sh names it).
    if std::env::var("SCENARIO_KEEP_DB").is_ok_and(|v| v == "1") {
        let name = t
            .pool
            .connect_options()
            .get_database()
            .unwrap_or_default()
            .to_owned();
        println!("SCENARIO_DATABASE={name}");
        return;
    }
    t.stop().await;
}

// ------------------------------------------------------------------ gaps
//
// What the month needed and, when it was first run, could not do without a
// workaround. Each finding is now closed; the tests keep it closed.

/// Period id by name.
async fn period(t: &TestApp, c: &str, name: &str) -> Value {
    let periods = run(
        t,
        "entity.list",
        json!({ "entity": "fiscal_period", "filters": { "company_id": c, "name": name } }),
    )
    .await;
    periods["items"][0]["id"].clone()
}

/// A journal entry of `amount` from `credit` to `debit`, posted or refused.
async fn journal(
    t: &TestApp,
    c: &str,
    date: &str,
    debit: &str,
    credit: &str,
    amount: &str,
) -> (StatusCode, Value) {
    let draft = draft(
        t,
        c,
        "journal_entry",
        date,
        json!({ "lines": [
            { "account_id": debit, "debit": amount },
            { "account_id": credit, "credit": amount } ] }),
    )
    .await;
    t.command(&t.admin, "journal_entry.post", json!({ "id": draft["id"] }))
        .await
}

/// Closed finding (month-end close): `company.setup` made one fiscal period
/// a year, so a company set up from a template could not close a month. It
/// now makes one a month (`"periods": "yearly"` for one a year), and returns
/// each year's first and last period for its year-end close.
#[tokio::test]
async fn gap_a_month_can_be_closed() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_manufacturer", "code": COMPANY, "name": "Ohio Widgets Inc",
                "first_year": 2026, "years": 1 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let (bank, revenue) = (
        created["roles"]["bank"]["id"].as_str().unwrap(),
        created["roles"]["revenue"]["id"].as_str().unwrap(),
    );
    let march = period(&t, &c, "2026-03").await;
    assert!(march.is_string(), "{created}");
    run(&t, "fiscal_period.close", json!({ "id": march })).await;
    let (status, body) = journal(&t, &c, "2026-03-31", bank, revenue, "1.00").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = journal(&t, &c, "2026-04-01", bank, revenue, "1.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The year still closes in one step, across its twelve months.
    let year = &created["fiscal_years"][0];
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/year_end_close",
            json!({ "company_id": c, "doc_date": "2026-12-31", "body": {
                "fiscal_period_id": year["last_period_id"],
                "first_period_id": year["first_period_id"],
                "retained_earnings_account_id": created["roles"]["retained_earnings"]["id"] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let posted = run(&t, "year_end_close.post", json!({ "id": draft["id"] })).await;
    assert_eq!(dec(&posted["summary"]["result"]), 1.0, "{posted}");
    t.stop().await;
}

/// A company whose years are one period each divides a year into months,
/// as long as the year is open and no year-end close refers to it. What is
/// posted stays where it was: postings are dated, not filed by period.
#[tokio::test]
async fn an_existing_year_is_divided_into_months() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": COMPANY, "name": "Ohio Widgets Inc",
                "first_year": 2026, "years": 2, "periods": "yearly" }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let (bank, revenue) = (
        created["roles"]["bank"]["id"].as_str().unwrap(),
        created["roles"]["revenue"]["id"].as_str().unwrap(),
    );
    let (fy2026, fy2027) = (
        period(&t, &c, "FY2026").await,
        period(&t, &c, "FY2027").await,
    );
    let (status, body) = journal(&t, &c, "2026-03-15", bank, revenue, "70.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let split = run(&t, "fiscal_period.split_months", json!({ "id": fy2026 })).await;
    let months = split["periods"].as_array().unwrap();
    assert_eq!(months.len(), 12, "{split}");
    assert_eq!(months[0]["name"], "2026-01");
    assert_eq!(months[1]["end_date"], "2026-02-28");
    assert_eq!(months[11]["end_date"], "2026-12-31");
    assert!(period(&t, &c, "FY2026").await.is_null());
    let march = run(
        &t,
        "gl.trial_balance",
        json!({ "company_id": c, "as_of": "2026-03-31" }),
    )
    .await;
    assert!(
        close(
            balance(&march, created["roles"]["bank"]["code"].as_str().unwrap()),
            70.0
        ),
        "{march}"
    );

    // The guard's setting alone does not let a period go: at commit, its
    // dates must be covered by open periods within them, all of them.
    let fy2027_id = uuid::Uuid::parse_str(fy2027.as_str().unwrap()).unwrap();
    for months in [0, 1] {
        let mut tx = t.pool.begin().await.unwrap();
        sqlx::query("SELECT set_config('erpcraft.period_split', $1, true)")
            .bind(fy2027_id.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("DELETE FROM fiscal_period WHERE id = $1")
            .bind(fy2027_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        if months == 1 {
            // Only January: the rest of the year would be in no period.
            sqlx::query(
                "INSERT INTO fiscal_period (id, company_id, name, start_date, end_date, status)
                 VALUES ($1, $2::text::uuid, '2027-01', '2027-01-01', '2027-01-31', 'open')",
            )
            .bind(uuid::Uuid::new_v4())
            .bind(&c)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        let error = tx.commit().await.unwrap_err().to_string();
        assert!(error.contains("do not cover"), "{error}");
    }
    assert_eq!(period(&t, &c, "FY2027").await, fy2027);

    // A month cannot be divided further.
    let (status, body) = t
        .command(
            &t.admin,
            "fiscal_period.split_months",
            json!({ "id": months[2]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // A closed year is not divided.
    run(&t, "fiscal_period.close", json!({ "id": fy2027 })).await;
    let (status, body) = t
        .command(
            &t.admin,
            "fiscal_period.split_months",
            json!({ "id": fy2027 }),
        )
        .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    run(&t, "fiscal_period.reopen", json!({ "id": fy2027 })).await;
    // Nor one a year-end close refers to.
    t.post(
        &t.admin,
        "/api/documents/year_end_close",
        json!({ "company_id": c, "doc_date": "2027-12-31", "body": {
            "fiscal_period_id": fy2027,
            "retained_earnings_account_id": created["roles"]["retained_earnings"]["id"] } }),
    )
    .await;
    let (status, body) = t
        .command(
            &t.admin,
            "fiscal_period.split_months",
            json!({ "id": fy2027 }),
        )
        .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert!(body.to_string().contains("year-end close"), "{body}");

    // March of the divided year closes; the rest of it goes on.
    run(&t, "fiscal_period.close", json!({ "id": months[2]["id"] })).await;
    let (status, _) = journal(&t, &c, "2026-03-20", bank, revenue, "1.00").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = journal(&t, &c, "2026-04-20", bank, revenue, "1.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

/// Closed finding (subledger as of a date): `open_item.list`'s `as_of` only
/// moved the aging. It now lists the items as they were that day: an item
/// paid in part in March and the rest in April shows its March balance, and
/// what the items were carried at ties to the ledger on that day.
#[tokio::test]
async fn gap_open_items_as_they_were_at_the_months_end() {
    let t = TestApp::start().await;
    let o = set_up(&t).await;
    let c = o.c.clone();
    let customer = t
        .create("customer", json!({ "code": "ACME", "name": "Acme Retail" }))
        .await;
    let invoice = done(
        &t,
        &c,
        "sales_invoice",
        "sales_invoice.post",
        "2026-03-20",
        json!({
        "customer_id": customer, "lines": [
            { "account_id": o.role("revenue"), "qty": "1", "unit_price": "100.00" } ] }),
    )
    .await;
    let open = run(
        &t,
        "open_item.list",
        json!({ "company_id": c, "kind": "receivable" }),
    )
    .await;
    let item = open["items"][0].clone();
    let pay = |date: &'static str, amount: &'static str| {
        let (t, c, item, bank) = (&t, c.clone(), item["id"].clone(), o.role("bank").to_owned());
        async move {
            done(
                t,
                &c,
                "payment",
                "payment.post",
                date,
                json!({
                "direction": "incoming", "bank_account_id": bank, "allocations": [
                    { "open_item_id": item, "amount": amount } ] }),
            )
            .await
        }
    };
    pay("2026-03-25", "40.00").await;
    pay("2026-04-05", "60.00").await;

    let as_of = |day: &'static str| {
        let (t, c) = (&t, c.clone());
        async move {
            run(
                t,
                "open_item.list",
                json!({ "company_id": c, "kind": "receivable", "as_of": day }),
            )
            .await
        }
    };
    let now = run(
        &t,
        "open_item.list",
        json!({ "company_id": c, "kind": "receivable" }),
    )
    .await;
    assert_eq!(now["total"], 0, "all paid by now: {now}");
    // Before the invoice there was nothing.
    assert_eq!(as_of("2026-03-19").await["total"], 0);
    // Before the first payment, all of it.
    assert!(close(dec(&as_of("2026-03-24").await["total_open"]), 100.0));
    let march = as_of("2026-03-31").await;
    assert_eq!(march["total"], 1, "{march}");
    assert!(close(dec(&march["items"][0]["open"]), 60.0), "{march}");
    assert!(close(dec(&march["items"][0]["settled"]), 40.0), "{march}");
    assert!(close(dec(&march["items"][0]["carried"]), 60.0), "{march}");
    let ledger = run(
        &t,
        "gl.trial_balance",
        json!({ "company_id": c, "as_of": "2026-03-31" }),
    )
    .await;
    assert!(
        close(
            dec(&march["total_carried"]),
            balance(&ledger, o.code("receivable"))
        ),
        "on March 31, {} was owed (the ledger), the open items say {}: {invoice}",
        balance(&ledger, o.code("receivable")),
        dec(&march["total_carried"])
    );
    // Asking for what is settled as well lists the item on any day after it.
    let all = run(
        &t,
        "open_item.list",
        json!({ "company_id": c, "kind": "receivable", "as_of": "2026-04-30", "include_settled": true }),
    )
    .await;
    assert_eq!(all["total"], 1, "{all}");
    assert!(close(dec(&all["items"][0]["open"]), 0.0), "{all}");
    t.stop().await;
}

/// Closed finding (setup output): `company.setup` returned ten of the US
/// template's sixteen roles. It now returns every role it sets up.
#[tokio::test]
async fn gap_setup_returns_every_role() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": COMPANY, "name": "Ohio Widgets Inc",
                "first_year": 2026, "years": 1 }),
    )
    .await;
    for role in [
        "fx_gain",
        "fx_loss",
        "discount_given",
        "discount_received",
        "asset_disposal_gain",
        "asset_disposal_loss",
    ] {
        assert!(
            created["roles"][role]["id"].is_string(),
            "{role} is not in {}",
            created["roles"]
        );
    }
    t.stop().await;
}
