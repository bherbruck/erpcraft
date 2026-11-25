//! What the installed modules declare. Built once at startup, then read-only.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use erpcraft_core::{Error, Result};
use erpcraft_db::{Migration, MigrationSet};
use erpcraft_rules::ColumnType;
use schemars::JsonSchema;
use serde::Serialize;

use crate::authz::Authenticator;
use crate::command::{CommandMeta, DynCommand, ResourceKind};
use crate::document::{DocumentHandler, Ledger};
use crate::outbox::Subscriber;
use crate::policy::PolicyPoint;

/// A unit of functionality. In-tree and outside modules implement the same
/// trait and get nothing else from the kernel.
pub trait Module: Send + Sync + 'static {
    /// Unique lowercase identifier. It owns the module's migrations, so it
    /// must never change once released.
    fn name(&self) -> &'static str;

    /// What the module is called in a menu. The name with a capital and
    /// spaces unless the module says otherwise.
    fn label(&self) -> String {
        humanize(self.name())
    }

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

    /// The business judgements this module makes that a company can
    /// configure. See [`PolicyPoint`].
    fn policies(&self) -> Vec<PolicyPoint> {
        vec![]
    }

    /// What reacts to committed changes. See [`Subscriber`].
    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![]
    }

    /// How callers prove who they are. At most one registered module may
    /// supply this.
    fn authenticator(&self) -> Option<Arc<dyn Authenticator>> {
        None
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
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
        /// What to show for each value. Every value has one.
        labels: BTreeMap<String, String>,
    },
    /// Reference to another entity through one of its fields.
    Link {
        entity: String,
        key: String,
        /// Only records with these values can be chosen here: field name,
        /// then the value as `entity.list` takes it in `filters`
        /// (`{"is_bank": "true"}`). Absent when any record will do.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        filters: BTreeMap<String, String>,
    },
    /// Reference to a record of the entity that another field of the same
    /// record names, by id: the party of a debt, the scope of a rule.
    EntityLink {
        /// The field that holds the entity's name.
        entity_field: String,
    },
    /// Reference to a document of one type, by id. It is shown by its
    /// number and found through `document.list`.
    DocumentLink {
        doc_type: String,
        /// Only documents in these states can be chosen. Empty: any state.
        states: Vec<String>,
    },
    /// Reference to a line of another document, by the line's id.
    LineLink {
        /// The field of this document's body that holds the document the
        /// line belongs to. It is a [`FieldType::DocumentLink`].
        document_field: String,
        /// The type of that document.
        doc_type: String,
        /// The collection of that document the line is in.
        collection: String,
    },
    /// Reference to a row that a query command lists, by one of the row's
    /// fields: something that is neither an entity nor a document, such as
    /// an open item. See [`Lookup`].
    Lookup(Lookup),
    /// Reference to a posting, by id. A posting is shown with its document.
    Posting,
}

impl FieldType {
    /// What the database stores the field as, for conditions over it.
    /// `None` for free-form JSON, which a condition cannot name.
    pub fn column_type(&self) -> Option<ColumnType> {
        Some(match self {
            FieldType::Uuid => ColumnType::Uuid,
            FieldType::Text | FieldType::Enum { .. } => ColumnType::Text,
            FieldType::Bool => ColumnType::Bool,
            FieldType::Int => ColumnType::Int,
            FieldType::Decimal => ColumnType::Decimal,
            FieldType::Date => ColumnType::Date,
            FieldType::Timestamp => ColumnType::Timestamp,
            FieldType::TextArray => ColumnType::TextArray,
            FieldType::EntityLink { .. }
            | FieldType::DocumentLink { .. }
            | FieldType::LineLink { .. }
            | FieldType::Lookup(_)
            | FieldType::Posting => ColumnType::Uuid,
            FieldType::Link { key, .. } if key == "id" => ColumnType::Uuid,
            FieldType::Link { .. } => ColumnType::Text,
            FieldType::Json => return None,
        })
    }
}

/// How to find what a [`FieldType::Lookup`] field refers to.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Lookup {
    /// The query command that lists the rows.
    pub command: String,
    /// The key of the command's output that holds the rows.
    pub rows: String,
    /// The field of a row whose value the referring field holds.
    pub key: String,
    /// The fields of a row that tell a person which one it is, in order.
    pub display: Vec<String>,
    /// Inputs to give the command so that it lists only what can be
    /// chosen here, by input name.
    pub params: BTreeMap<String, LookupParam>,
    /// The input of the command that takes a list of keys and lists the
    /// rows with those keys, whatever state they are in now: how to show a
    /// value that is already chosen, such as the open item a posted
    /// payment settled, which a plain list leaves out. Give it with the
    /// `params`. `None` when the command cannot do that.
    pub by_keys: Option<String>,
}

/// Where the value of one input of a lookup's command comes from.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum LookupParam {
    /// Always this value.
    Value { value: serde_json::Value },
    /// The value of a field of the document header or body, such as
    /// `company_id` or `direction`. With `map`, the field's value is
    /// looked up in it first: `outgoing` to `payable`.
    Field {
        field: String,
        map: Option<BTreeMap<String, String>>,
    },
}

impl Lookup {
    /// Rows of `command`'s output key `rows`, referred to by their `key`.
    pub fn new(command: &str, rows: &str, key: &str) -> Self {
        Lookup {
            command: command.into(),
            rows: rows.into(),
            key: key.into(),
            display: vec![],
            params: BTreeMap::new(),
            by_keys: None,
        }
    }

    /// The command's input `param` takes a list of keys and lists exactly
    /// those rows. See [`Lookup::by_keys`].
    pub fn by_keys(mut self, param: &str) -> Self {
        self.by_keys = Some(param.into());
        self
    }

    pub fn display(mut self, fields: &[&str]) -> Self {
        self.display = fields.iter().map(|f| f.to_string()).collect();
        self
    }

    /// Give the command's input `param` this value.
    pub fn value(mut self, param: &str, value: impl Into<serde_json::Value>) -> Self {
        let value = value.into();
        self.params
            .insert(param.into(), LookupParam::Value { value });
        self
    }

    /// Give the command's input `param` the value of `field`.
    pub fn field(mut self, param: &str, field: &str) -> Self {
        self.params.insert(
            param.into(),
            LookupParam::Field {
                field: field.into(),
                map: None,
            },
        );
        self
    }

    /// Give the command's input `param` what `map` says for the value of
    /// `field`, as pairs of the field's value and the input's.
    pub fn mapped(mut self, param: &str, field: &str, map: &[(&str, &str)]) -> Self {
        self.params.insert(
            param.into(),
            LookupParam::Field {
                field: field.into(),
                map: Some(
                    map.iter()
                        .map(|(from, to)| (from.to_string(), to.to_string()))
                        .collect(),
                ),
            },
        );
        self
    }
}

impl FieldType {
    pub fn link(entity: &str) -> Self {
        FieldType::Link {
            entity: entity.into(),
            key: "id".into(),
            filters: BTreeMap::new(),
        }
    }

    pub fn link_by(entity: &str, key: &str) -> Self {
        FieldType::Link {
            entity: entity.into(),
            key: key.into(),
            filters: BTreeMap::new(),
        }
    }

    /// A link offers only records whose `field` has this value, as an
    /// `entity.list` filter: `link("account").filter("is_bank", "true")`.
    /// It tells an interface what to offer; the command that uses the
    /// value still checks what it must. No effect on other field types.
    pub fn filter(mut self, field: &str, value: &str) -> Self {
        if let FieldType::Link { filters, .. } = &mut self {
            filters.insert(field.into(), value.into());
        }
        self
    }

    /// A record of the entity named in `entity_field` of the same record.
    pub fn entity_named_by(entity_field: &str) -> Self {
        FieldType::EntityLink {
            entity_field: entity_field.into(),
        }
    }

    /// A document of this type, in any state.
    pub fn document(doc_type: &str) -> Self {
        FieldType::document_in(doc_type, &[])
    }

    /// A document of this type in one of these states.
    pub fn document_in(doc_type: &str, states: &[&str]) -> Self {
        FieldType::DocumentLink {
            doc_type: doc_type.into(),
            states: states.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A line in `collection` of the document, of type `doc_type`, that
    /// the body field `document_field` refers to.
    pub fn line_of(document_field: &str, doc_type: &str, collection: &str) -> Self {
        FieldType::LineLink {
            document_field: document_field.into(),
            doc_type: doc_type.into(),
            collection: collection.into(),
        }
    }

    /// Whether the field holds a reference to something else.
    pub fn is_reference(&self) -> bool {
        matches!(
            self,
            FieldType::Link { .. }
                | FieldType::EntityLink { .. }
                | FieldType::DocumentLink { .. }
                | FieldType::LineLink { .. }
                | FieldType::Lookup(_)
                | FieldType::Posting
        )
    }

    /// One of these values, each shown as itself with a capital and
    /// spaces: "Not started" for `not_started`.
    pub fn options(values: &[&str]) -> Self {
        FieldType::Enum {
            values: values.iter().map(|v| v.to_string()).collect(),
            labels: values
                .iter()
                .map(|v| (v.to_string(), humanize(v)))
                .collect(),
        }
    }

    /// One of these values, as pairs of the value and what to show for it.
    pub fn labelled(options: &[(&str, &str)]) -> Self {
        FieldType::Enum {
            values: options.iter().map(|(v, _)| v.to_string()).collect(),
            labels: options
                .iter()
                .map(|(v, label)| (v.to_string(), label.to_string()))
                .collect(),
        }
    }
}

/// "Not started" for `not_started`.
pub fn humanize(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => words,
    }
}

/// "Companies" for "Company", "Stock entries" for "Stock entry". A label
/// that already ends in "s" is taken to be a plural.
fn pluralize(label: &str) -> String {
    let lower = label.to_lowercase();
    if lower.ends_with('s') {
        return label.to_owned();
    }
    let consonant_y = lower.ends_with('y')
        && !["ay", "ey", "oy", "uy"]
            .iter()
            .any(|end| lower.ends_with(end));
    if consonant_y {
        return format!("{}ies", &label[..label.len() - 1]);
    }
    if ["x", "ch", "sh"].iter().any(|end| lower.ends_with(end)) {
        return format!("{label}es");
    }
    format!("{label}s")
}

/// What a decimal number is, so that an interface can format and align it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecimalRole {
    /// An amount of money.
    Money,
    /// A quantity of something.
    Quantity,
    /// A price or a cost of one unit: money, with more decimal places.
    UnitPrice,
    /// A factor, such as an exchange rate.
    Rate,
    /// A share of a hundred: 19 means 19 %.
    Percent,
}

/// Where a currency or a unit of measure is to be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum ValueSource {
    /// The accounting currency of the record's or the document's company.
    Company,
    /// Another field beside this one: of the same record, the same line,
    /// or else of the document's body.
    Field { field: String },
    /// A field of the record that a link field beside this one refers to:
    /// the `uom` of the item in `item_id`.
    Linked { field: String, key: String },
    /// A field of the document's header, for a field of one of its lines:
    /// the `currency` of the order, for the `unit_price` of its lines.
    Document { field: String },
}

/// See [`DecimalRole`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DecimalMeta {
    pub role: DecimalRole,
    /// The most decimal places the value can have. For money this is what
    /// is stored; the currency says how many to show.
    pub scale: Option<u32>,
    /// For money and unit prices: which currency the amount is in.
    pub currency: Option<ValueSource>,
    /// For quantities: where the unit of measure is, when it is known.
    pub unit: Option<ValueSource>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
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
    /// For a decimal: what kind of number it is. `None` for other types.
    pub decimal: Option<DecimalMeta>,
    /// The value a new record or line starts with when none is given. For
    /// an entity's field it is read from the table when none is declared.
    pub default: Option<serde_json::Value>,
    /// One line that tells a person what to enter.
    pub help: Option<String>,
    /// For a field that holds a copy of a field of a linked record, such
    /// as a summary's `vendor_name`: the link field beside it and the field
    /// of the linked record. A reader who may not read that record, or that
    /// field of it, gets it blank and cannot filter or sort by it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shown_from: Option<ShownFrom>,
}

/// See [`FieldMeta::shown_from`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ShownFrom {
    /// The link field beside this one: `vendor_id`.
    pub link: String,
    /// The field of the linked record: `name`.
    pub field: String,
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
            decimal: None,
            default: None,
            help: None,
            shown_from: None,
        }
    }

    /// The field holds a copy of `field` of the record that the link field
    /// `link` beside it refers to. See [`FieldMeta::shown_from`].
    pub fn shown_from(mut self, link: &str, field: &str) -> Self {
        self.shown_from = Some(ShownFrom {
            link: link.into(),
            field: field.into(),
        });
        self
    }

    fn decimal(mut self, role: DecimalRole, scale: Option<u32>) -> Self {
        let money = matches!(role, DecimalRole::Money | DecimalRole::UnitPrice);
        self.decimal = Some(DecimalMeta {
            role,
            scale,
            currency: money.then_some(ValueSource::Company),
            unit: None,
        });
        self
    }

    /// An amount of money in the company's accounting currency, stored
    /// with 4 decimal places as every amount in the ledgers is.
    pub fn money(self) -> Self {
        self.decimal(DecimalRole::Money, Some(4))
    }

    /// A price or cost per unit in the company's accounting currency, with
    /// up to 6 decimal places.
    pub fn unit_price(self) -> Self {
        self.decimal(DecimalRole::UnitPrice, Some(6))
    }

    /// A quantity, with up to 6 decimal places.
    pub fn quantity(self) -> Self {
        self.decimal(DecimalRole::Quantity, Some(6))
    }

    /// A factor such as an exchange rate, with this many decimal places.
    pub fn rate(self, scale: u32) -> Self {
        self.decimal(DecimalRole::Rate, Some(scale))
    }

    /// A percentage, with this many decimal places.
    pub fn percent(self, scale: u32) -> Self {
        self.decimal(DecimalRole::Percent, Some(scale))
    }

    /// The amount is in the currency whose code the field `field` beside
    /// this one holds, not in the company's accounting currency.
    pub fn currency_in(mut self, field: &str) -> Self {
        if let Some(decimal) = &mut self.decimal {
            decimal.currency = Some(ValueSource::Field {
                field: field.into(),
            });
        }
        self
    }

    /// For a field of a document's line: the amount is in the currency
    /// whose code the field `field` of the document's header holds, such
    /// as the order's `currency` for the `unit_price` of its lines.
    pub fn currency_of_document(mut self, field: &str) -> Self {
        if let Some(decimal) = &mut self.decimal {
            decimal.currency = Some(ValueSource::Document {
                field: field.into(),
            });
        }
        self
    }

    /// The quantity is in the unit that field `key` of the record linked
    /// by `field` names: `.unit_of("item_id", "uom")`.
    pub fn unit_of(mut self, field: &str, key: &str) -> Self {
        if let Some(decimal) = &mut self.decimal {
            decimal.unit = Some(ValueSource::Linked {
                field: field.into(),
                key: key.into(),
            });
        }
        self
    }

    /// The quantity is in the unit the field `field` beside this one names.
    pub fn unit_in(mut self, field: &str) -> Self {
        if let Some(decimal) = &mut self.decimal {
            decimal.unit = Some(ValueSource::Field {
                field: field.into(),
            });
        }
        self
    }

    /// The most decimal places the value can have, where the role's usual
    /// number does not apply.
    pub fn scale(mut self, scale: u32) -> Self {
        if let Some(decimal) = &mut self.decimal {
            decimal.scale = Some(scale);
        }
        self
    }

    /// The value a new record or line starts with.
    pub fn default(mut self, value: impl Into<serde_json::Value>) -> Self {
        self.default = Some(value.into());
        self
    }

    /// One line that tells a person what to enter.
    pub fn help(mut self, text: &str) -> Self {
        self.help = Some(text.into());
        self
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

    /// The `id` of a line of a document body. The system sets it. A line
    /// sent back with its id keeps it when the body is saved, so what
    /// refers to the line stays valid; a line without one is new.
    pub fn line_id() -> Self {
        FieldMeta::new("id", "ID", FieldType::Uuid).readonly()
    }

    /// The position of a line of a document body, counted from 1. The
    /// system sets it from the order in which the lines are sent.
    pub fn line_no() -> Self {
        FieldMeta::new("line_no", "Line", FieldType::Int).readonly()
    }
}

/// A table exposed for generic listing, reading, creating and updating.
/// The table must have a `uuid` primary key named `id`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntityMeta {
    pub name: String,
    pub module: String,
    pub table: String,
    pub label: String,
    pub fields: Vec<FieldMeta>,
    pub order_by: Vec<String>,
    /// The text fields a person finds a record by, most telling first: a
    /// code, a name. Lists search them, and they are what a reference to
    /// the record displays.
    pub search: Vec<String>,
    /// The fields that tell a person which record this is, in order: what
    /// a reference to the record shows. The search fields unless declared.
    pub display: Vec<String>,
    /// What several of them are called: "Accounts".
    pub plural: String,
    /// The boolean field that is false once a record is no longer to be
    /// used. Lookups leave such records out; see `active_only` of
    /// `entity.list`. A boolean field named `active` unless declared.
    pub active_field: Option<String>,
    /// Set when a record belongs to a record of another entity and is
    /// edited inside it, like the lines of a bill of materials.
    pub parent: Option<ParentMeta>,
    /// Never written through the generic layer.
    pub read_only: bool,
    /// The column holding the owning company, used to scope access. `None`
    /// for records shared by all companies.
    pub company_field: Option<String>,
    /// The enum field that holds the state a record is in, such as the
    /// `status` of a fiscal period. Commands on the entity name states of
    /// this field in `from_states`. `None` for an entity without states.
    pub state_field: Option<String>,
    /// Fields that a change must send together or not at all, because one
    /// is checked or read against the other: a policy rule's `point` and
    /// `condition`. An update that sends some of a group and not the rest
    /// is refused.
    pub written_together: Vec<Vec<String>>,
    /// For a record without a company: reading it takes the grant for all
    /// companies, not one for any company. For what the whole installation
    /// shares and not every company should see: users, roles. Items and
    /// vendors are shared too and readable under any company's grant.
    pub read_needs_global: bool,
    /// Who may write what, beyond the permission: called with the actor,
    /// the record before (`None` when creating) and the fields sent (empty
    /// when deleting). A role may be written only by someone who holds
    /// what it gives.
    #[serde(skip)]
    pub write_check: Option<WriteCheck>,
    /// Checks the fields given to a create or an update, beyond what the
    /// field types and the table's constraints already enforce.
    #[serde(skip)]
    pub validator: Option<Validator>,
}

/// See [`EntityMeta::parent`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ParentMeta {
    /// The entity whose records own these.
    pub entity: String,
    /// The field of this entity that holds the owning record's id.
    pub field: String,
}

/// See [`EntityMeta::write_check`].
pub type WriteCheck = fn(
    &crate::authz::Actor,
    Option<&serde_json::Value>,
    &serde_json::Map<String, serde_json::Value>,
) -> Result<()>;

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
            search: vec![],
            display: vec![],
            plural: pluralize(label),
            active_field: None,
            parent: None,
            read_only: false,
            company_field: None,
            state_field: None,
            written_together: vec![],
            read_needs_global: false,
            write_check: None,
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

    pub fn search(mut self, fields: &[&str]) -> Self {
        self.search = fields.iter().map(|f| f.to_string()).collect();
        self
    }

    /// The fields that tell a person which record this is, in order.
    pub fn display(mut self, fields: &[&str]) -> Self {
        self.display = fields.iter().map(|f| f.to_string()).collect();
        self
    }

    /// What several of them are called, where adding an "s" is wrong.
    pub fn plural(mut self, plural: &str) -> Self {
        self.plural = plural.into();
        self
    }

    /// The boolean field that is false once a record is no longer used.
    pub fn active_field(mut self, field: &str) -> Self {
        self.active_field = Some(field.into());
        self
    }

    /// A record belongs to the record of `entity` that `field` links to,
    /// and is edited inside it.
    pub fn child_of(mut self, entity: &str, field: &str) -> Self {
        self.parent = Some(ParentMeta {
            entity: entity.into(),
            field: field.into(),
        });
        self
    }

    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Reading a record takes the grant for all companies. See
    /// [`EntityMeta::read_needs_global`].
    pub fn read_needs_global(mut self) -> Self {
        self.read_needs_global = true;
        self
    }

    /// See [`EntityMeta::write_check`].
    pub fn write_check(mut self, check: WriteCheck) -> Self {
        self.write_check = Some(check);
        self
    }

    /// These fields are changed together or not at all. See
    /// [`EntityMeta::written_together`].
    pub fn written_together(mut self, fields: &[&str]) -> Self {
        self.written_together
            .push(fields.iter().map(|f| (*f).to_owned()).collect());
        self
    }

    /// The enum field that holds the record's state. See
    /// [`EntityMeta::state_field`].
    pub fn state_field(mut self, field: &str) -> Self {
        self.state_field = Some(field.into());
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
            .filter_map(|field| Some((field.name.clone(), field.ty.column_type()?)))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StateMeta {
    pub name: String,
    pub label: String,
    /// The document's content cannot change while it is in this state.
    pub locked: bool,
    /// A document that has no number yet receives one when it enters this
    /// state. True for every locked state unless [`StateMeta::unnumbered`]
    /// says otherwise.
    #[serde(skip)]
    pub assigns_number: bool,
    /// How an interface colours the state. See [`Tone`] for the defaults.
    pub tone: Tone,
}

/// How a state is shown: a colour's meaning, not a colour.
///
/// Unless a module says otherwise: an editable state is `neutral`; a
/// locked one is `success`, except `open` and `released` (`info`: in
/// force, with more to follow), `cancelled` (`neutral`: it never
/// happened) and `reversed` (`danger`: it happened and was taken back).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

impl StateMeta {
    pub fn editable(name: &str, label: &str) -> Self {
        StateMeta {
            name: name.into(),
            label: label.into(),
            locked: false,
            assigns_number: false,
            tone: Tone::Neutral,
        }
    }

    pub fn locked(name: &str, label: &str) -> Self {
        let tone = match name {
            "open" | "released" => Tone::Info,
            "cancelled" => Tone::Neutral,
            "reversed" => Tone::Danger,
            _ => Tone::Success,
        };
        StateMeta {
            name: name.into(),
            label: label.into(),
            locked: true,
            assigns_number: true,
            tone,
        }
    }

    /// Show the state with this tone instead of the default.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Entering this state gives the document no number. For a state such
    /// as "cancelled", which a draft that never had a number can reach.
    pub fn unnumbered(mut self) -> Self {
        self.assigns_number = false;
        self
    }
}

/// A repeating part of a document body, such as its lines.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CollectionMeta {
    pub name: String,
    pub label: String,
    pub fields: Vec<FieldMeta>,
}

/// Fields of a collection's rows that stand for different kinds of row: a
/// row fills the fields of one alternative and leaves those of the others
/// empty. A sales line is a stock item with its warehouse, a charge, or an
/// account; a form asks which, then shows only that alternative's fields.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Alternatives {
    /// The collection whose rows these are.
    pub collection: String,
    /// The alternatives, in the order to offer them; the first is the
    /// usual one.
    pub one_of: Vec<Alternative>,
}

/// One kind of row of a collection; see [`Alternatives`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Alternative {
    /// What a person picks: "Stock item".
    pub label: String,
    /// The fields a row of this kind fills, in order. The fields of the
    /// collection named by no alternative are common to all of them.
    pub fields: Vec<String>,
}

impl Alternatives {
    /// `collection`'s rows are one of `options`: a label and its fields.
    pub fn new(collection: &str, options: &[(&str, &[&str])]) -> Self {
        Alternatives {
            collection: collection.into(),
            one_of: options
                .iter()
                .map(|(label, fields)| Alternative {
                    label: (*label).into(),
                    fields: fields.iter().map(|f| f.to_string()).collect(),
                })
                .collect(),
        }
    }
}

/// A kind of business document and the workflow its module gives it.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct DocTypeMeta {
    pub name: String,
    pub module: String,
    pub label: String,
    /// What several of them are called: "Purchase orders". The label with
    /// an "s" unless declared.
    pub plural: String,
    /// Numbers look like `PREFIX-2026-00001`.
    pub number_prefix: String,
    pub fields: Vec<FieldMeta>,
    pub collections: Vec<CollectionMeta>,
    /// Collections whose rows are of different kinds, each filling its own
    /// fields. See [`Alternatives`].
    pub alternatives: Vec<Alternatives>,
    pub states: Vec<StateMeta>,
    pub initial_state: String,
    /// Allowed `(from, to)` state changes.
    pub transitions: Vec<(String, String)>,
    /// The figures kept with the header of every document of this type,
    /// under `summary`: who it is with, what it comes to. Lists show them
    /// and sort by them without reading a body. The type's handler works
    /// them out; see `DocumentHandler::summary`.
    pub summary: Vec<FieldMeta>,
    /// The version of what the summary holds and how it is worked out.
    /// Bump it whenever that changes: documents whose summary was worked
    /// out by an older version are worked out again, by the server after
    /// it starts and by `document.refresh_summaries`.
    #[serde(skip)]
    #[schemars(skip)]
    pub summary_version: i32,
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

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModuleInfo {
    pub name: String,
    /// What the module is called in a menu: "General ledger".
    pub label: String,
    pub depends_on: Vec<String>,
}

/// Everything an interface needs to render or call the system.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Description {
    pub modules: Vec<ModuleInfo>,
    pub entities: Vec<EntityMeta>,
    pub doc_types: Vec<DocTypeMeta>,
    /// The fields every document has, whatever its type: `number`,
    /// `status`, `doc_date` and the rest of its header.
    pub document_header: Vec<FieldMeta>,
    pub commands: Vec<CommandMeta>,
    /// The business judgements a company can configure with policy rules.
    pub policies: Vec<PolicyPoint>,
    /// The ledgers, with the schema of the entries a document shows under
    /// each of its postings (`postings[].entries.<name>`).
    pub ledgers: Vec<LedgerInfo>,
}

/// A ledger as an interface sees it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LedgerInfo {
    /// The key of its entries under a posting: `gl`, `stock`.
    pub name: String,
    pub label: String,
    /// JSON Schema of one entry.
    pub entry_schema: serde_json::Value,
}

pub struct Registry {
    modules: Vec<ModuleInfo>,
    migrations: Vec<MigrationSet>,
    entities: BTreeMap<String, EntityMeta>,
    doc_types: BTreeMap<String, DocTypeEntry>,
    commands: BTreeMap<String, CommandEntry>,
    tools: BTreeMap<String, String>,
    policies: BTreeMap<String, PolicyPoint>,
    ledgers: Vec<Arc<dyn Ledger>>,
    subscribers: Vec<Arc<dyn Subscriber>>,
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
            policies: BTreeMap::new(),
            ledgers: vec![],
            subscribers: vec![],
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
                label: module.label(),
                depends_on: module.depends_on().iter().map(|d| d.to_string()).collect(),
            });
            reg.migrations.push(MigrationSet {
                module: name.into(),
                migrations: module.migrations(),
            });

            for mut entity in module.entities() {
                entity.module = name.into();
                if entity.display.is_empty() {
                    entity.display = entity.search.clone();
                }
                if entity.active_field.is_none() {
                    entity.active_field = entity
                        .fields
                        .iter()
                        .find(|f| f.name == "active" && matches!(f.ty, FieldType::Bool))
                        .map(|f| f.name.clone());
                }
                check_entity(&entity)?;
                if reg.entities.contains_key(&entity.name) {
                    return Err(bad(format!("entity '{}' is declared twice", entity.name)));
                }
                reg.entities.insert(entity.name.clone(), entity);
            }

            for handler in module.doc_types() {
                let mut meta = handler.meta();
                meta.module = name.into();
                if meta.plural.is_empty() {
                    meta.plural = pluralize(&meta.label);
                }
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

            for mut point in module.policies() {
                point.module = name.into();
                check_policy(&point)?;
                if reg.policies.contains_key(&point.name) {
                    return Err(bad(format!(
                        "policy point '{}' is declared twice",
                        point.name
                    )));
                }
                reg.policies.insert(point.name.clone(), point);
            }

            reg.ledgers.extend(module.ledgers());
            for subscriber in module.subscribers() {
                let subscriber_name = subscriber.name();
                if !is_ident(subscriber_name) {
                    return Err(bad(format!(
                        "subscriber name '{subscriber_name}' is not a lowercase identifier"
                    )));
                }
                if reg.subscribers.iter().any(|s| s.name() == subscriber_name) {
                    return Err(bad(format!(
                        "subscriber '{subscriber_name}' is registered twice"
                    )));
                }
                reg.subscribers.push(subscriber);
            }
            if let Some(authenticator) = module.authenticator() {
                if reg.authenticator.is_some() {
                    return Err(bad(format!(
                        "module '{name}' supplies an authenticator, but one is already registered"
                    )));
                }
                reg.authenticator = Some(authenticator);
            }
        }

        // Every module is known now, so it can be said what each command
        // is about.
        for entry in reg.commands.values_mut() {
            let meta = &mut entry.meta;
            let doc_type = reg.doc_types.get(&meta.resource);
            meta.resource_kind = if reg.entities.contains_key(&meta.resource) {
                Some(ResourceKind::Entity)
            } else if doc_type.is_some() {
                Some(ResourceKind::Document)
            } else {
                None
            };
            let takes_id = meta.input_schema.pointer("/properties/id").is_some();
            meta.target = meta
                .declared_target
                .unwrap_or(meta.resource_kind.is_some() && takes_id);
            // An entity's states are the values of its state field.
            let entity_states: Option<Vec<&String>> = reg
                .entities
                .get(&meta.resource)
                .and_then(|e| {
                    let field = e.state_field.as_ref()?;
                    e.fields.iter().find(|f| &f.name == field)
                })
                .and_then(|f| match &f.ty {
                    FieldType::Enum { values, .. } => Some(values.iter().collect()),
                    _ => None,
                });
            let primary_states = meta.primary_when.iter().flat_map(|w| &w.states);
            for state in meta.from_states.iter().chain(primary_states) {
                let known = match (&entity_states, doc_type) {
                    (Some(states), _) => states.contains(&state),
                    (None, Some(d)) => d.meta.state(state).is_some(),
                    (None, None) => false,
                };
                if !known {
                    return Err(bad(format!(
                        "command '{}' applies from state '{state}', which '{}' does not have",
                        meta.name, meta.resource
                    )));
                }
            }
            if let Some(when) = &meta.primary_when {
                for collection in when.empty.iter().chain(&when.not_empty) {
                    let known = doc_type
                        .is_some_and(|d| d.meta.collections.iter().any(|c| &c.name == collection));
                    if !known {
                        return Err(bad(format!(
                            "command '{}' is primary by the collection '{collection}', which '{}' \
                             does not have",
                            meta.name, meta.resource
                        )));
                    }
                }
            }
            if let Some(created) = &meta.creates
                && !reg.doc_types.contains_key(created)
            {
                return Err(bad(format!(
                    "command '{}' creates unknown document type '{created}'",
                    meta.name
                )));
            }
            if meta.report.is_some() && !meta.read_only {
                return Err(bad(format!(
                    "command '{}' is declared a report but is not a query",
                    meta.name
                )));
            }
        }
        let entries = reg.entries_schema();
        for entry in reg.commands.values_mut() {
            if let Some(slot) = entry
                .meta
                .output_schema
                .pointer_mut("/$defs/PostingView/properties/entries")
            {
                *slot = entries.clone();
            }
        }
        reg.check_references()?;
        Ok(reg)
    }

    /// Every reference a field declares must lead somewhere: to an entity,
    /// a document type, a state, a collection or a command that exists.
    /// Checked once every module is known, so a module may refer to what a
    /// later one declares.
    fn check_references(&self) -> Result<()> {
        for doc_type in self.doc_types.values() {
            let meta = &doc_type.meta;
            for field in &meta.summary {
                let Some(from) = &field.shown_from else {
                    continue;
                };
                let target =
                    meta.summary
                        .iter()
                        .find(|f| f.name == from.link)
                        .and_then(|f| match &f.ty {
                            FieldType::Link { entity, key, .. } if key == "id" => {
                                self.entities.get(entity)
                            }
                            _ => None,
                        });
                let Some(target) = target else {
                    return Err(bad(format!(
                        "summary field '{}' of '{}' is shown from '{}', which is not a link \
                         by id among its summary fields",
                        field.name, meta.name, from.link
                    )));
                };
                if !target.fields.iter().any(|f| f.name == from.field) {
                    return Err(bad(format!(
                        "summary field '{}' of '{}' is shown from field '{}' of '{}', which it \
                         does not have",
                        field.name, meta.name, from.field, target.name
                    )));
                }
            }
        }
        for entity in self.entities.values() {
            for group in &entity.written_together {
                if let Some(missing) = group
                    .iter()
                    .find(|name| !entity.fields.iter().any(|f| &f.name == *name))
                {
                    return Err(bad(format!(
                        "'{}' writes '{missing}' together with others, but has no such field",
                        entity.name
                    )));
                }
            }
            if let Some(state) = &entity.state_field {
                let is_enum = entity
                    .fields
                    .iter()
                    .any(|f| &f.name == state && matches!(f.ty, FieldType::Enum { .. }));
                if !is_enum {
                    return Err(bad(format!(
                        "the state field '{state}' of '{}' is not one of its choice fields",
                        entity.name
                    )));
                }
            }
        }
        let check = |owner: &str, siblings: &[FieldMeta], field: &FieldMeta| -> Result<()> {
            let at = format!("field '{}' of '{owner}'", field.name);
            match &field.ty {
                FieldType::Link {
                    entity,
                    key,
                    filters,
                } => {
                    let target = self
                        .entities
                        .get(entity)
                        .ok_or_else(|| bad(format!("{at} links to unknown entity '{entity}'")))?;
                    if let Some(unknown) = filters
                        .keys()
                        .find(|name| !target.fields.iter().any(|f| &f.name == *name))
                    {
                        return Err(bad(format!(
                            "{at} filters '{entity}' by unknown field '{unknown}'"
                        )));
                    }
                    if !target.fields.iter().any(|f| &f.name == key) {
                        return Err(bad(format!(
                            "{at} links to '{entity}' by unknown field '{key}'"
                        )));
                    }
                }
                FieldType::EntityLink { entity_field }
                    if !siblings.iter().any(|f| &f.name == entity_field) =>
                {
                    return Err(bad(format!(
                        "{at} takes its entity from unknown field '{entity_field}'"
                    )));
                }
                FieldType::DocumentLink { doc_type, states } => {
                    let target = self.doc_types.get(doc_type).ok_or_else(|| {
                        bad(format!("{at} links to unknown document type '{doc_type}'"))
                    })?;
                    if let Some(state) = states.iter().find(|s| target.meta.state(s).is_none()) {
                        return Err(bad(format!(
                            "{at} links to '{doc_type}' in unknown state '{state}'"
                        )));
                    }
                }
                FieldType::LineLink {
                    document_field,
                    doc_type,
                    collection,
                } => {
                    let target = self.doc_types.get(doc_type).ok_or_else(|| {
                        bad(format!("{at} links to unknown document type '{doc_type}'"))
                    })?;
                    if !target
                        .meta
                        .collections
                        .iter()
                        .any(|c| &c.name == collection)
                    {
                        return Err(bad(format!(
                            "{at} links to unknown collection '{collection}' of '{doc_type}'"
                        )));
                    }
                    let parent = self
                        .doc_types
                        .get(owner)
                        .and_then(|d| d.meta.fields.iter().find(|f| &f.name == document_field));
                    let agrees = matches!(
                        parent.map(|f| &f.ty),
                        Some(FieldType::DocumentLink { doc_type: named, .. }) if named == doc_type
                    );
                    if !agrees {
                        return Err(bad(format!(
                            "{at} takes its document from '{document_field}', which is not a \
                             link to a '{doc_type}'"
                        )));
                    }
                }
                FieldType::Lookup(lookup) => {
                    let found = self.commands.get(&lookup.command);
                    if !found.is_some_and(|c| c.meta.read_only) {
                        return Err(bad(format!(
                            "{at} looks up through '{}', which is not a query command",
                            lookup.command
                        )));
                    }
                    if let (Some(param), Some(command)) = (&lookup.by_keys, found)
                        && command
                            .meta
                            .input_schema
                            .pointer(&format!("/properties/{param}"))
                            .is_none()
                    {
                        return Err(bad(format!(
                            "{at} reads rows by key through input '{param}' of '{}', which has no such input",
                            lookup.command
                        )));
                    }
                }
                _ => {}
            }
            Ok(())
        };
        for entity in self.entities.values() {
            for field in &entity.fields {
                check(&entity.name, &entity.fields, field)?;
            }
            if let Some(parent) = &entity.parent {
                let links = entity.fields.iter().any(|f| {
                    f.name == parent.field
                        && matches!(&f.ty, FieldType::Link { entity, key, .. }
                            if entity == &parent.entity && key == "id")
                });
                if !links {
                    return Err(bad(format!(
                        "entity '{}' belongs to '{}' through '{}', which is not a link to it",
                        entity.name, parent.entity, parent.field
                    )));
                }
            }
        }
        for doc_type in self.doc_types.values() {
            let meta = &doc_type.meta;
            for field in &meta.fields {
                check(&meta.name, &meta.fields, field)?;
            }
            for collection in &meta.collections {
                for field in &collection.fields {
                    check(&meta.name, &collection.fields, field)?;
                }
            }
            for field in &meta.summary {
                check(&meta.name, &meta.summary, field)?;
            }
        }
        Ok(())
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
    /// fields of an entity, or the header of a document type with its
    /// summary's fields as `summary.<field>`. `None` when the name is
    /// neither.
    pub fn rule_columns(&self, resource: &str) -> Option<BTreeMap<String, ColumnType>> {
        if let Some(entity) = self.entities.get(resource) {
            return Some(entity.rule_columns());
        }
        self.doc_types
            .get(resource)
            .map(|entry| crate::document::rule_columns_of(&entry.meta))
    }

    pub fn policy(&self, name: &str) -> Result<&PolicyPoint> {
        self.policies
            .get(name)
            .ok_or_else(|| Error::not_found(format!("policy point '{name}'")))
    }

    pub fn policies(&self) -> impl Iterator<Item = &PolicyPoint> {
        self.policies.values()
    }

    pub fn entities(&self) -> impl Iterator<Item = &EntityMeta> {
        self.entities.values()
    }

    pub fn doc_types(&self) -> impl Iterator<Item = &DocTypeEntry> {
        self.doc_types.values()
    }

    pub fn commands(&self) -> impl Iterator<Item = &CommandEntry> {
        self.commands.values()
    }

    pub fn ledgers(&self) -> &[Arc<dyn Ledger>] {
        &self.ledgers
    }

    pub fn subscribers(&self) -> &[Arc<dyn Subscriber>] {
        &self.subscribers
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
            document_header: crate::document::header_fields(),
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
            policies: self
                .policies
                .values()
                .filter(|p| keep(&p.module))
                .cloned()
                .collect(),
            ledgers: self.ledger_infos(),
        }
    }

    fn ledger_infos(&self) -> Vec<LedgerInfo> {
        self.ledgers
            .iter()
            .map(|l| LedgerInfo {
                name: l.name().to_owned(),
                label: l.label(),
                entry_schema: l.entry_schema(),
            })
            .collect()
    }

    /// The schema of `postings[].entries`: an array of each ledger's rows,
    /// by ledger name. Put into every output schema that holds a posting
    /// view, which schemars can only describe as a map of anything.
    fn entries_schema(&self) -> serde_json::Value {
        let properties: serde_json::Map<String, serde_json::Value> = self
            .ledger_infos()
            .into_iter()
            .map(|l| {
                (
                    l.name,
                    serde_json::json!({
                        "type": "array",
                        "title": l.label,
                        "items": l.entry_schema,
                    }),
                )
            })
            .collect();
        serde_json::json!({
            "type": "object",
            "title": "Entries",
            "description": "The entries each ledger holds under this posting, by ledger name. \
                A ledger with no entries under the posting, or whose entries the reader may \
                not read, is left out.",
            "properties": properties,
            "additionalProperties": { "type": "array", "items": { "type": "object" } },
        })
    }
}

fn check_policy(point: &PolicyPoint) -> Result<()> {
    let name = &point.name;
    let valid = name
        .split_once('.')
        .is_some_and(|(a, b)| is_ident(a) && is_ident(b));
    if !valid {
        return Err(bad(format!(
            "policy point '{name}' must be named 'subject.judgement'"
        )));
    }
    if point.description.trim().is_empty() {
        return Err(bad(format!("policy point '{name}' needs a description")));
    }
    let mut facts = BTreeSet::new();
    for fact in &point.facts {
        if !is_ident(&fact.name) || !facts.insert(&fact.name) {
            return Err(bad(format!(
                "policy point '{name}' has an invalid or repeated fact '{}'",
                fact.name
            )));
        }
    }
    point.check().map_err(bad)
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
    for field in &entity.display {
        if !has(field) {
            return Err(bad(format!(
                "entity '{name}' is displayed by unknown field '{field}'"
            )));
        }
    }
    if let Some(field) = &entity.active_field {
        let boolean = entity
            .fields
            .iter()
            .any(|f| &f.name == field && matches!(f.ty, FieldType::Bool));
        if !boolean {
            return Err(bad(format!(
                "entity '{name}': '{field}' is not a boolean field, so it cannot say \
                 whether a record is still used"
            )));
        }
    }
    for field in &entity.search {
        let text = entity
            .fields
            .iter()
            .any(|f| &f.name == field && matches!(f.ty, FieldType::Text));
        if !text {
            return Err(bad(format!(
                "entity '{name}' searches '{field}', which is not a text field"
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

/// Alternatives name a collection of the type and fields of it, each field
/// in one alternative only; there are at least two, each with a label and
/// a field.
fn check_alternatives(meta: &DocTypeMeta, alternatives: &Alternatives) -> Result<()> {
    let name = &meta.name;
    let collection = &alternatives.collection;
    let Some(rows) = meta.collections.iter().find(|c| &c.name == collection) else {
        return Err(bad(format!(
            "document type '{name}' declares alternatives for unknown collection '{collection}'"
        )));
    };
    if alternatives.one_of.len() < 2 {
        return Err(bad(format!(
            "the alternatives of '{collection}' in '{name}' need at least two"
        )));
    }
    let mut seen = BTreeSet::new();
    for alternative in &alternatives.one_of {
        if alternative.label.trim().is_empty() || alternative.fields.is_empty() {
            return Err(bad(format!(
                "an alternative of '{collection}' in '{name}' has no label or no fields"
            )));
        }
        for field in &alternative.fields {
            if !rows.fields.iter().any(|f| &f.name == field) {
                return Err(bad(format!(
                    "alternative '{}' of '{collection}' in '{name}' names unknown field '{field}'",
                    alternative.label
                )));
            }
            if !seen.insert(field.as_str()) {
                return Err(bad(format!(
                    "field '{field}' of '{collection}' in '{name}' is in more than one alternative"
                )));
            }
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
    // Where an amount says its currency is, that field must exist: beside
    // it (the header, the same line, or the summary), or for a line's
    // field the header's.
    let source_of = |field: &FieldMeta| {
        field
            .decimal
            .as_ref()
            .and_then(|d| d.currency.as_ref())
            .cloned()
    };
    let has = |fields: &[FieldMeta], name: &str| fields.iter().any(|f| f.name == name);
    for (place, fields) in [("field", &meta.fields), ("summary field", &meta.summary)] {
        for field in fields {
            match source_of(field) {
                Some(ValueSource::Field { field: source }) if !has(fields, &source) => {
                    return Err(bad(format!(
                        "{place} '{}' of '{name}' takes its currency from '{source}', which is not beside it",
                        field.name
                    )));
                }
                Some(ValueSource::Document { field: source }) => {
                    return Err(bad(format!(
                        "{place} '{}' of '{name}' takes its currency from the document's '{source}': \
                         only a line's field can; use the field beside it",
                        field.name
                    )));
                }
                _ => {}
            }
        }
    }
    for alternatives in &meta.alternatives {
        check_alternatives(meta, alternatives)?;
    }
    for collection in &meta.collections {
        for field in &collection.fields {
            match source_of(field) {
                Some(ValueSource::Field { field: source })
                    if !has(&collection.fields, &source) && !has(&meta.fields, &source) =>
                {
                    return Err(bad(format!(
                        "field '{}' of '{}' in '{name}' takes its currency from '{source}', which it cannot find",
                        field.name, collection.name
                    )));
                }
                Some(ValueSource::Document { field: source }) if !has(&meta.fields, &source) => {
                    return Err(bad(format!(
                        "field '{}' of '{}' in '{name}' takes its currency from the document's '{source}', \
                         which the document does not have",
                        field.name, collection.name
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod alternatives_tests {
    use super::*;

    fn doc_type(alternatives: Alternatives) -> DocTypeMeta {
        DocTypeMeta {
            name: "order".into(),
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: ["item_id", "warehouse_id", "charge_id", "qty"]
                    .iter()
                    .map(|name| FieldMeta::new(name, name, FieldType::Text))
                    .collect(),
            }],
            alternatives: vec![alternatives],
            ..DocTypeMeta::default()
        }
    }

    #[test]
    fn alternatives_are_checked_against_their_collection() {
        let good = Alternatives::new(
            "lines",
            &[
                ("Stock", &["item_id", "warehouse_id"]),
                ("Charge", &["charge_id"]),
            ],
        );
        assert!(check_alternatives(&doc_type(good.clone()), &good).is_ok());
        for (bad, why) in [
            (
                Alternatives::new("rows", &[("A", &["item_id"]), ("B", &["charge_id"])]),
                "unknown collection",
            ),
            (
                Alternatives::new("lines", &[("A", &["item_id"])]),
                "at least two",
            ),
            (
                Alternatives::new("lines", &[("A", &["item_id"]), ("B", &["price"])]),
                "unknown field 'price'",
            ),
            (
                Alternatives::new("lines", &[("A", &["item_id"]), ("B", &["item_id"])]),
                "more than one alternative",
            ),
            (
                Alternatives::new("lines", &[("A", &["item_id"]), (" ", &["charge_id"])]),
                "no label",
            ),
        ] {
            let error = check_alternatives(&doc_type(bad.clone()), &bad).unwrap_err();
            assert!(error.to_string().contains(why), "{error}");
        }
    }
}
