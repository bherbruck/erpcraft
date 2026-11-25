//! Price lists: the price and discount an order line is proposed, by
//! customer, customer group or everyone, currency, dates and quantity
//! breaks, with discounts for a group of items; on purchases by vendor
//! and, without a list, the last price paid. A changed price goes to the
//! company's policy when the order is approved.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

struct Shop {
    company: String,
    warehouse: Value,
}

impl Shop {
    /// Draft an order of one line of `item`; the price left out unless
    /// given. The answer.
    #[allow(clippy::too_many_arguments)]
    async fn order(
        &self,
        t: &TestApp,
        kind: &str,
        party: &str,
        date: &str,
        item: &str,
        qty: &str,
        price: Option<&str>,
        currency: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut line = json!({ "item_id": item, "warehouse_id": self.warehouse, "qty": qty });
        if let Some(price) = price {
            line["unit_price"] = json!(price);
        }
        let mut body = match kind {
            "purchase" => json!({ "vendor_id": party }),
            _ => json!({ "customer_id": party }),
        };
        body["lines"] = json!([line]);
        if let Some(currency) = currency {
            body["currency"] = json!(currency);
        }
        t.post(
            &t.admin,
            &format!("/api/documents/{kind}_order"),
            json!({ "company_id": self.company, "doc_date": date, "body": body }),
        )
        .await
    }

    async fn priced(
        &self,
        t: &TestApp,
        kind: &str,
        party: &str,
        date: &str,
        item: &str,
        qty: &str,
    ) -> (f64, Option<f64>, Value) {
        let (status, draft) = self
            .order(t, kind, party, date, item, qty, None, None)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let line = &draft["body"]["lines"][0];
        let discount = line["discount_percent"]
            .as_str()
            .map(|_| dec(&line["discount_percent"]));
        (dec(&line["unit_price"]), discount, draft)
    }
}

#[tokio::test]
async fn order_lines_are_priced_from_lists_and_changes_go_to_the_policy() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                "first_year": 2026 }),
    )
    .await;
    let shop = Shop {
        company: created["company"]["id"].as_str().unwrap().to_owned(),
        warehouse: created["warehouse_id"].clone(),
    };
    let c = &shop.company;
    let widget = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let gadget = t
        .create("item", json!({ "code": "G", "name": "Gadget" }))
        .await;
    let acme = t
        .create("customer", json!({ "code": "ACME", "name": "Acme" }))
        .await;
    let bolt = t
        .create(
            "customer",
            json!({ "code": "BOLT", "name": "Bolt Wholesale" }),
        )
        .await;
    let cash = t
        .create("customer", json!({ "code": "CASH", "name": "Walk-in" }))
        .await;

    // Groups: Bolt is a wholesaler, the widget a fastener.
    let wholesale = t
        .create(
            "price_group",
            json!({ "kind": "customer", "code": "WHOLESALE", "name": "Wholesalers" }),
        )
        .await;
    t.create(
        "price_group_member",
        json!({ "group_id": wholesale, "member_id": bolt }),
    )
    .await;
    let fasteners = t
        .create(
            "price_group",
            json!({ "kind": "item", "code": "FAST", "name": "Fasteners" }),
        )
        .await;
    t.create(
        "price_group_member",
        json!({ "group_id": fasteners, "member_id": widget }),
    )
    .await;
    // A customer cannot join a group of items.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/price_group_member",
            json!({ "group_id": fasteners, "member_id": acme }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let list = |code: &str, scope: Option<(&str, &str)>, from: Option<&str>| {
        let mut list = json!({ "company_id": c, "code": code, "name": code, "side": "sales",
                               "currency": "USD", "valid_from": from });
        if let Some((kind, id)) = scope {
            list["scope_type"] = json!(kind);
            list["scope_id"] = json!(id);
        }
        list
    };
    let base = t.create("price_list", list("BASE", None, None)).await;
    for (item, min, price) in [
        (&widget, "0", "10.00"),
        (&widget, "100", "9.00"),
        (&gadget, "0", "20.00"),
    ] {
        t.create(
            "price_list_line",
            json!({ "price_list_id": base, "item_id": item, "min_qty": min, "unit_price": price }),
        )
        .await;
    }
    let trade = t
        .create(
            "price_list",
            list("TRADE", Some(("customer_group", &wholesale)), None),
        )
        .await;
    t.create(
        "price_list_line",
        json!({ "price_list_id": trade, "item_group_id": fasteners, "discount_percent": "10" }),
    )
    .await;
    let special = t
        .create(
            "price_list",
            list("ACME", Some(("customer", &acme)), Some("2026-03-01")),
        )
        .await;
    t.create(
        "price_list_line",
        json!({ "price_list_id": special, "item_id": widget, "unit_price": "8.50" }),
    )
    .await;

    // Everyone's price, and its quantity break.
    let (price, discount, _) = shop
        .priced(&t, "sales", &cash, "2026-02-10", &widget, "5")
        .await;
    assert_eq!((price, discount), (10.0, None));
    let (price, _, _) = shop
        .priced(&t, "sales", &cash, "2026-02-10", &widget, "150")
        .await;
    assert_eq!(price, 9.0);
    // A wholesaler: the price for everyone with its group's discount on
    // fasteners, and none on what is not one.
    let (price, discount, _) = shop
        .priced(&t, "sales", &bolt, "2026-02-10", &widget, "5")
        .await;
    assert_eq!((price, discount), (10.0, Some(10.0)));
    let (price, discount, _) = shop
        .priced(&t, "sales", &bolt, "2026-02-10", &gadget, "5")
        .await;
    assert_eq!((price, discount), (20.0, None));
    // Acme's own price, from March.
    let (price, _, _) = shop
        .priced(&t, "sales", &acme, "2026-02-10", &widget, "5")
        .await;
    assert_eq!(price, 10.0);
    let (price, _, draft) = shop
        .priced(&t, "sales", &acme, "2026-03-10", &widget, "5")
        .await;
    assert_eq!(price, 8.5);
    // Saved again as it was read, it keeps its price.
    let (status, saved) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_order/{}", id(&draft)),
            json!({ "version": draft["version"], "body": draft["body"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(dec(&saved["body"]["lines"][0]["unit_price"]), 8.5);
    // No list in euros: the line must give its price.
    let (status, body) = shop
        .order(
            &t,
            "sales",
            &cash,
            "2026-02-10",
            &widget,
            "5",
            None,
            Some("EUR"),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("unit price"), "{body}");

    // A changed price is allowed by default; a company that minds refuses
    // more than 2 % below the list.
    let (status, draft) = shop
        .order(
            &t,
            "sales",
            &cash,
            "2026-02-10",
            &widget,
            "5",
            Some("9.50"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(&t, "sales_order.approve", json!({ "id": id(&draft) })).await;
    t.create(
        "policy_rule",
        json!({ "company_id": c, "point": "price.override", "effect": "refuse",
                "condition": "facts.side == 'sales' && facts.change_percent < -2",
                "message": "more than 2 % below the price list" }),
    )
    .await;
    let (_, draft) = shop
        .order(
            &t,
            "sales",
            &cash,
            "2026-02-10",
            &widget,
            "5",
            Some("9.00"),
            None,
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "sales_order.approve", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("below the price list"), "{body}");
    let (_, draft) = shop
        .order(
            &t,
            "sales",
            &cash,
            "2026-02-10",
            &widget,
            "5",
            Some("9.90"),
            None,
        )
        .await;
    run(&t, "sales_order.approve", json!({ "id": id(&draft) })).await;
    // The proposal itself needs nothing.
    let (_, _, draft) = shop
        .priced(&t, "sales", &bolt, "2026-02-10", &widget, "5")
        .await;
    run(&t, "sales_order.approve", json!({ "id": id(&draft) })).await;
    t.stop().await;
}

#[tokio::test]
async fn purchases_are_priced_by_vendor_lists_and_the_last_price_paid() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                "first_year": 2026 }),
    )
    .await;
    let shop = Shop {
        company: created["company"]["id"].as_str().unwrap().to_owned(),
        warehouse: created["warehouse_id"].clone(),
    };
    let widget = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let listed = t
        .create("vendor", json!({ "code": "LIST", "name": "Listed Parts" }))
        .await;
    let other = t
        .create("vendor", json!({ "code": "OTHER", "name": "Other Parts" }))
        .await;
    let list = t
        .create(
            "price_list",
            json!({ "company_id": shop.company, "code": "LIST-V", "name": "Listed Parts 2026",
                    "side": "purchase", "currency": "USD", "scope_type": "vendor",
                    "scope_id": listed }),
        )
        .await;
    t.create(
        "price_list_line",
        json!({ "price_list_id": list, "item_id": widget, "unit_price": "4.00" }),
    )
    .await;
    // A sales list cannot be for a vendor.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/price_list",
            json!({ "company_id": shop.company, "code": "BAD", "name": "Bad", "side": "sales",
                    "currency": "USD", "scope_type": "vendor", "scope_id": listed }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (price, _, _) = shop
        .priced(&t, "purchase", &listed, "2026-03-01", &widget, "10")
        .await;
    assert_eq!(price, 4.0);
    // Nothing listed and nothing paid yet: the line gives its price.
    let (status, body) = shop
        .order(
            &t,
            "purchase",
            &other,
            "2026-03-01",
            &widget,
            "10",
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Bought at 4.40 and invoiced.
    let (status, order) = shop
        .order(
            &t,
            "purchase",
            &other,
            "2026-03-01",
            &widget,
            "10",
            Some("4.40"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    let order = run(&t, "purchase_order.approve", json!({ "id": id(&order) })).await;
    let line = &order["body"]["lines"][0]["id"];
    for (doc_type, body) in [
        (
            "purchase_receipt",
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10" }] }),
        ),
        (
            "vendor_invoice",
            json!({ "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": line, "qty": "10", "unit_price": "4.40" }] }),
        ),
    ] {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": shop.company, "doc_date": "2026-03-05", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(&t, &format!("{doc_type}.post"), json!({ "id": id(&draft) })).await;
    }
    // The next order of it is proposed the last price paid.
    let (price, _, draft) = shop
        .priced(&t, "purchase", &other, "2026-04-01", &widget, "10")
        .await;
    assert_eq!(price, 4.4);
    run(&t, "purchase_order.approve", json!({ "id": id(&draft) })).await;
    t.stop().await;
}
