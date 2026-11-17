//! Generic access to registered entities.
//!
//! Records travel as JSON. Postgres converts in both directions
//! (`to_jsonb`, `jsonb_populate_record`), so column types, defaults and
//! constraints are enforced by the table itself. Identifiers in the SQL
//! built here come only from the registry, never from the caller.
//!
//! Permission rules are applied here, so every module's entities get them:
//! row conditions become part of the query, and withheld fields are removed
//! from every record that leaves.

use std::collections::BTreeMap;
use std::fmt::Write;

use erpcraft_core::{Error, Result, new_id};
use erpcraft_rules::{Bind, ColumnType, Rule, SqlScope};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::Arguments;
use sqlx::postgres::PgArguments;
use uuid::Uuid;

use crate::authz::{Actor, RowScope, Target};
use crate::command::Ctx;
use crate::registry::{EntityMeta, FieldType};

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 500;

/// Turn rule parameters into query arguments, in order.
pub(crate) fn arguments(binds: &[Bind]) -> Result<PgArguments> {
    let mut arguments = PgArguments::default();
    for bind in binds {
        match bind {
            Bind::Text(value) => arguments.add(value.clone()),
            Bind::Texts(values) => arguments.add(values.clone()),
            Bind::Bool(value) => arguments.add(*value),
        }
        .map_err(Error::internal)?;
    }
    Ok(arguments)
}

/// The key under which every record carries its version token.
pub const VERSION_KEY: &str = "_version";

/// What a list is asked for.
#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    /// Field name and the exact value it must have.
    pub filters: Vec<(String, String)>,
    /// Text to look for in the entity's search fields.
    pub q: Option<String>,
    /// A condition over `record` and `user`, in the language of permission
    /// rules: `record.account_type == 'asset' && !record.is_group`.
    pub condition: Option<String>,
    /// Field names; a leading `-` sorts descending. Empty: the entity's
    /// own order.
    pub sort: Vec<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// One page of a list, with the number of records the whole list has.
#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
}

/// A `LIKE` pattern matching text that contains `q` literally.
pub(crate) fn like_pattern(q: &str) -> String {
    let mut pattern = String::with_capacity(q.len() + 2);
    pattern.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// Compile a condition the caller wrote into SQL over the row aliased `t`.
/// It can refer only to `columns`; leave out any the caller may not see.
pub(crate) fn caller_condition(
    actor: &Actor,
    source: &str,
    columns: &BTreeMap<String, ColumnType>,
    binds: &mut Vec<Bind>,
) -> Result<String> {
    let rule = Rule::parse(source)?;
    rule.check(columns)?;
    let user = actor.rule_user();
    let scope = SqlScope {
        alias: "t",
        columns,
        user: &user,
    };
    rule.to_sql(&scope, binds)
}

/// Split `-name` into the field and whether it sorts descending.
pub(crate) fn sort_key(key: &str) -> (&str, &'static str) {
    match key.strip_prefix('-') {
        Some(field) => (field, "DESC"),
        None => (key, "ASC"),
    }
}

/// JSON for one row aliased `t`. Decimals are returned as strings so that
/// no client rounds them through a float. The version token is the row's
/// `xmin`: Postgres changes it on every update, whoever makes it.
fn select_expr(meta: &EntityMeta) -> String {
    let mut extra: Vec<String> = meta
        .fields
        .iter()
        .filter(|f| matches!(f.ty, FieldType::Decimal))
        .map(|f| format!("'{0}', t.{0}::text", f.name))
        .collect();
    extra.push(format!("'{VERSION_KEY}', t.xmin::text"));
    format!("to_jsonb(t) || jsonb_build_object({})", extra.join(", "))
}

/// A record as rules see it: decimals are numbers, so a rule can compare
/// them. Rules decide access, never amounts, so the float is acceptable.
fn rule_record(meta: &EntityMeta, row: &Value) -> Value {
    let mut record = row.clone();
    for field in meta
        .fields
        .iter()
        .filter(|f| matches!(f.ty, FieldType::Decimal))
    {
        if let Some(number) = row
            .get(&field.name)
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
        {
            record[&field.name] = json!(number);
        }
    }
    record
}

/// Which grants count for this record. Records without a company are
/// readable under any grant, but changed only under a grant for all
/// companies.
fn target_of(meta: &EntityMeta, row: &Value, writing: bool) -> Target {
    match &meta.company_field {
        Some(field) => row
            .get(field)
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .map_or(Target::Global, Target::Company),
        None if writing => Target::Global,
        None => Target::Any,
    }
}

/// Remove the fields the actor may not read from a record about to leave.
fn present(actor: &Actor, meta: &EntityMeta, row: Value) -> Value {
    let permission = format!("{}:read", meta.name);
    if !actor.limits_fields(&permission) {
        return row;
    }
    let access = actor.fields(
        &permission,
        target_of(meta, &row, false),
        &rule_record(meta, &row),
    );
    match row {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(key, _)| key == VERSION_KEY || access.allows(key))
                .collect(),
        ),
        other => other,
    }
}

fn check_fields(meta: &EntityMeta, data: &Map<String, Value>, creating: bool) -> Result<()> {
    for (key, value) in data {
        let field = meta
            .fields
            .iter()
            .find(|f| &f.name == key)
            .ok_or_else(|| Error::validation(format!("unknown field '{key}'")))?;
        if field.readonly {
            return Err(Error::validation(format!(
                "field '{key}' is set by the system"
            )));
        }
        if field.immutable && !creating {
            return Err(Error::validation(format!(
                "field '{key}' cannot change after creation"
            )));
        }
        if field.required && value.is_null() {
            return Err(Error::validation(format!("field '{key}' is required")));
        }
        if let (FieldType::Enum { values }, Some(text)) = (&field.ty, value.as_str()) {
            if !values.iter().any(|v| v == text) {
                return Err(Error::validation(format!(
                    "field '{key}' must be one of: {}",
                    values.join(", ")
                )));
            }
        }
    }
    if creating {
        for field in meta.fields.iter().filter(|f| f.required) {
            if !data.contains_key(&field.name) {
                return Err(Error::validation(format!(
                    "field '{}' is required",
                    field.name
                )));
            }
        }
    }
    Ok(())
}

fn into_object(data: Value) -> Result<Map<String, Value>> {
    match data {
        Value::Object(map) => Ok(map),
        _ => Err(Error::validation("the record must be a JSON object")),
    }
}

fn writable(meta: &EntityMeta) -> Result<()> {
    if meta.read_only {
        return Err(Error::validation(format!(
            "{} records cannot be written directly",
            meta.name
        )));
    }
    Ok(())
}

fn withheld(field: &str) -> Error {
    Error::forbidden(format!("you may not set field '{field}'"))
}

pub async fn list(ctx: &mut Ctx<'_>, entity: &str, query: &ListQuery) -> Result<Page<Value>> {
    let (kernel, actor) = (ctx.kernel, ctx.actor);
    let meta = kernel.registry().entity(entity)?;
    let permission = format!("{entity}:read");
    let columns = meta.rule_columns();
    let scope = RowScope {
        alias: "t",
        columns: &columns,
        company_column: meta.company_field.as_deref(),
    };
    let field = |name: &str| meta.fields.iter().find(|f| f.name == name);

    let mut binds = vec![];
    let mut conditions = String::from("true");
    for (name, value) in &query.filters {
        if field(name).is_none() {
            return Err(Error::validation(format!("unknown filter field '{name}'")));
        }
        // Filtering by a field would reveal its values.
        if actor.may_withhold(&permission, name) {
            return Err(Error::forbidden(format!(
                "you may not filter by field '{name}'"
            )));
        }
        binds.push(Bind::Text(Some(value.clone())));
        let _ = write!(conditions, " AND t.{name}::text = ${}", binds.len());
    }
    if let Some(q) = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        // A field that may be withheld is not searched, or a search would
        // reveal its values.
        let fields: Vec<&String> = meta
            .search
            .iter()
            .filter(|name| !actor.may_withhold(&permission, name))
            .collect();
        if fields.is_empty() {
            return Err(Error::validation(format!(
                "{entity} records cannot be searched by text"
            )));
        }
        binds.push(Bind::Text(Some(like_pattern(q))));
        let n = binds.len();
        let parts: Vec<String> = fields
            .iter()
            .map(|name| format!("t.{name}::text ILIKE ${n}"))
            .collect();
        let _ = write!(conditions, " AND ({})", parts.join(" OR "));
    }
    if let Some(source) = &query.condition {
        // A field that may be withheld cannot be named, or the condition
        // would reveal its values.
        let visible: BTreeMap<String, ColumnType> = columns
            .iter()
            .filter(|(name, _)| !actor.may_withhold(&permission, name))
            .map(|(name, ty)| (name.clone(), *ty))
            .collect();
        let sql = caller_condition(actor, source, &visible, &mut binds)?;
        let _ = write!(conditions, " AND ({sql})");
    }
    if let Some(filter) = actor.row_filter(&permission, &scope, false, &mut binds)? {
        let _ = write!(conditions, " AND {filter}");
    }

    let mut order = vec![];
    for key in &query.sort {
        let (name, direction) = sort_key(key);
        match field(name) {
            Some(f) if !matches!(f.ty, FieldType::Json) => {}
            _ => return Err(Error::validation(format!("cannot sort by '{name}'"))),
        }
        // Sorting by a field would reveal the order of its values.
        if actor.may_withhold(&permission, name) {
            return Err(Error::forbidden(format!(
                "you may not sort by field '{name}'"
            )));
        }
        order.push(format!("t.{name} {direction}"));
    }
    if order.is_empty() {
        // The entity's own order, leaving out any field whose order the
        // caller may not learn.
        order.extend(
            meta.order_by
                .iter()
                .filter(|f| !actor.may_withhold(&permission, f))
                .map(|f| format!("t.{f}")),
        );
    }
    // The id makes the order total, so pages never overlap.
    order.push("t.id".into());

    let total: i64 = sqlx::query_scalar_with(
        &format!(
            "SELECT count(*) FROM {} AS t WHERE {conditions}",
            meta.table
        ),
        arguments(&binds)?,
    )
    .fetch_one(&mut *ctx.conn)
    .await?;

    let sql = format!(
        "SELECT {} FROM {} AS t WHERE {conditions} ORDER BY {} LIMIT ${} OFFSET ${}",
        select_expr(meta),
        meta.table,
        order.join(", "),
        binds.len() + 1,
        binds.len() + 2
    );
    let mut arguments = arguments(&binds)?;
    arguments
        .add(query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT))
        .and_then(|()| arguments.add(query.offset.unwrap_or(0).max(0)))
        .map_err(Error::internal)?;
    let rows = sqlx::query_scalar_with::<_, Value, _>(&sql, arguments)
        .fetch_all(&mut *ctx.conn)
        .await?;
    Ok(Page {
        items: rows
            .into_iter()
            .map(|row| present(actor, meta, row))
            .collect(),
        total,
    })
}

pub async fn get(ctx: &mut Ctx<'_>, entity: &str, id: Uuid) -> Result<Value> {
    let (kernel, actor) = (ctx.kernel, ctx.actor);
    let meta = kernel.registry().entity(entity)?;
    let columns = meta.rule_columns();
    let scope = RowScope {
        alias: "t",
        columns: &columns,
        company_column: meta.company_field.as_deref(),
    };

    let mut binds = vec![Bind::Text(Some(id.to_string()))];
    let mut sql = format!(
        "SELECT {} FROM {} AS t WHERE t.id = $1::uuid",
        select_expr(meta),
        meta.table
    );
    if let Some(filter) = actor.row_filter(&format!("{entity}:read"), &scope, false, &mut binds)? {
        let _ = write!(sql, " AND {filter}");
    }
    let row = sqlx::query_scalar_with::<_, Value, _>(&sql, arguments(&binds)?)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found(entity))?;
    Ok(present(actor, meta, row))
}

pub async fn create(ctx: &mut Ctx<'_>, entity: &str, data: Value) -> Result<Value> {
    let (kernel, actor) = (ctx.kernel, ctx.actor);
    let meta = kernel.registry().entity(entity)?;
    writable(meta)?;
    let mut data = into_object(data)?;
    check_fields(meta, &data, true)?;

    let permission = format!("{entity}:create");
    let target = match meta.company_field.as_deref() {
        // Shared records, and companies themselves, need a grant for all companies.
        None | Some("id") => Target::Global,
        Some(field) => {
            let company = data
                .get(field)
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| Error::validation(format!("field '{field}' is required")))?;
            Target::Company(company)
        }
    };
    let record = rule_record(meta, &Value::Object(data.clone()));
    actor.require_on(&permission, target, &record)?;
    let access = actor.fields(&permission, target, &record);
    if let Some(field) = data.keys().find(|key| !access.allows(key)) {
        return Err(withheld(field));
    }
    if let Some(validate) = meta.validator {
        validate(kernel.registry(), &data)?;
    }

    let id = new_id();
    ctx.touch(entity, id);
    data.insert("id".into(), json!(id));
    let columns: Vec<&str> = meta
        .fields
        .iter()
        .filter(|f| data.contains_key(&f.name))
        .map(|f| f.name.as_str())
        .collect();
    let columns = columns.join(", ");
    let sql = format!(
        "INSERT INTO {table} AS t ({columns}) SELECT {columns} FROM jsonb_populate_record(NULL::{table}, $1)
         RETURNING {select}",
        table = meta.table,
        select = select_expr(meta),
    );
    let row = sqlx::query_scalar::<_, Value>(&sql)
        .bind(Value::Object(data))
        .fetch_one(&mut *ctx.conn)
        .await?;

    Ok(written(actor, meta, row))
}

/// What a create or an update returns. Someone who may write a record is
/// not thereby allowed to read it: without that right they get its id and
/// version only.
fn written(actor: &Actor, meta: &EntityMeta, row: Value) -> Value {
    let readable = actor.can_on(
        &format!("{}:read", meta.name),
        target_of(meta, &row, false),
        &rule_record(meta, &row),
    );
    if readable {
        present(actor, meta, row)
    } else {
        json!({ "id": row["id"], VERSION_KEY: row[VERSION_KEY] })
    }
}

/// Change fields of one record. With `version`, the token the caller last
/// read, the change is refused if the record has changed since.
pub async fn update(
    ctx: &mut Ctx<'_>,
    entity: &str,
    id: Uuid,
    data: Value,
    version: Option<&str>,
) -> Result<Value> {
    let (kernel, actor) = (ctx.kernel, ctx.actor);
    let meta = kernel.registry().entity(entity)?;
    writable(meta)?;
    ctx.touch(entity, id);
    let data = into_object(data)?;
    if data.is_empty() {
        return Err(Error::validation("no fields to update"));
    }
    check_fields(meta, &data, false)?;

    // Lock the record as it is now, if the actor may update it.
    let permission = format!("{entity}:update");
    let columns = meta.rule_columns();
    let scope = RowScope {
        alias: "t",
        columns: &columns,
        company_column: meta.company_field.as_deref(),
    };
    let mut binds = vec![Bind::Text(Some(id.to_string()))];
    let mut sql = format!(
        "SELECT {} FROM {} AS t WHERE t.id = $1::uuid",
        select_expr(meta),
        meta.table
    );
    let shared = meta.company_field.is_none();
    if let Some(filter) = actor.row_filter(&permission, &scope, shared, &mut binds)? {
        let _ = write!(sql, " AND {filter}");
    }
    sql.push_str(" FOR UPDATE");
    let current = sqlx::query_scalar_with::<_, Value, _>(&sql, arguments(&binds)?)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found(entity))?;
    if let Some(version) = version {
        if current.get(VERSION_KEY).and_then(Value::as_str) != Some(version) {
            return Err(Error::conflict(format!(
                "this {entity} was changed by someone else after you read it"
            )));
        }
    }

    let access = actor.fields(
        &permission,
        target_of(meta, &current, true),
        &rule_record(meta, &current),
    );
    if let Some(field) = data.keys().find(|key| !access.allows(key)) {
        return Err(withheld(field));
    }
    if let Some(validate) = meta.validator {
        validate(kernel.registry(), &data)?;
    }

    let assignments: Vec<String> = meta
        .fields
        .iter()
        .filter(|f| data.contains_key(&f.name))
        .map(|f| format!("{0} = r.{0}", f.name))
        .collect();
    let sql = format!(
        "UPDATE {table} AS t SET {set} FROM jsonb_populate_record(NULL::{table}, $1) AS r
         WHERE t.id = $2 RETURNING {select}",
        table = meta.table,
        set = assignments.join(", "),
        select = select_expr(meta),
    );
    let changed = sqlx::query_scalar::<_, Value>(&sql)
        .bind(Value::Object(data))
        .bind(id)
        .fetch_one(&mut *ctx.conn)
        .await?;

    // The record must still be one the actor may update, or a user could
    // hand a record to a place they have no business with.
    if !actor.can_on(
        &permission,
        target_of(meta, &changed, true),
        &rule_record(meta, &changed),
    ) {
        return Err(Error::forbidden(
            "this change would put the record out of your reach",
        ));
    }
    Ok(written(actor, meta, changed))
}

/// Delete one record. Refused while anything still refers to it. With
/// `version`, also refused if the record changed since the caller read it.
pub async fn delete(
    ctx: &mut Ctx<'_>,
    entity: &str,
    id: Uuid,
    version: Option<&str>,
) -> Result<()> {
    let (kernel, actor) = (ctx.kernel, ctx.actor);
    let meta = kernel.registry().entity(entity)?;
    writable(meta)?;
    ctx.touch(entity, id);

    let permission = format!("{entity}:delete");
    let columns = meta.rule_columns();
    let scope = RowScope {
        alias: "t",
        columns: &columns,
        company_column: meta.company_field.as_deref(),
    };
    let mut binds = vec![Bind::Text(Some(id.to_string()))];
    let mut sql = format!(
        "SELECT {} FROM {} AS t WHERE t.id = $1::uuid",
        select_expr(meta),
        meta.table
    );
    // Shared records, and companies themselves, need a grant for all companies.
    let shared = matches!(meta.company_field.as_deref(), None | Some("id"));
    if let Some(filter) = actor.row_filter(&permission, &scope, shared, &mut binds)? {
        let _ = write!(sql, " AND {filter}");
    }
    sql.push_str(" FOR UPDATE");
    let current = sqlx::query_scalar_with::<_, Value, _>(&sql, arguments(&binds)?)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found(entity))?;
    if let Some(version) = version {
        if current.get(VERSION_KEY).and_then(Value::as_str) != Some(version) {
            return Err(Error::conflict(format!(
                "this {entity} was changed by someone else after you read it"
            )));
        }
    }

    let deleted = sqlx::query(&format!("DELETE FROM {} WHERE id = $1", meta.table))
        .bind(id)
        .execute(&mut *ctx.conn)
        .await;
    match deleted {
        Ok(_) => Ok(()),
        // A foreign key still points at the record.
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23503") => Err(
            Error::conflict(format!("this {entity} is in use and cannot be deleted")),
        ),
        Err(error) => Err(error.into()),
    }
}
