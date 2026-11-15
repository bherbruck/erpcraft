use std::str::FromStr;

use cedar_policy::{
    Authorizer, Context, Entities, EntityUid, PartialEntities, PartialEntityUid, PartialRequest, PolicySet, Request,
    Schema,
};
use serde_json::{Value, json};

const SCHEMA: &str = r#"
entity User = { warehouses: Set<String>, is_manager: Bool };
entity StockEntry = { warehouse: String, amount: Long };
action read appliesTo { principal: User, resource: StockEntry };
"#;

const POLICY: &str = r#"
permit(principal, action == Action::"read", resource)
when { principal.warehouses.contains(resource.warehouse) && (resource.amount < 10000 || principal.is_manager) };
"#;

/// Compile a residual condition (Cedar's JSON policy format) to a WHERE clause.
fn to_sql(e: &Value, params: &mut Vec<Value>) -> Result<String, String> {
    let (key, body) = e.as_object().and_then(|o| o.iter().next()).ok_or("not an expression")?;
    let mut bind = |v: Value| {
        params.push(v);
        format!("${}", params.len())
    };
    let pair = |params: &mut Vec<Value>| -> Result<(String, String), String> {
        Ok((to_sql(&body["left"], params)?, to_sql(&body["right"], params)?))
    };
    match key.as_str() {
        "Value" => Ok(bind(body.clone())),
        "Set" => Ok(bind(json!(
            body.as_array().unwrap().iter().map(|x| x["Value"].clone()).collect::<Vec<_>>()
        ))),
        "." if body["left"] == json!({"Var": "resource"}) => Ok(format!("t.{}", body["attr"].as_str().unwrap())),
        "&&" => pair(params).map(|(l, r)| format!("({l} AND {r})")),
        "||" => pair(params).map(|(l, r)| format!("({l} OR {r})")),
        "==" => pair(params).map(|(l, r)| format!("({l} = {r})")),
        o @ ("<" | "<=" | ">" | ">=") => pair(params).map(|(l, r)| format!("({l} {o} {r})")),
        "contains" => pair(params).map(|(l, r)| format!("({r} = ANY({l}))")),
        "!" => Ok(format!("(NOT {})", to_sql(&body["arg"], params)?)),
        other => Err(format!("unsupported operator {other}: {body}")),
    }
}

fn main() {
    let (schema, _warnings) = Schema::from_cedarschema_str(SCHEMA).expect("schema parses");
    let policies = PolicySet::from_str(POLICY).expect("policy parses");
    let user = json!({"uid": {"type": "User", "id": "dana"}, "attrs": {"warehouses": ["W1", "W2"], "is_manager": false}, "parents": []});
    let principal = EntityUid::from_str(r#"User::"dana""#).unwrap();
    let action = EntityUid::from_str(r#"Action::"read""#).unwrap();

    for (id, warehouse) in [("e1", "W1"), ("e3", "W3")] {
        let record = json!({"uid": {"type": "StockEntry", "id": id}, "attrs": {"warehouse": warehouse, "amount": 500}, "parents": []});
        let entities = Entities::from_json_value(json!([user, record]), Some(&schema)).unwrap();
        let resource = EntityUid::from_str(&format!(r#"StockEntry::"{id}""#)).unwrap();
        let request = Request::new(principal.clone(), action.clone(), resource, Context::empty(), Some(&schema)).unwrap();
        let answer = Authorizer::new().is_authorized(&request, &policies, &entities);
        println!("eval {} -> {:?}", record["attrs"], answer.decision());
    }

    // Which stock entries may dana read? The resource is unknown.
    let request = PartialRequest::new(
        PartialEntityUid::from_concrete(principal),
        action,
        PartialEntityUid::new("StockEntry".parse().unwrap(), None),
        Some(Context::empty()),
        &schema,
    )
    .expect("partial request");
    let known = PartialEntities::from_json_value(json!([user]), &schema).expect("partial entities");
    let response = policies.tpe(&request, &known, &schema).expect("partial evaluation");
    println!("decision without the record: {:?}", response.decision());
    for residual in response.residual_policies() {
        println!("residual cedar: {}", residual.to_cedar().unwrap_or_default().replace('\n', " "));
        let json = residual.to_json().expect("residual as json");
        let mut params = vec![];
        for condition in json["conditions"].as_array().unwrap() {
            println!("sql   {:?}", to_sql(&condition["body"], &mut params));
        }
        println!("binds {}", json!(params));
    }
}
