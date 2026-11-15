use monty::MontyRun;
use monty_types::{CompileOptions, MontyObject, PrintWriter, ResourceTracker};

const RULE: &str = r#"record["warehouse"] in user["warehouses"] and (record["amount"] < 10000 or user["is_manager"])"#;

fn s(v: &str) -> MontyObject {
    MontyObject::string(v)
}

fn main() {
    let user = MontyObject::dict([
        (s("warehouses"), MontyObject::list([s("W1"), s("W2")])),
        (s("is_manager"), MontyObject::bool(false)),
    ]);
    let mut runner = MontyRun::new(
        RULE.to_owned(),
        "rule.py",
        vec!["record".to_owned(), "user".to_owned()],
        CompileOptions::default(),
    )
    .expect("rule parses");
    for warehouse in ["W1", "W3"] {
        let record = MontyObject::dict([(s("warehouse"), s(warehouse)), (s("amount"), MontyObject::int(500))]);
        let result = runner.run(vec![record, user.clone()], ResourceTracker::default(), PrintWriter::Disabled);
        println!("eval warehouse={warehouse} -> {result:?}");
    }
    // Monty exposes a runner, not a syntax tree: there is nothing here to turn into SQL.
    // It also accepts whole programs, which a rule must not be.
    let program = "x = 0\nwhile x < 3:\n    x += 1\nx";
    let accepted = MontyRun::new(program.to_owned(), "p.py", vec![], CompileOptions::default()).is_ok();
    println!("a multi-statement program with a loop is accepted as a rule: {accepted}");
}
