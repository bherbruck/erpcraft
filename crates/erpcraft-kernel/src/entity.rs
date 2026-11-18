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
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::Arguments;
use sqlx::postgres::PgArguments;
use uuid::Uuid;

use crate::authz::{Actor, RowScope, Target};
use crate::command::Ctx;
use crate::placeholders::checked;
use crate::registry::{EntityMeta, FieldType};
use crate::schema::{self, TableInfo};

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
    /// Leave out the records that are no longer to be used: those whose
    /// `active_field` is false. Without effect on an entity that has none.
    pub active_only: bool,
    /// Do not count the whole list: `total` is then the size of the page.
    pub skip_total: bool,
}

/// One page of a list, with the number of records the whole list has.
#[derive(Debug, Clone, Serialize, JsonSchema)]
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
        None if writing || meta.read_needs_global => Target::Global,
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
        let field =
            meta.fields.iter().find(|f| &f.name == key).ok_or_else(|| {
                Error::validation(format!("unknown field '{key}'")).with_field(key)
            })?;
        if field.readonly {
            return Err(Error::field(key, "is set by the system"));
        }
        if field.immutable && !creating {
            return Err(Error::field(key, "cannot change after creation"));
        }
        if field.required && value.is_null() {
            return Err(Error::field(key, "is required"));
        }
        if let (FieldType::Enum { values, .. }, Some(text)) = (&field.ty, value.as_str())
            && !values.iter().any(|v| v == text)
        {
            return Err(Error::field(
                key,
                format_args!("must be one of: {}", values.join(", ")),
            ));
        }
    }
    if creating {
        for field in meta.fields.iter().filter(|f| f.required) {
            if !data.contains_key(&field.name) {
                return Err(Error::field(&field.name, "is required"));
            }
        }
    }
    Ok(())
}

/// Refuse a decimal that is not a number, or that has more decimal places
/// than its column keeps: Postgres would round it without saying so.
fn check_decimals(meta: &EntityMeta, table: &TableInfo, data: &Map<String, Value>) -> Result<()> {
    for field in meta
        .fields
        .iter()
        .filter(|f| matches!(f.ty, FieldType::Decimal))
    {
        let name = &field.name;
        let places = match data.get(name) {
            None | Some(Value::Null) => continue,
            Some(Value::String(text)) => decimal_places(text.trim()),
            Some(Value::Number(number)) => decimal_places(&number.to_string()),
            Some(_) => None,
        };
        let places = places.ok_or_else(|| {
            Error::field(
                name,
                "must be a number, written as a string such as \"12.50\"",
            )
        })?;
        if let Some(scale) = table.scale(name)
            && places > i64::from(scale)
        {
            return Err(Error::field(
                name,
                format_args!("has at most {scale} decimal places"),
            ));
        }
    }
    Ok(())
}

/// How many decimal places a number written as text needs, not counting
/// trailing zeros: 1 for `1.5000`, 3 for `25e-3`. `None` if the text is not
/// a decimal number. The text is examined as written, so no digit is lost
/// to a parser's own precision.
fn decimal_places(text: &str) -> Option<i64> {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (&text[..at], text[at + 1..].parse::<i32>().ok()?),
        None => (text, 0),
    };
    let unsigned = mantissa.strip_prefix(['+', '-']).unwrap_or(mantissa);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let digits = || whole.bytes().chain(fraction.bytes());
    if digits().count() == 0 || !digits().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let trailing_zeros = digits().rev().take_while(|b| *b == b'0').count();
    if trailing_zeros == digits().count() {
        return Some(0);
    }
    let places = fraction.len() as i64 - trailing_zeros as i64 - i64::from(exponent);
    Some(places.max(0))
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
    Error::forbidden(format!("you may not set field '{field}'")).with_field(field)
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
            return Err(
                Error::validation(format!("unknown filter field '{name}'")).with_field(name)
            );
        }
        // Filtering by a field would reveal its values.
        if actor.may_withhold(&permission, name) {
            return Err(
                Error::forbidden(format!("you may not filter by field '{name}'")).with_field(name),
            );
        }
        binds.push(Bind::Text(Some(value.clone())));
        let _ = write!(conditions, " AND t.{name}::text = ${}", binds.len());
    }
    if let (true, Some(name)) = (query.active_only, &meta.active_field) {
        // As with any filter: it would reveal a field's values.
        if actor.may_withhold(&permission, name) {
            return Err(
                Error::forbidden(format!("you may not filter by field '{name}'")).with_field(name),
            );
        }
        let _ = write!(conditions, " AND t.{name}");
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
    if let Some(filter) =
        actor.row_filter(&permission, &scope, meta.read_needs_global, &mut binds)?
    {
        let _ = write!(conditions, " AND {filter}");
    }

    let mut order = vec![];
    for key in &query.sort {
        let (name, direction) = sort_key(key);
        match field(name) {
            Some(f) if !matches!(f.ty, FieldType::Json) => {}
            _ => {
                return Err(Error::validation(format!("cannot sort by '{name}'")).with_field(name));
            }
        }
        // Sorting by a field would reveal the order of its values.
        if actor.may_withhold(&permission, name) {
            return Err(
                Error::forbidden(format!("you may not sort by field '{name}'")).with_field(name),
            );
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

    let total: Option<i64> = if query.skip_total {
        None
    } else {
        let sql = format!(
            "SELECT count(*) FROM {} AS t WHERE {conditions}",
            meta.table
        );
        let count = sqlx::query_scalar_with(&sql, checked(&sql, arguments(&binds)?)?)
            .fetch_one(&mut *ctx.conn)
            .await?;
        Some(count)
    };

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
    let rows = sqlx::query_scalar_with::<_, Value, _>(&sql, checked(&sql, arguments)?)
        .fetch_all(&mut *ctx.conn)
        .await?;
    Ok(Page {
        total: total.unwrap_or(rows.len() as i64),
        items: rows
            .into_iter()
            .map(|row| present(actor, meta, row))
            .collect(),
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
    if let Some(filter) = actor.row_filter(
        &format!("{entity}:read"),
        &scope,
        meta.read_needs_global,
        &mut binds,
    )? {
        let _ = write!(sql, " AND {filter}");
    }
    let row = sqlx::query_scalar_with::<_, Value, _>(&sql, checked(&sql, arguments(&binds)?)?)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found(entity))?;
    Ok(present(actor, meta, row))
}

/// Of `ids`, the records of `entity` the actor may read, by the same rules
/// as reading them: for showing what a document copied from records it
/// links to. An actor without the right to read the entity reads none.
pub async fn readable_ids(
    ctx: &mut Ctx<'_>,
    entity: &str,
    ids: &[Uuid],
) -> Result<std::collections::BTreeSet<Uuid>> {
    let (kernel, actor) = (ctx.kernel, ctx.actor);
    let meta = kernel.registry().entity(entity)?;
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let columns = meta.rule_columns();
    let scope = RowScope {
        alias: "t",
        columns: &columns,
        company_column: meta.company_field.as_deref(),
    };
    let mut binds = vec![Bind::Texts(ids.iter().map(Uuid::to_string).collect())];
    let mut sql = format!(
        "SELECT t.id FROM {} AS t WHERE t.id = ANY($1::uuid[])",
        meta.table
    );
    match actor.row_filter(
        &format!("{entity}:read"),
        &scope,
        meta.read_needs_global,
        &mut binds,
    ) {
        Ok(Some(filter)) => {
            let _ = write!(sql, " AND {filter}");
        }
        Ok(None) => {}
        Err(Error::Forbidden(_)) => return Ok(Default::default()),
        Err(error) => return Err(error),
    }
    let found: Vec<Uuid> = sqlx::query_scalar_with(&sql, checked(&sql, arguments(&binds)?)?)
        .fetch_all(&mut *ctx.conn)
        .await?;
    Ok(found.into_iter().collect())
}

/// Whether the actor reads every record of `entity` and its `field`
/// without a condition: what filtering or sorting by a copy of that field
/// needs, since no record is then left out.
pub fn reads_all(actor: &Actor, meta: &EntityMeta, field: &str) -> bool {
    if actor.is_admin {
        return true;
    }
    let permission = format!("{}:read", meta.name);
    let columns = meta.rule_columns();
    let scope = RowScope {
        alias: "t",
        columns: &columns,
        company_column: meta.company_field.as_deref(),
    };
    let mut binds = vec![];
    matches!(
        actor.row_filter(&permission, &scope, meta.read_needs_global, &mut binds),
        Ok(None)
    ) && !actor.may_withhold(&permission, field)
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
                .ok_or_else(|| Error::field(field, "is required"))?;
            Target::Company(company)
        }
    };
    let record = rule_record(meta, &Value::Object(data.clone()));
    actor.require_on(&permission, target, &record)?;
    let access = actor.fields(&permission, target, &record);
    if let Some(field) = data.keys().find(|key| !access.allows(key)) {
        return Err(withheld(field));
    }
    let table = kernel.tables().get(&mut *ctx.conn, &meta.table).await?;
    check_decimals(meta, &table, &data)?;
    if let Some(validate) = meta.validator {
        validate(kernel.registry(), &data)?;
    }
    if let Some(check) = meta.write_check {
        check(actor, None, &data)?;
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
        .await
        .map_err(|error| schema::describe(meta, &table, error))?;

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
    for group in &meta.written_together {
        let sent: Vec<&String> = group.iter().filter(|f| data.contains_key(*f)).collect();
        if !sent.is_empty()
            && let Some(missing) = group.iter().find(|f| !data.contains_key(*f))
        {
            return Err(Error::validation(format!(
                "send '{missing}' together with {}, so that they can be checked against each other",
                sent.iter()
                    .map(|f| format!("'{f}'"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ))
            .with_field(missing));
        }
    }

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
    let current = sqlx::query_scalar_with::<_, Value, _>(&sql, checked(&sql, arguments(&binds)?)?)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found(entity))?;
    if let Some(version) = version
        && current.get(VERSION_KEY).and_then(Value::as_str) != Some(version)
    {
        return Err(Error::stale(format!(
            "this {entity} was changed by someone else after you read it"
        )));
    }

    let access = actor.fields(
        &permission,
        target_of(meta, &current, true),
        &rule_record(meta, &current),
    );
    if let Some(field) = data.keys().find(|key| !access.allows(key)) {
        return Err(withheld(field));
    }
    let table = kernel.tables().get(&mut *ctx.conn, &meta.table).await?;
    check_decimals(meta, &table, &data)?;
    if let Some(validate) = meta.validator {
        validate(kernel.registry(), &data)?;
    }
    if let Some(check) = meta.write_check {
        check(actor, Some(&current), &data)?;
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
        .await
        .map_err(|error| schema::describe(meta, &table, error))?;

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
    let current = sqlx::query_scalar_with::<_, Value, _>(&sql, checked(&sql, arguments(&binds)?)?)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found(entity))?;
    if let Some(version) = version
        && current.get(VERSION_KEY).and_then(Value::as_str) != Some(version)
    {
        return Err(Error::stale(format!(
            "this {entity} was changed by someone else after you read it"
        )));
    }
    if let Some(check) = meta.write_check {
        check(actor, Some(&current), &Map::new())?;
    }

    let deleted = sqlx::query(&format!("DELETE FROM {} WHERE id = $1", meta.table))
        .bind(id)
        .execute(&mut *ctx.conn)
        .await;
    match deleted {
        Ok(_) => Ok(()),
        // A foreign key still points at the record.
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23503") => Err(
            Error::in_use(format!("this {entity} is in use and cannot be deleted")),
        ),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::decimal_places;

    #[test]
    fn decimal_places_are_counted_as_written() {
        for (text, places) in [
            ("1", 0),
            ("1.5", 1),
            ("-1.5000000", 1),
            ("+0.123456", 6),
            ("0.1234567", 7),
            (".5", 1),
            ("5.", 0),
            ("0.000", 0),
            ("25e-3", 3),
            ("100e-1", 0),
            ("1.25E1", 1),
            ("1e-7", 7),
            ("0e-9", 0),
            ("1.00000000000000000000000000000000000001", 38),
        ] {
            assert_eq!(decimal_places(text), Some(places), "{text}");
        }
        for text in [
            "", ".", "-", "many", "1,5", "1.2.3", "1e", "NaN", "0x10", "1 000",
        ] {
            assert_eq!(decimal_places(text), None, "{text}");
        }
    }
}
