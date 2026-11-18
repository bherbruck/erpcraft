//! Policies: the business judgements a company configures.
//!
//! An invariant holds for everyone: the books balance, an entry is never
//! edited. A policy is a judgement a careful company could make either
//! way: whether to accept a delivery of a few percent more than was
//! ordered, or an invoice above the order price.
//!
//! A module declares each such place as a [`PolicyPoint`]: its name, the
//! facts it supplies, and what happens when the company has configured
//! nothing. A company then configures the point with `policy_rule`
//! records: allow or refuse, when a condition over the facts and the user
//! holds. The module asks at the moment of the judgement:
//!
//! ```ignore
//! ctx.decide("purchase_receipt.over_receipt", company_id, json!({ ... })).await?;
//! ```
//!
//! The rules of a point are looked at in order of priority, a company's own
//! before those for all companies at the same priority, and the first whose
//! condition holds decides. If none does, the point's default decides.
//!
//! **Nothing undecidable loosens anything.** A rule whose condition cannot
//! be worked out (a user attribute that is missing, a text compared with a
//! number) does not match when it would allow, and does match when it would
//! refuse.
//!
//! An administrator is subject to policies like anyone else: a policy says
//! how the company works, not who may do what.
//!
//! Conditions see numbers as floating-point numbers, including exact
//! decimals such as quantities and prices. That is right for a threshold
//! or a percentage; a condition decides, it never produces an amount.

use std::collections::BTreeSet;

use erpcraft_core::{Error, Result};
use erpcraft_rules::Expression;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::command::Ctx;
use crate::registry::{DecimalMeta, DecimalRole, Registry, ValueSource};

/// The names a policy condition can use.
const NAMES: [&str; 2] = ["facts", "user"];

/// What kind of value a fact is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FactType {
    /// A number. An exact decimal may be supplied as its text; conditions
    /// see it as a floating-point number.
    Number,
    Text,
    Bool,
}

/// One fact a policy point supplies to its rules, read as `facts.<name>`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FactMeta {
    pub name: String,
    /// What a person reads: "Net total". The name in words unless the
    /// point says otherwise.
    pub label: String,
    #[serde(rename = "type")]
    pub ty: FactType,
    pub description: String,
    /// For a number: what kind of number it is, as for a field: money (and
    /// in which currency: `{"from": "field", "field": "currency"}` names
    /// another fact of the point), a quantity, a unit price, a rate, a
    /// percent. Absent for a plain count, such as days or lines.
    pub decimal: Option<DecimalMeta>,
    /// The fact depends on when it is asked (days from today), not only on
    /// what is decided: an approval given on one day still holds for it
    /// on another.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub changes_with_time: bool,
}

/// A fact's name in words: `over_percent` is "Over percent".
fn label_of(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Allow,
    Refuse,
}

impl Effect {
    fn opposite(self) -> Effect {
        match self {
            Effect::Allow => Effect::Refuse,
            Effect::Refuse => Effect::Allow,
        }
    }
}

/// What a policy point decides when no configured rule matches: `effect`
/// when `condition` holds or is absent, the opposite otherwise.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PolicyDefault {
    pub effect: Effect,
    /// An expression over `facts`.
    pub condition: Option<String>,
}

/// A place where a module makes a business judgement that a company can
/// configure. Declared in [`Module::policies`](crate::Module::policies).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PolicyPoint {
    /// `subject.judgement`, for example `purchase_receipt.over_receipt`.
    pub name: String,
    pub module: String,
    /// For people and agents: what is being judged, and when.
    pub description: String,
    pub facts: Vec<FactMeta>,
    pub default: PolicyDefault,
    /// What a refused user is told when neither the rule nor the module
    /// has better words.
    pub refusal: String,
    /// Whether a rule may ask for approval here. Not where a ledger checks
    /// the rule that allowed an entry itself (`stock.negative`): approval
    /// is a judgement, and a ledger's invariant is not one.
    pub approvable: bool,
}

impl PolicyPoint {
    /// A point that allows until told otherwise.
    pub fn new(name: &str, description: &str) -> Self {
        PolicyPoint {
            name: name.into(),
            module: String::new(),
            description: description.into(),
            facts: vec![],
            default: PolicyDefault {
                effect: Effect::Allow,
                condition: None,
            },
            refusal: "this is against the company's policy".into(),
            approvable: true,
        }
    }

    /// No rule may ask for approval at this point. See
    /// [`PolicyPoint::approvable`].
    pub fn not_approvable(mut self) -> Self {
        self.approvable = false;
        self
    }

    pub fn fact(mut self, name: &str, ty: FactType, description: &str) -> Self {
        self.facts.push(FactMeta {
            name: name.into(),
            label: label_of(name),
            ty,
            description: description.into(),
            decimal: None,
            changes_with_time: false,
        });
        self
    }

    /// The fact declared last depends on the day it is asked. See
    /// [`FactMeta::changes_with_time`].
    pub fn changes_with_time(mut self) -> Self {
        if let Some(fact) = self.facts.last_mut() {
            fact.changes_with_time = true;
        }
        self
    }

    /// The label of the fact declared last. See [`FactMeta::label`].
    pub fn labelled(mut self, label: &str) -> Self {
        if let Some(fact) = self.facts.last_mut() {
            fact.label = label.into();
        }
        self
    }

    fn decimal(mut self, role: DecimalRole, scale: u32, currency: Option<ValueSource>) -> Self {
        if let Some(fact) = self.facts.last_mut() {
            fact.decimal = Some(DecimalMeta {
                role,
                scale: Some(scale),
                currency,
                unit: None,
            });
        }
        self
    }

    /// The fact declared last is an amount of money: in the currency the
    /// fact `currency_fact` names, or, without one, in a currency the facts
    /// do not say.
    pub fn as_money(self, currency_fact: Option<&str>) -> Self {
        let currency = currency_fact.map(|field| ValueSource::Field {
            field: field.into(),
        });
        self.decimal(DecimalRole::Money, 4, currency)
    }

    /// The fact declared last is money in the company's accounting
    /// currency.
    pub fn as_company_money(self) -> Self {
        self.decimal(DecimalRole::Money, 4, Some(ValueSource::Company))
    }

    /// The fact declared last is a price of one unit, in the currency the
    /// fact `currency_fact` names, if any.
    pub fn as_unit_price(self, currency_fact: Option<&str>) -> Self {
        let currency = currency_fact.map(|field| ValueSource::Field {
            field: field.into(),
        });
        self.decimal(DecimalRole::UnitPrice, 6, currency)
    }

    /// The fact declared last is a quantity of goods.
    pub fn as_quantity(self) -> Self {
        self.decimal(DecimalRole::Quantity, 6, None)
    }

    /// The fact declared last is a share of a hundred.
    pub fn as_percent(self) -> Self {
        self.decimal(DecimalRole::Percent, 6, None)
    }

    /// The fact declared last is a factor, such as an exchange rate.
    pub fn as_rate(self) -> Self {
        self.decimal(DecimalRole::Rate, 10, None)
    }

    /// A number fact, the most common kind.
    pub fn number(self, name: &str, description: &str) -> Self {
        self.fact(name, FactType::Number, description)
    }

    /// A text fact.
    pub fn text(self, name: &str, description: &str) -> Self {
        self.fact(name, FactType::Text, description)
    }

    /// A fact that is true or false.
    pub fn boolean(self, name: &str, description: &str) -> Self {
        self.fact(name, FactType::Bool, description)
    }

    /// With no rule configured, refuse when this condition over `facts`
    /// holds and allow otherwise.
    pub fn refuse_when(mut self, condition: &str) -> Self {
        self.default = PolicyDefault {
            effect: Effect::Refuse,
            condition: Some(condition.into()),
        };
        self
    }

    /// With no rule configured, allow only when this condition over `facts`
    /// holds.
    pub fn allow_when(mut self, condition: &str) -> Self {
        self.default = PolicyDefault {
            effect: Effect::Allow,
            condition: Some(condition.into()),
        };
        self
    }

    /// With no rule configured, always refuse.
    pub fn refuse_by_default(mut self) -> Self {
        self.default = PolicyDefault {
            effect: Effect::Refuse,
            condition: None,
        };
        self
    }

    /// See [`PolicyPoint::refusal`].
    pub fn refusal(mut self, message: &str) -> Self {
        self.refusal = message.into();
        self
    }

    fn fact_names(&self) -> String {
        let names: Vec<&str> = self.facts.iter().map(|f| f.name.as_str()).collect();
        if names.is_empty() {
            "it supplies no facts".into()
        } else {
            format!("its facts are: {}", names.join(", "))
        }
    }

    /// Parse a condition for this point: it may read only the facts the
    /// point declares, and the user.
    fn condition(&self, source: &str) -> Result<Expression> {
        // Kept by the installation, so remembered with the stored rules.
        let expression = Expression::parse_stored(source, &NAMES)?;
        for fact in expression.fields_of("facts") {
            if !self.facts.iter().any(|f| f.name == fact) {
                return Err(Error::validation(format!(
                    "expression `{}`: the policy point '{}' supplies no fact '{fact}'; {}",
                    expression.source(),
                    self.name,
                    self.fact_names()
                )));
            }
        }
        Ok(expression)
    }

    /// Checked when the module is registered. An error is a plain text; the
    /// registry says which module it is about.
    pub(crate) fn check(&self) -> std::result::Result<(), String> {
        if let Some(condition) = &self.default.condition {
            let expression = self.condition(condition).map_err(|e| e.to_string())?;
            if !expression.fields_of("user").is_empty() {
                return Err(format!(
                    "the default of policy point '{}' must depend on facts only, not on the user",
                    self.name
                ));
            }
        }
        Ok(())
    }
}

/// The outcome of a policy point for one set of facts.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allowed,
    Refused(Refusal),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    /// The message of the rule that refused, if it has one. `None` when
    /// the rule has none or the point's default refused: the module then
    /// says it in its own words, or uses [`PolicyPoint::refusal`].
    pub message: Option<String>,
}

/// A decision and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Ruling {
    pub decision: Decision,
    /// The `policy_rule` that decided. `None`: no rule matched and the
    /// point's default decided.
    pub rule_id: Option<Uuid>,
}

struct LoadedRule {
    id: Uuid,
    effect: Effect,
    /// For a rule that asks for approval: who approves. The rule then
    /// counts as refusing until approval is granted.
    approval: Option<crate::approval::Approvers>,
    /// `None`: always. `Some(Err)`: stored, but no longer valid.
    condition: Option<Result<Expression>>,
    message: Option<String>,
}

/// A policy point with the rules that apply to one company and the acting
/// user, ready to decide any number of times. See [`Ctx::policy`].
pub struct Policy<'r> {
    point: &'r PolicyPoint,
    rules: Vec<LoadedRule>,
    default: Option<Result<Expression>>,
    user: Value,
    company_id: Uuid,
    /// Where an approval rule leaves what it needs, and where an approved
    /// run finds what was approved. Shared with the command's context.
    approval: crate::approval::Slot,
}

impl Policy<'_> {
    /// Decide for these facts: an object with every fact the point
    /// declares. A number may be given as a number or as the text of an
    /// exact decimal.
    pub fn evaluate(&self, facts: Value) -> Result<Decision> {
        self.ruling(facts).map(|ruling| ruling.decision)
    }

    /// Like [`Policy::evaluate`], and also says which rule decided. A
    /// ledger that must record why it let something through uses this.
    pub fn ruling(&self, facts: Value) -> Result<Ruling> {
        let point = self.point;
        let (facts, shown) = self.facts(facts)?;
        let variables = json!({ "facts": facts, "user": self.user });

        for rule in &self.rules {
            let holds = match &rule.condition {
                None => Ok(true),
                Some(Ok(expression)) => expression.is_true(&variables),
                Some(Err(error)) => Err(Error::validation(error.to_string())),
            };
            let matched = match holds {
                Ok(holds) => holds,
                // Undecidable: an allow rule does not match, a refuse rule does.
                Err(error) => {
                    tracing::warn!(
                        point = %point.name, rule = %rule.id, %error,
                        "a policy rule could not be evaluated; it is taken as {}",
                        if rule.effect == Effect::Refuse { "refusing" } else { "not matching" }
                    );
                    rule.effect == Effect::Refuse || rule.approval.is_some()
                }
            };
            if matched && let Some(approvers) = &rule.approval {
                return self.approval_ruling(rule, approvers, &facts, &shown);
            }
            if matched {
                return Ok(Ruling {
                    decision: match rule.effect {
                        Effect::Allow => Decision::Allowed,
                        Effect::Refuse => Decision::Refused(Refusal {
                            message: rule.message.as_deref().map(|m| fill(m, &shown)),
                        }),
                    },
                    rule_id: Some(rule.id),
                });
            }
        }

        let effect = match &self.default {
            None => point.default.effect,
            Some(Ok(expression)) => match expression.is_true(&variables) {
                Ok(true) => point.default.effect,
                Ok(false) => point.default.effect.opposite(),
                Err(error) => {
                    tracing::warn!(point = %point.name, %error, "the default of a policy point could not be evaluated; it refuses");
                    Effect::Refuse
                }
            },
            Some(Err(_)) => Effect::Refuse,
        };
        Ok(Ruling {
            decision: match effect {
                Effect::Allow => Decision::Allowed,
                Effect::Refuse => Decision::Refused(Refusal { message: None }),
            },
            rule_id: None,
        })
    }

    /// A rule that asks for approval decided. With approval granted for
    /// this point and these very facts (the run of an approved request),
    /// it allows. Otherwise what is needed is left in the slot and the
    /// command stops with an error the command executor turns into an
    /// approval request; approval given for other facts is stale.
    fn approval_ruling(
        &self,
        rule: &LoadedRule,
        approvers: &crate::approval::Approvers,
        facts: &Value,
        shown: &[(String, String)],
    ) -> Result<Ruling> {
        let point = self.point;
        let mut slot = self.approval.lock();
        // Facts that depend on the day are left out of the comparison: an
        // approval asked one day and given the next still holds for them.
        let lasting = |facts: &Value| {
            let mut facts = facts.clone();
            if let Some(map) = facts.as_object_mut() {
                for fact in point.facts.iter().filter(|f| f.changes_with_time) {
                    map.remove(&fact.name);
                }
            }
            facts
        };
        if let Some(granted) = &slot.granted {
            // The run of an approved request: allowed for exactly what was
            // approved, at each point that asked.
            let approved = granted
                .asks
                .iter()
                .any(|(name, granted)| *name == point.name && lasting(granted) == lasting(facts));
            if approved {
                return Ok(Ruling {
                    decision: Decision::Allowed,
                    rule_id: Some(rule.id),
                });
            }
            slot.stale = true;
            return Err(Error::state(format!(
                "the approval was not given for these facts at '{}': what it was about has \
                 changed since, so the approval no longer applies",
                point.name
            )));
        }
        let message = rule.message.as_deref().map(|m| fill(m, shown));
        let known = slot
            .needed
            .iter()
            .any(|n| n.point == point.name && n.facts == *facts);
        if !known {
            slot.needed.push(crate::approval::Needed {
                point: point.name.clone(),
                rule_id: rule.id,
                company_id: self.company_id,
                facts: facts.clone(),
                shown: shown.to_vec(),
                message: message.clone(),
                approvers: approvers.clone(),
            });
        }
        // Trying the command to learn every point that asks: go on. The
        // executor rolls it all back and keeps the request.
        if slot.collecting {
            return Ok(Ruling {
                decision: Decision::Allowed,
                rule_id: Some(rule.id),
            });
        }
        Err(Error::conflict(format!(
            "this needs approval{}",
            message.map(|m| format!(": {m}")).unwrap_or_default()
        )))
    }

    /// Decide, and turn a refusal into an error in plain words: the rule's
    /// message, or else the point's own.
    pub fn decide(&self, facts: Value) -> Result<()> {
        match self.evaluate(facts)? {
            Decision::Allowed => Ok(()),
            Decision::Refused(refusal) => Err(Error::validation(
                refusal
                    .message
                    .unwrap_or_else(|| self.point.refusal.clone()),
            )),
        }
    }

    /// The facts as conditions see them, and as messages show them. The
    /// module must supply exactly what the point declares; anything else is
    /// a mistake in the module, not in the request.
    fn facts(&self, facts: Value) -> Result<(Value, Vec<(String, String)>)> {
        let point = self.point;
        let wrong = |why: String| Error::internal(format!("policy point '{}': {why}", point.name));
        let Value::Object(mut given) = facts else {
            return Err(wrong("the facts must be an object".into()));
        };
        let mut seen = Map::new();
        let mut shown = vec![];
        for fact in &point.facts {
            let value = given
                .remove(&fact.name)
                .ok_or_else(|| wrong(format!("the fact '{}' was not supplied", fact.name)))?;
            let text = match &value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            let value = match (fact.ty, value) {
                (FactType::Number, Value::Number(n)) => Value::Number(n),
                (FactType::Number, Value::String(text)) => text
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .map(Value::from)
                    .ok_or_else(|| wrong(format!("the fact '{}' is not a number", fact.name)))?,
                (FactType::Text, Value::String(text)) => Value::String(text),
                (FactType::Bool, Value::Bool(b)) => Value::Bool(b),
                _ => {
                    return Err(wrong(format!(
                        "the fact '{}' is not of its declared type",
                        fact.name
                    )));
                }
            };
            seen.insert(fact.name.clone(), value);
            shown.push((fact.name.clone(), text));
        }
        if let Some(extra) = given.keys().next() {
            return Err(wrong(format!("the fact '{extra}' is not declared")));
        }
        Ok((Value::Object(seen), shown))
    }
}

/// A policy rule as read.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct RuleRow {
    point: String,
    id: Uuid,
    effect: String,
    condition: Option<String>,
    message: Option<String>,
    approver_role_id: Option<Uuid>,
    approver_permission: Option<String>,
    approvals_needed: i32,
    approval_days: i32,
}

/// The active rules of each company asked about within one command, in
/// the order they are tried. A command that changes policy rules and then
/// asks a point would see the rules as they were when first read; no
/// command does that.
pub(crate) type RuleCache = std::collections::HashMap<Uuid, Vec<RuleRow>>;

/// Put facts into a message: `{over_percent}` becomes the fact's value.
fn fill(message: &str, facts: &[(String, String)]) -> String {
    let mut filled = message.to_owned();
    for (name, value) in facts {
        filled = filled.replace(&format!("{{{name}}}"), value);
    }
    filled
}

impl<'a> Ctx<'a> {
    /// Load a policy point with the rules that apply to `company_id`: the
    /// company's own and those for all companies. Use it to decide for
    /// several lines of one document without reading the rules each time.
    pub async fn policy(&mut self, point: &str, company_id: Uuid) -> Result<Policy<'a>> {
        let point = self.kernel.registry().policy(point)?;
        if !self.policy_rules.contains_key(&company_id) {
            let all: Vec<RuleRow> = sqlx::query_as(
                "SELECT point, id, effect, condition, message, approver_role_id,
                        approver_permission, approvals_needed, approval_days
                 FROM policy_rule
                 WHERE active AND (company_id = $1 OR company_id IS NULL)
                 ORDER BY priority, (company_id IS NULL), created_at, id",
            )
            .bind(company_id)
            .fetch_all(&mut *self.conn)
            .await?;
            self.policy_rules.insert(company_id, all);
        }
        let rules = self.policy_rules[&company_id]
            .iter()
            .filter(|row| row.point == point.name)
            .map(|row| LoadedRule {
                id: row.id,
                // The table allows nothing else; anything else refuses.
                effect: if row.effect == "allow" {
                    Effect::Allow
                } else {
                    Effect::Refuse
                },
                approval: (row.effect == "approve" && point.approvable).then(|| {
                    crate::approval::Approvers {
                        role_id: row.approver_role_id,
                        permission: row.approver_permission.clone(),
                        needed: row.approvals_needed,
                        days: row.approval_days,
                    }
                }),
                condition: row
                    .condition
                    .as_deref()
                    .map(|source| point.condition(source)),
                message: row.message.clone().filter(|m| !m.trim().is_empty()),
            })
            .collect();
        Ok(Policy {
            point,
            rules,
            default: point
                .default
                .condition
                .as_deref()
                .map(|source| point.condition(source)),
            user: self.actor.rule_user(),
            company_id,
            approval: self.approval.clone(),
        })
    }

    /// Ask a policy point whether what is about to happen may happen in
    /// `company_id`. `facts` is an object with every fact the point
    /// declares. A refusal is a validation error carrying the message of
    /// the rule that refused, or else the point's own.
    pub async fn decide(&mut self, point: &str, company_id: Uuid, facts: Value) -> Result<()> {
        self.policy(point, company_id).await?.decide(facts)
    }
}

/// The policy point asked for every posting, in every module: may a
/// posting carry this date? The kernel declares it, because every posting
/// is opened by [`create_posting`](crate::document::create_posting) or
/// [`reverse_posting`](crate::document::reverse_posting).
pub const POSTING_DATE: &str = "posting.date";

pub(crate) fn kernel_points() -> Vec<PolicyPoint> {
    vec![
        PolicyPoint::new(
            POSTING_DATE,
            "Asked for every posting a document makes, in every module, and for every reversal:              may the posting carry this date? Unless configured otherwise any date is accepted              (a closed fiscal period still refuses it, whatever the policy says). A company that              wants to catch typing mistakes refuses dates far from today, for example when              `facts.days_from_today > 7 || facts.days_from_today < -60`, perhaps letting              reversals or one document type through. Today is the date in UTC.",
        )
        .number(
            "days_from_today",
            "The posting date minus today, in days: negative for a date in the past, positive              for one in the future",
        )
        .changes_with_time()
        .text(
            "doc_type",
            "The type of the document that posts, such as journal_entry or sales_invoice",
        )
        .text("posting_date", "The posting date, written as 2026-03-15")
        .boolean("reversal", "Whether the posting reverses an earlier one")
        .refusal("this posting date is not accepted"),
    ]
}

/// Ask [`POSTING_DATE`] about a posting `company_id` is about to make.
pub(crate) async fn check_posting_date(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    doc_type: &str,
    posting_date: chrono::NaiveDate,
    reversal: bool,
) -> Result<()> {
    let today = chrono::Utc::now().date_naive();
    let facts = json!({
        "days_from_today": (posting_date - today).num_days(),
        "doc_type": doc_type,
        "posting_date": posting_date.to_string(),
        "reversal": reversal,
    });
    ctx.decide(POSTING_DATE, company_id, facts).await
}

/// Check a `policy_rule` when it is saved, so that a stored rule can always
/// be evaluated: its point exists, and its condition parses and reads only
/// the facts that point supplies.
pub(crate) fn validate_rule(registry: &Registry, data: &Map<String, Value>) -> Result<()> {
    let point = data.get("point").and_then(Value::as_str);
    let condition = data.get("condition");
    // The condition is checked against the point's facts, and only the
    // fields sent are known here, so the two travel together.
    match (point, condition) {
        (None, None) => return Ok(()),
        (None, Some(Value::Null)) => return Ok(()),
        (None, Some(_)) => {
            return Err(Error::validation(
                "send 'point' together with 'condition', so that the condition can be checked \
                 against the facts of that policy point",
            )
            .with_field("point"));
        }
        (Some(_), None) => {
            return Err(Error::validation(
                "send 'condition' together with 'point': an expression over facts and user, or \
                 null for a rule that always applies",
            )
            .with_field("condition"));
        }
        (Some(_), Some(_)) => {}
    }
    let (Some(name), Some(condition)) = (point, condition) else {
        return Ok(());
    };
    let point = registry.policy(name).map_err(|_| {
        let known: BTreeSet<&str> = registry.policies().map(|p| p.name.as_str()).collect();
        Error::validation(format!(
            "there is no policy point '{name}'; the policy points are: {}",
            known.into_iter().collect::<Vec<_>>().join(", ")
        ))
        .with_field("point")
    })?;
    if data.get("effect").and_then(Value::as_str) == Some("approve") {
        if !point.approvable {
            return Err(Error::validation(format!(
                "policy point '{}' cannot ask for approval: a ledger checks the rule that \
                 allows it",
                point.name
            ))
            .with_field("effect"));
        }
        let named = |field: &str| data.get(field).is_some_and(|v| !v.is_null());
        if !named("approver_role_id") && !named("approver_permission") {
            return Err(Error::validation(
                "a rule that asks for approval says who approves: approver_role_id, \
                 approver_permission, or both",
            )
            .with_field("approver_role_id"));
        }
    }
    match condition {
        Value::Null => Ok(()),
        Value::String(source) => point
            .condition(source)
            .map(|_| ())
            .map_err(|error| error.with_field("condition")),
        _ => Err(
            Error::validation("'condition' must be an expression as text, or null")
                .with_field("condition"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_shows_the_facts_it_names() {
        let facts = vec![
            ("over_percent".to_owned(), "6".to_owned()),
            ("ordered".to_owned(), "100".to_owned()),
        ];
        assert_eq!(
            fill(
                "{over_percent}% over the {ordered} ordered; {other}",
                &facts
            ),
            "6% over the 100 ordered; {other}"
        );
    }

    #[test]
    fn a_point_checks_its_own_default() {
        let point = PolicyPoint::new("receipt.over", "Over-receipt")
            .number("over_by", "By how much")
            .as_quantity();
        assert!(
            point
                .clone()
                .refuse_when("facts.over_by > 0")
                .check()
                .is_ok()
        );
        let error = point
            .clone()
            .refuse_when("facts.nope > 0")
            .check()
            .unwrap_err();
        assert!(
            error.contains("supplies no fact 'nope'; its facts are: over_by"),
            "{error}"
        );
        assert!(
            point
                .clone()
                .refuse_when("facts.over_by >")
                .check()
                .is_err()
        );
        assert!(point.refuse_when("user.is_manager").check().is_err());
    }
}
