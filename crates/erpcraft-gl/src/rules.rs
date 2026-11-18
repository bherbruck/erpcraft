//! Account rules: which account a company posts to for a purpose.
//!
//! A module that posts a value asks [`account_for`] instead of keeping its
//! own setting, so that every module posting for the same purpose lands on
//! the same account.

use erpcraft_kernel::{EntityMeta, Error, FieldMeta, FieldType, PgConnection, Result};
use uuid::Uuid;

pub(crate) fn entity() -> EntityMeta {
    EntityMeta::new("account_rule", "Account rule")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(FieldMeta::new("purpose", "Purpose", FieldType::Text).required())
        .field(FieldMeta::new("scope_type", "Scope type", FieldType::Text))
        // The record the rule is for: of the entity `scope_type` names.
        .field(FieldMeta::new(
            "scope_id",
            "Scope",
            FieldType::entity_named_by("scope_type"),
        ))
        .field(FieldMeta::new("account_id", "Account", FieldType::link("account")).required())
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["purpose", "scope_type", "scope_id"])
        .search(&["purpose", "scope_type"])
        // The record is of the entity the type names.
        .written_together(&["scope_type", "scope_id"])
        .company_field("company_id")
}

/// The account a company posts to for `purpose`, such as `inventory`.
///
/// `scopes` says what the posting is about, most specific first, as pairs
/// of an entity name and a record id: `[("item", item_id), ("warehouse",
/// warehouse_id)]`. The account of the first scope that has an account
/// rule is returned; if none has, the company's default for the purpose,
/// which is the rule without a scope. The ledger does not know what a
/// scope is: it only matches the pair against the rules.
///
/// With no rule to go by, this is a validation error telling the user to
/// add one. The account is one of the company's and not a group account
/// when the rule is saved; whether it can still be posted to is checked,
/// as always, when the entry is written.
pub async fn account_for(
    conn: &mut PgConnection,
    company_id: Uuid,
    purpose: &str,
    scopes: &[(&'static str, Uuid)],
) -> Result<Uuid> {
    let types: Vec<&str> = scopes.iter().map(|(scope_type, _)| *scope_type).collect();
    let ids: Vec<Uuid> = scopes.iter().map(|(_, id)| *id).collect();
    let rules: Vec<(Option<String>, Option<Uuid>, Uuid)> = sqlx::query_as(
        "SELECT r.scope_type, r.scope_id, r.account_id FROM account_rule r
         WHERE r.company_id = $1 AND r.purpose = $2
           AND (r.scope_type IS NULL OR (r.scope_type, r.scope_id) IN
                (SELECT s.scope_type, s.scope_id
                 FROM unnest($3::text[], $4::uuid[]) AS s(scope_type, scope_id)))",
    )
    .bind(company_id)
    .bind(purpose)
    .bind(&types)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let rule_of = |scope: Option<(&str, Uuid)>| {
        rules
            .iter()
            .find(|(scope_type, scope_id, _)| scope_type.as_deref().zip(*scope_id) == scope)
            .map(|(_, _, account_id)| *account_id)
    };
    scopes
        .iter()
        .find_map(|(scope_type, id)| rule_of(Some((scope_type, *id))))
        .or_else(|| rule_of(None))
        .ok_or_else(|| {
            Error::validation(format!(
                "this company has no '{purpose}' account set up: add an account rule \
                 with the purpose '{purpose}'"
            ))
        })
}
