//! Commands: the only way business state changes or is queried.
//!
//! HTTP, MCP and the UI all call [`execute`]. It wraps the command's handler
//! in one database transaction and adds what every command needs:
//! authorization, idempotency, the audit record and the outbox event.

use std::sync::Arc;

use async_trait::async_trait;
use erpcraft_core::{Error, Result, new_id};
use erpcraft_db::PgConnection;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::Kernel;
use crate::authz::{Actor, Target};

/// What a command handler works with. `conn` is the command's transaction.
pub struct Ctx<'a> {
    pub conn: &'a mut PgConnection,
    pub actor: &'a Actor,
    pub kernel: &'a Kernel,
    /// Name of the command being run.
    pub command: &'a str,
    changed: bool,
}

impl Ctx<'_> {
    /// Declare that this run changed nothing, for example because it
    /// repeated a command that had already taken effect. No outbox event is
    /// published for it, so subscribers see each change once.
    pub fn no_change(&mut self) {
        self.changed = false;
    }

    /// Fail unless the actor holds `permission` for `company_id`, whatever
    /// the record. Rules with a condition cannot be decided this way and
    /// count against the actor; use [`Ctx::require_on`] when there is a
    /// record.
    pub fn require(&self, permission: &str, company_id: Uuid) -> Result<()> {
        self.actor.require(permission, company_id)
    }

    /// Fail unless the actor holds `permission` for this record of
    /// `company_id`.
    pub fn require_on(&self, permission: &str, company_id: Uuid, record: &Value) -> Result<()> {
        self.actor
            .require_on(permission, Target::Company(company_id), record)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// `subject.action`, for example `journal_entry.post`.
    pub name: &'static str,
    pub description: &'static str,
    /// The actor must hold this for at least one company before the handler
    /// runs. The handler still checks it against the company of the records
    /// it touches. `None` when the permission depends on the input.
    pub permission: Option<&'static str>,
    /// A query: changes nothing, so it is not audited.
    pub read_only: bool,
    /// Input or output holds secrets: neither is written to the audit log,
    /// the outbox or the idempotency table.
    pub sensitive: bool,
}

impl CommandSpec {
    /// A command that changes state.
    pub const fn action(name: &'static str, description: &'static str) -> Self {
        CommandSpec {
            name,
            description,
            permission: None,
            read_only: false,
            sensitive: false,
        }
    }

    /// A command that only reads.
    pub const fn query(name: &'static str, description: &'static str) -> Self {
        CommandSpec {
            name,
            description,
            permission: None,
            read_only: true,
            sensitive: false,
        }
    }

    pub const fn permission(mut self, permission: &'static str) -> Self {
        self.permission = Some(permission);
        self
    }

    pub const fn sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandMeta {
    pub name: String,
    pub module: String,
    pub description: String,
    pub permission: Option<String>,
    pub read_only: bool,
    #[serde(skip)]
    pub sensitive: bool,
    /// JSON Schema of the input.
    pub input_schema: Value,
}

impl CommandMeta {
    /// The command's name as an MCP tool.
    pub fn tool_name(&self) -> String {
        self.name.replace('.', "_")
    }
}

/// A business operation with typed input and output.
#[async_trait]
pub trait Command: Send + Sync + 'static {
    type Input: DeserializeOwned + JsonSchema + Send + 'static;
    type Output: Serialize + Send + 'static;

    fn spec(&self) -> CommandSpec;

    async fn run(&self, ctx: &mut Ctx<'_>, input: Self::Input) -> Result<Self::Output>;
}

/// A [`Command`] with its types erased, as the registry stores it.
#[async_trait]
pub trait DynCommand: Send + Sync + 'static {
    fn meta(&self) -> CommandMeta;

    async fn run_json(&self, ctx: &mut Ctx<'_>, input: Value) -> Result<Value>;
}

struct Erased<C>(C);

#[async_trait]
impl<C: Command> DynCommand for Erased<C> {
    fn meta(&self) -> CommandMeta {
        let spec = self.0.spec();
        CommandMeta {
            name: spec.name.into(),
            module: String::new(),
            description: spec.description.into(),
            permission: spec.permission.map(Into::into),
            read_only: spec.read_only,
            sensitive: spec.sensitive,
            input_schema: serde_json::to_value(schemars::schema_for!(C::Input))
                .unwrap_or_else(|_| json!({"type": "object"})),
        }
    }

    async fn run_json(&self, ctx: &mut Ctx<'_>, input: Value) -> Result<Value> {
        let input: C::Input = serde_json::from_value(input)
            .map_err(|e| Error::validation(format!("invalid input: {e}")))?;
        let output = self.0.run(ctx, input).await?;
        serde_json::to_value(output).map_err(Error::internal)
    }
}

/// Wrap a command for [`Module::commands`](crate::Module::commands).
pub fn command(cmd: impl Command) -> Arc<dyn DynCommand> {
    Arc::new(Erased(cmd))
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Retrying with the same key and input returns the first result
    /// without running the command again.
    pub idempotency_key: Option<String>,
    /// Run the command, return what it would produce, then roll back.
    /// A preview reserves nothing: the real run validates again.
    pub preview: bool,
}

/// Run a command by name on behalf of `actor`.
pub async fn execute(
    kernel: &Kernel,
    actor: &Actor,
    name: &str,
    input: Value,
    options: Options,
) -> Result<Value> {
    let entry = kernel.registry().command(name)?;
    let meta = &entry.meta;
    if let Some(permission) = &meta.permission {
        actor.require_any(permission)?;
    }

    if meta.read_only {
        let mut conn = kernel.pool().acquire().await?;
        let mut ctx = Ctx {
            conn: &mut conn,
            actor,
            kernel,
            command: name,
            changed: false,
        };
        return entry.handler.run_json(&mut ctx, input).await;
    }

    let mut tx = kernel.pool().begin().await?;

    let key = options
        .idempotency_key
        .filter(|_| !options.preview && !meta.sensitive);
    if let Some(key) = &key {
        let hash = format!(
            "{:x}",
            Sha256::digest(format!("{name}\n{input}").as_bytes())
        );
        // A concurrent request with the same key blocks here until the
        // first one commits or rolls back.
        let inserted = sqlx::query(
            "INSERT INTO idempotency_key (user_id, key, command, input_hash) VALUES ($1, $2, $3, $4)
             ON CONFLICT (user_id, key) DO NOTHING",
        )
        .bind(actor.user_id)
        .bind(key)
        .bind(name)
        .bind(&hash)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            let (command, input_hash, output): (String, String, Option<Value>) = sqlx::query_as(
                "SELECT command, input_hash, output FROM idempotency_key WHERE user_id = $1 AND key = $2",
            )
            .bind(actor.user_id)
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
            if command != name || input_hash != hash {
                return Err(Error::conflict(
                    "this idempotency key was already used with a different command or input",
                ));
            }
            return output.ok_or_else(|| {
                Error::conflict("the first request with this key has not finished")
            });
        }
    }

    let (output, changed) = {
        let mut ctx = Ctx {
            conn: &mut tx,
            actor,
            kernel,
            command: name,
            changed: true,
        };
        let output = entry.handler.run_json(&mut ctx, input.clone()).await?;
        (output, ctx.changed)
    };

    if options.preview {
        tx.rollback().await?;
        return Ok(output);
    }

    let recorded_input = (!meta.sensitive).then_some(&input);
    sqlx::query("INSERT INTO audit_log (id, user_id, command, input) VALUES ($1, $2, $3, $4)")
        .bind(new_id())
        .bind(actor.user_id)
        .bind(name)
        .bind(recorded_input)
        .execute(&mut *tx)
        .await?;

    if changed {
        let payload = if meta.sensitive {
            json!({"command": name, "user_id": actor.user_id})
        } else {
            json!({"command": name, "user_id": actor.user_id, "input": input, "output": output})
        };
        sqlx::query("INSERT INTO outbox_event (id, topic, payload) VALUES ($1, $2, $3)")
            .bind(new_id())
            .bind(name)
            .bind(&payload)
            .execute(&mut *tx)
            .await?;
    }

    if let Some(key) = &key {
        sqlx::query("UPDATE idempotency_key SET output = $3 WHERE user_id = $1 AND key = $2")
            .bind(actor.user_id)
            .bind(key)
            .bind(&output)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(output)
}
