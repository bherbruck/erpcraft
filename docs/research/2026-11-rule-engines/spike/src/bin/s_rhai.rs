use rhai::{Dynamic, Engine, Expr, Scope, Stmt};
use serde_json::{Value, json};

const RULE: &str = "record.warehouse in user.warehouses && (record.amount < 10000 || user.is_manager)";

fn to_sql(e: &Expr, user: &Value, params: &mut Vec<Value>) -> Result<String, String> {
    let mut bind = |v: Value| {
        params.push(v);
        format!("${}", params.len())
    };
    match e {
        Expr::IntegerConstant(i, _) => Ok(bind(json!(i))),
        Expr::BoolConstant(b, _) => Ok(bind(json!(b))),
        Expr::StringConstant(s, _) => Ok(bind(json!(s.as_str()))),
        Expr::Dot(pair, _, _) => match (&pair.lhs, &pair.rhs) {
            (Expr::Variable(var, _, _), Expr::Property(prop, _)) => match var.1.as_str() {
                "record" => Ok(format!("t.{}", prop.2)),
                "user" => Ok(bind(user[prop.2.as_str()].clone())),
                other => Err(format!("unknown variable {other}")),
            },
            _ => Err("unsupported property chain".into()),
        },
        Expr::And(items, _) | Expr::Or(items, _) => {
            let parts: Result<Vec<String>, String> = items.iter().map(|x| to_sql(x, user, params)).collect();
            let joiner = if matches!(e, Expr::And(..)) { " AND " } else { " OR " };
            Ok(format!("({})", parts?.join(joiner)))
        }
        Expr::FnCall(call, _) => {
            let args: Result<Vec<String>, String> = call.args.iter().map(|x| to_sql(x, user, params)).collect();
            let a = args?;
            match (call.name.as_str(), a.len()) {
                ("==", 2) => Ok(format!("({} = {})", a[0], a[1])),
                ("!=", 2) => Ok(format!("({} <> {})", a[0], a[1])),
                (o @ ("<" | "<=" | ">" | ">="), 2) => Ok(format!("({} {o} {})", a[0], a[1])),
                ("!", 1) => Ok(format!("(NOT {})", a[0])),
                // `x in list` is parsed as contains(list, x).
                ("contains", 2) => Ok(format!("({} = ANY({}))", a[1], a[0])),
                (other, _) => Err(format!("unsupported function {other}")),
            }
        }
        other => Err(format!("unsupported expression {other:?}")),
    }
}

fn main() {
    let user = json!({"warehouses": ["W1", "W2"], "is_manager": false});
    let mut engine = Engine::new();
    engine.set_optimization_level(rhai::OptimizationLevel::None);
    let ast = engine.compile_expression(RULE).expect("rule parses");

    for record in [json!({"warehouse": "W1", "amount": 500}), json!({"warehouse": "W3", "amount": 500})] {
        let mut scope = Scope::new();
        scope.push_dynamic("user", rhai::serde::to_dynamic(&user).unwrap());
        scope.push_dynamic("record", rhai::serde::to_dynamic(&record).unwrap());
        let result = engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast);
        println!("eval {record} -> {result:?}");
    }

    let mut params = vec![];
    let sql = match ast.statements() {
        [Stmt::Expr(expr)] => to_sql(expr, &user, &mut params),
        other => Err(format!("not a single expression: {} statements", other.len())),
    };
    println!("sql   {sql:?}");
    println!("binds {}", json!(params));

    // Is "expression only" actually restricted? Try things a rule must never do.
    for attempt in ["{ let x = 0; loop { x += 1; } }", "(|| { loop {} }).call()", "record.items.all(|i| i.qty > 0)"] {
        match engine.compile_expression(attempt) {
            Ok(_) => println!("compile_expression ACCEPTED: {attempt}"),
            Err(e) => println!("compile_expression rejected: {attempt}  ({e})"),
        }
    }
}
