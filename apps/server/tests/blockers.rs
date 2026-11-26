//! document.blockers for postings: a closed period, an approval a policy
//! asks for and one that is pending, and what is refused otherwise.

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

async fn blockers(t: &TestApp, doc_type: &str, id: &str, step: Option<&str>) -> Value {
    let mut input = json!({ "doc_type": doc_type, "id": id });
    if let Some(step) = step {
        input["step"] = json!(step);
    }
    let (status, out) = t.command(&t.admin, "document.blockers", input).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    out
}

#[tokio::test]
async fn a_posting_in_a_closed_period_says_which_period() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let draft = t.draft(&t.admin, &c, "10.00").await;
    let open = blockers(&t, "journal_entry", &id(&draft), None).await;
    assert_eq!(open["step"], "journal_entry.post");
    assert_eq!(open["clear"], true, "{open}");

    let (status, body) = t
        .command(&t.admin, "fiscal_period.close", json!({ "id": c.period }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let closed = blockers(&t, "journal_entry", &id(&draft), None).await;
    let list = closed["blockers"].as_array().unwrap();
    assert_eq!(list.len(), 1, "the trial's refusal is the same: {closed}");
    assert_eq!(list[0]["kind"], "period_closed");
    assert_eq!(list[0]["facts"]["period_id"], json!(c.period));
    assert_eq!(list[0]["resolve"][0]["tool"], "fiscal_period_reopen");
    assert_eq!(list[0]["resolve"][0]["input"]["id"], json!(c.period));
    t.stop().await;
}

#[tokio::test]
async fn an_approval_is_said_when_asked_for_and_while_it_waits() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let (status, rule) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": c.id, "point": "posting.date", "effect": "approve",
                    "condition": null, "approver_permission": "journal_entry:approve",
                    "message": "every posting needs a second pair of eyes" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{rule}");
    let draft = t.draft(&t.admin, &c, "10.00").await;
    let asked = blockers(&t, "journal_entry", &id(&draft), None).await;
    let list = asked["blockers"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{asked}");
    assert_eq!(list[0]["kind"], "approval_required");
    assert_eq!(
        list[0]["message"],
        "every posting needs a second pair of eyes"
    );
    assert_eq!(list[0]["resolve"][0]["tool"], "journal_entry_post");

    // Sent for approval: now it waits, and says for whom.
    let (status, pending) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{pending}");
    let waiting = blockers(&t, "journal_entry", &id(&draft), None).await;
    let list = waiting["blockers"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{waiting}");
    assert_eq!(list[0]["kind"], "approval_pending");
    assert_eq!(list[0]["resolve"][0]["tool"], "approval_approve");
    t.stop().await;
}

#[tokio::test]
async fn a_step_that_is_not_the_documents_is_refused() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let draft = t.draft(&t.admin, &c, "10.00").await;
    let (status, body) = t
        .command(
            &t.admin,
            "document.blockers",
            json!({ "doc_type": "journal_entry", "id": id(&draft), "step": "sales_order_deliver" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "step");
    t.stop().await;
}
