// Same scaffolding as the others, no rule engine: the size baseline.
use serde_json::json;
fn main() {
    let user = json!({"warehouses": ["W1", "W2"], "is_manager": false});
    println!("{user}");
}
