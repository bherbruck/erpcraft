//! Dimensions: what a ledger entry is about besides its account (a site, a
//! cost center, a project).
//!
//! A company defines its dimensions and their values. Every entry points to
//! an immutable dimension set, kept once per combination of values. Which
//! values a line gets is, per dimension: the value the caller states, else
//! the default that the dimension rules give for what the line is about
//! ([`dimension_for`], matched like [`account_for`](crate::account_for)),
//! most specific first, then for its account, then for the company. Whether
//! a combination is allowed, required or refused is the policy point
//! [`DIMENSIONS`], never a rule compiled in.

use std::collections::{BTreeMap, HashMap};

use erpcraft_kernel::{
    Ctx, Decision, EntityMeta, Error, FieldMeta, FieldType, PgConnection, PolicyPoint, Posting,
    Result,
};
use serde_json::json;
use uuid::Uuid;

use crate::GlLine;

/// The policy point asked for every line a posting writes.
pub const DIMENSIONS: &str = "gl.dimensions";

/// Which dimension values a ledger line gets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Dimensions {
    /// The defaults: the dimension rules of what the line is about
    /// ([`GlLine::scopes`]), of its account, and of the company.
    #[default]
    Defaults,
    /// These values (ids of `dimension_value`), at most one per dimension,
    /// and the defaults for the company's other dimensions.
    Stated(Vec<Uuid>),
    /// Exactly this set as an earlier entry recorded it (`None`: no
    /// dimensions). Nothing is added and the policy is not asked: the line
    /// carries on what was already in the books, as a year-end close does.
    Exact(Option<Uuid>),
}

pub(crate) fn entities() -> Vec<EntityMeta> {
    let company = || {
        FieldMeta::new("company_id", "Company", FieldType::link("company"))
            .required()
            .immutable()
    };
    let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
    vec![
        EntityMeta::new("dimension", "Dimension")
            .field(company())
            .field(
                FieldMeta::new("code", "Code", FieldType::Text)
                    .required()
                    .immutable()
                    .help("Such as SITE, COSTCENTER or PROJECT. It cannot change later."),
            )
            .field(FieldMeta::new("name", "Name", FieldType::Text).required())
            .field(FieldMeta::new("active", "Active", FieldType::Bool).help(
                "An inactive dimension gets no default value and cannot be stated on new entries.",
            ))
            .field(created())
            .order_by(&["code"])
            .search(&["code", "name"])
            .company_field("company_id"),
        EntityMeta::new("dimension_value", "Dimension value")
            .field(company())
            .field(
                FieldMeta::new("dimension_id", "Dimension", FieldType::link("dimension"))
                    .required()
                    .immutable(),
            )
            .field(
                FieldMeta::new("code", "Code", FieldType::Text)
                    .required()
                    .immutable(),
            )
            .field(FieldMeta::new("name", "Name", FieldType::Text).required())
            .field(FieldMeta::new("active", "Active", FieldType::Bool))
            .field(created())
            .order_by(&["dimension_id", "code"])
            .search(&["code", "name"])
            .child_of("dimension", "dimension_id")
            .company_field("company_id"),
        EntityMeta::new("dimension_set", "Dimension set")
            .field(company())
            .field(FieldMeta::new("label", "Values", FieldType::Text))
            .field(FieldMeta::new("key", "Key", FieldType::Text))
            .field(created())
            .order_by(&["label"])
            .search(&["label"])
            .display(&["label"])
            .read_only()
            .company_field("company_id"),
        EntityMeta::new("dimension_rule", "Dimension rule")
            .field(company())
            .field(
                FieldMeta::new("dimension_id", "Dimension", FieldType::link("dimension"))
                    .readonly(),
            )
            .field(
                FieldMeta::new("scope_type", "Scope type", FieldType::Text).help(
                    "The kind of record the rule is for: warehouse, site, item, customer, \
                     vendor, account, charge. Empty: the company's default.",
                ),
            )
            .field(FieldMeta::new(
                "scope_id",
                "Scope",
                FieldType::entity_named_by("scope_type"),
            ))
            .field(
                FieldMeta::new("value_id", "Value", FieldType::link("dimension_value")).required(),
            )
            .field(created())
            .order_by(&["dimension_id", "scope_type", "scope_id"])
            .search(&["scope_type"])
            .display(&["scope_type", "value_id"])
            .written_together(&["scope_type", "scope_id"])
            .company_field("company_id"),
    ]
}

pub(crate) fn policy() -> PolicyPoint {
    PolicyPoint::new(
        DIMENSIONS,
        "Asked for every line a posting writes to the general ledger, with the dimension \
         values the line ends up with: may the account carry this combination? Unless \
         configured otherwise every combination is accepted. A company that wants a cost \
         center on every expense refuses when `facts.account_type == 'expense' && \
         !('COSTCENTER' in facts.dimensions)`; one that keeps a project off the balance sheet \
         refuses when `facts.account_type == 'asset' && 'PROJECT' in facts.dimensions`; a \
         combination is refused with `'SITE:OHIO' in facts.values && 'COSTCENTER:DE-SALES' in \
         facts.values`. Not asked when a posting is reversed or when a line carries on a set \
         already recorded (a year-end close).",
    )
    .text(
        "doc_type",
        "The type of the document that posts, such as journal_entry",
    )
    .text("account_code", "The code of the line's account")
    .text(
        "account_type",
        "The type of the line's account: asset, liability, equity, income or expense",
    )
    .list(
        "dimensions",
        "The codes of the dimensions the line has a value for, such as ['COSTCENTER', 'SITE']",
    )
    .list(
        "values",
        "The line's values as DIMENSION:VALUE codes, such as ['COSTCENTER:ASSY', 'SITE:OHIO']",
    )
    .refusal("this combination of dimensions is not accepted on this account")
}

/// A dimension rule as matched.
struct Rule {
    dimension_id: Uuid,
    scope: Option<(String, Uuid)>,
    value_id: Uuid,
}

/// The active dimensions of a company, and the rules that apply to any of
/// `scopes` or to the company.
async fn load_rules(
    conn: &mut PgConnection,
    company_id: Uuid,
    scopes: &[(&str, Uuid)],
) -> Result<(Vec<Uuid>, Vec<Rule>)> {
    let dimensions: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM dimension WHERE company_id = $1 AND active ORDER BY code",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    if dimensions.is_empty() {
        return Ok((dimensions, vec![]));
    }
    let types: Vec<&str> = scopes.iter().map(|(scope_type, _)| *scope_type).collect();
    let ids: Vec<Uuid> = scopes.iter().map(|(_, id)| *id).collect();
    let rows: Vec<(Uuid, Option<String>, Option<Uuid>, Uuid)> = sqlx::query_as(
        "SELECT r.dimension_id, r.scope_type, r.scope_id, r.value_id
         FROM dimension_rule r
         JOIN dimension_value v ON v.id = r.value_id AND v.active
         WHERE r.company_id = $1
           AND (r.scope_type IS NULL OR (r.scope_type, r.scope_id) IN
                (SELECT s.scope_type, s.scope_id
                 FROM unnest($2::text[], $3::uuid[]) AS s(scope_type, scope_id)))",
    )
    .bind(company_id)
    .bind(&types)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let rules = rows
        .into_iter()
        .map(|(dimension_id, scope_type, scope_id, value_id)| Rule {
            dimension_id,
            scope: scope_type.zip(scope_id),
            value_id,
        })
        .collect();
    Ok((dimensions, rules))
}

/// The default value of each dimension: the rule of the first scope that
/// has one, else the company's.
fn defaults_of(dimensions: &[Uuid], rules: &[Rule], scopes: &[(&str, Uuid)]) -> Vec<(Uuid, Uuid)> {
    dimensions
        .iter()
        .filter_map(|dimension| {
            let rule_of = |scope: Option<(&str, Uuid)>| {
                rules
                    .iter()
                    .find(|r| {
                        r.dimension_id == *dimension
                            && r.scope.as_ref().map(|(t, id)| (t.as_str(), *id)) == scope
                    })
                    .map(|r| r.value_id)
            };
            scopes
                .iter()
                .find_map(|(scope_type, id)| rule_of(Some((scope_type, *id))))
                .or_else(|| rule_of(None))
                .map(|value| (*dimension, value))
        })
        .collect()
}

/// The default dimension values of a company for what a posting line is
/// about.
///
/// `scopes` says what that is, most specific first, as for
/// [`account_for`](crate::account_for): a warehouse and the locations above
/// it, its site, the item, the party, the account. For each active
/// dimension of the company the value of the first scope that has a
/// dimension rule is taken, else the company's default (the rule without a
/// scope), else none. Returns the ids of the values, at most one per
/// dimension.
pub async fn dimension_for(
    conn: &mut PgConnection,
    company_id: Uuid,
    scopes: &[(&'static str, Uuid)],
) -> Result<Vec<Uuid>> {
    let (dimensions, rules) = load_rules(conn, company_id, scopes).await?;
    Ok(defaults_of(&dimensions, &rules, scopes)
        .into_iter()
        .map(|(_, value)| value)
        .collect())
}

/// The dimension set of these values in the company, created the first
/// time the combination is used; `None` for no values. The values must be
/// the company's, at most one per dimension.
pub async fn dimension_set(
    conn: &mut PgConnection,
    company_id: Uuid,
    values: &[Uuid],
) -> Result<Option<Uuid>> {
    if values.is_empty() {
        return Ok(None);
    }
    Ok(sqlx::query_scalar("SELECT dimension_set_of($1, $2)")
        .bind(company_id)
        .bind(values)
        .fetch_one(&mut *conn)
        .await?)
}

/// The values of a dimension set.
pub async fn set_values(conn: &mut PgConnection, set_id: Option<Uuid>) -> Result<Vec<Uuid>> {
    let Some(set_id) = set_id else {
        return Ok(vec![]);
    };
    Ok(
        sqlx::query_scalar("SELECT value_id FROM dimension_set_value WHERE set_id = $1")
            .bind(set_id)
            .fetch_all(&mut *conn)
            .await?,
    )
}

/// A value as a line and the policy see it.
struct ValueInfo {
    dimension_id: Uuid,
    dimension_code: String,
    code: String,
    usable: bool,
}

async fn value_info(
    conn: &mut PgConnection,
    company_id: Uuid,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, ValueInfo>> {
    let rows: Vec<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT v.id, v.dimension_id, d.code, v.code, v.active AND d.active
         FROM dimension_value v JOIN dimension d ON d.id = v.dimension_id
         WHERE v.id = ANY ($2) AND v.company_id = $1",
    )
    .bind(company_id)
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, dimension_id, dimension_code, code, usable)| {
            (
                id,
                ValueInfo {
                    dimension_id,
                    dimension_code,
                    code,
                    usable,
                },
            )
        })
        .collect())
}

/// The dimension set of every line of a posting, in order: stated values
/// checked, defaults filled in, each line asked of the policy
/// [`DIMENSIONS`], and the sets found or created.
pub(crate) async fn stamp(
    ctx: &mut Ctx<'_>,
    posting: &Posting,
    lines: &[GlLine],
) -> Result<Vec<Option<Uuid>>> {
    let company_id = posting.company_id;
    if lines
        .iter()
        .all(|line| matches!(line.dimensions, Dimensions::Exact(_)))
    {
        return Ok(lines
            .iter()
            .map(|line| match line.dimensions {
                Dimensions::Exact(set) => set,
                _ => None,
            })
            .collect());
    }

    // Every scope any line is about, with its account.
    let mut all_scopes: Vec<(&str, Uuid)> = vec![];
    for line in lines {
        all_scopes.extend(line.scopes.iter().copied());
        all_scopes.push(("account", line.account_id));
    }
    all_scopes.sort_unstable();
    all_scopes.dedup();
    let (dimensions, rules) = load_rules(&mut *ctx.conn, company_id, &all_scopes).await?;

    let stated: Vec<Uuid> = lines
        .iter()
        .filter_map(|line| match &line.dimensions {
            Dimensions::Stated(values) => Some(values.iter().copied()),
            _ => None,
        })
        .flatten()
        .chain(rules.iter().map(|r| r.value_id))
        .collect();
    let info = value_info(&mut *ctx.conn, company_id, &stated).await?;

    // The values of each line, by dimension.
    let mut chosen: Vec<Option<BTreeMap<Uuid, Uuid>>> = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let n = index + 1;
        let fail = |msg: String| Error::validation(format!("line {n}: {msg}"));
        let mut values = BTreeMap::new();
        let given: &[Uuid] = match &line.dimensions {
            Dimensions::Exact(_) => {
                chosen.push(None);
                continue;
            }
            Dimensions::Defaults => &[],
            Dimensions::Stated(values) => values,
        };
        for value in given {
            let found = info.get(value).ok_or_else(|| {
                fail("a dimension value of the line is not one of the company's".into())
            })?;
            if !found.usable {
                return Err(fail(format!(
                    "the dimension value {}:{} is inactive",
                    found.dimension_code, found.code
                )));
            }
            if values.insert(found.dimension_id, *value).is_some() {
                return Err(fail(format!(
                    "the line has more than one value for the dimension {}",
                    found.dimension_code
                )));
            }
        }
        let mut scopes: Vec<(&str, Uuid)> = line.scopes.to_vec();
        scopes.push(("account", line.account_id));
        for (dimension, value) in defaults_of(&dimensions, &rules, &scopes) {
            values.entry(dimension).or_insert(value);
        }
        chosen.push(Some(values));
    }

    // The policy, per line, with what the line ends up with.
    let accounts: Vec<Uuid> = lines.iter().map(|l| l.account_id).collect();
    let account_rows: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, code, account_type FROM account WHERE id = ANY ($1)")
            .bind(&accounts)
            .fetch_all(&mut *ctx.conn)
            .await?;
    let account_of: HashMap<Uuid, (String, String)> = account_rows
        .into_iter()
        .map(|(id, code, account_type)| (id, (code, account_type)))
        .collect();
    let doc_type = posting
        .command
        .split('.')
        .next()
        .unwrap_or_default()
        .to_owned();
    let policy = ctx.policy(DIMENSIONS, company_id).await?;
    for (index, (line, values)) in lines.iter().zip(&chosen).enumerate() {
        let Some(values) = values else { continue };
        let mut labels: Vec<(String, String)> = values
            .values()
            .filter_map(|value| info.get(value))
            .map(|v| (v.dimension_code.clone(), v.code.clone()))
            .collect();
        labels.sort();
        // An account of another company is refused by the ledger itself.
        let (code, account_type) = account_of
            .get(&line.account_id)
            .cloned()
            .unwrap_or_default();
        let facts = json!({
            "doc_type": doc_type,
            "account_code": code,
            "account_type": account_type,
            "dimensions": labels.iter().map(|(d, _)| d).collect::<Vec<_>>(),
            "values": labels.iter().map(|(d, v)| format!("{d}:{v}")).collect::<Vec<_>>(),
        });
        if let Decision::Refused(refusal) = policy.evaluate(facts)? {
            let shown: Vec<String> = labels.iter().map(|(d, v)| format!("{d}:{v}")).collect();
            return Err(Error::validation(format!(
                "line {}: {} (account {code}, dimensions: {})",
                index + 1,
                refusal.message.unwrap_or_else(|| {
                    "this combination of dimensions is not accepted on this account".into()
                }),
                if shown.is_empty() {
                    "none".into()
                } else {
                    shown.join(", ")
                },
            )));
        }
    }

    // The sets, once per combination.
    let mut sets: HashMap<Vec<Uuid>, Option<Uuid>> = HashMap::new();
    let mut out = Vec::with_capacity(lines.len());
    for (line, values) in lines.iter().zip(chosen) {
        let set = match values {
            None => match line.dimensions {
                Dimensions::Exact(set) => set,
                _ => None,
            },
            Some(values) => {
                let key: Vec<Uuid> = values.into_values().collect();
                match sets.get(&key) {
                    Some(set) => *set,
                    None => {
                        let set = dimension_set(&mut *ctx.conn, company_id, &key).await?;
                        sets.insert(key, set);
                        set
                    }
                }
            }
        };
        out.push(set);
    }
    Ok(out)
}

/// Why these dimension values cannot be stated on a line of the company,
/// or `None` when they can: each is an active value of an active dimension
/// of the company, at most one per dimension.
pub async fn values_refusal(
    conn: &mut PgConnection,
    company_id: Uuid,
    values: &[Uuid],
) -> Result<Option<String>> {
    if values.is_empty() {
        return Ok(None);
    }
    let info = value_info(conn, company_id, values).await?;
    let mut seen = std::collections::HashSet::new();
    for value in values {
        let Some(found) = info.get(value) else {
            return Ok(Some("a dimension value is not one of the company's".into()));
        };
        if !found.usable {
            return Ok(Some(format!(
                "the dimension value {}:{} is inactive",
                found.dimension_code, found.code
            )));
        }
        if !seen.insert(found.dimension_id) {
            return Ok(Some(format!(
                "more than one value for the dimension {}",
                found.dimension_code
            )));
        }
    }
    Ok(None)
}
