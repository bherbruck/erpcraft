//! Who is acting and what they may do.
//!
//! A role holds permission rules. A rule allows or denies one
//! `resource:action`, optionally only for records matching a condition and
//! optionally only for some fields:
//!
//! ```json
//! "account:read"
//! { "allow": "stock_entry:read", "where": "record.warehouse in user.warehouses" }
//! { "allow": "item:update", "fields": ["description", "bin"] }
//! { "deny":  "item:read", "fields": ["cost"] }
//! { "deny":  "journal_entry:post", "where": "record.created_by == user.id" }
//! ```
//!
//! A role is granted to a user for one company or for all companies. The
//! outcome for a record is: denied if any deny rule applies, otherwise
//! allowed if any allow rule applies, otherwise denied. A rule that cannot
//! be decided never works in the caller's favor.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use erpcraft_core::{Error, Result};
use erpcraft_rules::{Bind, ColumnType, Rule, SqlScope};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Allow,
    Deny,
}

/// One rule of a role.
#[derive(Debug, Clone)]
pub struct PermissionRule {
    pub effect: Effect,
    /// `resource:action`; either side may be `*`.
    pub permission: String,
    /// Applies only to records for which this is true.
    pub condition: Option<Arc<Rule>>,
    /// Allow: the only fields granted. Deny: the fields withheld, leaving
    /// the rest of the record alone.
    pub fields: Option<Vec<String>>,
    /// The stored rule could not be read. It then never allows and always
    /// denies.
    pub broken: bool,
}

impl PermissionRule {
    /// Read a rule from its stored JSON form.
    pub fn from_json(value: &Value) -> Result<PermissionRule> {
        let bad = |why: &str| Error::validation(format!("permission rule {value}: {why}"));
        let (effect, permission, condition, fields) = match value {
            Value::String(permission) => (Effect::Allow, permission.clone(), None, None),
            Value::Object(rule) => {
                for key in rule.keys() {
                    if !matches!(key.as_str(), "allow" | "deny" | "where" | "fields") {
                        return Err(bad(&format!("unknown key '{key}'")));
                    }
                }
                let (effect, permission) = match (rule.get("allow"), rule.get("deny")) {
                    (Some(Value::String(p)), None) => (Effect::Allow, p.clone()),
                    (None, Some(Value::String(p))) => (Effect::Deny, p.clone()),
                    _ => return Err(bad("give exactly one of 'allow' or 'deny', as a string")),
                };
                let condition = match rule.get("where") {
                    None | Some(Value::Null) => None,
                    // A rule of a role is kept by the installation: it is
                    // remembered where no request can push it out.
                    Some(Value::String(source)) => Some(Arc::new(Rule::parse_stored(source)?)),
                    Some(_) => return Err(bad("'where' must be a string")),
                };
                let fields = match rule.get("fields") {
                    None | Some(Value::Null) => None,
                    Some(Value::Array(items)) if !items.is_empty() => Some(
                        items
                            .iter()
                            .map(|item| item.as_str().map(str::to_owned))
                            .collect::<Option<Vec<String>>>()
                            .ok_or_else(|| bad("'fields' must be a list of field names"))?,
                    ),
                    Some(_) => return Err(bad("'fields' must be a non-empty list of field names")),
                };
                (effect, permission, condition, fields)
            }
            _ => return Err(bad("must be a string or an object")),
        };
        let well_formed = permission == "*"
            || permission
                .split_once(':')
                .is_some_and(|(resource, action)| !resource.is_empty() && !action.is_empty());
        if !well_formed {
            return Err(bad("the permission must look like 'resource:action'"));
        }
        Ok(PermissionRule {
            effect,
            permission,
            condition,
            fields,
            broken: false,
        })
    }

    /// The resource a rule is about, unless it is a wildcard.
    pub fn resource(&self) -> Option<&str> {
        self.permission
            .split_once(':')
            .map(|(resource, _)| resource)
            .filter(|r| *r != "*")
    }

    fn to_json(&self) -> Value {
        let key = if self.effect == Effect::Allow {
            "allow"
        } else {
            "deny"
        };
        let mut rule = json!({ key: self.permission });
        if let Some(condition) = &self.condition {
            rule["where"] = json!(condition.source());
        }
        if let Some(fields) = &self.fields {
            rule["fields"] = json!(fields);
        }
        if self.broken {
            rule["broken"] = json!(true);
        }
        rule
    }

    /// Is the rule in force for this record? `unknown` is the answer when
    /// that cannot be decided.
    fn applies(&self, record: Option<&Value>, user: &Value, unknown: bool) -> bool {
        if self.broken {
            return unknown;
        }
        match (&self.condition, record) {
            (None, _) => true,
            (Some(_), None) => unknown,
            (Some(condition), Some(record)) => condition.matches(record, user).unwrap_or(unknown),
        }
    }
}

impl Serialize for PermissionRule {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

/// One role assignment, flattened to its rules.
#[derive(Debug, Clone, Serialize)]
pub struct Grant {
    /// `None`: valid for every company.
    pub company_id: Option<Uuid>,
    pub rules: Vec<PermissionRule>,
}

/// Which grants count for a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A record of this company: grants for it and grants for all companies.
    Company(Uuid),
    /// A record shared by all companies, being changed: only grants for all
    /// companies.
    Global,
    /// Any grant at all.
    Any,
}

/// Which fields of a record an actor may use for one action.
#[derive(Debug, Clone, Default)]
pub struct FieldAccess {
    all: bool,
    only: BTreeSet<String>,
    denied: BTreeSet<String>,
}

impl FieldAccess {
    pub fn everything() -> FieldAccess {
        FieldAccess {
            all: true,
            ..FieldAccess::default()
        }
    }

    /// The `id` of a record the actor can reach is never withheld.
    pub fn allows(&self, field: &str) -> bool {
        field == "id" || ((self.all || self.only.contains(field)) && !self.denied.contains(field))
    }

    pub fn unrestricted(&self) -> bool {
        self.all && self.denied.is_empty()
    }
}

/// How far an actor's use of one field goes, across all records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// On every record the actor can reach.
    All,
    /// On some records: it depends on the record or on its company.
    #[serde(rename = "some")]
    Partial,
    /// On no record.
    #[serde(rename = "none")]
    Never,
}

/// What a row filter is compiled against.
pub struct RowScope<'a> {
    pub alias: &'a str,
    pub columns: &'a BTreeMap<String, ColumnType>,
    /// The column holding the owning company, if records have one.
    pub company_column: Option<&'a str>,
}

/// Who is acting, and what they may do.
#[derive(Debug, Clone, Serialize)]
pub struct Actor {
    pub user_id: Uuid,
    pub email: String,
    pub name: String,
    /// An administrator may do everything; rules do not apply to them.
    pub is_admin: bool,
    /// Free-form facts about the user that rules can refer to, such as the
    /// warehouses they work in.
    pub attributes: Value,
    pub grants: Vec<Grant>,
    /// A cap set by the credential in use: the actor can do only what both
    /// these patterns and the grants allow. `None`: no cap.
    pub scopes: Option<Vec<String>>,
    /// How the credential in use was obtained, as the authenticator says:
    /// `password`, or `sso:` and a provider. A token made from another
    /// carries its origin on. `None` where nothing is known.
    #[serde(skip)]
    pub credential: Option<String>,
}

/// `pattern` is `resource:action` where either side may be `*`, or a bare `*`.
fn matches(pattern: &str, permission: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match (pattern.split_once(':'), permission.split_once(':')) {
        (Some((pr, pa)), Some((r, a))) => (pr == "*" || pr == r) && (pa == "*" || pa == a),
        _ => false,
    }
}

fn denied(permission: &str) -> Error {
    Error::forbidden(format!("permission '{permission}' is required"))
}

impl Actor {
    fn in_scope(&self, permission: &str) -> bool {
        match &self.scopes {
            None => true,
            Some(scopes) => scopes.iter().any(|s| matches(s, permission)),
        }
    }

    /// The user as rules see it: the attributes, plus `id`, `email` and
    /// `name`, which attributes cannot override.
    pub fn rule_user(&self) -> Value {
        let mut user = match &self.attributes {
            Value::Object(map) => map.clone(),
            _ => serde_json::Map::new(),
        };
        user.insert("id".into(), json!(self.user_id));
        user.insert("email".into(), json!(self.email));
        user.insert("name".into(), json!(self.name));
        Value::Object(user)
    }

    fn rules<'a>(
        &'a self,
        permission: &'a str,
        target: Target,
    ) -> impl Iterator<Item = (&'a Grant, &'a PermissionRule)> {
        self.grants
            .iter()
            .filter(move |grant| match target {
                Target::Company(id) => grant.company_id.is_none_or(|c| c == id),
                Target::Global => grant.company_id.is_none(),
                Target::Any => true,
            })
            .flat_map(|grant| grant.rules.iter().map(move |rule| (grant, rule)))
            .filter(move |(_, rule)| matches(&rule.permission, permission))
    }

    fn decide(&self, permission: &str, target: Target, record: Option<&Value>) -> bool {
        if !self.in_scope(permission) {
            return false;
        }
        if self.is_admin {
            return true;
        }
        let user = self.rule_user();
        let mut allowed = false;
        for (_, rule) in self.rules(permission, target) {
            match rule.effect {
                // A deny limited to fields withholds those fields, not the record.
                Effect::Deny if rule.fields.is_none() => {
                    if rule.applies(record, &user, true) {
                        return false;
                    }
                }
                Effect::Deny => {}
                Effect::Allow => allowed = allowed || rule.applies(record, &user, false),
            }
        }
        allowed
    }

    /// May the actor use `permission` in `company_id`, whatever the record?
    /// Rules with a condition cannot be decided here, so they do not allow
    /// and they do deny. Prefer [`Actor::can_on`] when there is a record.
    pub fn can(&self, permission: &str, company_id: Uuid) -> bool {
        self.decide(permission, Target::Company(company_id), None)
    }

    /// May the actor use `permission` on this record?
    pub fn can_on(&self, permission: &str, target: Target, record: &Value) -> bool {
        self.decide(permission, target, Some(record))
    }

    /// May the actor use `permission` on things shared by all companies?
    pub fn can_global(&self, permission: &str) -> bool {
        self.decide(permission, Target::Global, None)
    }

    /// Could the actor ever use `permission`, for some company and some
    /// record? A coarse check for deciding what to offer; the precise check
    /// still happens when the action runs.
    pub fn can_any(&self, permission: &str) -> bool {
        self.in_scope(permission)
            && (self.is_admin
                || self
                    .rules(permission, Target::Any)
                    .any(|(_, rule)| rule.effect == Effect::Allow && !rule.broken))
    }

    pub fn require(&self, permission: &str, company_id: Uuid) -> Result<()> {
        if self.can(permission, company_id) {
            Ok(())
        } else {
            Err(denied(permission))
        }
    }

    pub fn require_on(&self, permission: &str, target: Target, record: &Value) -> Result<()> {
        if self.can_on(permission, target, record) {
            Ok(())
        } else {
            Err(denied(permission))
        }
    }

    pub fn require_global(&self, permission: &str) -> Result<()> {
        if self.can_global(permission) {
            Ok(())
        } else {
            Err(denied(permission))
        }
    }

    pub fn require_any(&self, permission: &str) -> Result<()> {
        if self.can_any(permission) {
            Ok(())
        } else {
            Err(denied(permission))
        }
    }

    /// The fields of `record` the actor may use for `permission`.
    pub fn fields(&self, permission: &str, target: Target, record: &Value) -> FieldAccess {
        if self.is_admin {
            return FieldAccess::everything();
        }
        let user = self.rule_user();
        let mut access = FieldAccess::default();
        for (_, rule) in self.rules(permission, target) {
            match (rule.effect, &rule.fields) {
                (Effect::Allow, fields) => {
                    if rule.applies(Some(record), &user, false) {
                        match fields {
                            None => access.all = true,
                            Some(fields) => access.only.extend(fields.iter().cloned()),
                        }
                    }
                }
                (Effect::Deny, Some(fields)) => {
                    if rule.applies(Some(record), &user, true) {
                        access.denied.extend(fields.iter().cloned());
                    }
                }
                (Effect::Deny, None) => {}
            }
        }
        access
    }

    /// Does any rule limit fields for `permission`? When none does, there
    /// is nothing to work out per record.
    pub fn limits_fields(&self, permission: &str) -> bool {
        !self.is_admin
            && self
                .rules(permission, Target::Any)
                .any(|(_, rule)| rule.fields.is_some())
    }

    /// Might `field` be withheld from the actor on some record? Such a
    /// field must not be usable to filter or sort, or its values leak.
    pub fn may_withhold(&self, permission: &str, field: &str) -> bool {
        !self.is_admin
            && field != "id"
            && self.rules(permission, Target::Any).any(|(_, rule)| {
                match (rule.effect, &rule.fields) {
                    (Effect::Deny, Some(fields)) => fields.iter().any(|f| f == field),
                    (Effect::Allow, Some(fields)) => !fields.iter().any(|f| f == field),
                    _ => false,
                }
            })
    }

    /// How far the actor's use of `field` goes for `permission`. Meant for
    /// deciding what to show; it errs towards [`Reach::Partial`], and the
    /// precise answer is the one [`Actor::fields`] gives for a record.
    pub fn field_reach(&self, permission: &str, field: &str) -> Reach {
        if !self.can_any(permission) {
            return Reach::Never;
        }
        if self.is_admin || field == "id" {
            return Reach::All;
        }
        let (mut granted, mut limited) = (false, false);
        for (grant, rule) in self.rules(permission, Target::Any) {
            let named = rule
                .fields
                .as_ref()
                .is_some_and(|fields| fields.iter().any(|f| f == field));
            match (rule.effect, &rule.fields) {
                (Effect::Allow, _) if rule.broken => {}
                (Effect::Allow, None) => granted = true,
                (Effect::Allow, Some(_)) if named => granted = true,
                (Effect::Allow, Some(_)) => limited = true,
                (Effect::Deny, Some(_)) if named => {
                    // Withheld on every record of every company.
                    if grant.company_id.is_none() && (rule.broken || rule.condition.is_none()) {
                        return Reach::Never;
                    }
                    limited = true;
                }
                (Effect::Deny, _) => {}
            }
        }
        match (granted, limited) {
            (false, _) => Reach::Never,
            (true, true) => Reach::Partial,
            (true, false) => Reach::All,
        }
    }

    /// A SQL condition selecting the rows the actor may use `permission`
    /// on. `Ok(None)` means every row. Fails when the actor can reach no
    /// row at all. Parameters are appended to `binds`.
    ///
    /// With `global_only`, only grants for all companies count: use it when
    /// changing records that all companies share.
    pub fn row_filter(
        &self,
        permission: &str,
        scope: &RowScope<'_>,
        global_only: bool,
        binds: &mut Vec<Bind>,
    ) -> Result<Option<String>> {
        if !self.in_scope(permission) {
            return Err(denied(permission));
        }
        if self.is_admin {
            return Ok(None);
        }
        let user = self.rule_user();
        let sql_scope = SqlScope {
            alias: scope.alias,
            columns: scope.columns,
            user: &user,
        };
        let target = if global_only {
            Target::Global
        } else {
            Target::Any
        };

        // Each rule contributes "its company AND its condition".
        let term = |grant: &Grant,
                    rule: &PermissionRule,
                    binds: &mut Vec<Bind>|
         -> Result<Option<String>> {
            let mut parts = vec![];
            if let (Some(column), Some(company)) = (scope.company_column, grant.company_id) {
                binds.push(Bind::Text(Some(company.to_string())));
                parts.push(format!("{}.{column} = ${}::uuid", scope.alias, binds.len()));
            }
            if let Some(condition) = &rule.condition {
                parts.push(condition.to_sql(&sql_scope, binds)?);
            }
            Ok(if parts.is_empty() {
                None
            } else {
                Some(format!("({})", parts.join(" AND ")))
            })
        };

        // A rule without a condition, under a grant that is not limited to
        // one company here, allows every row: the other allows add nothing.
        // They are then not compiled at all, so that no value is bound that
        // the SQL does not use. sqlx caches prepared statements by their
        // text, and a text prepared with an unused parameter fails when the
        // same text later runs on that connection without it, and the
        // other way round.
        let mut allow_all = self.rules(permission, target).any(|(grant, rule)| {
            matches!(rule.effect, Effect::Allow)
                && !rule.broken
                && rule.condition.is_none()
                && (scope.company_column.is_none() || grant.company_id.is_none())
        });
        let mut allows = vec![];
        let mut denies = vec![];
        for (grant, rule) in self.rules(permission, target) {
            match rule.effect {
                Effect::Allow if rule.broken || allow_all => {}
                Effect::Allow => {
                    let mark = binds.len();
                    match term(grant, rule, binds) {
                        Ok(None) => allow_all = true,
                        Ok(Some(sql)) => allows.push(sql),
                        // A rule that cannot be compiled allows nothing.
                        Err(_) => binds.truncate(mark),
                    }
                }
                Effect::Deny if rule.fields.is_some() => {}
                Effect::Deny => {
                    let mark = binds.len();
                    let compiled = if rule.broken {
                        Ok(None)
                    } else {
                        term(grant, rule, binds)
                    };
                    match compiled {
                        Ok(Some(sql)) => denies.push(sql),
                        // Denies everything, or cannot be compiled: nothing is reachable.
                        Ok(None) => return Err(denied(permission)),
                        Err(_) => {
                            binds.truncate(mark);
                            return Err(denied(permission));
                        }
                    }
                }
            }
        }
        if !allow_all && allows.is_empty() {
            return Err(denied(permission));
        }

        let mut conditions = vec![];
        if !allow_all {
            conditions.push(format!("({})", allows.join(" OR ")));
        }
        if !denies.is_empty() {
            // A deny that evaluates to unknown also removes the row.
            conditions.push(format!("COALESCE(NOT ({}), false)", denies.join(" OR ")));
        }
        Ok(if conditions.is_empty() {
            None
        } else {
            Some(conditions.join(" AND "))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(items: &[Value]) -> Vec<PermissionRule> {
        items
            .iter()
            .map(|item| PermissionRule::from_json(item).unwrap())
            .collect()
    }

    fn actor(grants: Vec<(Option<Uuid>, Vec<Value>)>) -> Actor {
        Actor {
            user_id: Uuid::from_u128(99),
            email: "a@b.c".into(),
            name: "A".into(),
            is_admin: false,
            attributes: json!({"warehouses": ["W1"]}),
            credential: None,
            grants: grants
                .into_iter()
                .map(|(company_id, items)| Grant {
                    company_id,
                    rules: rules(&items),
                })
                .collect(),
            scopes: None,
        }
    }

    const A: Uuid = Uuid::from_u128(1);
    const B: Uuid = Uuid::from_u128(2);

    fn columns() -> BTreeMap<String, ColumnType> {
        BTreeMap::from([
            ("company_id".to_owned(), ColumnType::Uuid),
            ("warehouse".to_owned(), ColumnType::Text),
            ("created_by".to_owned(), ColumnType::Uuid),
        ])
    }

    fn filter(actor: &Actor, permission: &str) -> Result<(Option<String>, Vec<Bind>)> {
        let columns = columns();
        let scope = RowScope {
            alias: "t",
            columns: &columns,
            company_column: Some("company_id"),
        };
        let mut binds = vec![];
        actor
            .row_filter(permission, &scope, false, &mut binds)
            .map(|sql| (sql, binds))
    }

    #[test]
    fn patterns() {
        assert!(matches("*", "account:read"));
        assert!(matches("account:*", "account:read"));
        assert!(matches("*:read", "account:read"));
        assert!(!matches("account:read", "account:update"));
        assert!(!matches("account", "account:read"));
    }

    #[test]
    fn rules_are_read_from_json_strictly() {
        assert!(PermissionRule::from_json(&json!("account:read")).is_ok());
        assert!(
            PermissionRule::from_json(&json!({"deny": "item:read", "fields": ["cost"]})).is_ok()
        );
        for bad in [
            json!("account"),
            json!(5),
            json!({"allow": "a:b", "deny": "a:b"}),
            json!({"where": "true"}),
            json!({"allow": "a:b", "wher": "true"}),
            json!({"allow": "a:b", "where": "record.x >"}),
            json!({"allow": "a:b", "fields": []}),
            json!({"allow": "a:b", "fields": [1]}),
        ] {
            assert!(
                PermissionRule::from_json(&bad).is_err(),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn grants_are_scoped_to_their_company() {
        let user = actor(vec![(Some(A), vec![json!("account:*")])]);
        assert!(user.can("account:read", A));
        assert!(!user.can("account:read", B));
        assert!(!user.can_global("account:read"));
        assert!(user.can_any("account:read"));
        assert!(!user.can_any("role:read"));
    }

    #[test]
    fn scopes_cap_even_an_administrator() {
        let mut admin = actor(vec![]);
        admin.is_admin = true;
        admin.scopes = Some(vec!["*:read".into()]);
        assert!(admin.can("account:read", A));
        assert!(!admin.can("account:create", A));
        assert!(filter(&admin, "account:create").is_err());
    }

    #[test]
    fn a_condition_decides_per_record() {
        let user = actor(vec![(
            None,
            vec![json!({"allow": "stock:read", "where": "record.warehouse in user.warehouses"})],
        )]);
        assert!(user.can_on(
            "stock:read",
            Target::Company(A),
            &json!({"warehouse": "W1"})
        ));
        assert!(!user.can_on(
            "stock:read",
            Target::Company(A),
            &json!({"warehouse": "W2"})
        ));
        // Without a record, or without the field, the rule cannot allow.
        assert!(!user.can("stock:read", A));
        assert!(!user.can_on("stock:read", Target::Company(A), &json!({})));
        // But the user could read some stock, so the action is offered.
        assert!(user.can_any("stock:read"));
    }

    #[test]
    fn deny_wins_and_fails_closed() {
        let user = actor(vec![(
            None,
            vec![
                json!("journal_entry:*"),
                json!({"deny": "journal_entry:post", "where": "record.created_by == user.id"}),
            ],
        )]);
        let own = json!({"created_by": Uuid::from_u128(99)});
        let other = json!({"created_by": Uuid::from_u128(7)});
        assert!(!user.can_on("journal_entry:post", Target::Company(A), &own));
        assert!(user.can_on("journal_entry:post", Target::Company(A), &other));
        assert!(user.can_on("journal_entry:read", Target::Company(A), &own));
        // Undecidable deny: denied.
        assert!(!user.can("journal_entry:post", A));
        assert!(!user.can_on("journal_entry:post", Target::Company(A), &json!({})));

        let mut broken = actor(vec![(
            None,
            vec![json!("item:*"), json!({"deny": "item:read"})],
        )]);
        broken.grants[0].rules[1].permission = "item:update".into();
        broken.grants[0].rules[1].broken = true;
        assert!(broken.can("item:read", A));
        assert!(!broken.can("item:update", A));
    }

    #[test]
    fn field_rules_combine() {
        let user = actor(vec![(
            None,
            vec![
                json!("item:read"),
                json!({"deny": "item:read", "fields": ["cost"]}),
                json!({"allow": "item:update", "fields": ["description"]}),
            ],
        )]);
        let record = json!({});
        let read = user.fields("item:read", Target::Company(A), &record);
        assert!(read.allows("name") && read.allows("id") && !read.allows("cost"));
        let update = user.fields("item:update", Target::Company(A), &record);
        assert!(update.allows("description") && !update.allows("name"));
        // A field-only deny does not block the record itself.
        assert!(user.can("item:read", A));
        assert!(user.limits_fields("item:read") && user.may_withhold("item:read", "cost"));
        assert!(
            !user.may_withhold("item:read", "name") && user.may_withhold("item:update", "name")
        );
    }

    #[test]
    fn row_filters_compile_companies_conditions_and_denies() {
        let everything = actor(vec![(None, vec![json!("stock:read")])]);
        assert_eq!(filter(&everything, "stock:read").unwrap().0, None);
        assert!(filter(&everything, "stock:update").is_err());

        // A grant for all companies makes a narrower one moot, and nothing
        // is bound for it: the SQL would not use the value.
        let both = actor(vec![
            (Some(A), vec![json!("stock:read")]),
            (None, vec![json!("stock:read")]),
        ]);
        assert_eq!(filter(&both, "stock:read").unwrap(), (None, vec![]));
        let both_denied = actor(vec![
            (Some(A), vec![json!("stock:read")]),
            (
                None,
                vec![
                    json!("stock:read"),
                    json!({"deny": "stock:read", "where": "record.created_by == user.id"}),
                ],
            ),
        ]);
        let (sql, binds) = filter(&both_denied, "stock:read").unwrap();
        assert_eq!(
            sql.unwrap(),
            "COALESCE(NOT (((t.created_by = $1::uuid))), false)"
        );
        assert_eq!(binds.len(), 1);

        let one_company = actor(vec![(Some(A), vec![json!("stock:read")])]);
        let (sql, binds) = filter(&one_company, "stock:read").unwrap();
        assert_eq!(sql.unwrap(), "((t.company_id = $1::uuid))");
        assert_eq!(binds, [Bind::Text(Some(A.to_string()))]);

        let ruled = actor(vec![(
            Some(A),
            vec![
                json!({"allow": "stock:read", "where": "record.warehouse in user.warehouses"}),
                json!({"deny": "stock:read", "where": "record.created_by == user.id"}),
            ],
        )]);
        let (sql, binds) = filter(&ruled, "stock:read").unwrap();
        assert_eq!(
            sql.unwrap(),
            "((t.company_id = $1::uuid AND (t.warehouse = ANY($2::text[])))) \
             AND COALESCE(NOT ((t.company_id = $3::uuid AND (t.created_by = $4::uuid))), false)"
        );
        assert_eq!(binds.len(), 4);

        let denied_everywhere = actor(vec![(
            None,
            vec![json!("stock:read"), json!({"deny": "stock:read"})],
        )]);
        assert!(filter(&denied_everywhere, "stock:read").is_err());
    }
}
