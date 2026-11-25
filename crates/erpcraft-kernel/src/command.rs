//! Commands: the only way business state changes or is queried.
//!
//! HTTP, MCP and the UI all call [`execute`]. It wraps the command's handler
//! in one database transaction and adds what every command needs:
//! authorization, idempotency, the audit record and the outbox event.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use erpcraft_core::{Error, Result, new_id};
use erpcraft_db::{PgConnection, PgPool};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::Kernel;
use crate::authz::{Actor, Target};
use crate::registry::CommandEntry;

/// What a command handler works with. `conn` is the command's transaction.
pub struct Ctx<'a> {
    pub conn: &'a mut PgConnection,
    pub actor: &'a Actor,
    pub kernel: &'a Kernel,
    /// Name of the command being run.
    pub command: &'a str,
    changed: bool,
    touched: Subjects,
    /// The active policy rules of a company, read once per command: a
    /// posting asks several points, and each asked them anew before.
    pub(crate) policy_rules: crate::policy::RuleCache,
    /// Bodies of documents loaded in this command, by id and version, so
    /// that the view at the end does not load again what the summary just
    /// read. See [`crate::document::refresh_summary`].
    pub(crate) bodies: std::collections::HashMap<(Uuid, i32), Value>,
    /// The caller asked for a document's header and summary only.
    pub(crate) view: View,
    /// What an approval rule needs, or what was approved for this run.
    /// See [`crate::approval`].
    pub(crate) approval: crate::approval::Slot,
}

impl<'a> Ctx<'a> {
    /// A context for work the server does on its own, outside any command
    /// a caller ran: nothing is audited or published for it.
    pub(crate) fn internal(
        conn: &'a mut PgConnection,
        actor: &'a Actor,
        kernel: &'a Kernel,
        command: &'a str,
    ) -> Self {
        Ctx {
            conn,
            actor,
            kernel,
            command,
            changed: false,
            touched: Subjects::new(),
            policy_rules: Default::default(),
            bodies: Default::default(),
            view: View::Full,
            approval: Default::default(),
        }
    }

    /// The records this context touched, taken out of it.
    pub(crate) fn take_touched(&mut self) -> Subjects {
        std::mem::take(&mut self.touched)
    }
}

/// The records a command touched, as `(resource, id)`.
pub(crate) type Subjects = BTreeSet<(String, Uuid)>;

impl Ctx<'_> {
    /// Declare that this run changed nothing, for example because it
    /// repeated a command that had already taken effect. No outbox event is
    /// published for it, so subscribers see each change once.
    pub fn no_change(&mut self) {
        self.changed = false;
    }

    /// Note that this command creates, changes or acts on a record, so the
    /// command appears in that record's history. `resource` is an entity
    /// name, a document type, or `user`. The kernel's own functions for
    /// entities and documents already do this.
    pub fn touch(&mut self, resource: &str, id: Uuid) {
        self.touched.insert((resource.to_owned(), id));
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
    /// See [`CommandSpec::redact`].
    pub redact: &'static [&'static str],
    /// See [`CommandSpec::max_input_bytes`].
    pub max_input_bytes: Option<usize>,
    /// See [`CommandSpec::label`].
    pub label: Option<&'static str>,
    /// See [`CommandSpec::resource`].
    pub resource: Option<&'static str>,
    /// See [`CommandSpec::target`].
    pub target: Option<bool>,
    /// See [`CommandSpec::from_states`].
    pub from_states: &'static [&'static str],
    /// See [`CommandSpec::destructive`].
    pub destructive: bool,
    /// See [`CommandSpec::primary_when`].
    pub primary_when: Option<PrimaryWhenSpec>,
    /// See [`CommandSpec::report`].
    pub report: Option<&'static str>,
    /// See [`CommandSpec::creates`].
    pub creates: Option<&'static str>,
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
            redact: &[],
            max_input_bytes: None,
            label: None,
            resource: None,
            target: None,
            from_states: &[],
            destructive: false,
            primary_when: None,
            report: None,
            creates: None,
        }
    }

    /// A command that only reads.
    pub const fn query(name: &'static str, description: &'static str) -> Self {
        let mut spec = CommandSpec::action(name, description);
        spec.read_only = true;
        spec
    }

    pub const fn permission(mut self, permission: &'static str) -> Self {
        self.permission = Some(permission);
        self
    }

    pub const fn sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }

    /// Top-level input fields too large to keep, such as a file's content:
    /// the audit log and the outbox hold `{sha256, bytes}` of each instead
    /// of its value; for a field named `*_base64`, of the bytes it decodes
    /// to. Unlike [`CommandSpec::sensitive`], the rest of the
    /// input is kept, and the idempotency key still compares the whole
    /// input, so different contents are different requests.
    pub const fn redact(mut self, fields: &'static [&'static str]) -> Self {
        self.redact = fields;
        self
    }

    /// The largest request body the command takes, in bytes, when that is
    /// more than [`DEFAULT_MAX_INPUT_BYTES`]: a command that takes a file.
    /// The command still checks the size of what it was given.
    pub const fn max_input_bytes(mut self, bytes: usize) -> Self {
        self.max_input_bytes = Some(bytes);
        self
    }

    /// What a button or a menu entry for the command says. Without it the
    /// label is made from the action in the name: "Post" for
    /// `journal_entry.post`, "Close short" for `purchase_order.close_short`.
    pub const fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    /// The entity or document type the command is about, when that is not
    /// the part of its name before the dot.
    pub const fn resource(mut self, resource: &'static str) -> Self {
        self.resource = Some(resource);
        self
    }

    /// Whether the command acts on one record of its resource, named by
    /// the input `id`, so that an interface can offer it on that record.
    /// Without this, it does if its resource is an entity or a document
    /// type and its input has an `id`.
    pub const fn target(mut self, target: bool) -> Self {
        self.target = Some(target);
        self
    }

    /// For a command on a document, or on an entity with a state field
    /// (see [`crate::EntityMeta::state_field`]): the states it applies
    /// from. In any other state it is refused, or returns the record
    /// unchanged, so an interface need not offer it.
    pub const fn from_states(mut self, states: &'static [&'static str]) -> Self {
        self.from_states = states;
        self
    }

    /// The command takes something back or away: it reverses, cancels or
    /// deletes. An interface should make that plain and ask first.
    pub const fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// When the command is the one an interface should put first on a
    /// record: in one of these states, and with a collection of the
    /// document empty or not. See [`PrimaryWhenSpec`].
    pub const fn primary_when(mut self, when: PrimaryWhenSpec) -> Self {
        self.primary_when = Some(when);
        self
    }

    /// The command creates a document of this type and returns it: the
    /// receipt that follows from an order, for example. An interface goes
    /// to the new document instead of staying on the one it started from.
    pub const fn creates(mut self, doc_type: &'static str) -> Self {
        self.creates = Some(doc_type);
        self
    }

    /// The command is a report: a query whose result is worth a page of
    /// its own, under this title.
    pub const fn report(mut self, title: &'static str) -> Self {
        self.report = Some(title);
        self
    }
}

/// The largest request body a command takes unless it declares more
/// ([`CommandSpec::max_input_bytes`]).
pub const DEFAULT_MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;

/// See [`CommandSpec::primary_when`]: the command is the primary action
/// on a record in one of `states` (any state when empty), when the
/// collection `empty` names has no rows and the one `not_empty` names has
/// some. An interface takes the first command of a record that matches,
/// else its first command that is not destructive.
#[derive(Debug, Clone, Copy)]
pub struct PrimaryWhenSpec {
    pub states: &'static [&'static str],
    pub empty: Option<&'static str>,
    pub not_empty: Option<&'static str>,
}

impl PrimaryWhenSpec {
    pub const fn states(states: &'static [&'static str]) -> Self {
        PrimaryWhenSpec {
            states,
            empty: None,
            not_empty: None,
        }
    }

    pub const fn empty(mut self, collection: &'static str) -> Self {
        self.empty = Some(collection);
        self
    }

    pub const fn not_empty(mut self, collection: &'static str) -> Self {
        self.not_empty = Some(collection);
        self
    }
}

/// See [`CommandSpec::primary_when`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PrimaryWhen {
    /// The states of the record it is primary in; any state when empty.
    pub states: Vec<String>,
    /// A collection of the document that must have no rows.
    pub empty: Option<String>,
    /// A collection of the document that must have rows.
    pub not_empty: Option<String>,
}

/// What kind of thing a command's resource is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Entity,
    Document,
}

/// See [`CommandSpec::report`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReportMeta {
    pub title: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CommandMeta {
    pub name: String,
    pub module: String,
    /// What a button for the command says.
    pub label: String,
    pub description: String,
    /// What the command is about: by default the part of the name before
    /// the dot.
    pub resource: String,
    /// Whether `resource` is an entity or a document type. `None` when it
    /// is neither, as for `gl` in `gl.trial_balance`.
    pub resource_kind: Option<ResourceKind>,
    /// The part of the name after the dot.
    pub action: String,
    /// The command acts on one record of `resource`, named by the input
    /// `id`.
    pub target: bool,
    /// For a command on a document, or on an entity with a state field:
    /// the states it applies from, values of the document's state or of
    /// the entity's `state_field`. Empty when the command does not depend
    /// on a state.
    pub from_states: Vec<String>,
    /// The command reverses, cancels or deletes.
    pub destructive: bool,
    /// When an interface should make the command a record's primary
    /// action: the first command whose condition matches. Without one
    /// matching, the first command that is not destructive.
    pub primary_when: Option<PrimaryWhen>,
    /// Set when the command is a report to show as a page.
    pub report: Option<ReportMeta>,
    /// Set when the command creates a document of this type and returns
    /// it, as the commands do that draft the next document from one that
    /// exists.
    pub creates: Option<String>,
    pub permission: Option<String>,
    pub read_only: bool,
    #[serde(skip)]
    pub sensitive: bool,
    /// See [`CommandSpec::redact`].
    #[serde(skip)]
    pub redact: &'static [&'static str],
    /// The largest request body the command takes, in bytes.
    pub max_input_bytes: usize,
    /// JSON Schema of the input.
    pub input_schema: Value,
    /// JSON Schema of the output.
    pub output_schema: Value,
    /// What the declaration said about `target`, until the registry
    /// settles it.
    #[serde(skip)]
    pub(crate) declared_target: Option<bool>,
}

impl CommandMeta {
    /// The command's name as an MCP tool.
    pub fn tool_name(&self) -> String {
        self.name.replace('.', "_")
    }
}

/// Give every property of an input a title, unless its type states one:
/// "Company" for `company_id`, "Doc date" for `doc_date`. A form can then
/// label every input; the description, where there is one, says more.
fn titled(mut schema: Value) -> Value {
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, property) in properties {
            let Some(property) = property.as_object_mut() else {
                continue;
            };
            let title = match name.as_str() {
                "id" => "ID".to_owned(),
                other => crate::registry::humanize(other.strip_suffix("_id").unwrap_or(other)),
            };
            property.entry("title").or_insert(Value::String(title));
        }
    }
    schema
}

/// [`titled`] for every object anywhere in a schema, its definitions
/// included: the columns of a report's rows get a heading each.
pub(crate) fn titled_throughout(schema: Value) -> Value {
    let mut schema = titled(schema);
    match &mut schema {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                // Property names are not schemas; their schemas are.
                if key == "properties" {
                    if let Value::Object(properties) = value {
                        for property in properties.values_mut() {
                            *property = titled_throughout(std::mem::take(property));
                        }
                    }
                } else {
                    *value = titled_throughout(std::mem::take(value));
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                *item = titled_throughout(std::mem::take(item));
            }
        }
        _ => {}
    }
    schema
}

/// A business operation with typed input and output.
#[async_trait]
pub trait Command: Send + Sync + 'static {
    type Input: DeserializeOwned + JsonSchema + Send + 'static;
    type Output: Serialize + JsonSchema + Send + 'static;

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
        let (subject, action) = spec.name.split_once('.').unwrap_or((spec.name, ""));
        let schema = |schema: schemars::Schema| {
            serde_json::to_value(schema).unwrap_or_else(|_| json!({"type": "object"}))
        };
        CommandMeta {
            name: spec.name.into(),
            module: String::new(),
            label: spec
                .label
                .map_or_else(|| crate::registry::humanize(action), Into::into),
            description: spec.description.into(),
            resource: spec.resource.unwrap_or(subject).into(),
            resource_kind: None,
            action: action.into(),
            target: false,
            from_states: spec.from_states.iter().map(|s| (*s).into()).collect(),
            destructive: spec.destructive,
            primary_when: spec.primary_when.map(|w| PrimaryWhen {
                states: w.states.iter().map(|s| (*s).into()).collect(),
                empty: w.empty.map(Into::into),
                not_empty: w.not_empty.map(Into::into),
            }),
            report: spec.report.map(|title| ReportMeta {
                title: title.into(),
            }),
            creates: spec.creates.map(Into::into),
            permission: spec.permission.map(Into::into),
            read_only: spec.read_only,
            sensitive: spec.sensitive,
            redact: spec.redact,
            max_input_bytes: spec
                .max_input_bytes
                .unwrap_or(DEFAULT_MAX_INPUT_BYTES)
                .max(DEFAULT_MAX_INPUT_BYTES),
            input_schema: titled(schema(schemars::schema_for!(C::Input))),
            // As the output is written, not as it would be read: a decimal
            // is always a string on the way out.
            output_schema: {
                let output = schema(
                    schemars::generate::SchemaSettings::default()
                        .for_serialize()
                        .into_generator()
                        .into_root_schema_for::<C::Output>(),
                );
                // A report is shown as a table: every column has a heading.
                if spec.report.is_some() {
                    titled_throughout(output)
                } else {
                    output
                }
            },
            declared_target: spec.target,
        }
    }

    async fn run_json(&self, ctx: &mut Ctx<'_>, input: Value) -> Result<Value> {
        let input: C::Input = crate::input::parse(input, "input")?;
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
    /// How much of a document a command that returns one returns.
    pub view: View,
}

/// How much of a document a command returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum View {
    /// Header, summary, body, postings with their entries, and links.
    #[default]
    Full,
    /// Header and summary only: the body is null, and postings and links
    /// are empty. For a client that only needs to know what happened,
    /// such as an importer; it saves the queries that read the rest.
    Header,
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

    if meta.read_only {
        if let Some(permission) = &meta.permission {
            actor.require_any(permission)?;
        }
        let mut conn = kernel.pool().acquire().await?;
        let mut ctx = Ctx {
            conn: &mut conn,
            actor,
            kernel,
            command: name,
            changed: false,
            touched: Subjects::new(),
            policy_rules: Default::default(),
            bodies: Default::default(),
            view: options.view,
            approval: Default::default(),
        };
        return entry.handler.run_json(&mut ctx, input).await;
    }

    let preview = options.preview;
    let mut touched = Subjects::new();
    let result = run_action(kernel, actor, entry, &input, options, &mut touched).await;
    // A refused or failed attempt to change something is part of the
    // record. A preview attempts nothing.
    if let (Err(error), false) = (&result, preview) {
        let recorded_input = recorded(meta, &input);
        let message = if meta.sensitive {
            unrevealing(error).to_owned()
        } else {
            error.to_string()
        };
        if let Err(audit_error) = record_failure(
            kernel.pool(),
            actor.user_id,
            name,
            recorded_input.as_ref(),
            error.code(),
            &message,
            &touched,
        )
        .await
        {
            tracing::error!(%audit_error, command = name, "could not audit a failed command");
        }
    }
    result
}

/// What the audit log says about the failure of a command that handles
/// secrets. The rule: nothing that was made from the input is recorded.
/// An error's message is written by the handler or by the input parser and
/// may repeat what was sent, a secret pasted into the wrong field for one,
/// and no kind of error is exempt, because any of them can quote a value.
/// So only the kind of error is kept, as a fixed sentence. The caller
/// still gets the real message in the response.
fn unrevealing(error: &Error) -> &'static str {
    match error {
        Error::Validation(_) => "the input was refused",
        Error::Conflict(_) => "the request conflicted with the state of a record",
        Error::NotFound(_) => "a record was not found",
        Error::Forbidden(_) => "the request was not allowed",
        Error::Unauthorized => "not authenticated",
        Error::Internal(_) => "internal error",
    }
}

/// The input as the audit log and the outbox keep it: nothing for a
/// sensitive command, and `{sha256, bytes}` in place of each field it
/// redacts (see [`CommandSpec::redact`]).
pub(crate) fn recorded(meta: &CommandMeta, input: &Value) -> Option<Value> {
    if meta.sensitive {
        return None;
    }
    let Value::Object(map) = input else {
        return Some(input.clone());
    };
    let kept = map.iter().map(|(key, value)| {
        let value = match value {
            Value::Null => Value::Null,
            _ if meta.redact.contains(&key.as_str()) => redacted(key, value),
            _ => value.clone(),
        };
        (key.clone(), value)
    });
    Some(Value::Object(kept.collect()))
}

/// `{sha256, bytes}` of a redacted field: of the text, or of the bytes a
/// field named `*_base64` decodes to, so that the hash is the file's
/// whichever way it was sent. Base64 that does not decode is hashed as
/// text.
fn redacted(key: &str, value: &Value) -> Value {
    use base64::Engine as _;
    let text = match value {
        Value::String(s) => std::borrow::Cow::Borrowed(s.as_str()),
        other => std::borrow::Cow::Owned(other.to_string()),
    };
    let decoded = key
        .ends_with("_base64")
        .then(|| {
            base64::engine::general_purpose::STANDARD
                .decode(text.trim())
                .ok()
        })
        .flatten();
    let bytes = decoded.as_deref().unwrap_or(text.as_bytes());
    json!({
        "sha256": format!("{:x}", Sha256::digest(bytes)),
        "bytes": bytes.len(),
    })
}

/// Write the audit record of a failed command. The command's transaction
/// is gone, so this is a transaction of its own.
async fn record_failure(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    input: Option<&Value>,
    error_code: &str,
    error_message: &str,
    touched: &Subjects,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let audit_id = new_id();
    sqlx::query(
        "INSERT INTO audit_log (id, user_id, command, input, outcome, error_code, error_message)
         VALUES ($1, $2, $3, $4, 'error', $5, $6)",
    )
    .bind(audit_id)
    .bind(user_id)
    .bind(name)
    .bind(input)
    .bind(error_code)
    .bind(error_message)
    .execute(&mut *tx)
    .await?;
    record_subjects(&mut tx, audit_id, touched).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn record_subjects(
    conn: &mut PgConnection,
    audit_id: Uuid,
    touched: &Subjects,
) -> Result<()> {
    if touched.is_empty() {
        return Ok(());
    }
    let (resources, ids): (Vec<&str>, Vec<Uuid>) = touched
        .iter()
        .map(|(resource, id)| (resource.as_str(), *id))
        .unzip();
    sqlx::query(
        "INSERT INTO audit_subject (audit_id, resource, record_id)
         SELECT $1, * FROM unnest($2::text[], $3::uuid[])",
    )
    .bind(audit_id)
    .bind(resources)
    .bind(ids)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// `value` with the keys of every object in sorted order. JSON objects keep
/// the order their keys came in, so text hashed for comparison is written
/// from this.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

async fn run_action(
    kernel: &Kernel,
    actor: &Actor,
    entry: &CommandEntry,
    input: &Value,
    options: Options,
    touched: &mut Subjects,
) -> Result<Value> {
    let meta = &entry.meta;
    let name = meta.name.as_str();
    if let Some(permission) = &meta.permission {
        actor.require_any(permission)?;
    }

    let mut tx = kernel.pool().begin().await?;

    let key = options
        .idempotency_key
        .filter(|_| !options.preview && !meta.sensitive);
    if let Some(key) = &key {
        // The same input with its keys in another order is the same input.
        let input = sorted(input);
        let hash = format!(
            "{:x}",
            Sha256::digest(
                match options.view {
                    // A replay returns what was stored, so the view is part of
                    // what must be the same.
                    View::Full => format!("{name}\n{input}"),
                    View::Header => format!("{name}\nheader\n{input}"),
                }
                .as_bytes()
            )
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

    // Tried to the end to learn every point that asks for approval.
    let slot = crate::approval::Slot::collecting();
    let (output, changed) = {
        let mut ctx = Ctx {
            conn: &mut tx,
            actor,
            kernel,
            command: name,
            changed: true,
            touched: Subjects::new(),
            policy_rules: Default::default(),
            bodies: Default::default(),
            view: options.view,
            approval: slot.clone(),
        };
        let outcome = entry.handler.run_json(&mut ctx, input.clone()).await;
        *touched = std::mem::take(&mut ctx.touched);
        // Policy rules sent the command to approval: nothing it did
        // stays, and it is kept as a request instead, for every point that
        // asked. A command that fails anyway is told so, not sent.
        let needs = std::mem::take(&mut slot.lock().needed);
        if !needs.is_empty() {
            tx.rollback().await?;
            outcome?;
            // Its input would be kept on the request and in the audit log
            // until it runs: a command that handles secrets is not kept.
            if meta.sensitive {
                return Err(Error::state(format!(
                    "{name} handles secrets and is not kept waiting for approval; the policy \
                     rule that asks for approval at {} cannot apply to it",
                    needs[0].point
                )));
            }
            return crate::approval::request(
                kernel,
                actor,
                name,
                input,
                key.as_deref(),
                needs,
                options.preview,
            )
            .await;
        }
        (outcome?, ctx.changed)
    };

    if options.preview {
        tx.rollback().await?;
        return Ok(output);
    }

    let recorded_input = recorded(meta, input);
    let audit_id = new_id();
    sqlx::query("INSERT INTO audit_log (id, user_id, command, input) VALUES ($1, $2, $3, $4)")
        .bind(audit_id)
        .bind(actor.user_id)
        .bind(name)
        .bind(&recorded_input)
        .execute(&mut *tx)
        .await?;
    record_subjects(&mut tx, audit_id, touched).await?;

    if changed {
        let payload = match &recorded_input {
            None => json!({"command": name, "user_id": actor.user_id}),
            Some(input) => {
                json!({"command": name, "user_id": actor.user_id, "input": input, "output": output})
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_hash_alike_whatever_the_order_of_their_keys() {
        let a: Value =
            serde_json::from_str(r#"{"b": 1, "a": {"y": [{"q": 1, "p": 2}], "x": 2}}"#).unwrap();
        let b: Value =
            serde_json::from_str(r#"{"a": {"x": 2, "y": [{"p": 2, "q": 1}]}, "b": 1}"#).unwrap();
        assert_eq!(sorted(&a).to_string(), sorted(&b).to_string());
        assert_eq!(
            sorted(&a).to_string(),
            r#"{"a":{"x":2,"y":[{"p":2,"q":1}]},"b":1}"#
        );
    }
}
