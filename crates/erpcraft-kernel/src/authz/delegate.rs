//! Who may hand rights on: the rule against escalating one's own rights.
//!
//! An administrator may grant anything. Anyone else may grant, revoke,
//! write or edit a role only if they hold every right it gives themselves,
//! at least as far: for the company it is granted for or for all
//! companies, for at least the same fields, and without a condition
//! narrower than the role's. Where that cannot be told for certain, the
//! answer is no:
//!
//! - an allowing rule with a condition is covered only by an unconditional
//!   rule of one's own, or by the very same condition when that condition
//!   does not read the user (`user.<attribute>`): with another user's
//!   attributes the same text reaches other records;
//! - a deny rule anywhere in one's own roles that could touch what the
//!   role allows means one does not hold all of it;
//! - a credential limited to some permissions (a scoped token) hands on
//!   nothing;
//! - nobody grants or revokes a role for themselves.
//!
//! Denies in the role being granted take rights away, so they need no
//! cover.

use uuid::Uuid;

use super::actor::{Actor, Effect, PermissionRule};

/// Whether `pattern` matches everything `permission` does. Either may be
/// `*` or have `*` on a side.
fn covers(pattern: &str, permission: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if permission == "*" {
        return false;
    }
    match (pattern.split_once(':'), permission.split_once(':')) {
        (Some((pr, pa)), Some((r, a))) => (pr == "*" || pr == r) && (pa == "*" || pa == a),
        _ => false,
    }
}

/// Whether two patterns can match one permission in common.
fn overlap(a: &str, b: &str) -> bool {
    if a == "*" || b == "*" {
        return true;
    }
    match (a.split_once(':'), b.split_once(':')) {
        (Some((ar, aa)), Some((br, ba))) => {
            (ar == "*" || br == "*" || ar == br) && (aa == "*" || ba == "*" || aa == ba)
        }
        _ => false,
    }
}

/// Whether `own`, an allowing rule of the actor, gives at least what
/// `given` does.
fn holds(own: &PermissionRule, given: &PermissionRule) -> bool {
    if own.effect != Effect::Allow || own.broken || !covers(&own.permission, &given.permission) {
        return false;
    }
    let fields = match (&own.fields, &given.fields) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(own), Some(given)) => given.iter().all(|f| own.contains(f)),
    };
    let condition = match (&own.condition, &given.condition) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(own), Some(given)) => {
            own.source() == given.source() && !given.source().contains("user.")
        }
    };
    fields && condition
}

fn shown(rule: &PermissionRule) -> String {
    match (&rule.condition, &rule.fields) {
        (None, None) => format!("'{}'", rule.permission),
        _ => format!("'{}' (with its condition or fields)", rule.permission),
    }
}

impl Actor {
    /// Whether the actor may hand on `rules`, the rules of one role, for
    /// `company` (`None`: for all companies). The error says which right
    /// they lack.
    pub fn may_delegate(
        &self,
        rules: &[PermissionRule],
        company: Option<Uuid>,
    ) -> std::result::Result<(), String> {
        if self.is_admin {
            return Ok(());
        }
        if self.scopes.is_some() {
            return Err("a token limited to some permissions cannot hand rights on".into());
        }
        // The grants that count where the role would apply: for that
        // company, or for all companies.
        let own: Vec<&PermissionRule> = self
            .grants
            .iter()
            .filter(|g| g.company_id.is_none() || (company.is_some() && g.company_id == company))
            .flat_map(|g| g.rules.iter())
            .collect();
        // Every deny of one's own, wherever it applies: it narrows what one
        // holds, so what it touches cannot be handed on whole.
        let denies: Vec<&PermissionRule> = self
            .grants
            .iter()
            .flat_map(|g| g.rules.iter())
            .filter(|r| r.effect == Effect::Deny)
            .collect();
        let scope = if company.is_some() {
            "for that company"
        } else {
            "for all companies"
        };
        for given in rules.iter().filter(|r| r.effect == Effect::Allow) {
            if given.broken {
                return Err("the role has a rule that cannot be read".into());
            }
            if !own.iter().any(|own| holds(own, given)) {
                return Err(format!(
                    "you do not hold {} yourself {scope}, so you cannot hand it on",
                    shown(given)
                ));
            }
            if let Some(deny) = denies
                .iter()
                .find(|deny| overlap(&deny.permission, &given.permission))
            {
                return Err(format!(
                    "your own rights deny '{}', so you do not hold all of {}",
                    deny.permission,
                    shown(given)
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::Grant;
    use serde_json::json;

    fn rules(values: serde_json::Value) -> Vec<PermissionRule> {
        values
            .as_array()
            .unwrap()
            .iter()
            .map(|v| PermissionRule::from_json(v).unwrap())
            .collect()
    }

    fn actor(grants: Vec<(Option<Uuid>, serde_json::Value)>) -> Actor {
        Actor {
            user_id: Uuid::nil(),
            email: "a@example.com".into(),
            name: "A".into(),
            is_admin: false,
            attributes: json!({}),
            grants: grants
                .into_iter()
                .map(|(company_id, r)| Grant {
                    company_id,
                    rules: rules(r),
                })
                .collect(),
            scopes: None,
            credential: None,
        }
    }

    #[test]
    fn one_hands_on_only_what_one_holds() {
        let company = Some(Uuid::from_u128(1));
        let other = Some(Uuid::from_u128(2));
        let manager = actor(vec![
            (None, json!(["item:*", "user:assign_role"])),
            (
                company,
                json!(["account:read", { "allow": "stock:read", "where": "record.qty > 0" }]),
            ),
        ]);
        let ok = |r: serde_json::Value, c: Option<Uuid>| manager.may_delegate(&rules(r), c);
        assert!(ok(json!(["item:read", "item:update"]), None).is_ok());
        assert!(ok(json!(["account:read"]), company).is_ok());
        assert!(
            ok(json!([{ "deny": "*" }]), None).is_ok(),
            "a deny takes away"
        );
        // Wider than one's own.
        assert!(ok(json!(["*"]), None).is_err());
        assert!(ok(json!(["item:*", "vendor:read"]), None).is_err());
        // For all companies, or another company, what one holds for one.
        assert!(ok(json!(["account:read"]), None).is_err());
        assert!(ok(json!(["account:read"]), other).is_err());
        // Conditions: one's own condition is no cover for none, nor for
        // another one; the same one is.
        assert!(ok(json!(["stock:read"]), company).is_err());
        assert!(
            ok(
                json!([{ "allow": "stock:read", "where": "record.qty > 1" }]),
                company
            )
            .is_err()
        );
        assert!(
            ok(
                json!([{ "allow": "stock:read", "where": "record.qty > 0" }]),
                company
            )
            .is_ok()
        );
        assert!(
            ok(
                json!([{ "allow": "item:read", "where": "record.code == 'x'" }]),
                None
            )
            .is_ok()
        );
        // Fields: no fields is all of them.
        assert!(ok(json!([{ "allow": "item:read", "fields": ["name"] }]), None).is_ok());

        // A deny of one's own anywhere means one does not hold it all.
        let narrowed = actor(vec![(
            None,
            json!(["*", { "deny": "item:read", "fields": ["cost"] }]),
        )]);
        assert!(narrowed.may_delegate(&rules(json!(["*"])), None).is_err());
        assert!(
            narrowed
                .may_delegate(&rules(json!(["item:read"])), None)
                .is_err()
        );
        assert!(
            narrowed
                .may_delegate(&rules(json!(["vendor:read"])), None)
                .is_ok()
        );
        // A condition that reads the user is never the same reach.
        let by_user = actor(vec![(
            None,
            json!([{ "allow": "stock:read", "where": "record.warehouse in user.warehouses" }]),
        )]);
        assert!(by_user
            .may_delegate(
                &rules(json!([{ "allow": "stock:read", "where": "record.warehouse in user.warehouses" }])),
                None
            )
            .is_err());
        // A scoped token hands nothing on.
        let mut scoped = actor(vec![(None, json!(["*"]))]);
        scoped.scopes = Some(vec!["*".into()]);
        assert!(
            scoped
                .may_delegate(&rules(json!(["item:read"])), None)
                .is_err()
        );
    }
}
