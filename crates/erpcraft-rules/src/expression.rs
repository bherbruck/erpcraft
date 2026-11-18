//! Expressions that are only ever evaluated in memory.
//!
//! A [`Rule`](crate::Rule) must also compile to SQL, so it accepts little.
//! An [`Expression`] never becomes SQL, so it can say more: arithmetic and
//! comparisons between any two values, as a business policy needs.
//!
//! ```text
//! facts.over_percent <= 5 || (user.is_manager && facts.over_by * facts.unit_price < 100)
//! ```
//!
//! The syntax is the same CEL. What is supported:
//!
//! - fields of the names the caller allows, such as `facts.ordered` and
//!   `user.warehouses`, and `has(user.x)` to ask whether one is there;
//! - literals: numbers, text, `true`, `false`, `null`, and lists of them;
//! - `+ - * / %` and unary `-` on numbers; `+` also joins two texts;
//! - `== != < <= > >=` between two numbers or two texts;
//! - `&&`, `||`, `!`, `in`, and `condition ? a : b`.
//!
//! Every number is a floating-point number here, whatever it was before:
//! an integer, or an exact decimal that arrived as a number. That suits a
//! threshold or a percentage and nothing else. An expression decides
//! whether something may happen; it must never work out an amount that is
//! then stored.
//!
//! Evaluation is strict. A field that is missing, a comparison between a
//! number and a text, a division by zero: each is an error, not `false`.
//! So is comparing a value that turns out to be `null` with anything:
//! `user.restricted != true` is not true for a user whose `restricted` is
//! null, it is undecided, exactly as when the attribute is missing. The
//! one thing that can be asked about a null is whether it is one, by
//! writing the word: `facts.note == null`, `facts.note != null`.
//! The caller decides what an undecidable expression means, and it must be
//! the outcome that loosens nothing. `&&` and `||` look at their right side
//! only when the left side does not settle the answer.

use std::collections::BTreeSet;
use std::sync::Arc;

use cel::Program;
use cel::common::ast::{Expr, LiteralValue, SelectExpr, operators as op};
use erpcraft_core::{Error, Result};

use crate::cache::Origin;
use serde_json::Value;

/// A parsed expression over the names it was parsed for.
#[derive(Debug)]
pub struct Expression {
    source: String,
    /// Shared with every other rule and expression of the same source.
    program: Arc<Program>,
}

type Checked<T> = std::result::Result<T, String>;

fn invalid(source: &str, why: impl std::fmt::Display) -> Error {
    Error::validation(format!("expression `{source}`: {why}"))
}

/// A value while an expression is evaluated.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    List(Vec<Val>),
}

impl Val {
    fn kind(&self) -> &'static str {
        match self {
            Val::Null => "null",
            Val::Bool(_) => "true or false",
            Val::Number(_) => "a number",
            Val::Text(_) => "a text",
            Val::List(_) => "a list",
        }
    }

    fn from_json(value: &Value, what: &str) -> Checked<Val> {
        match value {
            Value::Null => Ok(Val::Null),
            Value::Bool(b) => Ok(Val::Bool(*b)),
            Value::Number(n) => n
                .as_f64()
                .map(Val::Number)
                .ok_or_else(|| format!("`{what}` is not a number that can be used")),
            Value::String(s) => Ok(Val::Text(s.clone())),
            Value::Array(items) => items
                .iter()
                .map(|item| Val::from_json(item, what))
                .collect::<Checked<Vec<Val>>>()
                .map(Val::List),
            Value::Object(_) => Err(format!(
                "`{what}` is not a single value; use one of its fields"
            )),
        }
    }

    fn into_json(self) -> Value {
        match self {
            Val::Null => Value::Null,
            Val::Bool(b) => Value::from(b),
            Val::Number(n) => Value::from(n),
            Val::Text(s) => Value::from(s),
            Val::List(items) => Value::Array(items.into_iter().map(Val::into_json).collect()),
        }
    }
}

/// `_+_` as a person writes it.
fn spelled(func: &str) -> &str {
    match func {
        op::IN => "in",
        op::CONDITIONAL => "?:",
        other => other.trim_matches('_'),
    }
}

/// The root name and the fields selected from it: `user.a.b` is
/// `("user", ["a", "b"])`. `None` when the expression is not such a path.
fn path(expr: &Expr) -> Option<(&str, Vec<&str>)> {
    let mut fields = vec![];
    let mut inner = expr;
    loop {
        match inner {
            Expr::Ident(name) => {
                fields.reverse();
                return Some((name.as_str(), fields));
            }
            Expr::Select(select) if !select.test => {
                fields.push(select.field.as_str());
                inner = &select.operand.expr;
            }
            _ => return None,
        }
    }
}

/// The path a select spells, whether it reads the field or only tests it.
fn select_path(select: &SelectExpr) -> Option<(&str, Vec<&str>)> {
    let (root, mut fields) = path(&select.operand.expr)?;
    fields.push(select.field.as_str());
    Some((root, fields))
}

const UNARY: [&str; 2] = [op::LOGICAL_NOT, op::NEGATE];
const BINARY: [&str; 14] = [
    op::LOGICAL_AND,
    op::LOGICAL_OR,
    op::EQUALS,
    op::NOT_EQUALS,
    op::LESS,
    op::LESS_EQUALS,
    op::GREATER,
    op::GREATER_EQUALS,
    op::ADD,
    op::SUBSTRACT,
    op::MULTIPLY,
    op::DIVIDE,
    op::MODULO,
    op::IN,
];

impl Expression {
    /// Parse an expression that a caller sent with a request. It may
    /// refer only to fields of `names`, for example `["facts", "user"]`.
    /// Everything this crate cannot evaluate is refused here, so a parsed
    /// expression can fail later only for what the values turn out to be.
    ///
    /// What parsing a source gave is remembered; see
    /// [`Rule::parse`](crate::Rule::parse).
    pub fn parse(source: &str, names: &[&str]) -> Result<Expression> {
        Expression::parsed(source, names, Origin::Sent)
    }

    /// Parse an expression the installation keeps, such as the condition
    /// of a policy rule; see [`Rule::parse_stored`](crate::Rule::parse_stored).
    pub fn parse_stored(source: &str, names: &[&str]) -> Result<Expression> {
        Expression::parsed(source, names, Origin::Stored)
    }

    fn parsed(source: &str, names: &[&str], origin: Origin) -> Result<Expression> {
        crate::parse::check_length(source, "an expression")?;
        let source = source.trim();
        if source.is_empty() {
            return Err(Error::validation("an expression cannot be empty"));
        }
        // Length and depth are limited here, before anything recursive
        // sees the expression; see the `parse` module.
        let program = crate::parse::compile(source, origin).map_err(|why| invalid(source, why))?;
        check(&program.expression().expr, names).map_err(|why| invalid(source, why))?;
        Ok(Expression {
            source: source.to_owned(),
            program,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// The fields of `name` the expression reads: `facts.a > facts.b`
    /// reads `a` and `b` of `facts`.
    pub fn fields_of(&self, name: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        collect(&self.program.expression().expr, name, &mut found);
        found
    }

    /// Work the expression out. `variables` is an object holding one object
    /// per name the expression was parsed for. An error means the answer
    /// could not be decided; see the module documentation.
    pub fn evaluate(&self, variables: &Value) -> Result<Value> {
        eval(&self.program.expression().expr, variables)
            .map(Val::into_json)
            .map_err(|why| invalid(&self.source, why))
    }

    /// [`Expression::evaluate`] for a condition: an answer that is not true
    /// or false is an error too.
    pub fn is_true(&self, variables: &Value) -> Result<bool> {
        match eval(&self.program.expression().expr, variables) {
            Ok(Val::Bool(answer)) => Ok(answer),
            Ok(other) => Err(invalid(
                &self.source,
                format!("the result is {}, not true or false", other.kind()),
            )),
            Err(why) => Err(invalid(&self.source, why)),
        }
    }
}

fn check(expr: &Expr, names: &[&str]) -> Checked<()> {
    match expr {
        Expr::Literal(LiteralValue::Bytes(_)) => {
            Err("byte strings cannot be used in an expression".into())
        }
        Expr::Literal(_) => Ok(()),
        Expr::List(list) => list
            .elements
            .iter()
            .try_for_each(|element| check(&element.expr, names)),
        Expr::Ident(name) if names.contains(&name.as_str()) => Err(format!(
            "`{name}` alone is not a value; use one of its fields"
        )),
        Expr::Ident(name) => Err(unknown(name, names)),
        Expr::Select(select) => {
            // `has(a.b)` is a select that only tests; it names a path too.
            match select_path(select) {
                Some((root, _)) if names.contains(&root) => Ok(()),
                Some((root, _)) => Err(unknown(root, names)),
                None => Err(format!("only fields of {} can be used", listed(names))),
            }
        }
        Expr::Call(call) if call.target.is_none() => {
            let name = call.func_name.as_str();
            let arity = if UNARY.contains(&name) {
                1
            } else if BINARY.contains(&name) {
                2
            } else if name == op::CONDITIONAL {
                3
            } else {
                return Err(format!(
                    "`{}` cannot be used in an expression",
                    spelled(name)
                ));
            };
            if call.args.len() != arity {
                return Err(format!(
                    "`{}` is used with the wrong number of values",
                    spelled(name)
                ));
            }
            call.args.iter().try_for_each(|arg| check(&arg.expr, names))
        }
        Expr::Call(call) => Err(format!(
            "`{}` cannot be used in an expression",
            call.func_name
        )),
        _ => Err("this kind of expression cannot be used".into()),
    }
}

fn listed(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(" and ")
}

fn unknown(name: &str, names: &[&str]) -> String {
    format!(
        "unknown name `{name}`; use {}",
        listed(names).replace(" and ", " or ")
    )
}

fn collect(expr: &Expr, name: &str, found: &mut BTreeSet<String>) {
    match expr {
        Expr::List(list) => list
            .elements
            .iter()
            .for_each(|element| collect(&element.expr, name, found)),
        Expr::Select(select) => {
            // Walk down to the root; the field selected from it is the
            // last one met on the way.
            let mut first = select.field.as_str();
            let mut inner = &select.operand.expr;
            loop {
                match inner {
                    Expr::Ident(root) => {
                        if root == name {
                            found.insert(first.to_owned());
                        }
                        break;
                    }
                    Expr::Select(next) => {
                        first = next.field.as_str();
                        inner = &next.operand.expr;
                    }
                    other => {
                        collect(other, name, found);
                        break;
                    }
                }
            }
        }
        Expr::Call(call) => call
            .args
            .iter()
            .for_each(|arg| collect(&arg.expr, name, found)),
        _ => {}
    }
}

/// The value at a path of field names, or `None` when a step is missing.
fn lookup<'v>(variables: &'v Value, root: &str, fields: &[&str]) -> Option<&'v Value> {
    let mut value = variables.get(root)?;
    for field in fields {
        value = value.as_object()?.get(*field)?;
    }
    Some(value)
}

/// Decide a condition for [`Rule::matches`](crate::Rule::matches), with
/// the strictness of an expression. `Err` is the reason it is undecided.
pub(crate) fn decide(expr: &Expr, variables: &Value) -> Checked<bool> {
    match eval(expr, variables)? {
        Val::Bool(answer) => Ok(answer),
        other => Err(format!("the result is {}, not true or false", other.kind())),
    }
}

fn eval(expr: &Expr, variables: &Value) -> Checked<Val> {
    match expr {
        Expr::Literal(literal) => Ok(match literal {
            LiteralValue::Boolean(b) => Val::Bool(*b.inner()),
            LiteralValue::Int(i) => Val::Number(*i.inner() as f64),
            LiteralValue::UInt(u) => Val::Number(*u.inner() as f64),
            LiteralValue::Double(d) => Val::Number(*d.inner()),
            LiteralValue::String(s) => Val::Text(s.inner().to_string()),
            LiteralValue::Null => Val::Null,
            LiteralValue::Bytes(_) => return Err("byte strings cannot be used".into()),
        }),
        Expr::List(list) => list
            .elements
            .iter()
            .map(|element| eval(&element.expr, variables))
            .collect::<Checked<Vec<Val>>>()
            .map(Val::List),
        Expr::Select(select) => {
            let (root, fields) = select_path(select).ok_or("only fields of a name can be used")?;
            let found = lookup(variables, root, &fields);
            if select.test {
                return Ok(Val::Bool(found.is_some()));
            }
            let what = format!("{root}.{}", fields.join("."));
            match found {
                Some(value) => Val::from_json(value, &what),
                None => Err(format!("`{what}` is not there")),
            }
        }
        Expr::Call(call) if call.target.is_none() => {
            let name = call.func_name.as_str();
            let arg = |index: usize| -> Checked<Val> {
                let arg = call.args.get(index).ok_or("a value is missing")?;
                eval(&arg.expr, variables)
            };
            let truth = |index: usize| -> Checked<bool> {
                match arg(index)? {
                    Val::Bool(b) => Ok(b),
                    other => Err(format!(
                        "`{}` needs true or false, not {}",
                        spelled(name),
                        other.kind()
                    )),
                }
            };
            match name {
                op::LOGICAL_NOT => Ok(Val::Bool(!truth(0)?)),
                op::LOGICAL_AND => Ok(Val::Bool(truth(0)? && truth(1)?)),
                op::LOGICAL_OR => Ok(Val::Bool(truth(0)? || truth(1)?)),
                op::CONDITIONAL => {
                    if truth(0)? {
                        arg(1)
                    } else {
                        arg(2)
                    }
                }
                op::NEGATE => match arg(0)? {
                    Val::Number(n) => Ok(Val::Number(-n)),
                    other => Err(format!("`-` needs a number, not {}", other.kind())),
                },
                op::EQUALS | op::NOT_EQUALS => {
                    let written_null = |index: usize| {
                        matches!(
                            call.args.get(index).map(|arg| &arg.expr),
                            Some(Expr::Literal(LiteralValue::Null))
                        )
                    };
                    let equal = match (written_null(0), written_null(1)) {
                        // `x == null` asks whether x is null. It is the
                        // only question a null answers.
                        (true, _) => arg(1)? == Val::Null,
                        (_, true) => arg(0)? == Val::Null,
                        _ => same(&arg(0)?, &arg(1)?)?,
                    };
                    Ok(Val::Bool(equal == (name == op::EQUALS)))
                }
                op::LESS | op::LESS_EQUALS | op::GREATER | op::GREATER_EQUALS => {
                    let order = match (arg(0)?, arg(1)?) {
                        (Val::Number(a), Val::Number(b)) => a.partial_cmp(&b),
                        (Val::Text(a), Val::Text(b)) => Some(a.cmp(&b)),
                        (a, b) => {
                            return Err(format!(
                                "`{}` cannot compare {} with {}",
                                spelled(name),
                                a.kind(),
                                b.kind()
                            ));
                        }
                    };
                    let order = order.ok_or("a number that is not a number cannot be compared")?;
                    Ok(Val::Bool(match name {
                        op::LESS => order.is_lt(),
                        op::LESS_EQUALS => order.is_le(),
                        op::GREATER => order.is_gt(),
                        _ => order.is_ge(),
                    }))
                }
                op::ADD | op::SUBSTRACT | op::MULTIPLY | op::DIVIDE | op::MODULO => {
                    let result = match (arg(0)?, arg(1)?) {
                        (Val::Text(a), Val::Text(b)) if name == op::ADD => {
                            return Ok(Val::Text(a + &b));
                        }
                        (Val::Number(a), Val::Number(b)) => match name {
                            op::ADD => a + b,
                            op::SUBSTRACT => a - b,
                            op::MULTIPLY => a * b,
                            _ if b == 0.0 => return Err("division by zero".into()),
                            op::DIVIDE => a / b,
                            _ => a % b,
                        },
                        (a, b) => {
                            return Err(format!(
                                "`{}` needs two numbers, not {} and {}",
                                spelled(name),
                                a.kind(),
                                b.kind()
                            ));
                        }
                    };
                    if result.is_finite() {
                        Ok(Val::Number(result))
                    } else {
                        Err("the result is too large to be a number".into())
                    }
                }
                op::IN => match (arg(0)?, arg(1)?) {
                    (item, Val::List(items)) => {
                        let mut found = false;
                        for candidate in &items {
                            found = found || same(&item, candidate)?;
                        }
                        Ok(Val::Bool(found))
                    }
                    (_, other) => Err(format!(
                        "the right side of `in` must be a list, not {}",
                        other.kind()
                    )),
                },
                other => Err(format!("`{}` cannot be used", spelled(other))),
            }
        }
        _ => Err("this kind of expression cannot be used".into()),
    }
}

/// Are two values equal? Only values of one kind can be compared, and a
/// null cannot be compared at all: whether it equals or differs from
/// something is not known, as for a value that is missing. Answering
/// "they differ" would let `user.restricted != true` pass for a user whose
/// attribute is null.
fn same(a: &Val, b: &Val) -> Checked<bool> {
    match (a, b) {
        (Val::Null, _) | (_, Val::Null) => Err(
            "a value that is null cannot be compared; ask `== null` or `!= null` about it first"
                .into(),
        ),
        (Val::Bool(a), Val::Bool(b)) => Ok(a == b),
        (Val::Number(a), Val::Number(b)) => Ok(a == b),
        (Val::Text(a), Val::Text(b)) => Ok(a == b),
        (Val::List(a), Val::List(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for (a, b) in a.iter().zip(b) {
                if !same(a, b)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (a, b) => Err(format!("cannot compare {} with {}", a.kind(), b.kind())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NAMES: [&str; 2] = ["facts", "user"];

    fn parse(source: &str) -> Expression {
        Expression::parse(source, &NAMES).unwrap()
    }

    fn facts(facts: Value) -> Value {
        json!({ "facts": facts, "user": { "id": "u1", "warehouses": ["W1", "W2"], "is_manager": false } })
    }

    fn truth(source: &str, variables: &Value) -> bool {
        parse(source).is_true(variables).unwrap()
    }

    fn undecided(source: &str, variables: &Value) -> String {
        match parse(source).is_true(variables) {
            Err(e) => e.to_string(),
            Ok(answer) => panic!("`{source}` should not be decidable, but is {answer}"),
        }
    }

    fn refused(source: &str) -> String {
        match Expression::parse(source, &NAMES) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("`{source}` should be refused"),
        }
    }

    #[test]
    fn an_expression_is_safe_to_share_between_threads() {
        fn check<T: Send + Sync>() {}
        check::<Expression>();
    }

    #[test]
    fn does_arithmetic_on_any_mix_of_numbers() {
        let v = facts(json!({ "ordered": 100, "received": 97.5, "receiving": 8 }));
        // An integer, a decimal and a literal of either kind mix freely.
        assert!(truth(
            "facts.received + facts.receiving - facts.ordered == 5.5",
            &v
        ));
        assert!(truth(
            "(facts.received + facts.receiving - facts.ordered) / facts.ordered * 100 > 5",
            &v
        ));
        assert!(truth("facts.ordered * 1.05 >= 105", &v));
        assert!(truth("facts.receiving % 3 == 2", &v));
        assert!(truth("-facts.receiving < 0", &v));
        assert_eq!(
            parse("facts.ordered / 8").evaluate(&v).unwrap(),
            json!(12.5)
        );
    }

    #[test]
    fn compares_two_facts() {
        let v = facts(json!({ "order_price": 10.0, "invoice_price": 10.2, "currency": "USD" }));
        assert!(truth("facts.invoice_price > facts.order_price", &v));
        assert!(!truth(
            "facts.invoice_price <= facts.order_price * 1.01",
            &v
        ));
        assert!(truth("facts.invoice_price <= facts.order_price * 1.02", &v));
        assert!(truth("facts.currency == 'USD'", &v));
        assert!(truth("facts.currency != 'EUR'", &v));
        assert!(truth("facts.currency < 'ZAR'", &v));
    }

    #[test]
    fn combines_conditions() {
        let v = facts(json!({ "over_percent": 4.0, "warehouse": "W2", "note": null }));
        assert!(truth(
            "facts.over_percent <= 5 && facts.warehouse in user.warehouses",
            &v
        ));
        assert!(truth("user.is_manager || !(facts.over_percent > 5)", &v));
        assert!(!truth("facts.warehouse in ['W7', 'W8']", &v));
        assert!(truth("facts.over_percent in [3, 4, 5]", &v));
        assert!(truth("facts.note == null", &v));
        assert!(!truth("facts.note != null", &v));
        assert!(truth("facts.warehouse != null", &v));
        assert!(truth("null != facts.warehouse", &v));
        assert!(!truth("facts.warehouse == null", &v));
        assert!(truth("null == null", &v));
        assert!(truth(
            "(user.is_manager ? 10 : 5) >= facts.over_percent",
            &v
        ));
        assert!(truth("true", &v));
        assert!(truth("has(user.warehouses) && !has(user.limit)", &v));
    }

    #[test]
    fn what_cannot_be_decided_is_an_error_not_an_answer() {
        let v = facts(json!({ "over_by": 2, "ordered": 0, "name": "bolt" }));
        assert!(undecided("facts.missing > 1", &v).contains("`facts.missing` is not there"));
        assert!(undecided("user.limit > 1", &v).contains("`user.limit` is not there"));
        assert!(undecided("facts.name > 1", &v).contains("cannot compare a text with a number"));
        assert!(undecided("facts.name == 1", &v).contains("cannot compare a text with a number"));
        assert!(undecided("facts.over_by / facts.ordered > 1", &v).contains("division by zero"));
        assert!(undecided("facts.over_by", &v).contains("not true or false"));
        assert!(undecided("facts.over_by && true", &v).contains("needs true or false"));
        assert!(undecided("facts.over_by in facts.name", &v).contains("must be a list"));
        assert!(undecided("facts.name * 2 > 1", &v).contains("needs two numbers"));
        // A missing value is an error wherever it is looked at ...
        assert!(undecided("facts.missing > 1 || true", &v).contains("is not there"));
        assert!(undecided("true && facts.missing > 1", &v).contains("is not there"));
        // ... and is not looked at when the left side settles the answer.
        assert!(truth("true || facts.missing > 1", &v));
        assert!(!truth("false && facts.missing > 1", &v));
    }

    #[test]
    fn a_null_is_no_more_decidable_than_a_missing_value() {
        let v = json!({
            "facts": { "note": null, "name": "bolt", "qty": 2, "tags": ["a", null], "none": null },
            "user": { "restricted": null, "warehouses": null, "is_manager": false },
        });
        let null = "a value that is null cannot be compared";
        // The case that mattered: an allow rule for everyone who is not
        // restricted must not pass someone whose attribute is null.
        assert!(undecided("user.restricted != true", &v).contains(null));
        assert!(undecided("!(user.restricted == true)", &v).contains(null));
        assert!(undecided("true != user.restricted", &v).contains(null));
        assert!(undecided("user.restricted == false", &v).contains(null));
        // The same as for an attribute that is not there at all.
        assert!(undecided("user.missing != true", &v).contains("is not there"));
        // Null against any kind of value, on either side, and two nulls
        // that are not written as `null`.
        assert!(undecided("facts.note != 'x'", &v).contains(null));
        assert!(undecided("facts.name != facts.note", &v).contains(null));
        assert!(undecided("facts.note == 1", &v).contains(null));
        assert!(undecided("facts.note == facts.none", &v).contains(null));
        assert!(undecided("facts.note != facts.none", &v).contains(null));
        assert!(undecided("facts.note < 1", &v).contains("cannot compare null"));
        // In a list too.
        assert!(undecided("facts.note in ['a', 'b']", &v).contains(null));
        assert!(undecided("'z' in facts.tags", &v).contains(null));
        assert!(undecided("facts.name in user.warehouses", &v).contains("must be a list"));
        assert!(undecided("[facts.note] == ['x']", &v).contains(null));
        // Used as a condition it is not true or false either.
        assert!(undecided("user.restricted || true", &v).contains("needs true or false"));
        assert!(undecided("!user.restricted", &v).contains("needs true or false"));
        // What can be asked of a null: whether it is one.
        assert!(truth("user.restricted == null", &v));
        assert!(!truth("user.restricted != null", &v));
        assert!(truth(
            "user.restricted != null || user.is_manager == false",
            &v
        ));
        assert!(truth(
            "user.restricted == null ? true : user.restricted != true",
            &v
        ));
        assert!(undecided("user.missing == null", &v).contains("is not there"));
    }

    #[test]
    fn names_the_fields_it_reads() {
        let expression = parse(
            "facts.over_percent <= 5 && (facts.over_by * 2 < user.limit.amount || has(facts.note)) \
             && facts.warehouse in [user.home, 'W1']",
        );
        assert_eq!(
            expression
                .fields_of("facts")
                .into_iter()
                .collect::<Vec<_>>(),
            ["note", "over_by", "over_percent", "warehouse"]
        );
        assert_eq!(
            expression.fields_of("user").into_iter().collect::<Vec<_>>(),
            ["home", "limit"]
        );
        assert!(parse("1 < 2").fields_of("facts").is_empty());
    }

    #[test]
    fn refuses_what_it_cannot_evaluate() {
        assert!(refused("record.qty > 1").contains("unknown name `record`; use `facts` or `user`"));
        assert!(refused("qty > 1").contains("unknown name `qty`"));
        assert!(refused("facts > 1").contains("`facts` alone is not a value"));
        assert!(refused("facts.tags.all(t, t != 'x')").contains("cannot be used"));
        assert!(refused("facts.name.startsWith('W')").contains("`startsWith` cannot be used"));
        assert!(refused("size(facts.tags) > 1").contains("`size` cannot be used"));
        assert!(refused("facts['name'] == 'x'").contains("cannot be used"));
        assert!(refused("{'a': 1}.a == 1").contains("only fields of `facts` and `user`"));
        assert!(refused("facts.qty >").contains("expression `facts.qty >`"));
        assert!(refused("  ").contains("cannot be empty"));
    }

    #[test]
    fn the_sql_subset_of_rules_is_not_widened() {
        use crate::{ColumnType, Rule};
        use std::collections::BTreeMap;
        let columns = BTreeMap::from([("qty".to_owned(), ColumnType::Int)]);
        // What an expression evaluates, a rule that must filter still refuses.
        assert!(Expression::parse("record.qty + 1 > 2", &["record"]).is_ok());
        let rule = Rule::parse("record.qty + 1 > 2").unwrap();
        assert!(rule.check(&columns).is_err());
        assert!(Rule::parse("facts.qty > 2").is_err());
    }
}
