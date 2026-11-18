//! Approvals: a policy rule that asks a second person before a command
//! runs, and the command then running as the person who asked.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    item: String,
    warehouse: String,
    vendor: String,
    buyer: String,
    buyer_id: String,
    controller: String,
    second_controller: String,
    clerk: String,
}

async fn setup(t: &TestApp) -> Fixture {
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
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let mut accounts = vec![];
    for (code, kind) in [
        ("1300", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
        ("5200", "expense"),
    ] {
        accounts.push(
            t.create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind }),
            )
            .await,
        );
    }
    t.create(
        "purchasing_settings",
        json!({ "company_id": c.id, "grni_account_id": accounts[2],
                "payable_account_id": accounts[1], "price_variance_account_id": accounts[3] }),
    )
    .await;
    t.inventory_rule(&c.id, &accounts[0]).await;
    let (buyer, buyer_id) = t
        .user_with_rules(
            "buyer@example.com",
            json!(["purchase_order:*", "approval:read"]),
            Some(&c.id),
        )
        .await;
    let (controller, _) = t
        .user_with_rules(
            "controller@example.com",
            json!(["purchase_order:read", "purchase_order:approve_large"]),
            Some(&c.id),
        )
        .await;
    let (second_controller, _) = t
        .user_with_rules(
            "controller2@example.com",
            json!(["purchase_order:read", "purchase_order:approve_large"]),
            Some(&c.id),
        )
        .await;
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!(["purchase_order:read"]),
            Some(&c.id),
        )
        .await;
    // Orders over 10,000 need a controller; over 50,000, two.
    for (priority, condition, needed) in [
        (10, "facts.total_net > 50000", 2),
        (20, "facts.total_net > 10000", 1),
    ] {
        t.create(
            "policy_rule",
            json!({
                "company_id": c.id, "point": "purchase_order.approve", "effect": "approve",
                "condition": condition, "priority": priority,
                "approver_permission": "purchase_order:approve_large",
                "approvals_needed": needed,
                "message": "an order of {total_net} {currency} needs a controller's approval",
            }),
        )
        .await;
    }
    Fixture {
        c,
        item,
        warehouse,
        vendor,
        buyer,
        buyer_id,
        controller,
        second_controller,
        clerk,
    }
}

impl Fixture {
    /// A draft order of `qty` widgets at 200.00, by the buyer.
    async fn order(&self, t: &TestApp, qty: &str) -> Value {
        let (status, draft) = t
            .post(
                &self.buyer,
                "/api/documents/purchase_order",
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": {
                    "vendor_id": self.vendor,
                    "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": "200.00" }],
                } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        draft
    }

    async fn approve_order(&self, t: &TestApp, order: &Value) -> (StatusCode, Value) {
        t.command(
            &self.buyer,
            "purchase_order.approve",
            json!({ "id": order["id"] }),
        )
        .await
    }

    /// Ask for approval of the order and return the request's id.
    async fn pending(&self, t: &TestApp, order: &Value) -> String {
        let (status, body) = self.approve_order(t, order).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], "pending_approval");
        body["approval_request"]["id"].as_str().unwrap().to_owned()
    }
}

async fn status_of(t: &TestApp, order: &Value) -> String {
    let (_, doc) = t
        .get(
            &t.admin,
            &format!("/api/documents/purchase_order/{}", id(order)),
        )
        .await;
    doc["status"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_large_order_waits_for_a_controller_and_then_runs_as_the_buyer() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // A small order is approved at once.
    let small = f.order(&t, "10").await;
    let (status, body) = f.approve_order(&t, &small).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "open");

    // A large one: a preview says it needs approval, and creates nothing.
    let large = f.order(&t, "100").await;
    let (status, preview) = t
        .post(
            &f.buyer,
            "/api/commands/purchase_order.approve?preview=true",
            json!({ "id": large["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let pending = &preview["result"];
    assert_eq!(pending["status"], "pending_approval", "{preview}");
    assert_eq!(pending["preview"], true);
    assert_eq!(pending["approval_request"]["id"], Value::Null);
    assert_eq!(
        pending["approval_request"]["approvers"]["permission"],
        "purchase_order:approve_large"
    );
    assert_eq!(t.count("SELECT count(*) FROM approval_request").await, 0);

    // Asked for real: 202, the order stays a draft, and asking again finds
    // the same request.
    let (status, body) = f.approve_order(&t, &large).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let request = &body["approval_request"];
    assert_eq!(request["point"], "purchase_order.approve");
    assert_eq!(request["command"], "purchase_order.approve");
    assert_eq!(
        request["message"],
        "an order of 20000.0000 USD needs a controller's approval"
    );
    assert_eq!(request["approvers"]["approvals_needed"], 1);
    let request_id = request["id"].as_str().unwrap().to_owned();
    assert_eq!(status_of(&t, &large).await, "draft");
    assert_eq!(f.pending(&t, &large).await, request_id);
    assert_eq!(t.count("SELECT count(*) FROM approval_request").await, 1);

    // Nobody approves their own request, an administrator neither; and
    // someone who is not an approver cannot.
    let (status, body) = t
        .command(&f.buyer, "approval.approve", json!({ "id": request_id }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = t
        .command(&f.clerk, "approval.approve", json!({ "id": request_id }))
        .await;
    assert_ne!(status, StatusCode::OK);
    let (_, listed) = t.command(&f.clerk, "approval.list", json!({})).await;
    assert_eq!(listed["total"], 0);
    let (_, mine) = t
        .command(&f.buyer, "approval.list", json!({ "scope": "mine" }))
        .await;
    assert_eq!(mine["items"][0]["id"], request_id.as_str());

    // The controller sees it, and approves: it runs, as the buyer, once.
    let (_, listed) = t.command(&f.controller, "approval.list", json!({})).await;
    assert_eq!(listed["items"][0]["id"], request_id.as_str(), "{listed}");
    assert_eq!(listed["items"][0]["requested_by_name"], "Test User");
    let (status, done) = t
        .command(
            &f.controller,
            "approval.approve",
            json!({ "id": request_id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["status"], "executed", "{done}");
    assert_eq!(done["result"]["status"], "open");
    assert_eq!(done["result"]["number"], "PO-2026-00002");
    assert_eq!(done["decisions"][0]["decision"], "approve");
    assert_eq!(status_of(&t, &large).await, "open");
    // Once: a second approval is refused, and the order was numbered once.
    let (status, body) = t
        .command(
            &f.second_controller,
            "approval.approve",
            json!({ "id": request_id }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        t.count("SELECT count(*) FROM document WHERE number = 'PO-2026-00002'")
            .await,
        1
    );

    // The order's history shows who asked and who approved.
    let history = t
        .command(
            &t.admin,
            "audit.history",
            json!({ "resource": "purchase_order", "id": large["id"] }),
        )
        .await
        .1;
    let entries: Vec<(String, String)> = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["command"].as_str().unwrap_or_default().to_owned(),
                e["user_id"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert!(
        entries
            .iter()
            .any(|(c, u)| c == "purchase_order.approve" && u == &f.buyer_id),
        "{history}"
    );
    assert!(
        entries.iter().any(|(c, _)| c == "approval.approve"),
        "{history}"
    );

    // Each step is an event, and the order's own event says it was approved.
    for topic in ["approval.requested", "approval.approved"] {
        assert!(
            t.count(&format!(
                "SELECT count(*) FROM outbox_event WHERE topic = '{topic}'"
            ))
            .await
                >= 1,
            "{topic}"
        );
    }
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM outbox_event WHERE topic = 'purchase_order.approve'
             AND payload->>'approval_request_id' = '{request_id}'"
        ))
        .await,
        1
    );
    t.stop().await;
}

#[tokio::test]
async fn two_approvers_rejection_staleness_and_expiry() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Over 50,000: two controllers.
    let huge = f.order(&t, "300").await;
    let request = f.pending(&t, &huge).await;
    let approve = |token: String, id: String| {
        let t = &t;
        async move {
            t.command(&token, "approval.approve", json!({ "id": id }))
                .await
        }
    };
    let (status, body) = approve(f.controller.clone(), request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "pending");
    assert_eq!(status_of(&t, &huge).await, "draft");
    let (status, _) = approve(f.controller.clone(), request.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "one person counts once");
    let (status, body) = approve(f.second_controller.clone(), request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "executed");
    assert_eq!(status_of(&t, &huge).await, "open");

    // Rejected, with a reason; asked again, a new request.
    let order = f.order(&t, "100").await;
    let request = f.pending(&t, &order).await;
    let (status, _) = t
        .command(&f.controller, "approval.reject", json!({ "id": request }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = t
        .command(
            &f.controller,
            "approval.reject",
            json!({ "id": request, "reason": "not this quarter" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "rejected");
    assert_eq!(body["error"], "not this quarter");
    assert_eq!(status_of(&t, &order).await, "draft");
    let again = f.pending(&t, &order).await;
    assert_ne!(again, request);

    // The order changed after the request: the approval is void.
    let (_, current) = t
        .get(
            &f.buyer,
            &format!("/api/documents/purchase_order/{}", id(&order)),
        )
        .await;
    let (status, body) = t
        .patch(
            &f.buyer,
            &format!("/api/documents/purchase_order/{}", id(&order)),
            json!({ "version": current["version"], "body": {
                "vendor_id": f.vendor,
                "lines": [{ "item_id": f.item, "warehouse_id": f.warehouse, "qty": "120", "unit_price": "200.00" }],
            } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = approve(f.controller.clone(), again.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "stale", "{body}");
    assert!(body["error"].as_str().unwrap().contains("changed"));
    assert_eq!(status_of(&t, &order).await, "draft");

    // A request nobody decided in time expires; asking again starts anew.
    let fresh = f.pending(&t, &order).await;
    sqlx::query(
        "UPDATE approval_request SET expires_at = now() - interval '1 second' WHERE id = $1::uuid",
    )
    .bind(&fresh)
    .execute(&t.pool)
    .await
    .unwrap();
    let (status, body) = approve(f.controller.clone(), fresh.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("expired")
    );
    let (_, listed) = t.command(&f.controller, "approval.list", json!({})).await;
    assert!(
        !listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == fresh.as_str())
    );
    let renewed = f.pending(&t, &order).await;
    assert_ne!(renewed, fresh);
    assert!(
        t.count("SELECT count(*) FROM outbox_event WHERE topic = 'approval.rejected'")
            .await
            >= 1
    );
    t.stop().await;
}

#[tokio::test]
async fn an_administrator_is_subject_to_the_same_rule_and_mcp_says_pending() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "100").await;

    // The administrator asks; the administrator cannot approve it.
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.approve",
            json!({ "id": order["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let request = body["approval_request"]["id"].clone();
    let (status, _) = t
        .command(&t.admin, "approval.approve", json!({ "id": request }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Over MCP the tool succeeds and says it is pending.
    let other = f.order(&t, "100").await;
    let result = t
        .tool(
            &f.buyer,
            "purchase_order_approve",
            json!({ "id": other["id"] }),
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["status"], "pending_approval");
    assert!(result["structuredContent"]["approval_request"]["id"].is_string());

    // Approval exists only where a policy point allows it.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": f.c.id, "point": "stock.negative", "effect": "approve",
                    "approver_permission": "stock:approve" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": f.c.id, "point": "vendor_credit_note.without_return",
                    "effect": "approve", "condition": null }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "approver_role_id");
    for point in ["vendor_credit_note.without_return", "payment.post"] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/entities/policy_rule",
                json!({ "company_id": f.c.id, "point": point, "effect": "approve",
                        "condition": null, "approver_permission": "payment:approve" }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{point}: {body}");
    }
    t.stop().await;
}

#[tokio::test]
async fn a_large_payment_waits_for_the_treasurer() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let doc = |doc_type: &str, body: Value| {
        let t = &t;
        let path = format!("/api/documents/{doc_type}");
        let company = f.c.id.clone();
        async move {
            let (status, draft) = t
                .post(
                    &t.admin,
                    &path,
                    json!({ "company_id": company, "doc_date": "2026-03-15", "body": body }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{draft}");
            draft
        }
    };
    // Bought and invoiced: 2,000 owed to the vendor.
    let order = doc(
        "purchase_order",
        json!({ "vendor_id": f.vendor, "lines": [{ "item_id": f.item, "warehouse_id": f.warehouse, "qty": "10", "unit_price": "200.00" }] }),
    )
    .await;
    let (status, order) = t
        .command(
            &t.admin,
            "purchase_order.approve",
            json!({ "id": order["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    let line = &order["body"]["lines"][0]["id"];
    for (doc_type, command, body) in [
        (
            "purchase_receipt",
            "purchase_receipt.post",
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10" }] }),
        ),
        (
            "vendor_invoice",
            "vendor_invoice.post",
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10", "unit_price": "200.00" }] }),
        ),
    ] {
        let draft = doc(doc_type, body).await;
        let (status, body) = t
            .command(&t.admin, command, json!({ "id": draft["id"] }))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (_, owed) = t
        .command(&t.admin, "open_item.list", json!({ "company_id": f.c.id }))
        .await;
    let item = owed["items"][0]["id"].clone();

    // Paying more than 1,000 needs the treasurer.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "payment.post", "effect": "approve",
                "condition": "facts.amount > 1000",
                "approver_permission": "payment:approve_large" }),
    )
    .await;
    let (treasurer, _) = t
        .user_with_rules(
            "treasurer@example.com",
            json!(["payment:read", "payment:approve_large"]),
            Some(&f.c.id),
        )
        .await;
    let small = doc(
        "payment",
        json!({ "direction": "outgoing", "bank_account_id": f.c.cash,
                "allocations": [{ "open_item_id": item, "amount": "500.00" }] }),
    )
    .await;
    let (status, body) = t
        .command(&t.admin, "payment.post", json!({ "id": small["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let large = doc(
        "payment",
        json!({ "direction": "outgoing", "bank_account_id": f.c.cash,
                "allocations": [{ "open_item_id": item, "amount": "1500.00" }] }),
    )
    .await;
    let (status, body) = t
        .command(&t.admin, "payment.post", json!({ "id": large["id"] }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["approval_request"]["facts"]["direction"], "outgoing");
    let request = body["approval_request"]["id"].clone();
    let (_, owed) = t
        .command(&t.admin, "open_item.list", json!({ "company_id": f.c.id }))
        .await;
    assert_eq!(common::dec(&owed["total_open"]), 1500.0, "nothing paid yet");

    let (status, body) = t
        .command(&treasurer, "approval.approve", json!({ "id": request }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "executed", "{body}");
    assert_eq!(body["result"]["status"], "posted");
    let (_, owed) = t
        .command(&t.admin, "open_item.list", json!({ "company_id": f.c.id }))
        .await;
    assert_eq!(owed["total"], 0, "{owed}");
    t.stop().await;
}

#[tokio::test]
async fn approvals_are_listed_by_a_known_status_only() {
    let t = TestApp::start().await;
    for status in [
        "pending", "executed", "failed", "stale", "rejected", "expired",
    ] {
        let (code, body) = t
            .command(
                &t.admin,
                "approval.list",
                json!({ "scope": "all", "status": status }),
            )
            .await;
        assert_eq!(code, StatusCode::OK, "{status}: {body}");
    }
    let (code, body) = t
        .command(
            &t.admin,
            "approval.list",
            json!({ "scope": "all", "status": "approved" }),
        )
        .await;
    assert!(code.is_client_error(), "{body}");
    t.stop().await;
}

#[tokio::test]
async fn an_approval_is_for_the_document_as_it_was_when_asked() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let other = t
        .create("vendor", json!({ "code": "V002", "name": "Someone Else" }))
        .await;
    // 20,000 with Parts Inc, sent for approval.
    let order = f.order(&t, "100").await;
    let request = f.pending(&t, &order).await;
    // The buyer then makes it an order with someone else, at the same
    // total: the facts the controller sees are the very same.
    let (_, current) = t
        .get(
            &f.buyer,
            &format!("/api/documents/purchase_order/{}", id(&order)),
        )
        .await;
    let (status, body) = t
        .patch(
            &f.buyer,
            &format!("/api/documents/purchase_order/{}", id(&order)),
            json!({ "version": current["version"], "body": {
                "vendor_id": other,
                "lines": [{ "item_id": f.item, "warehouse_id": f.warehouse, "qty": "100", "unit_price": "200.00" }],
            } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Approved, it does not run: what was approved is not this order.
    let (status, body) = t
        .command(&f.controller, "approval.approve", json!({ "id": request }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "stale", "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("document changed"),
        "{body}"
    );
    assert_eq!(status_of(&t, &order).await, "draft");
    t.stop().await;
}

#[tokio::test]
async fn one_approval_covers_every_point_and_line_that_asked() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Receiving more than was ordered needs a controller, line by line;
    // and so does posting more than 30 days back.
    for (point, condition) in [
        ("purchase_receipt.over_receipt", "facts.over_by > 0"),
        ("posting.date", "facts.days_from_today < -30"),
    ] {
        t.create(
            "policy_rule",
            json!({ "company_id": f.c.id, "point": point, "effect": "approve",
                    "condition": condition, "priority": 5,
                    "approver_permission": "purchase_order:approve_large" }),
        )
        .await;
    }
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/purchase_order",
            json!({ "company_id": f.c.id, "doc_date": "2026-01-02", "body": {
                "vendor_id": f.vendor, "lines": [
                    { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "10", "unit_price": "2.00" },
                    { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "10", "unit_price": "3.00" },
                ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, order) = t
        .command(
            &t.admin,
            "purchase_order.approve",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    // Two lines over by different amounts, dated long ago: three asks.
    let (status, receipt) = t
        .post(
            &t.admin,
            "/api/documents/purchase_receipt",
            json!({ "company_id": f.c.id, "doc_date": "2026-01-02", "body": {
                "purchase_order_id": order["id"], "lines": [
                    { "order_line_id": order["body"]["lines"][0]["id"], "qty": "12" },
                    { "order_line_id": order["body"]["lines"][1]["id"], "qty": "13" },
                ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_receipt.post",
            json!({ "id": receipt["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let request = body["approval_request"]["id"].as_str().unwrap().to_owned();
    let asked: Vec<String> = sqlx::query_scalar(
        "SELECT e->>'point' FROM approval_request, jsonb_array_elements(needs) e
         WHERE id = $1::uuid ORDER BY 1",
    )
    .bind(&request)
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        asked,
        [
            "posting.date",
            "purchase_receipt.over_receipt",
            "purchase_receipt.over_receipt"
        ]
    );
    // Approved on another day: the days from today differ, which does not
    // matter; everything else is as approved, so it runs.
    sqlx::query(
        "UPDATE approval_request SET needs = (
            SELECT jsonb_agg(CASE WHEN e->>'point' = 'posting.date'
                                  THEN jsonb_set(e, '{facts,days_from_today}', '-9999')
                                  ELSE e END)
            FROM jsonb_array_elements(needs) e)
         WHERE id = $1::uuid",
    )
    .bind(&request)
    .execute(&t.pool)
    .await
    .unwrap();
    let (status, body) = t
        .command(&f.controller, "approval.approve", json!({ "id": request }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "executed", "{body}");
    let (_, posted) = t
        .get(
            &t.admin,
            &format!("/api/documents/purchase_receipt/{}", id(&receipt)),
        )
        .await;
    assert_eq!(posted["status"], "posted");
    t.stop().await;
}

#[tokio::test]
async fn an_approved_run_keeps_the_scopes_it_was_asked_with() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // The buyer asks through a token for purchase orders only.
    let (status, token) = t
        .command(
            &f.buyer,
            "auth.create_token",
            json!({ "name": "agent", "scopes": ["purchase_order:*"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{token}");
    let token = token["token"].as_str().unwrap().to_owned();
    let order = f.order(&t, "100").await;
    let (status, body) = t
        .command(
            &token,
            "purchase_order.approve",
            json!({ "id": order["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let request = body["approval_request"]["id"].as_str().unwrap().to_owned();
    let scopes: Value =
        sqlx::query_scalar("SELECT scopes FROM approval_request WHERE id = $1::uuid")
            .bind(&request)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(scopes, json!(["purchase_order:*"]));
    // Run with them: had the token not covered the command, the run would
    // be refused, as it is when the scopes on record do not.
    sqlx::query("UPDATE approval_request SET scopes = '[\"account:read\"]' WHERE id = $1::uuid")
        .bind(&request)
        .execute(&t.pool)
        .await
        .unwrap();
    let (status, body) = t
        .command(&f.controller, "approval.approve", json!({ "id": request }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "failed", "{body}");
    assert_eq!(status_of(&t, &order).await, "draft");
    t.stop().await;
}
