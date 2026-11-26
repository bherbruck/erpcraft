//! Time-phased planning: a bike of a frame of two tubes, made at two
//! plants, planned back from the days its customers want it.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

struct Works {
    c: String,
    east: String,
    west: String,
    vendor: String,
    customer: String,
    bike: String,
    frame: String,
    tube: String,
}

async fn ok(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn draft(t: &TestApp, w: &Works, doc_type: &str, date: &str, body: Value) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": w.c, "doc_date": date, "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
    draft
}

async fn works(t: &TestApp) -> Works {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_manufacturer", "code": "BIKES", "name": "Bikes Inc",
                    "first_year": 2026, "years": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let c = body["company"]["id"].as_str().unwrap().to_owned();
    let site = |code: &'static str| {
        let c = c.clone();
        async move {
            t.create(
                "site",
                json!({ "company_id": c, "code": code, "name": code }),
            )
            .await
        }
    };
    let east_site = site("EAST").await;
    let west_site = site("WEST").await;
    let east = t
        .create(
            "warehouse",
            json!({ "company_id": c, "code": "E1", "name": "East plant", "site_id": east_site }),
        )
        .await;
    let west = t
        .create(
            "warehouse",
            json!({ "company_id": c, "code": "W1", "name": "West plant", "site_id": west_site }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "MILL", "name": "Tube Mill" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "SHOP", "name": "Bike Shop" }))
        .await;
    let item = |code: &'static str| async move {
        t.create("item", json!({ "code": code, "name": code, "uom": "each" }))
            .await
    };
    let bike = item("BIKE").await;
    let frame = item("FRAME").await;
    let tube = item("TUBE").await;
    // A bike is a frame, three days to assemble; a frame two tubes, five
    // days to weld. Tubes come from the mill in ten days, 25 at least.
    let bom = |product: &String, code: &'static str, lead: i64| {
        let product = product.clone();
        async move {
            t.create(
                "bom",
                json!({ "item_id": product, "code": code, "name": code, "output_qty": "1",
                        "lead_time_days": lead }),
            )
            .await
        }
    };
    let bike_bom = bom(&bike, "BIKE-1", 3).await;
    let frame_bom = bom(&frame, "FRAME-1", 5).await;
    t.create(
        "bom_line",
        json!({ "bom_id": bike_bom, "item_id": frame, "qty": "1" }),
    )
    .await;
    t.create(
        "bom_line",
        json!({ "bom_id": frame_bom, "item_id": tube, "qty": "2" }),
    )
    .await;
    t.create(
        "item_vendor",
        json!({ "company_id": c, "item_id": tube, "vendor_id": vendor, "lead_time_days": 10,
                "min_order_qty": "25" }),
    )
    .await;
    for (warehouse, safety) in [(&east, "0"), (&west, "2")] {
        t.create(
            "reorder_rule",
            json!({ "company_id": c, "item_id": bike, "warehouse_id": warehouse,
                    "method": "mrp", "safety_stock": safety }),
        )
        .await;
    }
    Works {
        c,
        east,
        west,
        vendor,
        customer,
        bike,
        frame,
        tube,
    }
}

fn of<'a>(plan: &'a Value, item: &str, warehouse: &str) -> Vec<&'a Value> {
    plan["planned_orders"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["item_id"] == item && p["warehouse_id"] == warehouse)
        .collect()
}

#[tokio::test]
async fn a_three_level_bike_is_planned_back_from_its_dates_at_two_sites() {
    let t = TestApp::start().await;
    let w = works(&t).await;

    // Ten bikes wanted at the east plant on the 20th, four at the west
    // plant on the 15th.
    for (warehouse, qty, date) in [(&w.east, "10", "2026-12-20"), (&w.west, "4", "2026-12-15")] {
        let order = draft(
            &t,
            &w,
            "sales_order",
            "2026-12-01",
            json!({ "customer_id": w.customer, "requested_date": date, "lines": [
                { "item_id": w.bike, "warehouse_id": warehouse, "qty": qty,
                  "unit_price": "500" }] }),
        )
        .await;
        ok(&t, "sales_order.approve", json!({ "id": id(&order) })).await;
    }
    // Eight tubes already ordered for the east plant, due on the 25th:
    // too late.
    let po = draft(
        &t,
        &w,
        "purchase_order",
        "2026-11-20",
        json!({ "vendor_id": w.vendor, "expected_date": "2026-12-25", "lines": [
            { "item_id": w.tube, "warehouse_id": w.east, "qty": "8", "unit_price": "4" }] }),
    )
    .await;
    assert_eq!(po["body"]["expected_date"], "2026-12-25");
    let po = ok(&t, "purchase_order.approve", json!({ "id": id(&po) })).await;

    let plan = ok(
        &t,
        "planning.mrp",
        json!({ "company_id": w.c, "as_of": "2026-12-01" }),
    )
    .await;
    assert_eq!(plan["levels"][&w.bike], 0, "{plan}");
    assert_eq!(plan["levels"][&w.frame], 1);
    assert_eq!(plan["levels"][&w.tube], 2);

    // East: bikes due the 20th, started the 17th; frames due the 17th,
    // started the 12th; tubes needed the 12th: the 8 ordered, expedited,
    // and 25 more (the mill's least for 12), ordered the 2nd.
    let east_bike = of(&plan, &w.bike, &w.east);
    assert_eq!(east_bike.len(), 1, "{plan}");
    assert_eq!(dec(&east_bike[0]["qty"]), 10.0);
    assert_eq!(east_bike[0]["due_date"], "2026-12-20");
    assert_eq!(east_bike[0]["release_date"], "2026-12-17");
    assert_eq!(east_bike[0]["source"], "make");
    assert_eq!(east_bike[0]["pegs"][0]["kind"], "sales_order");
    let east_frame = of(&plan, &w.frame, &w.east);
    assert_eq!(east_frame[0]["due_date"], "2026-12-17");
    assert_eq!(east_frame[0]["release_date"], "2026-12-12");
    assert_eq!(east_frame[0]["pegs"][0]["kind"], "planned_order");
    let east_tube = of(&plan, &w.tube, &w.east);
    assert_eq!(east_tube.len(), 1, "{plan}");
    assert_eq!(dec(&east_tube[0]["qty"]), 25.0);
    assert_eq!(east_tube[0]["due_date"], "2026-12-12");
    assert_eq!(east_tube[0]["release_date"], "2026-12-02");
    assert_eq!(east_tube[0]["source"], "buy");
    assert_eq!(east_tube[0]["vendor_id"], json!(w.vendor));
    let exceptions = plan["exceptions"].as_array().unwrap();
    let expedite = exceptions
        .iter()
        .find(|e| e["kind"] == "expedite" && e["document_id"] == po["id"])
        .unwrap_or_else(|| panic!("{plan}"));
    assert_eq!(expedite["to_date"], "2026-12-12");
    assert_eq!(expedite["number"], po["number"]);

    // West: two bikes kept safe now (too late to start: released
    // late), four for the 15th; the tubes for the safe ones are late too,
    // and one lot of 25 serves both.
    let west_bike = of(&plan, &w.bike, &w.west);
    assert_eq!(west_bike.len(), 2, "{plan}");
    assert_eq!(west_bike[0]["pegs"][0]["kind"], "safety_stock");
    assert_eq!(dec(&west_bike[1]["qty"]), 4.0);
    assert_eq!(west_bike[1]["due_date"], "2026-12-15");
    let west_tube = of(&plan, &w.tube, &w.west);
    assert_eq!(west_tube.len(), 1, "{plan}");
    assert_eq!(dec(&west_tube[0]["qty"]), 25.0);
    assert!(
        exceptions
            .iter()
            .any(|e| e["kind"] == "release_late" && e["warehouse_id"] == json!(w.west)),
        "{plan}"
    );

    // Drafted from the plan: a production order per planned order to make,
    // purchase orders for the tubes, expected when they are due.
    let made = ok(
        &t,
        "planning.draft_production_orders",
        json!({ "company_id": w.c, "doc_date": "2026-12-01", "release_by": "2026-12-31" }),
    )
    .await;
    // East: a bike, a frame; west: two bikes, two frames.
    assert_eq!(
        made["production_order_ids"].as_array().unwrap().len(),
        6,
        "{made}"
    );
    let bought = ok(
        &t,
        "planning.draft_purchase_orders",
        json!({ "company_id": w.c, "doc_date": "2026-12-01", "release_by": "2026-12-31" }),
    )
    .await;
    let orders = bought["purchase_order_ids"].as_array().unwrap();
    assert_eq!(orders.len(), 2, "{bought}");
    let (_, east_po) = t
        .get(
            &t.admin,
            &format!(
                "/api/documents/purchase_order/{}",
                orders[0].as_str().unwrap()
            ),
        )
        .await;
    assert!(east_po["body"]["expected_date"].is_string(), "{east_po}");
    // Planned again: the drafts meet every need.
    let again = ok(
        &t,
        "planning.mrp",
        json!({ "company_id": w.c, "as_of": "2026-12-01" }),
    )
    .await;
    assert!(
        again["planned_orders"].as_array().unwrap().is_empty(),
        "{again}"
    );

    // The reorder suggestions leave what MRP plans alone.
    let suggestions = ok(&t, "planning.suggestions", json!({ "company_id": w.c })).await;
    assert!(
        suggestions["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["item_id"] != json!(w.tube) && s["item_id"] != json!(w.bike)),
        "{suggestions}"
    );
    t.stop().await;
}
