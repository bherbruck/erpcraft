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

use std::collections::BTreeMap;

use cel::common::ast::{Expr, LiteralValue, operators as op};
use cel::{Context, Program, Value as CelValue};
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
    /// The fields of `record` a rule may use.
    pub columns: &'a BTreeMap<String, ColumnType>,
    /// The acting user, as the rule sees it.
    pub user: &'a Value,
}

/// A parsed rule.
#[derive(Debug)]
pub struct Rule {
    source: String,
    program: Program,
}

fn invalid(source: &str, why: impl std::fmt::Display) -> Error {
    Error::validation(format!("rule `{source}`: {why}"))
}

impl Rule {
    /// Parse a rule. It may refer only to `record` and `user`.
    pub fn parse(source: &str) -> Result<Rule> {
        let source = source.trim();
        if source.is_empty() {
            return Err(Error::validation("a rule cannot be empty"));
        }
        let program = Program::compile(source).map_err(|e| invalid(source, format!("{e:?}")))?;
        for variable in program.references().variables() {
            if variable != "record" && variable != "user" {
                return Err(invalid(
                    source,
                    format!("unknown name `{variable}`; use `record` or `user`"),
                ));
            }
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
    pub fn matches(&self, record: &Value, user: &Value) -> Result<bool> {
        let mut context = Context::default();
        context
            .add_variable("record", record.clone())
            .and_then(|()| context.add_variable("user", user.clone()))
            .map_err(|e| invalid(&self.source, e))?;
        match self.program.execute(&context) {
            Ok(CelValue::Bool(result)) => Ok(result),
            Ok(_) => Err(invalid(&self.source, "the result is not true or false")),
            Err(e) => Err(invalid(&self.source, e)),
        }
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
        let scope = SqlScope {
            alias: "t",
            columns,
            user: &Value::Null,
        };
        self.to_sql(&scope, &mut vec![]).map(|_| ())
    }
}

enum Operand {
    Column(String, ColumnType),
    Value(Value),
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

impl Compiler<'_, '_> {
    fn bind(&mut self, bind: Bind) -> String {
        self.binds.push(bind);
        format!("${}", self.binds.len())
    }

    fn column(&self, name: &str, ty: ColumnType) -> String {
        let _ = ty;
        format!("{}.{}", self.scope.alias, name)
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
                        _ => return Err("only fields of `record` and `user` can be used".into()),
                    }
                };
                path.reverse();
                match root {
                    "record" => match path.as_slice() {
                        [field] => match self.scope.columns.get(*field) {
                            Some(ty) => Ok(Operand::Column((*field).to_owned(), *ty)),
                            None => Err(format!(
                                "the record has no field `{field}` that a rule can use"
                            )),
                        },
                        _ => Err(format!("`record.{}` is not a plain field", path.join("."))),
                    },
                    "user" => {
                        let mut value = self.scope.user;
                        for key in &path {
                            value = value.get(key).unwrap_or(&Value::Null);
                        }
                        Ok(Operand::Value(value.clone()))
                    }
                    other => Err(format!("unknown name `{other}`; use `record` or `user`")),
                }
            }
            Expr::Call(call) if call.target.is_none() => {
                let mut args = Vec::with_capacity(call.args.len());
                for arg in &call.args {
                    args.push(self.operand(&arg.expr)?);
                }
                let mut args = args.into_iter();
                let name = call.func_name.as_str();
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
                    _ => Err(format!("`{name}` cannot be used in a rule")),
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
            Operand::Column(name, ColumnType::Bool) => Ok(self.column(&name, ColumnType::Bool)),
            Operand::Column(name, _) => Err(format!("field `{name}` is not true or false")),
            Operand::Value(Value::Bool(b)) => {
                Ok(format!("{}::boolean", self.bind(Bind::Bool(Some(b)))))
            }
            // An attribute the user does not have: neither true nor false.
            Operand::Value(Value::Null) => Ok(format!("{}::boolean", self.bind(Bind::Bool(None)))),
            Operand::Value(_) => {
                Err("a value that is not true or false is used as a condition".into())
            }
        }
    }

    fn compare(&mut self, left: Operand, right: Operand, sql_op: &str) -> Compiled<Operand> {
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
            (Operand::Column(a, at), Operand::Column(b, bt)) => {
                format!("({} {sql_op} {})", self.column(&a, at), self.column(&b, bt))
            }
            (Operand::Column(name, ty), Operand::Value(value)) => {
                let column = self.column(&name, ty);
                if value.is_null() {
                    return null_test(column);
                }
                let param = self.bind(Bind::Text(Some(scalar_text(&value)?)));
                format!("({column} {sql_op} {param}::{})", ty.pg())
            }
            (Operand::Value(value), Operand::Column(name, ty)) => {
                let column = self.column(&name, ty);
                if value.is_null() {
                    return null_test(column);
                }
                let param = self.bind(Bind::Text(Some(scalar_text(&value)?)));
                format!("({param}::{} {sql_op} {column})", ty.pg())
            }
            (Operand::Value(a), Operand::Value(b)) => {
                // A missing attribute compares as unknown, which filters the row out.
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
                // An attribute the user does not have: an empty list, so nothing matches.
                Value::Null => Ok(vec![]),
                _ => Err("the right side of `in` must be a list".into()),
            }
        };
        let sql = match (item, list) {
            (Operand::Column(name, ty), Operand::Value(list)) => {
                if ty == ColumnType::TextArray {
                    return Err(format!("field `{name}` is itself a list"));
                }
                let param = self.bind(Bind::Texts(texts(&list)?));
                format!("({} = ANY({param}::{}[]))", self.column(&name, ty), ty.pg())
            }
            (Operand::Value(item), Operand::Column(name, ColumnType::TextArray)) => {
                let item = if item.is_null() {
                    None
                } else {
                    Some(scalar_text(&item)?)
                };
                let param = self.bind(Bind::Text(item));
                format!(
                    "({param}::text = ANY({}))",
                    self.column(&name, ColumnType::TextArray)
                )
            }
            (Operand::Column(a, at), Operand::Column(b, ColumnType::TextArray)) => {
                format!(
                    "({}::text = ANY({}))",
                    self.column(&a, at),
                    self.column(&b, ColumnType::TextArray)
                )
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
        ])
    }

    fn sql(rule: &str, user: Value) -> (String, Vec<Bind>) {
        let columns = columns();
        let scope = SqlScope {
            alias: "t",
            columns: &columns,
            user: &user,
        };
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
        assert_eq!(sql, "((t.warehouse = ANY($1::text[])) OR $2::boolean)");
        assert_eq!(binds, [Bind::Texts(vec![]), Bind::Bool(None)]);
    }

    #[test]
    fn refuses_what_cannot_filter() {
        assert!(refused("record.nope == 1").contains("no field `nope`"));
        assert!(refused("record.tags.all(t, t != 'x')").contains("cannot be used in a rule"));
        assert!(refused("record.warehouse.startsWith('W')").contains("cannot be used in a rule"));
        assert!(refused("record.qty + 1 > 2").contains("cannot be used in a rule"));
        assert!(refused("record.warehouse").contains("not true or false"));
        assert!(refused("record.qty > null").contains("null can only be tested"));
        assert!(refused("record.a.b == 1").contains("not a plain field"));
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
}
