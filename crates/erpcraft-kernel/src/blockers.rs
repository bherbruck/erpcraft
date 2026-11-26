//! What stands in the way of a document's next step: `document.blockers`.
//!
//! "Why didn't this order ship?" is asked by people and agents alike, and
//! the answer is spread over stock, reservations, approvals, periods,
//! account rules and policies. This command gathers it in one answer.
//!
//! Three sources, in this order:
//!
//! 1. The kernel's own checks: the document is in a state the step does
//!    not apply from, the caller may not run the step, an approval request
//!    for it is pending.
//! 2. The checks modules register ([`BlockerCheck`]): what they can say
//!    about every line at once, such as the stock short on each line of an
//!    order.
//! 3. A trial: the step is run as the caller, in a transaction that is
//!    rolled back, with a step that creates a document followed by that
//!    document's posting (an order's delivery is posted to ship). Its
//!    refusal, and every policy that would send it for approval, are
//!    blockers too. A trial stops at its first refusal, which is why the
//!    checks come first.
//!
//! Each blocker says what would resolve it, as the commands to run.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::command::{CommandMeta, CommandSpec, Ctx};
use crate::document::{self, Document};
use crate::{Command, Error, Result};

/// A command that would resolve a blocker.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Resolution {
    /// The command, such as `fiscal_period.reopen`.
    pub command: String,
    /// The same as an MCP tool name, such as `fiscal_period_reopen`.
    pub tool: String,
    /// What running it would do about the blocker.
    pub why: String,
    /// Input to start from, where it is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

impl Resolution {
    pub fn new(command: &str, why: impl Into<String>) -> Self {
        Resolution {
            command: command.into(),
            tool: command.replace('.', "_"),
            why: why.into(),
            input: None,
        }
    }

    pub fn input(mut self, input: Value) -> Self {
        self.input = Some(input);
        self
    }
}

/// One thing that stands in the way.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Blocker {
    /// What kind of thing: `state`, `forbidden`, `approval_pending`,
    /// `approval_required`, `stock_short`, `reserved_elsewhere`,
    /// `period_closed`, `no_period`, or `refused` for what the trial was
    /// refused with otherwise.
    pub kind: String,
    /// A sentence for a person.
    pub message: String,
    /// The collection and line (from 1) of the document it is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The figures it is about: quantities, the period, the rule.
    pub facts: Value,
    /// What would resolve it. Empty where nothing the caller could run
    /// would, such as a missing permission.
    pub resolve: Vec<Resolution>,
    /// The trial's refusal on the same line says the same thing less
    /// well, and is left out.
    #[serde(skip)]
    pub covers_refusal: bool,
}

impl Blocker {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        Blocker {
            kind: kind.into(),
            message: message.into(),
            collection: None,
            line: None,
            field: None,
            facts: json!({}),
            resolve: vec![],
            covers_refusal: false,
        }
    }

    pub fn line(mut self, collection: &str, line: usize) -> Self {
        self.collection = Some(collection.into());
        self.line = Some(line);
        self
    }

    pub fn facts(mut self, facts: Value) -> Self {
        self.facts = facts;
        self
    }

    pub fn resolve(mut self, resolution: Resolution) -> Self {
        self.resolve.push(resolution);
        self
    }

    /// See [`Blocker::covers_refusal`].
    pub fn covers_refusal(mut self) -> Self {
        self.covers_refusal = true;
        self
    }

    /// What the trial was refused with.
    fn from_error(error: &Error, step: &str) -> Self {
        let detail = error.to_json();
        let kind = match error {
            Error::Forbidden(_) => "forbidden",
            Error::Conflict(_) => detail["reason"].as_str().unwrap_or("conflict"),
            Error::NotFound(_) => "not_found",
            _ => "refused",
        };
        let message = error.to_string();
        let mut blocker = Blocker::new(kind, message.clone());
        blocker.collection = detail["collection"].as_str().map(Into::into);
        blocker.line = detail["line"].as_u64().map(|l| l as usize);
        blocker.field = detail["field"].as_str().map(Into::into);
        blocker.facts = json!({ "step": step });
        // Refusals whose remedy is known by their words.
        let purpose = message
            .split("add an account rule with the purpose '")
            .nth(1)
            .and_then(|rest| rest.split('\'').next());
        if let Some(purpose) = purpose {
            blocker.kind = "account_rule_missing".into();
            blocker.facts["purpose"] = json!(purpose);
            blocker = blocker.resolve(
                Resolution::new(
                    "entity.create",
                    format!("add an account rule for '{purpose}'"),
                )
                .input(json!({ "entity": "account_rule", "data": { "purpose": purpose } })),
            );
        } else if message.contains("closed") && message.contains("period") {
            blocker.kind = "period_closed".into();
            blocker = blocker.resolve(Resolution::new(
                "fiscal_period.reopen",
                "reopen the period the step would post into (entity_list fiscal_period finds it)",
            ));
        }
        blocker
    }
}

/// What a module can say about a step of a document before it is tried.
/// See the module documentation.
#[async_trait]
pub trait BlockerCheck: Send + Sync + 'static {
    /// The blockers of running `step` (a command name) on `doc`; empty
    /// when this check has nothing to say about it.
    async fn check(&self, ctx: &mut Ctx<'_>, doc: &Document, step: &str) -> Result<Vec<Blocker>>;
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockersInput {
    /// The document type, such as `sales_order`.
    pub doc_type: String,
    /// Document id.
    pub id: Uuid,
    /// The step to check, as a command or tool name (`sales_order.deliver`
    /// or `sales_order_deliver`). By default the step the document is
    /// waiting for: its posting, or for an order its delivery.
    pub step: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct BlockersOutput {
    pub doc_type: String,
    pub id: Uuid,
    pub number: Option<String>,
    pub status: String,
    /// The step checked, as a command.
    pub step: String,
    /// The step tried after it, when the first creates a document: its
    /// posting.
    pub then: Option<String>,
    /// Nothing stands in the way: the step would go through now.
    pub clear: bool,
    pub blockers: Vec<Blocker>,
}

pub struct Blockers;

#[async_trait]
impl Command for Blockers {
    type Input = BlockersInput;
    type Output = BlockersOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "document.blockers",
            "What stands in the way of a document's next step: why an order does not ship, an \
             invoice does not post, a payment run does not post. Each blocker has a kind (state, \
             forbidden, approval_pending, approval_required, stock_short, reserved_elsewhere, \
             period_closed, no_period, or refused with the refusal's own words), the line it is \
             about, its figures, and the commands that would resolve it. The step is tried as \
             you, and nothing is saved; `clear` is true when it would go through now. By \
             default the step is the document's posting, or for an order its delivery; give \
             `step` for another.",
        )
        .label("Blockers")
        .resource("document")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: BlockersInput) -> Result<BlockersOutput> {
        let doc = document::header(ctx, &input.doc_type, input.id).await?;
        let registry = ctx.kernel.registry();
        let step = match input.step.as_deref() {
            Some(name) => registry
                .command(name)
                .or_else(|_| {
                    registry
                        .command_by_tool(name)
                        .ok_or_else(|| Error::field("step", format!("names no command: '{name}'")))
                })?
                .meta
                .clone(),
            None => default_step(ctx, &doc.doc_type)?,
        };
        if step.resource != doc.doc_type {
            return Err(Error::field(
                "step",
                format!("{} is not a step of a {}", step.name, doc.doc_type),
            ));
        }
        let then = then_step(ctx, &step);
        let mut blockers = Vec::new();

        let applies = step.from_states.is_empty() || step.from_states.contains(&doc.status);
        if !applies {
            blockers.push(state_blocker(ctx, &doc, &step));
        }
        let permitted = step
            .permission
            .as_deref()
            .is_none_or(|p| ctx.actor.can_any(p));
        if !permitted {
            blockers.push(Blocker::new(
                "forbidden",
                format!("you may not run {}; someone who may has to", step.name),
            ));
        }
        blockers.extend(pending_approvals(ctx, &doc).await?);
        let checks = registry.blocker_checks().to_vec();
        for check in checks {
            blockers.extend(check.check(ctx, &doc, &step.name).await?);
        }
        if applies && permitted {
            let tried = trial(ctx, &doc, &step, then.as_ref()).await?;
            let covering: Vec<(Option<String>, Option<usize>)> = blockers
                .iter()
                .filter(|b| b.covers_refusal)
                .map(|b| (b.collection.clone(), b.line))
                .collect();
            let waiting = blockers
                .iter()
                .any(|b| b.kind == "approval_pending" && b.facts["command"] == json!(step.name));
            for blocker in tried {
                // Asked for already: the pending request says it.
                if waiting && blocker.kind == "approval_required" {
                    continue;
                }
                let covered = blocker.kind == "refused"
                    && covering.iter().any(|(collection, line)| {
                        blocker.line.is_none()
                            || (blocker.collection == *collection && blocker.line == *line)
                    });
                if !covered {
                    blockers.push(blocker);
                }
            }
        }
        Ok(BlockersOutput {
            doc_type: doc.doc_type,
            id: doc.id,
            number: doc.number,
            status: doc.status,
            step: step.name.clone(),
            then: then.map(|t| t.name),
            clear: blockers.is_empty(),
            blockers,
        })
    }
}

/// The step a document is waiting for: its posting; else, for a document
/// that leads to others (an order), the first step that creates a document
/// with a posting of its own; else its first step that is not destructive.
fn default_step(ctx: &Ctx<'_>, doc_type: &str) -> Result<CommandMeta> {
    let registry = ctx.kernel.registry();
    if let Ok(post) = registry.command(&format!("{doc_type}.post")) {
        return Ok(post.meta.clone());
    }
    let steps: Vec<&CommandMeta> = registry
        .commands()
        .map(|c| &c.meta)
        .filter(|m| m.resource == doc_type && !m.read_only && !m.destructive)
        .collect();
    let leads_on = steps.iter().find(|m| {
        m.creates
            .as_ref()
            .is_some_and(|c| registry.command(&format!("{c}.post")).is_ok())
    });
    leads_on
        .or(steps.first())
        .map(|m| (*m).clone())
        .ok_or_else(|| Error::validation(format!("a {doc_type} has no step to check")))
}

/// The posting of the document a step creates.
fn then_step(ctx: &Ctx<'_>, step: &CommandMeta) -> Option<CommandMeta> {
    let created = step.creates.as_ref()?;
    ctx.kernel
        .registry()
        .command(&format!("{created}.post"))
        .ok()
        .map(|c| c.meta.clone())
}

/// The document is not in a state the step applies from: what would take
/// it on from where it is.
fn state_blocker(ctx: &Ctx<'_>, doc: &Document, step: &CommandMeta) -> Blocker {
    let label = doc.number.clone().unwrap_or_else(|| "this one".into());
    let mut blocker = Blocker::new(
        "state",
        format!(
            "{} {label} is {}; {} applies from {}",
            doc.doc_type,
            doc.status,
            step.name,
            step.from_states.join(" or ")
        ),
    )
    .facts(json!({ "status": doc.status, "from_states": step.from_states }));
    for next in ctx
        .kernel
        .registry()
        .commands()
        .map(|c| &c.meta)
        .filter(|m| {
            m.resource == doc.doc_type
                && !m.read_only
                && !m.destructive
                && m.from_states.contains(&doc.status)
        })
    {
        blocker = blocker.resolve(
            Resolution::new(
                &next.name,
                format!("from {}: {}", doc.status, next.description),
            )
            .input(json!({ "id": doc.id })),
        );
    }
    blocker
}

/// Approval requests waiting for this document.
async fn pending_approvals(ctx: &mut Ctx<'_>, doc: &Document) -> Result<Vec<Blocker>> {
    let pending: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, command, point, message FROM approval_request
         WHERE status = 'pending' AND company_id = $2
           AND (target_id = $1 OR input->>'id' = $1::text)
         ORDER BY requested_at",
    )
    .bind(doc.id)
    .bind(doc.company_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(pending
        .into_iter()
        .map(|(id, command, point, message)| {
            Blocker::new(
                "approval_pending",
                format!(
                    "{command} waits for approval ({}){}",
                    point,
                    message.map(|m| format!(": {m}")).unwrap_or_default()
                ),
            )
            .facts(json!({ "approval_request_id": id, "command": command, "point": point }))
            .resolve(
                Resolution::new("approval.approve", "approve it; the step then runs")
                    .input(json!({ "id": id })),
            )
            .resolve(
                Resolution::new("approval.get", "see who may approve it")
                    .input(json!({ "id": id })),
            )
        })
        .collect())
}

/// Run the step, and its follow-on, as the caller and roll back: its
/// refusal and the approvals it would ask for.
async fn trial(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    step: &CommandMeta,
    then: Option<&CommandMeta>,
) -> Result<Vec<Blocker>> {
    let registry = ctx.kernel.registry();
    let mut tx = sqlx::Connection::begin(&mut *ctx.conn).await?;
    let slot = crate::approval::Slot::collecting();
    let mut blockers = Vec::new();
    {
        let mut trial = Ctx {
            conn: &mut tx,
            actor: ctx.actor,
            kernel: ctx.kernel,
            command: &step.name,
            changed: true,
            touched: Default::default(),
            policy_rules: Default::default(),
            bodies: Default::default(),
            view: crate::command::View::Header,
            approval: slot.clone(),
        };
        let first = registry
            .command(&step.name)?
            .handler
            .run_json(&mut trial, json!({ "id": doc.id }))
            .await;
        match first {
            Err(error) => blockers.push(Blocker::from_error(&error, &step.name)),
            Ok(output) => {
                let created = output["id"].as_str().and_then(|id| id.parse::<Uuid>().ok());
                if let (Some(then), Some(created)) = (then, created) {
                    let permitted = then
                        .permission
                        .as_deref()
                        .is_none_or(|p| ctx.actor.can_any(p));
                    if !permitted {
                        blockers.push(Blocker::new(
                            "forbidden",
                            format!("you may not run {}; someone who may has to", then.name),
                        ));
                    } else {
                        trial.command = &then.name;
                        let second = registry
                            .command(&then.name)?
                            .handler
                            .run_json(&mut trial, json!({ "id": created }))
                            .await;
                        if let Err(error) = second {
                            blockers.push(Blocker::from_error(&error, &then.name));
                        }
                    }
                }
            }
        }
    }
    tx.rollback().await?;
    let needed = std::mem::take(&mut slot.lock().needed);
    for need in needed {
        let shown: serde_json::Map<String, Value> = need
            .shown
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        blockers.push(
            Blocker::new(
                "approval_required",
                need.message.clone().unwrap_or_else(|| {
                    format!("the company's policy {} asks for approval", need.point)
                }),
            )
            .facts(json!({ "point": need.point, "rule_id": need.rule_id, "facts": shown }))
            .resolve(
                Resolution::new(
                    &step.name,
                    "run it: it is kept as an approval request and runs once approved",
                )
                .input(json!({ "id": doc.id })),
            ),
        );
    }
    Ok(blockers)
}
