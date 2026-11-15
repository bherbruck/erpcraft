use cel::common::ast::{Expr, LiteralValue, operators as op};
use cel::{Context, Program};
use serde_json::{Value, json};

const RULE: &str = "record.warehouse in user.warehouses && (record.amount < 10000 || user.is_manager)";

/// Compile the subset of CEL that maps to a WHERE clause. Anything else is refused.
fn to_sql(e: &Expr, user: &Value, params: &mut Vec<Value>) -> Result<String, String> {
    let mut bind = |v: Value| {
        params.push(v);
        format!("${}", params.len())
    };
    match e {
        Expr::Literal(LiteralValue::Int(i)) => Ok(bind(json!(*i.inner()))),
        Expr::Literal(LiteralValue::Boolean(b)) => Ok(bind(json!(*b.inner()))),
        Expr::Literal(LiteralValue::String(s)) => Ok(bind(json!(s.inner().to_string()))),
        Expr::Select(s) if !s.test => match &s.operand.expr {
            Expr::Ident(root) if root == "record" => Ok(format!("t.{}", s.field)),
            Expr::Ident(root) if root == "user" => Ok(bind(user[&s.field].clone())),
            _ => Err(format!("unsupported field access .{}", s.field)),
        },
        Expr::Call(c) if c.target.is_none() => {
            let args: Result<Vec<String>, String> = c.args.iter().map(|a| to_sql(&a.expr, user, params)).collect();
            let a = args?;
            let binary = |sql_op: &str| Ok(format!("({} {} {})", a[0], sql_op, a[1]));
            match c.func_name.as_str() {
                op::LOGICAL_AND => binary("AND"),
                op::LOGICAL_OR => binary("OR"),
                op::LOGICAL_NOT => Ok(format!("(NOT {})", a[0])),
                op::EQUALS => binary("="),
                op::NOT_EQUALS => binary("<>"),
                op::LESS => binary("<"),
                op::LESS_EQUALS => binary("<="),
                op::GREATER => binary(">"),
                op::GREATER_EQUALS => binary(">="),
                op::IN => Ok(format!("({} = ANY({}))", a[0], a[1])),
                other => Err(format!("unsupported function {other}")),
            }
        }
        other => Err(format!("unsupported expression {other:?}")),
    }
}

fn main() {
    let user = json!({"warehouses": ["W1", "W2"], "is_manager": false});
    let program = Program::compile(RULE).expect("rule parses");

    for record in [json!({"warehouse": "W1", "amount": 500}), json!({"warehouse": "W3", "amount": 500})] {
        let mut ctx = Context::default();
        ctx.add_variable("user", user.clone()).unwrap();
        ctx.add_variable("record", record.clone()).unwrap();
        println!("eval {record} -> {:?}", program.execute(&ctx));
    }

    let mut params = vec![];
    println!("sql   {:?}", to_sql(&program.expression().expr, &user, &mut params));
    println!("binds {}", json!(params));

    let bad = Program::compile("record.items.all(i, i.qty > 0)").unwrap();
    println!("refused {:?}", to_sql(&bad.expression().expr, &user, &mut vec![]));
}
