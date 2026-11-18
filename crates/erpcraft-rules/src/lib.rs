//! Rule expressions.
//!
//! A rule is one expression that is true or false for a record and a user:
//!
//! ```text
//! record.warehouse in user.warehouses && record.amount < 10000
//! ```
//!
//! A rule can be used two ways, and both must agree:
//!
//! - [`Rule::matches`] evaluates it in memory against one record.
//! - [`Rule::to_sql`] compiles it to a `WHERE` condition, so a list is
//!   filtered by the database and never after the fact.
//!
//! The syntax is CEL. This crate is the only place that knows it: the rest
//! of erpcraft sees [`Rule`] and nothing of the engine behind it.
//!
//! Only part of CEL can become SQL: comparisons, `in`, `&&`, `||`, `!`,
//! literals, fields of `record` and attributes of `user`. [`Rule::to_sql`]
//! refuses everything else, and rules are checked with it when they are
//! saved, so a stored rule can always filter.
//!
//! An [`Expression`] is the wider form for conditions that are only ever
//! evaluated in memory, such as business policies: it adds arithmetic and
//! comparisons between any two values, and can never become SQL.

mod cache;
mod expression;
mod parse;
mod text;

pub use expression::Expression;

use std::collections::BTreeMap;
use std::sync::Arc;

use cache::Origin;
use cel::Program;
use cel::common::ast::{Expr, LiteralValue, operators as op};
use erpcraft_core::{Error, Result};
use serde_json::Value;

/// The type of a record field, as the database stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Uuid,
    Text,
    Bool,
    Int,
    Decimal,
    Date,
    Timestamp,
    TextArray,
}

impl ColumnType {
    fn pg(self) -> &'static str {
        match self {
            ColumnType::Uuid => "uuid",
            ColumnType::Text => "text",
            ColumnType::Bool => "boolean",
            ColumnType::Int => "bigint",
            ColumnType::Decimal => "numeric",
            ColumnType::Date => "date",
            ColumnType::Timestamp => "timestamptz",
            ColumnType::TextArray => "text[]",
        }
    }
}

/// A value to bind to a compiled condition. Values travel as text and are
/// cast in the SQL to the type of the column they are compared with.
#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    Text(Option<String>),
    Texts(Vec<String>),
    Bool(Option<bool>),
}

/// What a rule is compiled against.
pub struct SqlScope<'a> {
    /// Alias of the table the record comes from.
    pub alias: &'a str,
    /// The fields of `record` a rule may use. A name with a dot, such as
    /// `summary.total`, is the field `total` of the JSON column `summary`:
    /// the rule writes `record.summary.total`. Names come from the registry,
    /// never from the caller.
    pub columns: &'a BTreeMap<String, ColumnType>,
    /// The acting user, as the rule sees it.
    pub user: &'a Value,
    /// Values sent with the condition, which it reads as `params.<name>`:
    /// `record.customer_id in params.customers`. Like everything else they
    /// are bound as parameters; a long list costs the source nothing.
    /// `Value::Null` when none were sent.
    pub params: &'a Value,
    /// The time zone of the reader (an IANA name the caller has checked):
    /// the day `today()` is, and the zone a day is in when a timestamp is
    /// compared with it. `None`: the database session's zone.
    pub time_zone: Option<&'a str>,
}

impl<'a> SqlScope<'a> {
    /// A scope with no parameters and the session's time zone.
    pub fn new(alias: &'a str, columns: &'a BTreeMap<String, ColumnType>, user: &'a Value) -> Self {
        SqlScope {
            alias,
            columns,
            user,
            params: &Value::Null,
            time_zone: None,
        }
    }
}

/// The functions of dates a list's condition can use. They read the clock
/// or compute, so they are for conditions sent with a request only: a
/// stored rule is also decided in memory, where they do not exist.
const DATE_FUNCTIONS: &[&str] = &[
    "today",
    "addDays",
    "addMonths",
    "startOfMonth",
    "endOfMonth",
    "startOfQuarter",
    "endOfQuarter",
    "startOfYear",
    "endOfYear",
];

/// A parsed rule.
#[derive(Debug)]
pub struct Rule {
    source: String,
    /// Shared with every other rule and expression of the same source.
    program: Arc<Program>,
}

fn invalid(source: &str, why: impl std::fmt::Display) -> Error {
    Error::validation(format!("rule `{source}`: {why}"))
}

impl Rule {
    /// Parse a rule that a caller sent with a request, such as the
    /// `where` of a list. It may refer only to `record` and `user`.
    ///
    /// What parsing a source gave is remembered, so parsing the same text
    /// again is a lookup. Rules parsed this way share a small cache that
    /// they can push only each other out of.
    pub fn parse(source: &str) -> Result<Rule> {
        Rule::parsed(source, Origin::Sent)
    }

    /// Parse a rule the installation keeps, such as a permission rule of
    /// a role. The same as [`Rule::parse`], except for where it is
    /// remembered: with the other stored rules, out of reach of whatever
    /// callers send. Use it only for sources that someone trusted to
    /// configure the installation wrote.
    pub fn parse_stored(source: &str) -> Result<Rule> {
        Rule::parsed(source, Origin::Stored)
    }

    fn parsed(source: &str, origin: Origin) -> Result<Rule> {
        parse::check_length(source, "a rule")?;
        let source = source.trim();
        if source.is_empty() {
            return Err(Error::validation("a rule cannot be empty"));
        }
        // Length and depth are limited here, before anything recursive
        // sees the rule; see the `parse` module.
        let program = parse::compile(source, origin).map_err(|why| invalid(source, why))?;
        let references = program.references();
        for variable in references.variables() {
            match (variable, origin) {
                ("record" | "user", _) | ("params", Origin::Sent) => {}
                ("params", Origin::Stored) => {
                    return Err(invalid(
                        source,
                        "`params` are sent with a list's condition; a rule that is kept cannot use them",
                    ));
                }
                _ => {
                    return Err(invalid(
                        source,
                        format!("unknown name `{variable}`; use `record` or `user`"),
                    ));
                }
            }
        }
        if origin == Origin::Stored
            && let Some(function) = references
                .functions()
                .into_iter()
                .find(|f| DATE_FUNCTIONS.contains(f))
        {
            return Err(invalid(
                source,
                format!(
                    "`{function}()` can be used in a list's condition, not in a rule that is kept"
                ),
            ));
        }
        Ok(Rule {
            source: source.to_owned(),
            program,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Is the rule true for this record and user? An error means it could
    /// not be decided, for example because a field it needs is missing.
    /// Callers must treat that as the unsafe outcome for their case.
    ///
    /// It is decided as strictly as an [`Expression`] is, and never more
    /// generously than [`Rule::to_sql`] filters:
    ///
    /// - a user attribute that is null is an attribute the user does not
    ///   have, so anything that reads it is undecided;
    /// - a record field that is null can be asked `== null` or `!= null`,
    ///   and compared with nothing else: the database would not say that
    ///   it differs from a value either.
    pub fn matches(&self, record: &Value, user: &Value) -> Result<bool> {
        let variables = serde_json::json!({ "record": record, "user": without_nulls(user) });
        expression::decide(&self.program.expression().expr, &variables)
            .map_err(|why| invalid(&self.source, why))
    }

    /// Compile the rule to a SQL condition. Parameters are numbered from
    /// `binds.len() + 1` and appended to `binds`.
    pub fn to_sql(&self, scope: &SqlScope<'_>, binds: &mut Vec<Bind>) -> Result<String> {
        let mut compiler = Compiler { scope, binds };
        let root = compiler.operand(&self.program.expression().expr);
        root.and_then(|operand| compiler.condition(operand))
            .map_err(|why| invalid(&self.source, why))
    }

    /// Check that the rule can be compiled for records with these fields.
    pub fn check(&self, columns: &BTreeMap<String, ColumnType>) -> Result<()> {
        self.to_sql(&SqlScope::new("t", columns, &Value::Null), &mut vec![])
            .map(|_| ())
    }
}

/// A user as a rule sees them: an attribute set to null is left out, at
/// any level, so that it reads as missing.
fn without_nulls(user: &Value) -> Value {
    match user {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(name, value)| (name.clone(), without_nulls(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A value the database works out: a column, a field of a JSON column, or
/// a date computed from `today()`. `name` is how the rule wrote it, for
/// messages.
struct Sql {
    sql: String,
    name: String,
    ty: ColumnType,
}

enum Operand {
    Sql(Sql),
    Value(Value),
    /// An attribute the user does not have, or has as null. Nothing can
    /// be decided from it: whatever uses it is neither true nor false, so
    /// an allow rule does not match and a deny rule does.
    Unknown,
    /// An already compiled condition.
    Condition(String),
}

struct Compiler<'a, 'b> {
    scope: &'a SqlScope<'a>,
    binds: &'b mut Vec<Bind>,
}

type Compiled<T> = std::result::Result<T, String>;

fn scalar_text(value: &Value) -> Compiled<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err("a list or object cannot be compared with a single value".into()),
    }
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Number(_) => "numeric",
        Value::Bool(_) => "boolean",
        _ => "text",
    }
}

/// A plain day, `2026-11-18`, as opposed to a moment with a time.
fn is_day(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// A name of the registry that goes into SQL text: letters, digits and `_`.
fn identifier(name: &str) -> Compiled<&str> {
    if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Ok(name)
    } else {
        Err(format!("`{name}` is not a field name"))
    }
}

impl Compiler<'_, '_> {
    fn bind(&mut self, bind: Bind) -> String {
        self.binds.push(bind);
        format!("${}", self.binds.len())
    }

    /// The SQL for a field of `record`: `t.name`, or for `summary.total`
    /// the JSON field cast to its type.
    fn column(&self, name: &str, ty: ColumnType) -> Compiled<String> {
        let alias = self.scope.alias;
        match name.split_once('.') {
            None => Ok(format!("{alias}.{}", identifier(name)?)),
            Some((column, field)) => Ok(format!(
                "({alias}.{}->>'{}')::{}",
                identifier(column)?,
                identifier(field)?,
                ty.pg()
            )),
        }
    }

    /// A date argument: a date the database works out, or a day written as text.
    fn date(&mut self, operand: Operand, function: &str) -> Compiled<String> {
        match operand {
            Operand::Sql(Sql {
                sql,
                ty: ColumnType::Date,
                ..
            }) => Ok(sql),
            Operand::Sql(Sql {
                sql,
                ty: ColumnType::Timestamp,
                ..
            }) => Ok(self.day_of(&sql)),
            Operand::Value(Value::String(day)) if is_day(&day) => {
                Ok(format!("{}::date", self.bind(Bind::Text(Some(day)))))
            }
            _ => Err(format!("`{function}` needs a date")),
        }
    }

    /// The day of a timestamp in the reader's time zone.
    fn day_of(&mut self, sql: &str) -> String {
        match self.scope.time_zone {
            Some(zone) => {
                let zone = self.bind(Bind::Text(Some(zone.to_owned())));
                format!("({sql} AT TIME ZONE {zone}::text)::date")
            }
            None => format!("({sql})::date"),
        }
    }

    /// A whole number of days or months, bound.
    fn count(&mut self, operand: Operand, function: &str) -> Compiled<String> {
        match operand {
            Operand::Value(Value::Number(n))
                if n.as_i64().is_some_and(|n| n.unsigned_abs() <= 100_000) =>
            {
                Ok(format!(
                    "{}::int",
                    self.bind(Bind::Text(Some(n.to_string())))
                ))
            }
            _ => Err(format!("`{function}` needs a whole number")),
        }
    }

    fn date_function(&mut self, name: &str, args: Vec<Operand>) -> Compiled<Operand> {
        let mut args = args.into_iter();
        let mut next = || {
            args.next()
                .ok_or_else(|| format!("`{name}` is missing a value"))
        };
        // date_trunc of a timestamp without zone, so the session's zone never shifts the day.
        let trunc = |unit: &str, day: &str| format!("date_trunc('{unit}', ({day})::timestamp)");
        let sql = match name {
            "today" => match self.scope.time_zone {
                Some(zone) => {
                    let zone = self.bind(Bind::Text(Some(zone.to_owned())));
                    format!("(now() AT TIME ZONE {zone}::text)::date")
                }
                None => "current_date".to_owned(),
            },
            "addDays" => {
                let day = self.date(next()?, name)?;
                let n = self.count(next()?, name)?;
                format!("(({day}) + {n})")
            }
            "addMonths" => {
                let day = self.date(next()?, name)?;
                let n = self.count(next()?, name)?;
                format!("(({day}) + make_interval(months => {n}))::date")
            }
            "startOfMonth" | "startOfQuarter" | "startOfYear" => {
                let unit = &name["startOf".len()..].to_lowercase();
                let day = self.date(next()?, name)?;
                format!("{}::date", trunc(unit, &day))
            }
            "endOfMonth" | "endOfQuarter" | "endOfYear" => {
                let unit = name["endOf".len()..].to_lowercase();
                let day = self.date(next()?, name)?;
                // Postgres has no interval of a quarter.
                let span = if unit == "quarter" {
                    "3 months"
                } else if unit == "month" {
                    "1 month"
                } else {
                    "1 year"
                };
                format!(
                    "({} + interval '{span}' - interval '1 day')::date",
                    trunc(&unit, &day)
                )
            }
            _ => return Err(format!("`{name}` cannot be used in a rule")),
        };
        if args.next().is_some() {
            return Err(format!("`{name}` takes fewer values"));
        }
        Ok(Operand::Sql(Sql {
            sql,
            name: format!("{name}()"),
            ty: ColumnType::Date,
        }))
    }

    fn text_test(&mut self, name: &str, target: Operand, needle: Operand) -> Compiled<Operand> {
        let Some(test) = text::TextTest::named(name) else {
            return Err(format!("`{name}` cannot be used in a rule"));
        };
        let column = match target {
            Operand::Sql(column) if column.ty == ColumnType::Text => column,
            Operand::Sql(column) => {
                return Err(format!(
                    "`{name}` needs a text, and `{}` is not one",
                    column.name
                ));
            }
            _ => {
                return Err(format!(
                    "`{name}` can be asked of a field of the record only"
                ));
            }
        };
        let needle = match needle {
            Operand::Value(Value::String(needle)) => needle,
            Operand::Unknown => return Ok(Operand::Condition(self.unknown())),
            _ => return Err(format!("`{name}` needs a text to look for")),
        };
        let pattern = self.bind(Bind::Text(Some(test.pattern(&needle))));
        Ok(Operand::Condition(format!(
            "({} {} {pattern}::text)",
            column.sql,
            test.sql_operator()
        )))
    }

    fn operand(&mut self, expr: &Expr) -> Compiled<Operand> {
        match expr {
            Expr::Literal(literal) => Ok(Operand::Value(match literal {
                LiteralValue::Boolean(b) => Value::from(*b.inner()),
                LiteralValue::Int(i) => Value::from(*i.inner()),
                LiteralValue::UInt(u) => Value::from(*u.inner()),
                LiteralValue::Double(d) => Value::from(*d.inner()),
                LiteralValue::String(s) => Value::from(s.inner().to_string()),
                LiteralValue::Null => Value::Null,
                LiteralValue::Bytes(_) => {
                    return Err("byte strings cannot be used in a rule".into());
                }
            })),
            Expr::List(list) => {
                let mut items = Vec::with_capacity(list.elements.len());
                for element in &list.elements {
                    match self.operand(&element.expr)? {
                        Operand::Value(value) => items.push(value),
                        Operand::Unknown => items.push(Value::Null),
                        _ => return Err("a list in a rule may hold only values, not fields".into()),
                    }
                }
                Ok(Operand::Value(Value::Array(items)))
            }
            Expr::Select(select) if !select.test => {
                let mut path = vec![select.field.as_str()];
                let mut inner = &select.operand.expr;
                let root = loop {
                    match inner {
                        Expr::Ident(name) => break name.as_str(),
                        Expr::Select(next) if !next.test => {
                            path.push(next.field.as_str());
                            inner = &next.operand.expr;
                        }
                        _ => {
                            return Err(
                                "only fields of `record`, `user` and `params` can be used".into()
                            );
                        }
                    }
                };
                path.reverse();
                match root {
                    "record" => {
                        let name = path.join(".");
                        match self.scope.columns.get(&name) {
                            Some(ty) if path.len() <= 2 => Ok(Operand::Sql(Sql {
                                sql: self.column(&name, *ty)?,
                                name: format!("record.{name}"),
                                ty: *ty,
                            })),
                            _ if path.len() == 1 => Err(format!(
                                "the record has no field `{name}` that a rule can use"
                            )),
                            _ => Err(format!("`record.{name}` is not a field a rule can use")),
                        }
                    }
                    "user" => {
                        let mut value = self.scope.user;
                        for key in &path {
                            value = value.get(key).unwrap_or(&Value::Null);
                        }
                        Ok(match value {
                            Value::Null => Operand::Unknown,
                            value => Operand::Value(value.clone()),
                        })
                    }
                    "params" => {
                        let mut value = self.scope.params;
                        for key in &path {
                            value = value.get(key).unwrap_or(&Value::Null);
                        }
                        match value {
                            Value::Null => Err(format!("`params.{}` was not sent", path.join("."))),
                            value => Ok(Operand::Value(value.clone())),
                        }
                    }
                    other => Err(format!("unknown name `{other}`; use `record` or `user`")),
                }
            }
            Expr::Call(call) => {
                let mut args = Vec::with_capacity(call.args.len());
                for arg in &call.args {
                    args.push(self.operand(&arg.expr)?);
                }
                let name = call.func_name.as_str();
                if let Some(target) = &call.target {
                    let target = self.operand(&target.expr)?;
                    let mut args = args.into_iter();
                    return match (args.next(), args.next()) {
                        (Some(needle), None) => self.text_test(name, target, needle),
                        _ => Err(format!("`{name}` takes one text")),
                    };
                }
                if DATE_FUNCTIONS.contains(&name) {
                    return self.date_function(name, args);
                }
                let mut args = args.into_iter();
                match (name, args.next(), args.next(), args.next()) {
                    (op::LOGICAL_NOT, Some(a), None, None) => {
                        Ok(Operand::Condition(format!("(NOT {})", self.condition(a)?)))
                    }
                    (op::LOGICAL_AND | op::LOGICAL_OR, Some(a), Some(b), None) => {
                        let joiner = if name == op::LOGICAL_AND { "AND" } else { "OR" };
                        let (a, b) = (self.condition(a)?, self.condition(b)?);
                        Ok(Operand::Condition(format!("({a} {joiner} {b})")))
                    }
                    (op::EQUALS, Some(a), Some(b), None) => self.compare(a, b, "="),
                    (op::NOT_EQUALS, Some(a), Some(b), None) => self.compare(a, b, "<>"),
                    (op::LESS, Some(a), Some(b), None) => self.compare(a, b, "<"),
                    (op::LESS_EQUALS, Some(a), Some(b), None) => self.compare(a, b, "<="),
                    (op::GREATER, Some(a), Some(b), None) => self.compare(a, b, ">"),
                    (op::GREATER_EQUALS, Some(a), Some(b), None) => self.compare(a, b, ">="),
                    (op::IN, Some(a), Some(b), None) => self.member(a, b),
                    _ => Err(format!(
                        "`{}` cannot be used in a rule",
                        name.trim_matches('_')
                    )),
                }
            }
            Expr::Ident(name) => Err(format!(
                "`{name}` alone is not a value; use one of its fields"
            )),
            _ => Err("this kind of expression cannot be used in a rule".into()),
        }
    }

    /// Turn an operand into something usable where true or false is expected.
    fn condition(&mut self, operand: Operand) -> Compiled<String> {
        match operand {
            Operand::Condition(sql) => Ok(sql),
            Operand::Sql(Sql {
                sql,
                ty: ColumnType::Bool,
                ..
            }) => Ok(sql),
            Operand::Sql(Sql { name, .. }) => Err(format!("`{name}` is not true or false")),
            Operand::Value(Value::Bool(b)) => {
                Ok(format!("{}::boolean", self.bind(Bind::Bool(Some(b)))))
            }
            // An attribute the user does not have: neither true nor false.
            Operand::Unknown | Operand::Value(Value::Null) => Ok(self.unknown()),
            Operand::Value(_) => {
                Err("a value that is not true or false is used as a condition".into())
            }
        }
    }

    /// A condition that is neither true nor false.
    fn unknown(&mut self) -> String {
        format!("{}::boolean", self.bind(Bind::Bool(None)))
    }

    /// A value compared with a database value of type `ty`, bound and cast.
    /// A day compared with a timestamp is midnight in the reader's zone.
    fn typed_value(&mut self, value: &Value, ty: ColumnType) -> Compiled<String> {
        let text = scalar_text(value)?;
        if ty == ColumnType::Timestamp
            && is_day(&text)
            && let Some(zone) = self.scope.time_zone
        {
            let param = self.bind(Bind::Text(Some(text)));
            let zone = self.bind(Bind::Text(Some(zone.to_owned())));
            return Ok(format!("({param}::timestamp AT TIME ZONE {zone}::text)"));
        }
        let param = self.bind(Bind::Text(Some(text)));
        Ok(format!("{param}::{}", ty.pg()))
    }

    /// Two database values; a date against a timestamp is the date's midnight in the reader's zone.
    fn pair(&mut self, a: Sql, b: Sql) -> (String, String) {
        let midnight = |this: &mut Self, sql: &str| match this.scope.time_zone {
            Some(zone) => {
                let zone = this.bind(Bind::Text(Some(zone.to_owned())));
                format!("(({sql})::timestamp AT TIME ZONE {zone}::text)")
            }
            None => sql.to_owned(),
        };
        match (a.ty, b.ty) {
            (ColumnType::Timestamp, ColumnType::Date) => (a.sql, midnight(self, &b.sql)),
            (ColumnType::Date, ColumnType::Timestamp) => (midnight(self, &a.sql), b.sql),
            _ => (a.sql, b.sql),
        }
    }

    fn compare(&mut self, left: Operand, right: Operand, sql_op: &str) -> Compiled<Operand> {
        // Only `null` as it is written tests a field for null. An
        // attribute the user lacks must not: `record.parent_id ==
        // user.manager` would otherwise match every record without a
        // parent for a user without a manager.
        if matches!(
            (&left, &right),
            (
                Operand::Unknown,
                Operand::Sql(_) | Operand::Value(_) | Operand::Unknown
            ) | (Operand::Sql(_) | Operand::Value(_), Operand::Unknown)
        ) {
            return Ok(Operand::Condition(self.unknown()));
        }
        let null_test = |column: String| match sql_op {
            "=" => Ok(Operand::Condition(format!("({column} IS NULL)"))),
            "<>" => Ok(Operand::Condition(format!("({column} IS NOT NULL)"))),
            _ => Err("null can only be tested with == or !=".to_owned()),
        };
        let sql = match (left, right) {
            (Operand::Condition(_), _) | (_, Operand::Condition(_)) => {
                return Err(
                    "a condition cannot be compared; combine conditions with && and ||".into(),
                );
            }
            (Operand::Sql(a), Operand::Sql(b)) => {
                let (a, b) = self.pair(a, b);
                format!("({a} {sql_op} {b})")
            }
            (Operand::Sql(column), Operand::Value(value)) => {
                if value.is_null() {
                    return null_test(column.sql);
                }
                let param = self.typed_value(&value, column.ty)?;
                format!("({} {sql_op} {param})", column.sql)
            }
            (Operand::Value(value), Operand::Sql(column)) => {
                if value.is_null() {
                    return null_test(column.sql);
                }
                let param = self.typed_value(&value, column.ty)?;
                format!("({param} {sql_op} {})", column.sql)
            }
            (Operand::Unknown, _) | (_, Operand::Unknown) => unreachable!("handled above"),
            (Operand::Value(a), Operand::Value(b)) => {
                // A null compares as unknown, which filters the row out.
                let ty = if a.is_null() {
                    value_type(&b)
                } else {
                    value_type(&a)
                };
                let text = |v: &Value| {
                    if v.is_null() {
                        Ok(None)
                    } else {
                        scalar_text(v).map(Some)
                    }
                };
                let (a, b) = (text(&a)?, text(&b)?);
                let (a, b) = (self.bind(Bind::Text(a)), self.bind(Bind::Text(b)));
                format!("({a}::{ty} {sql_op} {b}::{ty})")
            }
        };
        Ok(Operand::Condition(sql))
    }

    fn member(&mut self, item: Operand, list: Operand) -> Compiled<Operand> {
        let texts = |value: &Value| -> Compiled<Vec<String>> {
            match value {
                Value::Array(items) => items.iter().map(scalar_text).collect(),
                _ => Err("the right side of `in` must be a list".into()),
            }
        };
        // Whether something is in a list the user does not have, or
        // whether a value the user does not have is in a list, is not
        // known. "It is not" would let a deny rule pass.
        if matches!(item, Operand::Unknown) || matches!(list, Operand::Unknown) {
            if matches!(item, Operand::Condition(_)) || matches!(list, Operand::Condition(_)) {
                return Err("the right side of `in` must be a list".into());
            }
            return Ok(Operand::Condition(self.unknown()));
        }
        let sql = match (item, list) {
            (Operand::Sql(column), Operand::Value(list)) => {
                if column.ty == ColumnType::TextArray {
                    return Err(format!("`{}` is itself a list", column.name));
                }
                let param = self.bind(Bind::Texts(texts(&list)?));
                format!("({} = ANY({param}::{}[]))", column.sql, column.ty.pg())
            }
            (
                Operand::Value(item),
                Operand::Sql(Sql {
                    sql,
                    ty: ColumnType::TextArray,
                    ..
                }),
            ) => {
                let item = if item.is_null() {
                    None
                } else {
                    Some(scalar_text(&item)?)
                };
                let param = self.bind(Bind::Text(item));
                format!("({param}::text = ANY({sql}))")
            }
            (
                Operand::Sql(a),
                Operand::Sql(Sql {
                    sql,
                    ty: ColumnType::TextArray,
                    ..
                }),
            ) => {
                format!("({}::text = ANY({sql}))", a.sql)
            }
            (Operand::Value(item), Operand::Value(list)) => {
                let found = !item.is_null() && texts(&list)?.contains(&scalar_text(&item)?);
                format!("{}::boolean", self.bind(Bind::Bool(Some(found))))
            }
            _ => return Err("the right side of `in` must be a list".into()),
        };
        Ok(Operand::Condition(sql))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn columns() -> BTreeMap<String, ColumnType> {
        BTreeMap::from([
            ("warehouse".to_owned(), ColumnType::Text),
            ("amount".to_owned(), ColumnType::Decimal),
            ("qty".to_owned(), ColumnType::Int),
            ("active".to_owned(), ColumnType::Bool),
            ("owner_id".to_owned(), ColumnType::Uuid),
            ("parent_id".to_owned(), ColumnType::Uuid),
            ("doc_date".to_owned(), ColumnType::Date),
            ("tags".to_owned(), ColumnType::TextArray),
            ("name".to_owned(), ColumnType::Text),
            ("created_at".to_owned(), ColumnType::Timestamp),
            ("summary.total".to_owned(), ColumnType::Decimal),
            ("summary.vendor_id".to_owned(), ColumnType::Uuid),
        ])
    }

    fn sql(rule: &str, user: Value) -> (String, Vec<Bind>) {
        let columns = columns();
        let scope = SqlScope::new("t", &columns, &user);
        let mut binds = vec![];
        let sql = Rule::parse(rule)
            .unwrap()
            .to_sql(&scope, &mut binds)
            .unwrap();
        (sql, binds)
    }

    fn refused(rule: &str) -> String {
        match Rule::parse(rule).and_then(|r| r.check(&columns())) {
            Err(e) => e.to_string(),
            Ok(()) => panic!("`{rule}` should be refused"),
        }
    }

    fn text(s: &str) -> Bind {
        Bind::Text(Some(s.to_owned()))
    }

    #[test]
    fn a_rule_is_safe_to_share_between_threads() {
        fn check<T: Send + Sync>() {}
        check::<Rule>();
    }

    #[test]
    fn evaluates_against_a_record() {
        let rule = Rule::parse(
            "record.warehouse in user.warehouses && (record.amount < 10000 || user.is_manager)",
        )
        .unwrap();
        let clerk = json!({"warehouses": ["W1", "W2"], "is_manager": false});
        let manager = json!({"warehouses": ["W1"], "is_manager": true});
        assert!(
            rule.matches(&json!({"warehouse": "W1", "amount": 500.5}), &clerk)
                .unwrap()
        );
        assert!(
            !rule
                .matches(&json!({"warehouse": "W3", "amount": 500.5}), &clerk)
                .unwrap()
        );
        assert!(
            !rule
                .matches(&json!({"warehouse": "W1", "amount": 20000.0}), &clerk)
                .unwrap()
        );
        assert!(
            rule.matches(&json!({"warehouse": "W1", "amount": 20000.0}), &manager)
                .unwrap()
        );
    }

    #[test]
    fn an_undecidable_rule_is_an_error_not_an_answer() {
        let rule = Rule::parse("record.warehouse in user.warehouses").unwrap();
        // The user has no such attribute.
        assert!(
            rule.matches(&json!({"warehouse": "W1"}), &json!({}))
                .is_err()
        );
        // The rule does not produce true or false.
        assert!(
            Rule::parse("record.amount")
                .unwrap()
                .matches(&json!({"amount": 1}), &json!({}))
                .is_err()
        );
    }

    #[test]
    fn compiles_to_sql_with_typed_parameters() {
        let user = json!({"warehouses": ["W1", "W2"], "is_manager": false, "id": "0190-abc"});
        let (sql, binds) = sql(
            "record.warehouse in user.warehouses && (record.amount < 10000 || user.is_manager)",
            user.clone(),
        );
        assert_eq!(
            sql,
            "((t.warehouse = ANY($1::text[])) AND ((t.amount < $2::numeric) OR $3::boolean))"
        );
        assert_eq!(
            binds,
            [
                Bind::Texts(vec!["W1".into(), "W2".into()]),
                text("10000"),
                Bind::Bool(Some(false))
            ]
        );

        let (sql, binds) = super::tests::sql(
            "record.owner_id != user.id && record.doc_date >= '2026-01-01'",
            user,
        );
        assert_eq!(
            sql,
            "((t.owner_id <> $1::uuid) AND (t.doc_date >= $2::date))"
        );
        assert_eq!(binds, [text("0190-abc"), text("2026-01-01")]);
    }

    #[test]
    fn compiles_the_rest_of_the_subset() {
        let none = json!({});
        assert_eq!(sql("!record.active", none.clone()).0, "(NOT t.active)");
        assert_eq!(
            sql("record.parent_id == null", none.clone()).0,
            "(t.parent_id IS NULL)"
        );
        assert_eq!(
            sql("record.parent_id != null", none.clone()).0,
            "(t.parent_id IS NOT NULL)"
        );
        assert_eq!(
            sql("record.qty in [1, 2, 3]", none.clone()).0,
            "(t.qty = ANY($1::bigint[]))"
        );
        assert_eq!(
            sql("'urgent' in record.tags", none.clone()).0,
            "($1::text = ANY(t.tags))"
        );
        assert_eq!(
            sql("record.qty <= record.amount", none.clone()).0,
            "(t.qty <= t.amount)"
        );
        assert_eq!(sql("5 > record.qty", none).0, "($1::bigint > t.qty)");
    }

    #[test]
    fn a_missing_user_attribute_matches_nothing() {
        let (sql, binds) = sql(
            "record.warehouse in user.warehouses || user.is_manager",
            json!({}),
        );
        assert_eq!(sql, "($1::boolean OR $2::boolean)");
        assert_eq!(binds, [Bind::Bool(None), Bind::Bool(None)]);
    }

    #[test]
    fn a_null_decides_nothing_in_memory_or_in_sql() {
        // In memory: a null attribute is a missing one, and a null field
        // compares with nothing.
        let undecided = |rule: &str, record: Value, user: Value| {
            let error = Rule::parse(rule)
                .unwrap()
                .matches(&record, &user)
                .expect_err(rule);
            error.to_string()
        };
        let restricted = json!({ "restricted": null, "team": { "lead": null } });
        for rule in [
            "user.restricted != true",
            "!(user.restricted == true)",
            "user.restricted == null",
            "record.owner_id == user.restricted",
            "record.owner_id != user.team.lead",
            "record.warehouse in user.restricted",
            "!user.restricted",
        ] {
            let why = undecided(
                rule,
                json!({ "owner_id": null, "warehouse": "W1" }),
                restricted.clone(),
            );
            assert!(why.contains("is not there"), "{why}");
            // Exactly what a user without the attribute gets.
            let missing = undecided(
                rule,
                json!({ "owner_id": null, "warehouse": "W1" }),
                json!({ "team": {} }),
            );
            assert_eq!(why, missing);
        }
        let user = json!({ "id": "u1", "is_manager": false });
        let orphan = json!({ "owner_id": null, "warehouse": null });
        for rule in [
            "record.owner_id != user.id",
            "record.owner_id == user.id",
            "record.owner_id == record.warehouse",
            "record.warehouse in ['W1']",
        ] {
            let why = undecided(rule, orphan.clone(), user.clone());
            assert!(why.contains("null cannot be compared"), "{why}");
        }
        // A field can still be asked whether it is null.
        let is = |rule: &str| Rule::parse(rule).unwrap().matches(&orphan, &user).unwrap();
        assert!(is("record.owner_id == null"));
        assert!(!is("record.owner_id != null"));
        assert!(is("record.owner_id == null || record.owner_id == user.id"));

        // In SQL: the same questions are unknown, never an `IS NULL` test.
        let unknown = ("$1::boolean".to_owned(), vec![Bind::Bool(None)]);
        for rule in [
            "record.parent_id == user.manager",
            "record.parent_id != user.manager",
            "user.manager == record.parent_id",
            "user.restricted != true",
            "user.restricted == null",
            "record.warehouse in user.warehouses",
            "user.home in record.tags",
        ] {
            for user in [
                json!({}),
                json!({ "manager": null, "restricted": null, "warehouses": null, "home": null }),
            ] {
                assert_eq!(sql(rule, user), unknown, "{rule}");
            }
        }
        assert_eq!(
            sql("!(user.restricted == true)", json!({ "restricted": null })).0,
            "(NOT $1::boolean)"
        );
        // Written out, `null` still tests the field.
        assert_eq!(
            sql("record.parent_id == null", json!({})).0,
            "(t.parent_id IS NULL)"
        );
    }

    #[test]
    fn refuses_what_cannot_filter() {
        assert!(refused("record.nope == 1").contains("no field `nope`"));
        assert!(refused("record.tags.all(t, t != 'x')").contains("cannot be used in a rule"));
        assert!(refused("record.warehouse.matches('W.*')").contains("cannot be used in a rule"));
        assert!(refused("record.qty + 1 > 2").contains("cannot be used in a rule"));
        assert!(refused("record.warehouse").contains("not true or false"));
        assert!(refused("record.qty > null").contains("null can only be tested"));
        assert!(refused("record.a.b == 1").contains("not a field a rule can use"));
        assert!(
            Rule::parse("company.id == 1")
                .unwrap_err()
                .to_string()
                .contains("unknown name `company`")
        );
        assert!(Rule::parse("record.qty >").is_err());
        assert!(Rule::parse("  ").is_err());
    }

    #[test]
    fn values_never_reach_the_sql_text() {
        let (sql, binds) = sql(
            "record.warehouse == \"x'; DROP TABLE account; --\"",
            json!({}),
        );
        assert_eq!(sql, "(t.warehouse = $1::text)");
        assert_eq!(binds, [text("x'; DROP TABLE account; --")]);
    }

    fn scoped(rule: &str, params: Value, zone: Option<&str>) -> (String, Vec<Bind>) {
        let columns = columns();
        let user = json!({});
        let scope = SqlScope {
            params: &params,
            time_zone: zone,
            ..SqlScope::new("t", &columns, &user)
        };
        let mut binds = vec![];
        let sql = Rule::parse(rule)
            .unwrap()
            .to_sql(&scope, &mut binds)
            .unwrap();
        (sql, binds)
    }

    #[test]
    fn text_methods_compile_to_like_with_the_wildcards_escaped() {
        let (contains, binds) = sql("record.name.containsIgnoreCase('50%_off')", json!({}));
        assert_eq!(contains, "(t.name ILIKE $1::text)");
        assert_eq!(binds, vec![text("%50\\%\\_off%")]);
        let (both, binds) = sql(
            "record.name.startsWith('Ac') && record.name.endsWith('me')",
            json!({}),
        );
        assert_eq!(both, "((t.name LIKE $1::text) AND (t.name LIKE $2::text))");
        assert_eq!(binds, vec![text("Ac%"), text("%me")]);
        let (equal, binds) = sql("record.name.equalsIgnoreCase('ACME')", json!({}));
        assert_eq!(equal, "(t.name ILIKE $1::text)");
        assert_eq!(binds, vec![text("ACME")]);
        // In memory as in SQL; a null decides nothing.
        let rule = Rule::parse("record.name.containsIgnoreCase('CORP')").unwrap();
        assert!(
            rule.matches(&json!({"name": "Acme Corp"}), &json!({}))
                .unwrap()
        );
        assert!(
            !Rule::parse("record.name.contains('corp')")
                .unwrap()
                .matches(&json!({"name": "Acme Corp"}), &json!({}))
                .unwrap()
        );
        assert!(rule.matches(&json!({"name": null}), &json!({})).is_err());
        assert!(refused("record.qty.contains('1')").contains("needs a text"));
        assert!(
            refused("record.name.contains(record.warehouse)").contains("needs a text to look for")
        );
        assert!(refused("record.name.matches('a.*')").contains("cannot be used"));
    }

    #[test]
    fn a_field_of_a_json_column_is_cast_to_its_type() {
        let (summary, binds) = sql(
            "record.summary.total >= '100.50' && record.summary.vendor_id == null",
            json!({}),
        );
        assert_eq!(
            summary,
            "(((t.summary->>'total')::numeric >= $1::numeric) AND ((t.summary->>'vendor_id')::uuid IS NULL))"
        );
        assert_eq!(binds, vec![text("100.50")]);
        assert!(refused("record.summary.missing == 1").contains("not a field"));
        assert!(refused("record.summary == 1").contains("no field `summary`"));
    }

    #[test]
    fn params_are_bound_however_long() {
        let ids: Vec<String> = (0..500)
            .map(|i| format!("00000000-0000-0000-0000-{i:012}"))
            .collect();
        let (sql, binds) = scoped(
            "record.owner_id in params.owners",
            json!({"owners": ids}),
            None,
        );
        assert_eq!(sql, "(t.owner_id = ANY($1::uuid[]))");
        assert!(matches!(&binds[0], Bind::Texts(list) if list.len() == 500));
        let columns = columns();
        let user = json!({});
        let scope = SqlScope::new("t", &columns, &user);
        let err = Rule::parse("record.owner_id in params.owners")
            .unwrap()
            .to_sql(&scope, &mut vec![])
            .unwrap_err();
        assert!(err.to_string().contains("`params.owners` was not sent"));
    }

    #[test]
    fn dates_relative_to_today_in_the_readers_zone() {
        let (sql, binds) = scoped(
            "record.doc_date >= startOfMonth(addMonths(today(), -1)) && record.doc_date <= endOfMonth(addMonths(today(), -1))",
            Value::Null,
            Some("Europe/Berlin"),
        );
        assert_eq!(
            sql,
            "((t.doc_date >= date_trunc('month', ((((now() AT TIME ZONE $1::text)::date) + make_interval(months => $2::int))::date)::timestamp)::date) \
AND (t.doc_date <= (date_trunc('month', ((((now() AT TIME ZONE $3::text)::date) + make_interval(months => $4::int))::date)::timestamp) + interval '1 month' - interval '1 day')::date))"
        );
        assert_eq!(
            binds,
            vec![
                text("Europe/Berlin"),
                text("-1"),
                text("Europe/Berlin"),
                text("-1")
            ]
        );
        let (sql, _) = scoped("record.doc_date > addDays(today(), -30)", Value::Null, None);
        assert_eq!(sql, "(t.doc_date > ((current_date) + $1::int))");
        assert!(refused("record.doc_date > addDays(today(), 1.5)").contains("whole number"));
        assert!(refused("record.doc_date > addDays(record.name, 1)").contains("needs a date"));
    }

    #[test]
    fn a_day_against_a_timestamp_is_midnight_in_the_readers_zone() {
        let (sql, binds) = scoped(
            "record.created_at >= '2026-11-18'",
            Value::Null,
            Some("America/New_York"),
        );
        assert_eq!(
            sql,
            "(t.created_at >= ($1::timestamp AT TIME ZONE $2::text))"
        );
        assert_eq!(binds, vec![text("2026-11-18"), text("America/New_York")]);
        // A moment with its own offset keeps it.
        let (sql, _) = scoped(
            "record.created_at >= '2026-11-18T10:00:00Z'",
            Value::Null,
            Some("America/New_York"),
        );
        assert_eq!(sql, "(t.created_at >= $1::timestamptz)");
        let (sql, _) = scoped("record.created_at < today()", Value::Null, Some("UTC"));
        assert_eq!(
            sql,
            "(t.created_at < (((now() AT TIME ZONE $1::text)::date)::timestamp AT TIME ZONE $2::text))"
        );
        // Without a zone, as before.
        let (sql, _) = scoped("record.created_at >= '2026-11-18'", Value::Null, None);
        assert_eq!(sql, "(t.created_at >= $1::timestamptz)");
    }

    #[test]
    fn a_kept_rule_cannot_read_the_clock_or_params() {
        let stored = |s: &str| Rule::parse_stored(s).unwrap_err().to_string();
        assert!(
            stored("record.doc_date >= today()")
                .contains("`today()` can be used in a list's condition")
        );
        assert!(stored("record.owner_id in params.x").contains("`params` are sent"));
        assert!(Rule::parse_stored("record.name.startsWithIgnoreCase('a')").is_ok());
    }
}
