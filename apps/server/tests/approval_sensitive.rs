//! A command that handles secrets is never kept waiting for approval: its
//! input would sit on the request and in the audit log until it ran.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::TestApp;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, DynCommand, Module, PolicyPoint, Result, async_trait, command,
};
use serde_json::{Value, json};
use uuid::Uuid;

const POINT: &str = "vault.store";

struct Vault;

impl Module for Vault {
    fn name(&self) -> &'static str {
        "vault"
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![PolicyPoint::new(POINT, "Asked when a secret is stored.")]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Store)]
    }
}

struct Store;

#[async_trait]
impl Command for Store {
    /// `{"company_id": ..., "secret": ...}`.
    type Input = Value;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action("vault.store", "Store a secret.").sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: Value) -> Result<Value> {
        let company: Uuid = input["company_id"]
            .as_str()
            .unwrap_or_default()
            .parse()
            .unwrap();
        ctx.decide(POINT, company, json!({})).await?;
        Ok(json!({ "stored": input["secret"].as_str().map(str::len) }))
    }
}

#[tokio::test]
async fn a_command_that_handles_secrets_is_not_kept_for_approval() {
    let t = TestApp::start_with(|builder| builder.module(Vault)).await;
    let c = t.company("ACME", "USD").await;
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": c.id, "point": POINT, "effect": "approve", "priority": 1, "condition": null,
                    "approver_permission": "vault:approve" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = t
        .command(
            &t.admin,
            "vault.store",
            json!({ "company_id": c.id, "secret": "hunter2" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("handles secrets"),
        "{body}"
    );
    // Nothing of it was kept.
    assert_eq!(t.count("SELECT count(*) FROM approval_request").await, 0);
    assert_eq!(
        t.count("SELECT count(*) FROM audit_log WHERE input::text LIKE '%hunter2%'")
            .await,
        0
    );
    t.stop().await;
}
