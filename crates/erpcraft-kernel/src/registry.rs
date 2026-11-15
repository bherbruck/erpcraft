//! What the installed modules declare. Built once at startup, then read-only.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use erpcraft_core::{Error, Result};
use erpcraft_db::{Migration, MigrationSet};
use erpcraft_rules::ColumnType;
use serde::Serialize;

use crate::authz::Authenticator;
use crate::command::{CommandMeta, DynCommand};
use crate::document::{DocumentHandler, Ledger};

/// A unit of functionality. In-tree and outside modules implement the same
/// trait and get nothing else from the kernel.
pub trait Module: Send + Sync + 'static {
    /// Unique lowercase identifier. It owns the module's migrations, so it
    /// must never change once released.
    fn name(&self) -> &'static str;

    /// Modules that must be registered before this one.
    fn depends_on(&self) -> Vec<&'static str> {
        vec![]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![]
    }

    fn ledgers(&self) -> Vec<Arc<dyn Ledger>> {
        vec![]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![]
    }

    /// How callers prove who they are. At most one registered module may
    /// supply this.
    fn authenticator(&self) -> Option<Arc<dyn Authenticator>> {
        None
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldType {
    Uuid,
    Text,
    Bool,
    Int,
    /// Exact decimal. Always a string in JSON.
    Decimal,
    Date,
    Timestamp,
    TextArray,
    /// Free-form JSON.
    Json,
    Enum {
        values: Vec<String>,
    },
    /// Reference to another entity through one of its fields.
    Link {
        entity: String,
        key: String,
    },
}

impl FieldType {
    pub fn link(entity: &str) -> Self {
        FieldType::Link {
            entity: entity.into(),
            key: "id".into(),
        }
    }

    pub fn link_by(entity: &str, key: &str) -> Self {
        FieldType::Link {
            entity: entity.into(),
            key: key.into(),
        }
    }

    pub fn options(values: &[&str]) -> Self {
        FieldType::Enum {
            values: values.iter().map(|v| v.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldMeta {
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub ty: FieldType,
    /// Must be present and non-null when the record is created.
    pub required: bool,
    /// Set by the system; callers can never write it.
    pub readonly: bool,
    /// Can be set when the record is created, never afterwards.
    pub immutable: bool,
}

impl FieldMeta {
    pub fn new(name: &str, label: &str, ty: FieldType) -> Self {
        FieldMeta {
            name: name.into(),
            label: label.into(),
            ty,
            required: false,
            readonly: false,
            immutable: false,
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn readonly(mut self) -> Self {
        self.readonly = true;
        self
    }

    pub fn immutable(mut self) -> Self {
        self.immutable = true;
        self
    }
}

/// A table exposed for generic listing, reading, creating and updating.
/// The table must have a `uuid` primary key named `id`.
#[derive(Debug, Clone, Serialize)]
pub struct EntityMeta {
    pub name: String,
    pub module: String,
    pub table: String,
    pub label: String,
    pub fields: Vec<FieldMeta>,
    pub order_by: Vec<String>,
    /// Never written through the generic layer.
    pub read_only: bool,
    /// The column holding the owning company, used to scope access. `None`
    /// for records shared by all companies.
    pub company_field: Option<String>,
    /// Checks the fields given to a create or an update, beyond what the
    /// field types and the table's constraints already enforce.
    #[serde(skip)]
    pub validator: Option<Validator>,
}

/// See [`EntityMeta::validator`]. Receives only the fields the caller sent.
pub type Validator = fn(&Registry, &serde_json::Map<String, serde_json::Value>) -> Result<()>;

impl EntityMeta {
    /// Starts with the `id` field already declared.
    pub fn new(name: &str, label: &str) -> Self {
        EntityMeta {
            name: name.into(),
            module: String::new(),
            table: name.into(),
            label: label.into(),
            fields: vec![FieldMeta::new("id", "ID", FieldType::Uuid).readonly()],
            order_by: vec!["id".into()],
            read_only: false,
            company_field: None,
            validator: None,
        }
    }

    pub fn field(mut self, field: FieldMeta) -> Self {
        self.fields.push(field);
        self
    }

    pub fn order_by(mut self, fields: &[&str]) -> Self {
        self.order_by = fields.iter().map(|f| f.to_string()).collect();
        self
    }

    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    pub fn company_field(mut self, field: &str) -> Self {
        self.company_field = Some(field.into());
        self
    }

    pub fn validator(mut self, validator: Validator) -> Self {
        self.validator = Some(validator);
        self
    }

    /// The fields a rule can refer to, with their database types.
    pub fn rule_columns(&self) -> BTreeMap<String, ColumnType> {
        self.fields
            .iter()
            .filter_map(|field| {
                let ty = match &field.ty {
                    FieldType::Uuid => ColumnType::Uuid,
                    FieldType::Text | FieldType::Enum { .. } => ColumnType::Text,
                    FieldType::Bool => ColumnType::Bool,
                    FieldType::Int => ColumnType::Int,
                    FieldType::Decimal => ColumnType::Decimal,
                    FieldType::Date => ColumnType::Date,
                    FieldType::Timestamp => ColumnType::Timestamp,
                    FieldType::TextArray => ColumnType::TextArray,
                    FieldType::Link { key, .. } if key == "id" => ColumnType::Uuid,
                    FieldType::Link { .. } => ColumnType::Text,
                    FieldType::Json => return None,
                };
                Some((field.name.clone(), ty))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StateMeta {
    pub name: String,
    pub label: String,
    /// The document's content cannot change while it is in this state.
    pub locked: bool,
}

impl StateMeta {
    pub fn editable(name: &str, label: &str) -> Self {
        StateMeta {
            name: name.into(),
            label: label.into(),
            locked: false,
        }
    }

    pub fn locked(name: &str, label: &str) -> Self {
        StateMeta {
            name: name.into(),
            label: label.into(),
            locked: true,
        }
    }
}

/// A repeating part of a document body, such as its lines.
#[derive(Debug, Clone, Serialize)]
pub struct CollectionMeta {
    pub name: String,
    pub label: String,
    pub fields: Vec<FieldMeta>,
}

/// A kind of business document and the workflow its module gives it.
#[derive(Debug, Clone, Serialize)]
pub struct DocTypeMeta {
    pub name: String,
    pub module: String,
    pub label: String,
    /// Numbers look like `PREFIX-2026-00001`.
    pub number_prefix: String,
    pub fields: Vec<FieldMeta>,
    pub collections: Vec<CollectionMeta>,
    pub states: Vec<StateMeta>,
    pub initial_state: String,
    /// Allowed `(from, to)` state changes.
    pub transitions: Vec<(String, String)>,
}

impl DocTypeMeta {
    pub fn state(&self, name: &str) -> Option<&StateMeta> {
        self.states.iter().find(|s| s.name == name)
    }
}

pub struct DocTypeEntry {
    pub meta: DocTypeMeta,
    pub handler: Arc<dyn DocumentHandler>,
}

pub struct CommandEntry {
    pub meta: CommandMeta,
    pub handler: Arc<dyn DynCommand>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleInfo {
    pub name: String,
    pub depends_on: Vec<String>,
}

/// Everything an interface needs to render or call the system.
#[derive(Debug, Clone, Serialize)]
pub struct Description {
    pub modules: Vec<ModuleInfo>,
    pub entities: Vec<EntityMeta>,
    pub doc_types: Vec<DocTypeMeta>,
    pub commands: Vec<CommandMeta>,
}

pub struct Registry {
    modules: Vec<ModuleInfo>,
    migrations: Vec<MigrationSet>,
    entities: BTreeMap<String, EntityMeta>,
    doc_types: BTreeMap<String, DocTypeEntry>,
    commands: BTreeMap<String, CommandEntry>,
    tools: BTreeMap<String, String>,
    ledgers: Vec<Arc<dyn Ledger>>,
    authenticator: Option<Arc<dyn Authenticator>>,
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn bad(msg: String) -> Error {
    Error::Internal(format!("invalid module registration: {msg}"))
}

impl Registry {
    pub(crate) fn build(modules: Vec<Box<dyn Module>>) -> Result<Registry> {
        let mut reg = Registry {
            modules: vec![],
            migrations: vec![],
            entities: BTreeMap::new(),
            doc_types: BTreeMap::new(),
            commands: BTreeMap::new(),
            tools: BTreeMap::new(),
            ledgers: vec![],
            authenticator: None,
        };
        let mut seen = BTreeSet::new();

        for module in modules {
            let name = module.name();
            if !is_ident(name) {
                return Err(bad(format!(
                    "module name '{name}' is not a lowercase identifier"
                )));
            }
            if !seen.insert(name) {
                return Err(bad(format!("module '{name}' is registered twice")));
            }
            for dep in module.depends_on() {
                if !seen.contains(dep) {
                    return Err(bad(format!(
                        "module '{name}' depends on '{dep}', which must be registered before it"
                    )));
                }
            }
            reg.modules.push(ModuleInfo {
                name: name.into(),
                depends_on: module.depends_on().iter().map(|d| d.to_string()).collect(),
            });
            reg.migrations.push(MigrationSet {
                module: name.into(),
                migrations: module.migrations(),
            });

            for mut entity in module.entities() {
                entity.module = name.into();
                check_entity(&entity)?;
                if reg.entities.contains_key(&entity.name) {
                    return Err(bad(format!("entity '{}' is declared twice", entity.name)));
                }
                reg.entities.insert(entity.name.clone(), entity);
            }

            for handler in module.doc_types() {
                let mut meta = handler.meta();
                meta.module = name.into();
                check_doc_type(&meta)?;
                if reg.doc_types.contains_key(&meta.name) {
                    return Err(bad(format!(
                        "document type '{}' is declared twice",
                        meta.name
                    )));
                }
                reg.doc_types
                    .insert(meta.name.clone(), DocTypeEntry { meta, handler });
            }

            for handler in module.commands() {
                let mut meta = handler.meta();
                meta.module = name.into();
                let valid = meta
                    .name
                    .split_once('.')
                    .is_some_and(|(a, b)| is_ident(a) && is_ident(b));
                if !valid {
                    return Err(bad(format!(
                        "command '{}' must be named 'subject.action'",
                        meta.name
                    )));
                }
                if reg.commands.contains_key(&meta.name)
                    || reg.tools.contains_key(&meta.tool_name())
                {
                    return Err(bad(format!("command '{}' is declared twice", meta.name)));
                }
                reg.tools.insert(meta.tool_name(), meta.name.clone());
                reg.commands
                    .insert(meta.name.clone(), CommandEntry { meta, handler });
            }

            reg.ledgers.extend(module.ledgers());
            if let Some(authenticator) = module.authenticator() {
                if reg.authenticator.is_some() {
                    return Err(bad(format!(
                        "module '{name}' supplies an authenticator, but one is already registered"
                    )));
                }
                reg.authenticator = Some(authenticator);
            }
        }
        Ok(reg)
    }

    pub fn migration_sets(&self) -> &[MigrationSet] {
        &self.migrations
    }

    pub fn entity(&self, name: &str) -> Result<&EntityMeta> {
        self.entities
            .get(name)
            .ok_or_else(|| Error::not_found(format!("entity '{name}'")))
    }

    pub fn doc_type(&self, name: &str) -> Result<&DocTypeEntry> {
        self.doc_types
            .get(name)
            .ok_or_else(|| Error::not_found(format!("document type '{name}'")))
    }

    pub fn command(&self, name: &str) -> Result<&CommandEntry> {
        self.commands
            .get(name)
            .ok_or_else(|| Error::not_found(format!("command '{name}'")))
    }

    /// Look a command up by the name it has as an MCP tool.
    pub fn command_by_tool(&self, tool: &str) -> Option<&CommandEntry> {
        self.tools
            .get(tool)
            .and_then(|name| self.commands.get(name))
    }

    /// The fields of a resource that permission rules can refer to: the
    /// fields of an entity, or the header of a document type. `None` when
    /// the name is neither.
    pub fn rule_columns(&self, resource: &str) -> Option<BTreeMap<String, ColumnType>> {
        if let Some(entity) = self.entities.get(resource) {
            return Some(entity.rule_columns());
        }
        self.doc_types
            .contains_key(resource)
            .then(crate::document::rule_columns)
    }

    pub fn commands(&self) -> impl Iterator<Item = &CommandEntry> {
        self.commands.values()
    }

    pub fn ledgers(&self) -> &[Arc<dyn Ledger>] {
        &self.ledgers
    }

    pub fn authenticator(&self) -> Option<&Arc<dyn Authenticator>> {
        self.authenticator.as_ref()
    }

    /// Describe the system, optionally limited to one module.
    pub fn describe(&self, module: Option<&str>) -> Description {
        let keep = |m: &str| module.is_none_or(|want| want == m);
        Description {
            modules: self
                .modules
                .iter()
                .filter(|m| keep(&m.name))
                .cloned()
                .collect(),
            entities: self
                .entities
                .values()
                .filter(|e| keep(&e.module))
                .cloned()
                .collect(),
            doc_types: self
                .doc_types
                .values()
                .filter(|d| keep(&d.meta.module))
                .map(|d| d.meta.clone())
                .collect(),
            commands: self
                .commands
                .values()
                .filter(|c| keep(&c.meta.module))
                .map(|c| c.meta.clone())
                .collect(),
        }
    }
}

fn check_entity(entity: &EntityMeta) -> Result<()> {
    let name = &entity.name;
    if !is_ident(name) || !is_ident(&entity.table) {
        return Err(bad(format!("entity '{name}' has an invalid name or table")));
    }
    for field in &entity.fields {
        if !is_ident(&field.name) {
            return Err(bad(format!(
                "entity '{name}' has an invalid field name '{}'",
                field.name
            )));
        }
    }
    let has = |f: &str| entity.fields.iter().any(|x| x.name == f);
    if !has("id") {
        return Err(bad(format!("entity '{name}' must declare an id field")));
    }
    for field in &entity.order_by {
        if !has(field) {
            return Err(bad(format!(
                "entity '{name}' orders by unknown field '{field}'"
            )));
        }
    }
    if let Some(cf) = &entity.company_field {
        let field = entity.fields.iter().find(|f| &f.name == cf);
        match field {
            None => {
                return Err(bad(format!(
                    "entity '{name}' has unknown company field '{cf}'"
                )));
            }
            // A record must never move between companies.
            Some(f) if cf != "id" && !f.immutable && !f.readonly => {
                return Err(bad(format!(
                    "entity '{name}': company field '{cf}' must be immutable"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_doc_type(meta: &DocTypeMeta) -> Result<()> {
    let name = &meta.name;
    if !is_ident(name) {
        return Err(bad(format!("document type '{name}' has an invalid name")));
    }
    if meta.state(&meta.initial_state).is_none() {
        return Err(bad(format!(
            "document type '{name}' has no state '{}'",
            meta.initial_state
        )));
    }
    for (from, to) in &meta.transitions {
        if meta.state(from).is_none() || meta.state(to).is_none() {
            return Err(bad(format!(
                "document type '{name}' has a transition between unknown states '{from}' and '{to}'"
            )));
        }
    }
    Ok(())
}
