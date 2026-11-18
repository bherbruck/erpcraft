//! Parsing that cannot overflow the stack.
//!
//! A rule or an expression is text that any signed-in user can send (the
//! `where` of a list) or store (a permission rule, a policy). The engine
//! and this crate both walk the parsed tree by recursion, one stack frame
//! or more per level, and a stack overflow does not fail one request: it
//! aborts the whole server. So nothing recursive may ever see a source or
//! a tree whose size the caller chose freely. Three limits see to that:
//!
//! - **Length.** A source longer than [`MAX_SOURCE_LEN`] is refused before
//!   it is parsed.
//! - **The parser's own stack.** The CEL parser limits how deeply brackets
//!   nest (96 levels) but pays a great deal of stack for each level, and
//!   has no limit on chains such as `1+1+1+...`, which cost stack per term.
//!   It therefore runs on a thread of its own with [`PARSE_STACK`] bytes of
//!   stack, whatever stack the caller happens to be on.
//! - **Depth.** A parsed tree deeper than [`MAX_DEPTH`] is refused, by a
//!   check that uses a loop and not recursion, and is dropped on the
//!   parsing thread, because dropping a tree recurses as well. What comes
//!   back to the caller is at most [`MAX_DEPTH`] levels deep, so every
//!   later walk (checking, compiling to SQL, evaluating, dropping) is
//!   bounded on any stack.
//!
//! What was measured, with cel 0.16 on x86-64 Linux, as the smallest stack
//! on which parsing and dropping the worst source of each shape that fits
//! in [`MAX_SOURCE_LEN`] survives:
//!
//! | shape                                   | unoptimized | optimized |
//! |-----------------------------------------|-------------|-----------|
//! | nested `[`, `(`, `f(`, `{1:` (96 levels) | 15 to 17 MB | 0.7 to 0.9 MB |
//! | `1+1+...`, 2,000 terms                  | 3.1 MB      | 0.8 MB    |
//! | `a.b.b...`, 2,000 fields                | 3.7 MB      | 0.9 MB    |
//! | `a[0][0]...`, `a.f().f()...`, `!!!...`  | under 2.2 MB | under 0.6 MB |
//!
//! The thread is paid once for each source: what parsing gave is kept, see
//! the `cache` module.
//!
//! Without the thread, 11 nested parentheses (23 characters) were enough
//! to abort an unoptimized build on a 2 MB stack, which is what a request
//! runs on. So no length limit alone could have made the parser safe.

use std::sync::{Arc, LazyLock};

use cel::common::ast::{EntryExpr, Expr};
use cel::{ParseErrors, Program};
use erpcraft_core::{Error, Result};

use crate::cache::{Bounds, Cache, Origin, Refusal};

/// The longest source accepted, in bytes. A condition a person writes is
/// a line or two; the longest in this repository is under 200 characters.
/// 4,000 leaves room for a long list of codes, and keeps the worst the
/// parser can be made to do within a quarter of [`PARSE_STACK`].
pub(crate) const MAX_SOURCE_LEN: usize = 4_000;

/// The deepest tree accepted: the number of nested operations, counting
/// the value at the bottom. `a && b && c` is three levels deep, because
/// a chain nests to the left. 64 is far beyond what a person writes and
/// costs a recursive walk a few kilobytes of stack per level at most.
pub(crate) const MAX_DEPTH: usize = 64;

/// The stack the parser gets: about four times the most it was seen to
/// need in an unoptimized build (see the module documentation). It is
/// address space, not memory: only the pages touched are ever used.
const PARSE_STACK: usize = 64 * 1024 * 1024;

/// Refuse a source that is too long. `what` is "a rule" or "an expression".
pub(crate) fn check_length(source: &str, what: &str) -> Result<()> {
    if source.len() > MAX_SOURCE_LEN {
        return Err(Error::validation(format!(
            "{what} can be at most {MAX_SOURCE_LEN} characters long; this one has {}",
            source.chars().count()
        )));
    }
    Ok(())
}

/// The one cache of this process. `ERPCRAFT_RULE_CACHE=off` in the
/// environment makes it keep nothing, for measuring what it saves and as
/// a way out should it ever be suspected.
static CACHE: LazyLock<Cache> =
    LazyLock::new(|| match std::env::var("ERPCRAFT_RULE_CACHE").as_deref() {
        Ok("off") => Cache::disabled(),
        _ => Cache::new(Bounds::DEFAULT),
    });

/// Parse a source that passed [`check_length`], or take what parsing it
/// gave before. The error is the reason, for the caller to put into its
/// own message.
pub(crate) fn compile(source: &str, origin: Origin) -> std::result::Result<Arc<Program>, String> {
    CACHE.compile(source, origin, uncached)
}

/// Parse a source that passed [`check_length`], on a stack of its own.
pub(crate) fn uncached(source: &str) -> std::result::Result<Program, Refusal> {
    debug_assert!(source.len() <= MAX_SOURCE_LEN);
    let lasting = |why: String| Refusal { why, lasting: true };
    let parse = || {
        let program = Program::compile(source).map_err(|e| lasting(unreadable(&e)))?;
        if deeper_than(&program.expression().expr, MAX_DEPTH) {
            // `program` is dropped here, on the large stack.
            return Err(lasting(format!(
                "it is nested more than {MAX_DEPTH} levels deep; split it into simpler conditions"
            )));
        }
        Ok(program)
    };
    // Not being able to start the parser says nothing about the source.
    let passing = |why: String| Refusal {
        why,
        lasting: false,
    };
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("rule-parser".into())
            .stack_size(PARSE_STACK)
            .spawn_scoped(scope, parse)
            .map_err(|e| passing(format!("it could not be parsed: {e}")))?
            .join()
            .unwrap_or_else(|_| Err(passing("it could not be parsed".into())))
    })
}

/// What the parser objected to, as one sentence: what and where. The
/// parser reports in the terms of its grammar, with the list of every
/// token it would have accepted; a person needs the place and the thing.
/// Only its first complaint is told: the ones after it are mostly
/// consequences of the first.
fn unreadable(errors: &ParseErrors) -> String {
    // A token the parser's own grammar names, as a person would write it.
    fn token(quoted: &str) -> String {
        let token = quoted.trim();
        let token = token
            .strip_prefix('\'')
            .and_then(|t| t.strip_suffix('\''))
            .unwrap_or(token);
        match token {
            "<EOF>" => "the end".into(),
            other => format!("`{}`", other.trim()),
        }
    }
    // The part of a message before the list of expected tokens.
    fn subject(rest: &str) -> &str {
        rest.split(" expecting ").next().unwrap_or(rest)
    }
    if errors
        .errors
        .iter()
        .any(|e| e.msg.contains("Recursion limit"))
    {
        return "it cannot be read: brackets are nested too deeply".into();
    }
    let Some(error) = errors
        .errors
        .iter()
        .find(|e| e.msg.starts_with("Syntax error: "))
        .or(errors.errors.first())
    else {
        return "it cannot be read".into();
    };
    let message = error.msg.strip_prefix("Syntax error: ").unwrap_or("");
    let what = if let Some(rest) = message.strip_prefix("token recognition error at: ") {
        let found = token(rest);
        if found.starts_with("`'") || found.starts_with("`\"") {
            "a text is not closed with a quote".to_owned()
        } else if found == "`=`" {
            "`=` cannot be used; write `==` to compare".to_owned()
        } else {
            format!("{found} cannot be used")
        }
    } else if let Some(rest) = message.strip_prefix("missing ") {
        match rest.split_once(" at ") {
            Some((missing, _)) => format!("{} is missing", token(missing)),
            None => "something is missing".to_owned(),
        }
    } else if let Some(rest) = message
        .strip_prefix("extraneous input ")
        .or_else(|| message.strip_prefix("mismatched input "))
        .or_else(|| message.strip_prefix("no viable alternative at input "))
    {
        match token(subject(rest)).as_str() {
            "the end" => "it ends too early".to_owned(),
            found => format!("{found} is not expected"),
        }
    } else {
        // A complaint in no form known here is not passed on: it is
        // written for the parser's authors.
        "something is not in its place".to_owned()
    };
    let (line, column) = error.pos;
    if line < 1 || column < 1 {
        return format!("it cannot be read: {what}");
    }
    format!("it cannot be read: {what}, at line {line}, column {column}")
}

/// Is the tree more than `limit` levels deep? A loop over an explicit
/// list of what is left to visit, so the answer costs no stack however
/// deep the tree is, and it stops at the first branch that is too deep.
fn deeper_than(root: &Expr, limit: usize) -> bool {
    let mut pending = vec![(root, 1usize)];
    while let Some((expr, depth)) = pending.pop() {
        if depth > limit {
            return true;
        }
        let below = depth + 1;
        let mut push = |expr| pending.push((expr, below));
        match expr {
            Expr::Unspecified | Expr::Ident(_) | Expr::Literal(_) => {}
            Expr::Call(call) => {
                if let Some(target) = &call.target {
                    push(&target.expr);
                }
                call.args.iter().for_each(|arg| push(&arg.expr));
            }
            Expr::Comprehension(comprehension) => {
                push(&comprehension.iter_range.expr);
                push(&comprehension.accu_init.expr);
                push(&comprehension.loop_cond.expr);
                push(&comprehension.loop_step.expr);
                push(&comprehension.result.expr);
            }
            Expr::List(list) => list.elements.iter().for_each(|e| push(&e.expr)),
            Expr::Select(select) => push(&select.operand.expr),
            Expr::Map(map) => map.entries.iter().for_each(|entry| match &entry.expr {
                EntryExpr::MapEntry(entry) => {
                    push(&entry.key.expr);
                    push(&entry.value.expr);
                }
                EntryExpr::StructField(field) => push(&field.value.expr),
            }),
            Expr::Struct(fields) => fields.entries.iter().for_each(|entry| match &entry.expr {
                EntryExpr::MapEntry(entry) => {
                    push(&entry.key.expr);
                    push(&entry.value.expr);
                }
                EntryExpr::StructField(field) => push(&field.value.expr),
            }),
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Expression, Rule};

    /// A stack far smaller than any request runs on. Without the limits
    /// each of these sources overflows it, which would abort the test run.
    const SMALL_STACK: usize = 256 * 1024;

    fn on_a_small_stack<T: Send>(work: impl FnOnce() -> T + Send) -> T {
        on_a_stack_of(SMALL_STACK, work)
    }

    fn on_a_stack_of<T: Send>(bytes: usize, work: impl FnOnce() -> T + Send) -> T {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(bytes)
                .spawn_scoped(scope, work)
                .unwrap()
                .join()
                .unwrap()
        })
    }

    /// Both entry points, on the small stack: the error each one gives.
    fn refused(source: &str) -> [String; 2] {
        on_a_small_stack(|| {
            [
                Rule::parse(source).unwrap_err().to_string(),
                Expression::parse(source, &["record", "user"])
                    .unwrap_err()
                    .to_string(),
            ]
        })
    }

    fn nested(open: &str, inner: &str, close: &str, levels: usize) -> String {
        format!("{}{inner}{}", open.repeat(levels), close.repeat(levels))
    }

    #[test]
    fn a_source_that_is_too_long_is_refused_before_it_is_parsed() {
        // 50,000 terms: a tree 50,000 levels deep, had it been parsed.
        let chain = vec!["1"; 50_000].join("+");
        let [rule, expression] = refused(&format!("{chain} > 0"));
        assert_eq!(
            rule,
            "a rule can be at most 4000 characters long; this one has 100003"
        );
        assert_eq!(
            expression,
            "an expression can be at most 4000 characters long; this one has 100003"
        );
        for source in [
            nested("(", "true", ")", 30_000),
            nested("[", "1", "]", 30_000),
            format!("record.a{} == 1", ".b".repeat(30_000)),
            format!("{}true", "!".repeat(30_000)),
        ] {
            for message in refused(&source) {
                assert!(message.contains("at most 4000 characters"), "{message}");
            }
        }
        // The message does not carry the source back.
        assert!(rule.len() < 100);
    }

    #[test]
    fn a_short_source_that_nests_deeply_is_refused() {
        // Each fits the length limit and is as deep as its length allows.
        let chain = |term: &str, by: &str, terms: usize| vec![term; terms].join(by);
        for source in [
            format!("{} > 0", chain("1", "+", 1_990)),
            format!("record.a{} == 1", ".b".repeat(1_900)),
            format!("record.a{} == 1", "[0]".repeat(1_300)),
            format!("record.a{} == 1", ".f()".repeat(990)),
            // One level more than the limit.
            format!("{} > 0", chain("1", "+", MAX_DEPTH)),
            // Brackets, up to and past the parser's own limit of 96.
            nested("(", "true", ")", 1_900),
            nested("[", "1", "]", 95),
            nested("[", "1", "]", 1_900),
            nested("size(", "1", ")", 95),
            nested("size(", "1", ")", 660),
            nested("{1:", "1", "}", 95),
            nested("{1:", "1", "}", 990),
            nested("[1].all(x,", "true", ")", 95),
            nested("(true?1:", "1", ")", 440),
            // Never closed.
            "(".repeat(3_990),
            "[".repeat(3_990),
        ] {
            assert!(source.len() <= MAX_SOURCE_LEN);
            for message in refused(&source) {
                assert!(
                    message.contains("nested more than 64 levels deep")
                        || message.contains("it cannot be read: "),
                    "{message}"
                );
            }
        }
        // The parser folds repeated `!` and `-` away, so these are not
        // deep, whatever they look like. They must only do no harm.
        on_a_small_stack(|| {
            for source in [
                format!("{}true", "!".repeat(3_990)),
                format!("{}true", "!".repeat(3_991)),
                format!("{}1 > 0", "-".repeat(3_990)),
                format!("{}1 > 0", "-".repeat(3_991)),
            ] {
                let _ = Rule::parse(&source);
                let _ = Expression::parse(&source, &["record"]);
            }
        });
        // The reason is readable when the tree was the problem.
        let [rule, _] = refused(&format!("{} > 0", chain("1", "+", MAX_DEPTH)));
        assert!(
            rule.ends_with(
                "it is nested more than 64 levels deep; split it into simpler conditions"
            ),
            "{rule}"
        );
    }

    /// The deepest tree that is accepted, walked every way this crate
    /// walks one, on half the stack a request has. Measured: the most any
    /// of these needs is 0.4 MB unoptimized and 45 KB optimized.
    #[test]
    fn what_is_accepted_is_shallow_enough_to_use() {
        use std::collections::BTreeMap;
        let record = serde_json::json!({ "qty": 1 });
        // Every term is looked at: none settles the answer early.
        let all = vec!["record.qty == 1"; MAX_DEPTH - 2].join(" && ");
        let sum = format!("{} > 0", vec!["record.qty"; MAX_DEPTH - 2].join(" + "));
        on_a_stack_of(1024 * 1024, || {
            let rule = Rule::parse(&all).unwrap();
            assert!(rule.matches(&record, &serde_json::json!({})).unwrap());
            let columns = BTreeMap::from([("qty".to_owned(), crate::ColumnType::Int)]);
            rule.check(&columns).unwrap();
            for source in [&all, &sum] {
                let expression = Expression::parse(source, &["record"]).unwrap();
                assert!(
                    expression
                        .is_true(&serde_json::json!({ "record": record }))
                        .unwrap()
                );
                assert_eq!(expression.fields_of("record").len(), 1);
            }
        });
        // Exactly at the limit. And a chain of `&&` or `||` is not deep
        // however long it is: the parser builds a balanced tree of it.
        let at_limit = format!("{} > 0", vec!["1"; MAX_DEPTH - 1].join("+"));
        let long = vec!["record.qty == 1"; 200].join(" && ");
        on_a_stack_of(1024 * 1024, || {
            let variables = serde_json::json!({ "record": record });
            let at_limit = Expression::parse(&at_limit, &["record"]).unwrap();
            assert!(at_limit.is_true(&variables).unwrap());
            assert!(
                Rule::parse(&long)
                    .unwrap()
                    .matches(&record, &record)
                    .unwrap()
            );
            let long = Expression::parse(&long, &["record"]).unwrap();
            assert!(long.is_true(&variables).unwrap());
        });
        // Parentheses alone add no levels: the parser's own limit of 96
        // applies to them, and on its own stack it is safe.
        on_a_small_stack(|| {
            assert!(Rule::parse(&nested("(", "true", ")", 90)).is_ok());
            assert!(Rule::parse(&nested("(", "true", ")", 100)).is_err());
        });
    }

    #[test]
    fn a_source_that_cannot_be_read_is_told_in_a_sentence() {
        for (source, why) in [
            ("(record.a == 1", "`)` is missing, at line 1, column 15"),
            (
                "record.a == 1)",
                "`)` is not expected, at line 1, column 14",
            ),
            ("record.qty >", "it ends too early, at line 1, column 13"),
            (
                "record.a == 1 &&",
                "it ends too early, at line 1, column 17",
            ),
            ("record.a # 1", "`#` cannot be used, at line 1, column 10"),
            (
                "record.a = 1",
                "`=` cannot be used; write `==` to compare, at line 1, column 10",
            ),
            (
                "record.a == 'x",
                "a text is not closed with a quote, at line 1, column 13",
            ),
            ("record.a 1", "`1` is not expected, at line 1, column 10"),
            ("record..a", "`..` is not expected, at line 1, column 8"),
            ("[1, 2", "it ends too early, at line 1, column 6"),
            (
                "record.a ==\n  ) 1",
                "`)` is not expected, at line 2, column 3",
            ),
            (
                &nested("(", "true", ")", 100),
                "brackets are nested too deeply",
            ),
        ] {
            let rule = Rule::parse(source).unwrap_err().to_string();
            let expression = Expression::parse(source, &["record"])
                .unwrap_err()
                .to_string();
            let source = source.trim();
            assert_eq!(rule, format!("rule `{source}`: it cannot be read: {why}"));
            assert_eq!(
                expression,
                format!("expression `{source}`: it cannot be read: {why}")
            );
            // Nothing of the parser's own vocabulary gets through.
            for word in ["ParseError", "Syntax error", "expecting", "EOF", "{"] {
                assert!(!rule.contains(word), "{rule}");
            }
        }
    }

    #[test]
    fn the_depth_check_counts_levels() {
        let depth = |source: &str| {
            let program = Program::compile(source).unwrap();
            let expr = &program.expression().expr;
            (1..200).find(|limit| !deeper_than(expr, *limit)).unwrap()
        };
        assert_eq!(depth("true"), 1);
        assert_eq!(depth("a.b"), 2);
        assert_eq!(depth("a.b == 1"), 3);
        assert_eq!(depth("1 + 1 + 1 + 1"), 4);
        assert_eq!(depth("[1, [2, [3]]]"), 4);
        assert_eq!(depth("a ? b : (c ? d : e.f)"), 4);
        assert_eq!(depth("{'k': [a.b]}"), 4);
        assert_eq!(depth("x.f(a.b.c)"), 4);
    }
}
