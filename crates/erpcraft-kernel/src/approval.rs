//! Approvals: the third outcome of a policy rule.
//!
//! A `policy_rule` with effect `approve` names who approves (a role, a
//! permission, or both) and how many must. Where such a rule decides, the
//! command does not run: the command executor rolls it back and keeps it as
//! an **approval request**, with the exact input, who asked, the point, the
//! rule and the facts. The caller is told the command is pending (see
//! [`pending_response`]).
//!
//! Others approve or reject it with `approval.approve` and
//! `approval.reject`. When enough have approved, the command runs, once,
//! as the person who asked, with the recorded input, and the point allows
//! for exactly the facts that were approved. If the facts differ by then
//! (the order was edited) the approval does not apply: the request is
//! stale. Nobody approves their own request, administrators included.
//!
//! What an approval is bound to:
//!
//! - **Every point that asked.** The executor tries the command to the end
//!   (a point that asks lets the try go on), so a request holds every
//!   point and every set of facts that asked: each line of a per-line
//!   point, and other points of the same command (`posting.date` with
//!   `payment.post`). One approval grants them all; the approvers are
//!   those of the first rule that asked, as many as the most demanding
//!   rule wants. A command that fails anyway is told so and not sent.
//! - **The document, as it was.** When the command acts on a document (its
//!   input names one as `id`), the request records the document's version.
//!   Any change to it before the run makes the request stale, however
//!   alike the facts: another vendor or another bank account at the same
//!   total is not what was approved. The document is not locked while a
//!   request waits: its owner may still correct it, and is then asked to
//!   send it for approval again. A command that acts on no document is
//!   approved on its facts and its exact input.
//! - **The facts, but not the clock.** A fact that depends on the day it is
//!   asked (`posting.date`'s `days_from_today`) is left out of the
//!   comparison.
//! - **The requester's credential.** The run is capped by the scopes of the
//!   token the request was made with.
//!
//! A command that handles secrets (`sensitive`) is never kept for
//! approval: its input would sit on the request and in the audit log until
//! it ran. A rule that asks for approval where such a command asks is
//! refused when the command runs.
//! Requests expire (14 days unless the rule says otherwise); a rejected,
//! failed, stale or expired one can be asked again by running the command
//! again.
//!
//! Approval exists only where a module asks a policy point: nothing a
//! ledger forbids outright can be approved.

use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Duration, Utc};
use erpcraft_core::{Error, Result, new_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::Kernel;
use crate::authz::{Actor, load_actor};
use crate::command::{Command, CommandSpec, Ctx, DynCommand, command};
use crate::entity::Page;

/// Who may approve, as a rule says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Approvers {
    /// Holders of this role, for the request's company or for all
    /// companies.
    pub role_id: Option<Uuid>,
    /// Holders of this permission for the request's company.
    pub permission: Option<String>,
    /// How many distinct people must approve.
    pub needed: i32,
    /// Days until the request expires.
    pub days: i32,
}

/// What an approval rule needs before the command may run.
#[derive(Debug, Clone)]
pub struct Needed {
    pub point: String,
    pub rule_id: Uuid,
    pub company_id: Uuid,
    pub facts: Value,
    pub shown: Vec<(String, String)>,
    pub message: Option<String>,
    pub approvers: Approvers,
}

/// What was approved, for the run of an approved request: every point
/// that asked, with the facts it asked on.
#[derive(Debug, Clone)]
pub struct Granted {
    pub request_id: Uuid,
    pub asks: Vec<(String, Value)>,
}

#[derive(Debug, Default)]
pub struct SlotState {
    /// Every point that asked for approval, in the order they asked, each
    /// with its facts once.
    pub needed: Vec<Needed>,
    pub granted: Option<Granted>,
    /// An approval was granted, and the facts are not those approved.
    pub stale: bool,
    /// The command executor tries the command to the end to learn every
    /// point that asks (a per-line point asks once per line, and a
    /// command may ask more than one point): a point that asks lets the
    /// try go on, and the executor rolls it all back and keeps the request.
    /// Elsewhere the first point that asks stops the command.
    pub collecting: bool,
}

/// Shared between a command's context and the policies it loads.
#[derive(Debug, Clone, Default)]
pub struct Slot(Arc<Mutex<SlotState>>);

impl Slot {
    pub fn lock(&self) -> MutexGuard<'_, SlotState> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A slot for the command executor's try: see [`SlotState::collecting`].
    pub(crate) fn collecting() -> Slot {
        let slot = Slot::default();
        slot.lock().collecting = true;
        slot
    }

    fn granted(granted: Granted) -> Slot {
        let slot = Slot::default();
        slot.lock().granted = Some(granted);
        slot
    }
}

// ------------------------------------------------------------- the record

#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub struct ApprovalRequest {
    pub id: Uuid,
    pub company_id: Uuid,
    /// The command waiting, such as `purchase_order.approve`.
    pub command: String,
    /// Exactly what it was given.
    pub input: Value,
    /// Who asked.
    pub requested_by: Uuid,
    #[sqlx(default)]
    /// Their name.
    pub requested_by_name: Option<String>,
    pub requested_at: DateTime<Utc>,
    /// The policy point that asked for approval.
    pub point: String,
    /// The policy rule that did.
    pub rule_id: Option<Uuid>,
    /// The facts it decided on, as a person reads them.
    pub shown_facts: Value,
    /// The rule's message, with the facts filled in.
    pub message: Option<String>,
    /// Holders of this role approve.
    pub approver_role_id: Option<Uuid>,
    /// Holders of this permission approve.
    pub approver_permission: Option<String>,
    /// How many distinct people must approve.
    pub approvals_needed: i32,
    pub expires_at: DateTime<Utc>,
    /// `pending`, `executed`, `failed`, `stale`, `rejected`, or `expired`
    /// (pending past `expires_at`).
    pub status: String,
    /// What the command returned when it ran.
    pub result: Option<Value>,
    /// Why it did not run, or was rejected.
    pub error: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    /// Who approved or rejected, in order.
    #[sqlx(skip)]
    pub decisions: Vec<DecisionView>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub struct DecisionView {
    pub user_id: Uuid,
    /// Their name.
    pub name: String,
    /// Whose decision it was, when it was made in their place by the
    /// person they delegated their approvals to.
    pub on_behalf_of: Option<Uuid>,
    /// `approve` or `reject`.
    pub decision: String,
    pub reason: Option<String>,
    pub decided_at: DateTime<Utc>,
}

const COLUMNS: &str = "r.id, r.company_id, r.command, r.input, r.requested_by,
    u.name AS requested_by_name, r.requested_at, r.point, r.rule_id, r.shown_facts, r.message,
    r.approver_role_id, r.approver_permission, r.approvals_needed, r.expires_at,
    CASE WHEN r.status = 'pending' AND r.expires_at <= now() THEN 'expired' ELSE r.status END
        AS status,
    r.result, r.error, r.decided_at";

async fn load(conn: &mut crate::PgConnection, id: Uuid, lock: bool) -> Result<ApprovalRequest> {
    let mut request: ApprovalRequest = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM approval_request r JOIN app_user u ON u.id = r.requested_by
         WHERE r.id = $1{}",
        if lock { " FOR UPDATE OF r" } else { "" }
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found("approval request"))?;
    request.decisions = sqlx::query_as(
        "SELECT d.user_id, u.name, d.on_behalf_of, d.decision, d.reason, d.decided_at
         FROM approval_decision d JOIN app_user u ON u.id = d.user_id
         WHERE d.request_id = $1 ORDER BY d.decided_at, u.name",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(request)
}

/// What a command that needs approval returns instead of its output, over
/// HTTP with status 202 and over MCP as the tool's result.
pub fn pending_response(request: &ApprovalRequest, preview: bool) -> Value {
    json!({
        "status": "pending_approval",
        "preview": preview,
        "approval_request": {
            "id": if preview { Value::Null } else { json!(request.id) },
            "command": request.command,
            "point": request.point,
            "message": request.message,
            "facts": request.shown_facts,
            "approvers": {
                "role_id": request.approver_role_id,
                "permission": request.approver_permission,
                "approvals_needed": request.approvals_needed,
            },
            "expires_at": request.expires_at,
        },
    })
}

/// Whether a command's result is a pending approval rather than its
/// output.
pub fn is_pending(output: &Value) -> bool {
    output.get("status").and_then(Value::as_str) == Some("pending_approval")
}

/// Keep a command a rule sent to approval. Called by the command executor
/// after it rolled the command back. A request still pending for the same
/// command, input and person (or the same idempotency key) is returned
/// instead of a second one.
pub(crate) async fn request(
    kernel: &Kernel,
    actor: &Actor,
    command: &str,
    input: &Value,
    idempotency_key: Option<&str>,
    needs: Vec<Needed>,
    preview: bool,
) -> Result<Value> {
    // The first point that asked is what the request shows and whose rule
    // names the approvers; as many must approve as the most demanding of
    // the rules that asked.
    let Some(needed) = needs.first().cloned() else {
        return Err(Error::internal("an approval request without a point"));
    };
    let most = needs
        .iter()
        .map(|n| n.approvers.needed)
        .max()
        .unwrap_or(needed.approvers.needed);
    let asks: Vec<Value> = needs
        .iter()
        .map(|n| json!({ "point": n.point, "rule_id": n.rule_id, "facts": n.facts }))
        .collect();
    let expires_at = Utc::now() + Duration::days(i64::from(needed.approvers.days));
    let shown: serde_json::Map<String, Value> = needed
        .shown
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let draft = ApprovalRequest {
        id: Uuid::nil(),
        company_id: needed.company_id,
        command: command.into(),
        input: input.clone(),
        requested_by: actor.user_id,
        requested_by_name: Some(actor.name.clone()),
        requested_at: Utc::now(),
        point: needed.point.clone(),
        rule_id: Some(needed.rule_id),
        shown_facts: Value::Object(shown),
        message: needed.message.clone(),
        approver_role_id: needed.approvers.role_id,
        approver_permission: needed.approvers.permission.clone(),
        approvals_needed: most,
        expires_at,
        status: "pending".into(),
        result: None,
        error: None,
        decided_at: None,
        decisions: vec![],
    };
    if preview {
        return Ok(pending_response(&draft, true));
    }
    let mut tx = kernel.pool().begin().await?;
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM approval_request
         WHERE requested_by = $1 AND command = $2 AND status = 'pending' AND expires_at > now()
           AND (input = $3 OR (idempotency_key IS NOT NULL AND idempotency_key = $4))
         ORDER BY requested_at DESC LIMIT 1",
    )
    .bind(actor.user_id)
    .bind(command)
    .bind(input)
    .bind(idempotency_key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = existing {
        let found = load(&mut tx, id, false).await?;
        tx.commit().await?;
        return Ok(pending_response(&found, false));
    }
    // The document the command acts on, as it stands now: any change to
    // it before the run makes the approval stale, whatever the facts say.
    let target: Option<(Uuid, i32)> = match input.get("id").and_then(Value::as_str) {
        Some(id) => match id.parse::<Uuid>() {
            Ok(id) => {
                sqlx::query_as("SELECT id, version FROM document WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
            }
            Err(_) => None,
        },
        None => None,
    };
    let id = new_id();
    sqlx::query(
        "INSERT INTO approval_request (id, company_id, command, input, requested_by,
             idempotency_key, point, rule_id, facts, shown_facts, message, approver_role_id,
             approver_permission, approvals_needed, expires_at, needs, target_id,
             target_version, scopes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                 $18, $19)",
    )
    .bind(id)
    .bind(draft.company_id)
    .bind(command)
    .bind(input)
    .bind(actor.user_id)
    .bind(idempotency_key)
    .bind(&draft.point)
    .bind(needed.rule_id)
    .bind(&needed.facts)
    .bind(&draft.shown_facts)
    .bind(&draft.message)
    .bind(draft.approver_role_id)
    .bind(&draft.approver_permission)
    .bind(draft.approvals_needed)
    .bind(expires_at)
    .bind(Value::Array(asks))
    .bind(target.map(|(id, _)| id))
    .bind(target.map(|(_, version)| version))
    .bind(actor.scopes.as_ref().map(|scopes| json!(scopes)))
    .execute(&mut *tx)
    .await?;
    let audit_id = new_id();
    sqlx::query(
        "INSERT INTO audit_log (id, user_id, command, input, outcome, error_code, error_message)
         VALUES ($1, $2, $3, $4, 'error', 'pending_approval', $5)",
    )
    .bind(audit_id)
    .bind(actor.user_id)
    .bind(command)
    .bind(input)
    .bind(format!("sent for approval ({})", draft.point))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO audit_subject (audit_id, resource, record_id) VALUES ($1, 'approval_request', $2)",
    )
    .bind(audit_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let found = load(&mut tx, id, false).await?;
    outbox(&mut tx, "approval.requested", &found, None).await?;
    tx.commit().await?;
    Ok(pending_response(&found, false))
}

async fn outbox(
    conn: &mut crate::PgConnection,
    topic: &str,
    request: &ApprovalRequest,
    by: Option<&Actor>,
) -> Result<()> {
    sqlx::query("INSERT INTO outbox_event (id, topic, payload) VALUES ($1, $2, $3)")
        .bind(new_id())
        .bind(topic)
        .bind(json!({
            "approval_request_id": request.id,
            "company_id": request.company_id,
            "command": request.command,
            "point": request.point,
            "message": request.message,
            "requested_by": request.requested_by,
            "status": request.status,
            "by": by.map(|a| a.user_id),
            "approvers": {
                "role_id": request.approver_role_id,
                "permission": request.approver_permission,
                "approvals_needed": request.approvals_needed,
            },
        }))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// --------------------------------------------------------- who approves

/// Whether the actor is one of the people who may approve the request.
/// Never its requester; otherwise an administrator always is.
async fn qualifies(ctx: &mut Ctx<'_>, request: &ApprovalRequest) -> Result<bool> {
    Ok(standing(ctx, request).await?.is_some())
}

/// Whether `actor` may decide the request in their own right.
async fn in_own_right(
    conn: &mut crate::PgConnection,
    actor: &Actor,
    request: &ApprovalRequest,
) -> Result<bool> {
    if actor.user_id == request.requested_by {
        return Ok(false);
    }
    if actor.is_admin {
        return Ok(true);
    }
    if let Some(permission) = &request.approver_permission
        && actor.scopes.is_none()
        && actor.can(permission, request.company_id)
    {
        return Ok(true);
    }
    if let Some(role) = request.approver_role_id {
        let holds: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM role_assignment WHERE user_id = $1 AND role_id = $2
                            AND (company_id IS NULL OR company_id = $3))",
        )
        .bind(actor.user_id)
        .bind(role)
        .bind(request.company_id)
        .fetch_one(&mut *conn)
        .await?;
        return Ok(holds && actor.scopes.is_none());
    }
    Ok(false)
}

/// On what grounds the actor may decide the request: `Some(None)` in their
/// own right, `Some(Some(user))` in the place of a user who delegated
/// their approvals to them and has not decided it yet, `None` not at all.
/// The requester never decides their own request, in person or by proxy,
/// and a delegation is followed one step only.
async fn standing(ctx: &mut Ctx<'_>, request: &ApprovalRequest) -> Result<Option<Option<Uuid>>> {
    if ctx.actor.user_id == request.requested_by {
        return Ok(None);
    }
    if in_own_right(&mut *ctx.conn, ctx.actor, request).await? {
        return Ok(Some(None));
    }
    if ctx.actor.scopes.is_some() {
        return Ok(None);
    }
    let delegators: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT d.user_id FROM approval_delegation d
         JOIN app_user u ON u.id = d.user_id AND u.active
         WHERE d.delegate_id = $1 AND current_date BETWEEN d.starts_on AND d.ends_on
           AND d.user_id <> $2
         ORDER BY d.user_id",
    )
    .bind(ctx.actor.user_id)
    .bind(request.requested_by)
    .fetch_all(&mut *ctx.conn)
    .await?;
    for delegator in delegators {
        let decided = request
            .decisions
            .iter()
            .any(|d| d.on_behalf_of.unwrap_or(d.user_id) == delegator);
        if decided {
            continue;
        }
        let Ok(actor) = load_actor(ctx.kernel.pool(), delegator, None).await else {
            continue;
        };
        // A delegation stands only while its giver may delegate.
        if !actor.can_any(DELEGATE) {
            continue;
        }
        if in_own_right(&mut *ctx.conn, &actor, request).await? {
            return Ok(Some(Some(delegator)));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------- commands

/// The right to let someone else decide one's approvals.
pub const DELEGATE: &str = "approval:delegate";

pub(crate) fn commands() -> Vec<Arc<dyn DynCommand>> {
    vec![
        command(List),
        command(Get),
        command(Approve),
        command(Reject),
        command(Delegate),
        command(Delegations),
        command(EndDelegation),
    ]
}

#[derive(Deserialize, JsonSchema, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Scope {
    /// Pending requests the caller may approve and has not decided on.
    #[default]
    ToApprove,
    /// The caller's own requests.
    Mine,
    /// Every request: takes approval:read for all companies.
    All,
}

/// Where an approval request stands.
#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    /// Waiting for approvers.
    Pending,
    /// Approved and run.
    Executed,
    /// Approved, but the command failed when run.
    Failed,
    /// Approved, but what it was asked on had changed since.
    Stale,
    /// Turned down by an approver.
    Rejected,
    /// Not decided before it expired.
    Expired,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Executed => "executed",
            Status::Failed => "failed",
            Status::Stale => "stale",
            Status::Rejected => "rejected",
            Status::Expired => "expired",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    /// `to_approve` (the default), `mine`, or `all`.
    #[serde(default)]
    scope: Scope,
    /// Only requests in this status.
    status: Option<Status>,
    /// At most 200; 50 when omitted.
    limit: Option<i64>,
}

struct List;

#[async_trait::async_trait]
impl Command for List {
    type Input = ListInput;
    type Output = Page<ApprovalRequest>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "approval.list",
            "Approval requests, newest first: by default the pending ones the caller may approve \
             (never their own), or the caller's own (`mine`), or all of them (`all`, with \
             approval:read for all companies).",
        )
        .label("Approvals")
        .resource("approval")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ListInput) -> Result<Page<ApprovalRequest>> {
        if input.scope == Scope::All {
            ctx.actor.require_global("approval:read")?;
        }
        let limit = input.limit.unwrap_or(50).clamp(1, 200);
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT r.id FROM approval_request r
             WHERE ($1 <> 'mine' OR r.requested_by = $2)
               AND ($1 <> 'to_approve' OR (r.status = 'pending' AND r.expires_at > now()
                    AND r.requested_by <> $2
                    AND NOT EXISTS (SELECT 1 FROM approval_decision d
                                    WHERE d.request_id = r.id AND d.user_id = $2)))
             ORDER BY r.requested_at DESC LIMIT 1000",
        )
        .bind(match input.scope {
            Scope::ToApprove => "to_approve",
            Scope::Mine => "mine",
            Scope::All => "all",
        })
        .bind(ctx.actor.user_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut items = vec![];
        for id in ids {
            let request = load(&mut *ctx.conn, id, false).await?;
            if input.status.is_some_and(|s| s.as_str() != request.status) {
                continue;
            }
            if input.scope == Scope::ToApprove && !qualifies(ctx, &request).await? {
                continue;
            }
            items.push(request);
            if items.len() as i64 >= limit {
                break;
            }
        }
        Ok(Page {
            total: items.len() as i64,
            items,
        })
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RequestRef {
    /// Approval request id.
    id: Uuid,
}

/// Whether the actor may see a request: they asked, they may decide it,
/// or they read all requests.
async fn visible(ctx: &mut Ctx<'_>, request: &ApprovalRequest) -> Result<bool> {
    Ok(request.requested_by == ctx.actor.user_id
        || ctx.actor.can_global("approval:read")
        || request
            .decisions
            .iter()
            .any(|d| d.user_id == ctx.actor.user_id)
        || qualifies(ctx, request).await?)
}

struct Get;

#[async_trait::async_trait]
impl Command for Get {
    type Input = RequestRef;
    type Output = ApprovalRequest;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "approval.get",
            "One approval request with who decided what, for its requester, for whoever may \
             decide it, and for holders of approval:read for all companies.",
        )
        .label("Approval request")
        .resource("approval")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RequestRef) -> Result<ApprovalRequest> {
        let request = load(&mut *ctx.conn, input.id, false).await?;
        if !visible(ctx, &request).await? {
            return Err(Error::not_found("approval request"));
        }
        Ok(request)
    }
}

/// The checks before a decision: the request is pending and not expired,
/// and the actor may decide it and has not yet.
async fn decidable(ctx: &mut Ctx<'_>, id: Uuid) -> Result<(ApprovalRequest, Option<Uuid>)> {
    ctx.touch("approval_request", id);
    let request = load(&mut *ctx.conn, id, true).await?;
    if request.requested_by == ctx.actor.user_id {
        return Err(Error::forbidden(
            "nobody approves or rejects their own request",
        ));
    }
    let on_behalf_of = standing(ctx, &request).await?;
    if on_behalf_of.is_none() {
        if visible(ctx, &request).await? {
            return Err(Error::forbidden(
                "you are not one of the people who may decide this request",
            ));
        }
        return Err(Error::not_found("approval request"));
    }
    if request.status != "pending" {
        return Err(Error::state(format!(
            "the request is {}; a decision is no longer taken",
            request.status
        )));
    }
    let on_behalf_of = on_behalf_of.flatten();
    let authority = on_behalf_of.unwrap_or(ctx.actor.user_id);
    if request
        .decisions
        .iter()
        .any(|d| d.user_id == ctx.actor.user_id || d.on_behalf_of.unwrap_or(d.user_id) == authority)
    {
        return Err(Error::state("you decided on this request already"));
    }
    Ok((request, on_behalf_of))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DecideInput {
    /// Approval request id.
    id: Uuid,
    /// Why: needed to reject, optional to approve.
    reason: Option<String>,
}

struct Approve;

#[async_trait::async_trait]
impl Command for Approve {
    type Input = DecideInput;
    type Output = ApprovalRequest;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "approval.approve",
            "Approve a pending request. Its requester never can. When as many distinct people \
             as the rule asks have approved, the command runs, once, as the person who asked, \
             with exactly the input they gave, and its result is kept on the request. If what \
             it was about has changed since it was asked for, it does not run and the request \
             is stale.",
        )
        .label("Approve")
        .resource("approval")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DecideInput) -> Result<ApprovalRequest> {
        let (request, on_behalf_of) = decidable(ctx, input.id).await?;
        sqlx::query(
            "INSERT INTO approval_decision (request_id, user_id, decision, reason, on_behalf_of)
             VALUES ($1, $2, 'approve', $3, $4)",
        )
        .bind(request.id)
        .bind(ctx.actor.user_id)
        .bind(
            input
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty()),
        )
        .bind(on_behalf_of)
        .execute(&mut *ctx.conn)
        .await?;
        let approvals = request
            .decisions
            .iter()
            .filter(|d| d.decision == "approve")
            .count()
            + 1;
        if (approvals as i32) < request.approvals_needed {
            let request = load(&mut *ctx.conn, request.id, false).await?;
            outbox(
                &mut *ctx.conn,
                "approval.approved",
                &request,
                Some(ctx.actor),
            )
            .await?;
            return Ok(request);
        }
        run_approved(ctx, &request).await?;
        let request = load(&mut *ctx.conn, request.id, false).await?;
        outbox(
            &mut *ctx.conn,
            "approval.approved",
            &request,
            Some(ctx.actor),
        )
        .await?;
        Ok(request)
    }
}

/// Run the approved command as its requester, inside the approval's
/// transaction behind a savepoint: a run that is refused leaves the
/// decisions, and the request says why it did not run.
async fn run_approved(ctx: &mut Ctx<'_>, request: &ApprovalRequest) -> Result<()> {
    #[allow(clippy::type_complexity)]
    let (facts, needs, target_id, target_version, scopes): (
        Value,
        Option<Value>,
        Option<Uuid>,
        Option<i32>,
        Option<Value>,
    ) = sqlx::query_as(
        "SELECT facts, needs, target_id, target_version, scopes FROM approval_request
         WHERE id = $1",
    )
    .bind(request.id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    // What was approved: every point that asked, with its facts.
    let asks: Vec<(String, Value)> = match needs.as_ref().and_then(Value::as_array) {
        Some(needs) => needs
            .iter()
            .map(|n| {
                (
                    n["point"].as_str().unwrap_or_default().to_owned(),
                    n["facts"].clone(),
                )
            })
            .collect(),
        None => vec![(request.point.clone(), facts)],
    };
    // The document as the approvers saw it: changed since, the approval
    // does not apply, however alike the facts.
    if let (Some(target), Some(version)) = (target_id, target_version) {
        let now: Option<i32> = sqlx::query_scalar("SELECT version FROM document WHERE id = $1")
            .bind(target)
            .fetch_optional(&mut *ctx.conn)
            .await?;
        if now != Some(version) {
            sqlx::query(
                "UPDATE approval_request SET status = 'stale', error = $2, decided_at = now()
                 WHERE id = $1",
            )
            .bind(request.id)
            .bind(
                "the document changed after approval was asked for: what was approved is \
                 not what would run, so the approval no longer applies",
            )
            .execute(&mut *ctx.conn)
            .await?;
            return Ok(());
        }
    }
    let kernel = ctx.kernel;
    let entry = kernel.registry().command(&request.command)?;
    // As the requester, and capped by the credential they asked with.
    let scopes: Option<Vec<String>> = scopes.and_then(|s| serde_json::from_value(s).ok());
    let requester = load_actor(kernel.pool(), request.requested_by, scopes).await;
    sqlx::query("SAVEPOINT approval_run")
        .execute(&mut *ctx.conn)
        .await?;
    let slot = Slot::granted(Granted {
        request_id: request.id,
        asks,
    });
    let (outcome, touched) = match &requester {
        Err(_) => (
            Err(Error::forbidden(
                "the person who asked can no longer sign in",
            )),
            Default::default(),
        ),
        Ok(requester) => {
            let permitted = match &entry.meta.permission {
                Some(permission) => requester.require_any(permission),
                None => Ok(()),
            };
            match permitted {
                Err(error) => (Err(error), Default::default()),
                Ok(()) => {
                    let mut run =
                        Ctx::internal(&mut *ctx.conn, requester, kernel, entry.meta.name.as_str());
                    run.approval = slot.clone();
                    let outcome = entry
                        .handler
                        .run_json(&mut run, request.input.clone())
                        .await;
                    let touched = run.take_touched();
                    (outcome, touched)
                }
            }
        }
    };
    let decided_by: Vec<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM approval_decision WHERE request_id = $1 AND decision = 'approve'
         ORDER BY decided_at",
    )
    .bind(request.id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    match outcome {
        Ok(output) => {
            sqlx::query("RELEASE SAVEPOINT approval_run")
                .execute(&mut *ctx.conn)
                .await?;
            // The run is on record as the requester's command, and the
            // approval touches what it touched, so each record's history
            // shows both who asked and who approved.
            let audit_id = new_id();
            sqlx::query(
                "INSERT INTO audit_log (id, user_id, command, input) VALUES ($1, $2, $3, $4)",
            )
            .bind(audit_id)
            .bind(request.requested_by)
            .bind(&request.command)
            .bind((!entry.meta.sensitive).then_some(&request.input))
            .execute(&mut *ctx.conn)
            .await?;
            let mut subjects = touched.clone();
            subjects.insert(("approval_request".into(), request.id));
            crate::command::record_subjects(&mut *ctx.conn, audit_id, &subjects).await?;
            for (resource, id) in &touched {
                ctx.touch(resource, *id);
            }
            let payload = if entry.meta.sensitive {
                json!({ "command": request.command, "user_id": request.requested_by,
                        "approval_request_id": request.id, "approved_by": decided_by })
            } else {
                json!({ "command": request.command, "user_id": request.requested_by,
                        "input": request.input, "output": output,
                        "approval_request_id": request.id, "approved_by": decided_by })
            };
            sqlx::query("INSERT INTO outbox_event (id, topic, payload) VALUES ($1, $2, $3)")
                .bind(new_id())
                .bind(&request.command)
                .bind(&payload)
                .execute(&mut *ctx.conn)
                .await?;
            sqlx::query(
                "UPDATE approval_request SET status = 'executed', result = $2, decided_at = now()
                 WHERE id = $1",
            )
            .bind(request.id)
            .bind((!entry.meta.sensitive).then_some(&output))
            .execute(&mut *ctx.conn)
            .await?;
        }
        Err(error) => {
            sqlx::query("ROLLBACK TO SAVEPOINT approval_run")
                .execute(&mut *ctx.conn)
                .await?;
            let status = if slot.lock().stale { "stale" } else { "failed" };
            sqlx::query(
                "UPDATE approval_request SET status = $2, error = $3, decided_at = now()
                 WHERE id = $1",
            )
            .bind(request.id)
            .bind(status)
            .bind(error.to_string())
            .execute(&mut *ctx.conn)
            .await?;
        }
    }
    Ok(())
}

struct Reject;

#[async_trait::async_trait]
impl Command for Reject {
    type Input = DecideInput;
    type Output = ApprovalRequest;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "approval.reject",
            "Reject a pending request, with a reason. The command does not run; its requester \
             can ask again by running it again. Its requester never decides it.",
        )
        .label("Reject")
        .resource("approval")
        .target(false)
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DecideInput) -> Result<ApprovalRequest> {
        let reason = input
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or_else(|| Error::field("reason", "say why the request is rejected"))?
            .to_owned();
        let (request, on_behalf_of) = decidable(ctx, input.id).await?;
        sqlx::query(
            "INSERT INTO approval_decision (request_id, user_id, decision, reason, on_behalf_of)
             VALUES ($1, $2, 'reject', $3, $4)",
        )
        .bind(request.id)
        .bind(ctx.actor.user_id)
        .bind(&reason)
        .bind(on_behalf_of)
        .execute(&mut *ctx.conn)
        .await?;
        sqlx::query(
            "UPDATE approval_request SET status = 'rejected', error = $2, decided_at = now()
             WHERE id = $1",
        )
        .bind(request.id)
        .bind(&reason)
        .execute(&mut *ctx.conn)
        .await?;
        let request = load(&mut *ctx.conn, request.id, false).await?;
        outbox(
            &mut *ctx.conn,
            "approval.rejected",
            &request,
            Some(ctx.actor),
        )
        .await?;
        Ok(request)
    }
}

// ------------------------------------------------------------- delegation

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DelegateInput {
    /// Who decides in the caller's place.
    delegate_id: Uuid,
    /// The first day. Today when omitted.
    starts_on: Option<chrono::NaiveDate>,
    /// The last day.
    ends_on: chrono::NaiveDate,
    /// Why, for the record: a holiday, a leave.
    reason: Option<String>,
}

/// A delegation of approvals.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub struct DelegationView {
    pub id: Uuid,
    /// Whose approvals are delegated.
    pub user_id: Uuid,
    pub user_name: String,
    /// Who decides in their place.
    pub delegate_id: Uuid,
    pub delegate_name: String,
    pub starts_on: chrono::NaiveDate,
    pub ends_on: chrono::NaiveDate,
    pub reason: Option<String>,
}

const DELEGATION: &str = "SELECT d.id, d.user_id, u.name AS user_name, d.delegate_id,
        v.name AS delegate_name, d.starts_on, d.ends_on, d.reason
    FROM approval_delegation d
    JOIN app_user u ON u.id = d.user_id
    JOIN app_user v ON v.id = d.delegate_id";

struct Delegate;

#[async_trait::async_trait]
impl Command for Delegate {
    type Input = DelegateInput;
    type Output = DelegationView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "approval.delegate",
            "Let someone else decide, from one day to another, the approval requests the \
             caller could decide: while away, for example. They decide in the caller's place \
             and their decision says so; it counts as the caller's, so one person is never \
             counted twice. They never decide a request of their own, and what they could \
             decide in their own right they decide as themselves. Takes approval:delegate \
             (in any company), and a token limited to some permissions cannot delegate: what \
             is handed on is the caller's whole standing as an approver. A delegation counts \
             only while the caller still holds approval:delegate.",
        )
        .permission(DELEGATE)
        .label("Delegate approvals")
        .resource("approval")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DelegateInput) -> Result<DelegationView> {
        // The delegate decides with the caller's full standing, not the
        // token's: a limited token cannot hand that on, as it cannot hand
        // on a role (`may_delegate` refuses it for the same reason).
        if ctx.actor.scopes.is_some() {
            return Err(Error::forbidden(
                "a token limited to some permissions cannot delegate approvals; sign in or use \
                 an unlimited token",
            ));
        }
        let starts_on = input.starts_on.unwrap_or_else(|| Utc::now().date_naive());
        if input.ends_on < starts_on {
            return Err(Error::field(
                "ends_on",
                "the last day comes after the first",
            ));
        }
        if input.delegate_id == ctx.actor.user_id {
            return Err(Error::field(
                "delegate_id",
                "approvals are delegated to someone else",
            ));
        }
        let active: Option<bool> = sqlx::query_scalar("SELECT active FROM app_user WHERE id = $1")
            .bind(input.delegate_id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
        if active != Some(true) {
            return Err(Error::field(
                "delegate_id",
                "the delegate must be an active user",
            ));
        }
        let id = new_id();
        sqlx::query(
            "INSERT INTO approval_delegation (id, user_id, delegate_id, starts_on, ends_on, reason)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(ctx.actor.user_id)
        .bind(input.delegate_id)
        .bind(starts_on)
        .bind(input.ends_on)
        .bind(
            input
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty()),
        )
        .execute(&mut *ctx.conn)
        .await?;
        ctx.touch("app_user", ctx.actor.user_id);
        Ok(sqlx::query_as(&format!("{DELEGATION} WHERE d.id = $1"))
            .bind(id)
            .fetch_one(&mut *ctx.conn)
            .await?)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DelegationsInput {}

#[derive(Serialize, JsonSchema)]
struct DelegationsOutput {
    /// What the caller delegated, and what was delegated to them, that has
    /// not ended.
    items: Vec<DelegationView>,
}

struct Delegations;

#[async_trait::async_trait]
impl Command for Delegations {
    type Input = DelegationsInput;
    type Output = DelegationsOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "approval.delegations",
            "The caller's delegations of approvals that have not ended: those they gave and \
             those given to them.",
        )
        .label("Delegations")
        .resource("approval")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, _input: DelegationsInput) -> Result<DelegationsOutput> {
        Ok(DelegationsOutput {
            items: sqlx::query_as(&format!(
                "{DELEGATION} WHERE (d.user_id = $1 OR d.delegate_id = $1)
                   AND d.ends_on >= current_date
                 ORDER BY d.starts_on, d.id"
            ))
            .bind(ctx.actor.user_id)
            .fetch_all(&mut *ctx.conn)
            .await?,
        })
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EndInput {
    /// The delegation.
    id: Uuid,
}

struct EndDelegation;

#[async_trait::async_trait]
impl Command for EndDelegation {
    type Input = EndInput;
    type Output = DelegationView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "approval.end_delegation",
            "End a delegation of the caller's approvals today; one that has not started is \
             ended before it starts. Decisions already made in their place stand.",
        )
        .label("End delegation")
        .resource("approval")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: EndInput) -> Result<DelegationView> {
        let today = Utc::now().date_naive();
        let found: Option<DelegationView> = sqlx::query_as(&format!(
            "{DELEGATION} WHERE d.id = $1 AND d.user_id = $2 FOR UPDATE OF d"
        ))
        .bind(input.id)
        .bind(ctx.actor.user_id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let mut view = found.ok_or_else(|| Error::not_found("delegation"))?;
        if view.starts_on >= today {
            // Not begun: it never will.
            sqlx::query("DELETE FROM approval_delegation WHERE id = $1")
                .bind(view.id)
                .execute(&mut *ctx.conn)
                .await?;
            view.ends_on = view.starts_on;
        } else if view.ends_on >= today {
            // In force: its last day was yesterday.
            let yesterday = today.pred_opt().unwrap_or(today);
            sqlx::query("UPDATE approval_delegation SET ends_on = $2 WHERE id = $1")
                .bind(view.id)
                .bind(yesterday)
                .execute(&mut *ctx.conn)
                .await?;
            view.ends_on = yesterday;
        } else {
            ctx.no_change();
        }
        Ok(view)
    }
}
