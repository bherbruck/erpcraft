//! Setting up a company: everything a new company needs before its first
//! document, in one command, from a template.
//!
//! A [`SetupTemplate`] is mostly data: a chart of accounts, which account
//! plays which role (bank, inventory, payables, ...), tax codes with their
//! accounts, a warehouse, and when the fiscal year starts. This crate
//! carries `us_small_business`, `ca_small_business` and `de_skr03`. Another module adds its
//! own, for another country or trade, with [`register`]:
//!
//! ```ignore
//! erpcraft_setup::register(SetupTemplate::from_json(include_str!("fr_pcg.json"))?)?;
//! ```
//!
//! `company.setup` creates the company from a template through the
//! kernel's ordinary entity writes, in the command's one transaction, so
//! every validation and permission applies, the audit log names every
//! record created, and a failure leaves nothing behind.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};

use chrono::{Datelike, Months, NaiveDate};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DynCommand, Error, JsonSchema, Module, Result, async_trait,
    command, entity, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const PERMISSION: &str = "company:setup";
const MAX_YEARS: u32 = 5;

// ------------------------------------------------------------- templates

/// One account of a template's chart.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountSpec {
    /// The account number, unique in the chart.
    pub code: String,
    pub name: String,
    /// `asset`, `liability`, `equity`, `income` or `expense`.
    #[serde(rename = "type")]
    pub account_type: String,
    /// A group account only organizes the chart; nothing is posted to it.
    #[serde(default)]
    pub group: bool,
    /// The code of the group account it belongs to, listed before it.
    pub parent: Option<String>,
    /// A bank or cash account, which payments offer. The `bank` role's
    /// account is one whether or not this says so.
    #[serde(default)]
    pub bank: bool,
}

/// Which account of the chart does which job, by code.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Roles {
    /// The bank account payments go through.
    pub bank: String,
    /// The value of goods in stock: the company's default inventory
    /// account rule.
    pub inventory: String,
    /// Where differences in stock value go: the default `stock_variance`
    /// account rule.
    pub stock_variance: String,
    /// Goods received, not yet invoiced.
    pub grni: String,
    /// What is owed to vendors.
    pub payable: String,
    /// What customers owe.
    pub receivable: String,
    /// Sales revenue.
    pub revenue: String,
    /// Cost of goods sold.
    pub cogs: String,
    /// Differences between the invoice and the order price of goods.
    pub price_variance: String,
    /// Where a year-end close puts the year's result.
    pub retained_earnings: String,
    /// Realized exchange gains: the default `fx_gain` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx_gain: Option<String>,
    /// Realized exchange losses: the default `fx_loss` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx_loss: Option<String>,
    /// Early-payment discounts vendors give the company: the default
    /// `discount_received` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discount_received: Option<String>,
    /// Early-payment discounts the company gives customers: the default
    /// `discount_given` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discount_given: Option<String>,
    /// What selling an asset for more than its book value gains: the
    /// default `asset_disposal_gain` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_disposal_gain: Option<String>,
    /// What selling or scrapping an asset for less than its book value
    /// loses: the default `asset_disposal_loss` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_disposal_loss: Option<String>,
    /// Goods on their way between sites: the default `inventory_in_transit`
    /// account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_in_transit: Option<String>,
    /// Labor, machine time and overhead taken into what is made, at
    /// standard: the default `absorbed_cost` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absorbed_cost: Option<String>,
    /// The difference between actual and standard time on a production
    /// order: the default `production_variance` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub production_variance: Option<String>,
    /// Small differences a payment leaves, written off: the default
    /// `write_off` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_off: Option<String>,
    /// Exchange gains on open items revalued at a period's end: the default
    /// `fx_unrealized_gain` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx_unrealized_gain: Option<String>,
    /// Exchange losses on open items revalued at a period's end: the
    /// default `fx_unrealized_loss` account rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx_unrealized_loss: Option<String>,
    /// Goods the company makes. No rule is made for it: the company points
    /// the items it makes here with an `inventory` rule scoped to the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_goods: Option<String>,
}

impl Roles {
    /// The roles a template may leave out, each the default account rule
    /// for the purpose of its name.
    fn rules(&self) -> [(&'static str, &Option<String>); 12] {
        [
            ("fx_gain", &self.fx_gain),
            ("fx_loss", &self.fx_loss),
            ("discount_received", &self.discount_received),
            ("discount_given", &self.discount_given),
            ("asset_disposal_gain", &self.asset_disposal_gain),
            ("asset_disposal_loss", &self.asset_disposal_loss),
            ("inventory_in_transit", &self.inventory_in_transit),
            ("absorbed_cost", &self.absorbed_cost),
            ("production_variance", &self.production_variance),
            ("write_off", &self.write_off),
            ("fx_unrealized_gain", &self.fx_unrealized_gain),
            ("fx_unrealized_loss", &self.fx_unrealized_loss),
        ]
    }

    /// Every role the template fills: the ones every template has, the
    /// rules it gives, and accounts with a job but no rule.
    fn every(&self) -> Vec<(&'static str, &str)> {
        let mut out: Vec<(&'static str, &str)> = self.all().to_vec();
        out.extend(
            self.rules()
                .into_iter()
                .filter_map(|(role, code)| code.as_deref().map(|c| (role, c))),
        );
        if let Some(code) = &self.finished_goods {
            out.push(("finished_goods", code));
        }
        out
    }
}

/// A kind of fixed asset a template sets up for the company, with the
/// accounts that hold it (account rules with scope `asset_class`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetClassSpec {
    pub code: String,
    pub name: String,
    /// `straight_line`, `declining_balance` or `none` (land).
    pub method: String,
    /// For `declining_balance`: percent a year, as a decimal string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declining_rate: Option<String>,
    /// Useful life in months; none for `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub useful_life_months: Option<i32>,
    /// The account that holds the cost.
    pub cost: String,
    /// The account that holds the accumulated depreciation. The cost
    /// account itself where depreciation is written off directly, as in
    /// SKR 03.
    pub accumulated: String,
    /// Where depreciation is expensed.
    pub expense: String,
}

/// Payment terms a template offers. Terms are shared by all companies: an
/// existing one with the same code and the same days and discount is
/// reused.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TermsSpec {
    pub code: String,
    pub name: String,
    /// Days until due, from the invoice's date or the end of its month.
    pub net_days: i32,
    /// Count from the last day of the invoice's month.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub end_of_month: bool,
    /// Early-payment discount in percent, as text: `"2"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discount_percent: Option<String>,
    /// Days within which the discount may be taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discount_days: Option<i32>,
}

impl Roles {
    fn all(&self) -> [(&'static str, &str); 10] {
        [
            ("bank", &self.bank),
            ("inventory", &self.inventory),
            ("stock_variance", &self.stock_variance),
            ("grni", &self.grni),
            ("payable", &self.payable),
            ("receivable", &self.receivable),
            ("revenue", &self.revenue),
            ("cogs", &self.cogs),
            ("price_variance", &self.price_variance),
            ("retained_earnings", &self.retained_earnings),
        ]
    }
}

/// A tax code and the accounts a company posts it to, or a group of codes
/// a line carries together.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaxSpec {
    /// Tax codes are shared by all companies: an existing code with the
    /// same rate is reused.
    pub code: String,
    pub name: String,
    /// Percent of the net amount, as text: `"19"`. For a group, the rates
    /// of its components added up, for people to read; the components'
    /// own rates are what is charged.
    pub rate: String,
    /// The account for tax collected on sales, by code. Not for a group:
    /// each component posts to its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected: Option<String>,
    /// The account for tax paid to vendors, by code: an asset where the
    /// tax is reclaimed (VAT, GST, QST), an expense where it is not (PST,
    /// US sales tax). Not for a group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid: Option<String>,
    /// For a group: the codes it is made of, in the order they are worked
    /// out, each listed before it in the template.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
    /// As a component: charged on the net amount plus the taxes before it
    /// in its group.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compound: bool,
    /// Who the tax is owed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    /// The VAT category of EN 16931 a sale under the code is (`S`, `Z`,
    /// `E`, `AE`, `K`, `G`, `O`, `L`, `M`), as an e-invoice writes it.
    /// Unstated, a rate above 0 % is standard rated. Not for a group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vat_category: Option<String>,
    /// For `E`, `AE`, `K`, `G` and `O`: the reason as a VATEX code, such as
    /// `VATEX-EU-IC` (BT-121), and in words (BT-120).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exemption_reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exemption_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WarehouseSpec {
    pub code: String,
    pub name: String,
}

/// What a new company is made of. See the module documentation.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetupTemplate {
    /// Lowercase letters, digits and underscores: `de_skr03`.
    pub code: String,
    pub name: String,
    pub description: String,
    /// ISO 3166 country code: `US`, `DE`.
    pub country: String,
    /// The accounting currency unless the caller names another.
    pub currency: String,
    /// The month the fiscal year starts in, 1 to 12.
    pub start_month: u32,
    /// Where the template's data comes from, for whoever checks it.
    #[serde(default)]
    pub sources: Vec<String>,
    pub accounts: Vec<AccountSpec>,
    pub roles: Roles,
    #[serde(default)]
    pub tax_codes: Vec<TaxSpec>,
    /// Payment terms vendors and customers can be given.
    #[serde(default)]
    pub payment_terms: Vec<TermsSpec>,
    pub warehouse: Option<WarehouseSpec>,
    /// Kinds of fixed asset, for the assets module.
    #[serde(default)]
    pub asset_classes: Vec<AssetClassSpec>,
}

impl SetupTemplate {
    /// Read a template from JSON and check that it holds together.
    pub fn from_json(text: &str) -> std::result::Result<SetupTemplate, String> {
        let template: SetupTemplate =
            serde_json::from_str(text).map_err(|e| format!("the template is not valid: {e}"))?;
        template.check()?;
        Ok(template)
    }

    /// Every reference in the template leads to an account of its chart
    /// that can be posted to, and every value is one the server accepts.
    pub fn check(&self) -> std::result::Result<(), String> {
        let at = |what: String| format!("template '{}': {what}", self.code);
        let code_ok = !self.code.is_empty()
            && self
                .code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !code_ok {
            return Err(at("the code must be lowercase letters, digits and _".into()));
        }
        if !(1..=12).contains(&self.start_month) {
            return Err(at("start_month must be 1 to 12".into()));
        }
        let mut seen: HashMap<&str, &AccountSpec> = HashMap::new();
        for account in &self.accounts {
            if !["asset", "liability", "equity", "income", "expense"]
                .contains(&account.account_type.as_str())
            {
                return Err(at(format!(
                    "account {} has type '{}'",
                    account.code, account.account_type
                )));
            }
            if let Some(parent) = &account.parent {
                match seen.get(parent.as_str()) {
                    Some(p) if p.group => {}
                    _ => {
                        return Err(at(format!(
                            "account {} names {parent} as its group, which is not a group listed before it",
                            account.code
                        )));
                    }
                }
            }
            if account.bank && (account.group || account.account_type != "asset") {
                return Err(at(format!(
                    "account {} is marked a bank account but is not an asset that can be posted to",
                    account.code
                )));
            }
            if seen.insert(&account.code, account).is_some() {
                return Err(at(format!("account {} is listed twice", account.code)));
            }
        }
        let postable = |role: &str, code: &str| match seen.get(code) {
            Some(a) if !a.group => Ok(()),
            Some(_) => Err(at(format!("{role} is {code}, a group account"))),
            None => Err(at(format!("{role} is {code}, which is not in the chart"))),
        };
        for (role, code) in self.roles.every() {
            postable(role, code)?;
        }
        let mut classes = HashSet::new();
        for class in &self.asset_classes {
            if !classes.insert(&class.code) {
                return Err(at(format!("asset class {} is listed twice", class.code)));
            }
            let rate_ok = match (class.method.as_str(), &class.declining_rate) {
                ("declining_balance", Some(rate)) => rate
                    .parse::<Decimal>()
                    .is_ok_and(|r| r > Decimal::ZERO && r <= Decimal::ONE_HUNDRED),
                ("straight_line" | "none", None) => true,
                _ => false,
            };
            let life_ok = match class.useful_life_months {
                Some(months) => (1..=1200).contains(&months),
                None => class.method == "none",
            };
            if !rate_ok || !life_ok {
                return Err(at(format!(
                    "asset class {}: give straight_line or declining_balance (with a rate above \
                     0 and up to 100) and a life of 1 to 1200 months, or none",
                    class.code
                )));
            }
            for (what, code) in [
                ("cost", &class.cost),
                ("accumulated", &class.accumulated),
                ("expense", &class.expense),
            ] {
                postable(&format!("asset class {} {what}", class.code), code)?;
            }
        }
        let mut terms = HashSet::new();
        for spec in &self.payment_terms {
            if !terms.insert(&spec.code) {
                return Err(at(format!("payment terms {} are listed twice", spec.code)));
            }
            let percent_ok = match &spec.discount_percent {
                Some(percent) => percent
                    .parse::<Decimal>()
                    .is_ok_and(|p| p > Decimal::ZERO && p < Decimal::ONE_HUNDRED),
                None => true,
            };
            if spec.net_days < 0
                || !percent_ok
                || spec.discount_percent.is_some() != spec.discount_days.is_some()
                || spec
                    .discount_days
                    .is_some_and(|d| d < 0 || d > spec.net_days)
            {
                return Err(at(format!(
                    "payment terms {}: give net days from 0, and a discount percent between 0 \
                     and 100 with days no more than the net days, or neither",
                    spec.code
                )));
            }
        }
        let mut codes = HashSet::new();
        for tax in &self.tax_codes {
            if !codes.insert(&tax.code) {
                return Err(at(format!("tax code {} is listed twice", tax.code)));
            }
            let rate: Decimal = tax
                .rate
                .parse()
                .map_err(|_| at(format!("tax code {} has rate '{}'", tax.code, tax.rate)))?;
            if rate < Decimal::ZERO || rate > Decimal::from(100) {
                return Err(at(format!("tax code {} has rate {rate}", tax.code)));
            }
            if let Some(category) = tax.vat_category.as_deref() {
                if !["S", "Z", "E", "AE", "K", "G", "O", "L", "M"].contains(&category) {
                    return Err(at(format!(
                        "tax code {} has VAT category '{category}', which is not one of EN 16931",
                        tax.code
                    )));
                }
                if !tax.components.is_empty() {
                    return Err(at(format!(
                        "tax group {} has a VAT category: a group has none of its own",
                        tax.code
                    )));
                }
            }
            let reason = tax.exemption_reason.is_some() || tax.exemption_reason_code.is_some();
            if reason
                && !matches!(
                    tax.vat_category.as_deref(),
                    Some("E" | "AE" | "K" | "G" | "O")
                )
            {
                return Err(at(format!(
                    "tax code {} has an exemption reason, which only a code of VAT category E, \
                     AE, K, G or O has",
                    tax.code
                )));
            }
            if tax.vat_category.as_deref() == Some("E") && !reason {
                return Err(at(format!(
                    "tax code {} is exempt (E) and must say why: give an exemption reason",
                    tax.code
                )));
            }
            if tax.components.is_empty() {
                let missing =
                    |side: &str| at(format!("tax code {} has no {side} account", tax.code));
                let collected = tax
                    .collected
                    .as_deref()
                    .ok_or_else(|| missing("collected"))?;
                let paid = tax.paid.as_deref().ok_or_else(|| missing("paid"))?;
                postable(&format!("tax {} collected", tax.code), collected)?;
                postable(&format!("tax {} paid", tax.code), paid)?;
                continue;
            }
            // A group: its components come before it, are plain codes,
            // and add up to the rate it shows.
            if tax.collected.is_some() || tax.paid.is_some() || tax.compound {
                return Err(at(format!(
                    "tax group {} posts through its components: it has no accounts of its own and is not compounding",
                    tax.code
                )));
            }
            let mut total = Decimal::ZERO;
            for component in &tax.components {
                let Some(part) = self
                    .tax_codes
                    .iter()
                    .take_while(|t| t.code != tax.code)
                    .find(|t| &t.code == component)
                else {
                    return Err(at(format!(
                        "tax group {} names {component}, which is not a tax code listed before it",
                        tax.code
                    )));
                };
                if !part.components.is_empty() {
                    return Err(at(format!(
                        "tax group {} names {component}, which is a group itself",
                        tax.code
                    )));
                }
                total += part.rate.parse::<Decimal>().unwrap_or_default();
            }
            if total != rate {
                return Err(at(format!(
                    "tax group {} shows rate {rate}, but its components add up to {total}",
                    tax.code
                )));
            }
        }
        Ok(())
    }
}

fn builtin() -> Vec<SetupTemplate> {
    [
        include_str!("templates/us_small_business.json"),
        include_str!("templates/ca_small_business.json"),
        include_str!("templates/de_skr03.json"),
        include_str!("templates/us_manufacturer.json"),
        include_str!("templates/ca_manufacturer.json"),
        include_str!("templates/de_skr03_manufacturer.json"),
    ]
    .into_iter()
    .map(|text| SetupTemplate::from_json(text).expect("a built-in setup template is broken"))
    .collect()
}

fn registry() -> &'static RwLock<Vec<SetupTemplate>> {
    static TEMPLATES: OnceLock<RwLock<Vec<SetupTemplate>>> = OnceLock::new();
    TEMPLATES.get_or_init(|| RwLock::new(builtin()))
}

/// Offer a template to `company.setup`, alongside the built-in ones. A
/// country or trade module calls this when it is constructed. A code can
/// be registered once.
pub fn register(template: SetupTemplate) -> std::result::Result<(), String> {
    template.check()?;
    let mut templates = registry().write().map_err(|_| "the registry is broken")?;
    if templates.iter().any(|t| t.code == template.code) {
        return Err(format!(
            "a setup template '{}' exists already",
            template.code
        ));
    }
    templates.push(template);
    Ok(())
}

/// Every template, built-in and registered.
pub fn templates() -> Vec<SetupTemplate> {
    registry().read().map(|t| t.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------- module

pub struct SetupModule;

impl Module for SetupModule {
    fn name(&self) -> &'static str {
        "setup"
    }

    /// Accounts and fiscal periods belong to the general ledger. Settings
    /// of other modules are filled when those modules are installed.
    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl"]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Setup), command(Templates)]
    }
}

// ------------------------------------------------------------- templates

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TemplatesInput {
    /// Only templates for this country (ISO 3166 code, such as `DE`).
    country: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct TemplateInfo {
    /// What to give `company.setup` as `template`.
    code: String,
    name: String,
    description: String,
    /// ISO 3166 country code.
    country: String,
    /// The accounting currency a company gets unless another is named.
    currency: String,
    /// The month the fiscal year starts in.
    start_month: u32,
    /// How many accounts the chart has.
    accounts: usize,
    /// The tax codes the company can use.
    tax_codes: Vec<String>,
    /// Which account does which job, by code.
    roles: BTreeMap<String, String>,
}

#[derive(Serialize, JsonSchema)]
struct TemplatesOutput {
    /// The templates, by code.
    items: Vec<TemplateInfo>,
}

struct Templates;

#[async_trait]
impl Command for Templates {
    type Input = TemplatesInput;
    type Output = TemplatesOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "setup.templates",
            "The templates a new company can be set up from with company.setup: per country, a \
             chart of accounts, which account does which job, tax codes and the fiscal year. \
             Anyone signed in may list them.",
        )
        .label("Setup templates")
    }

    async fn run(&self, _ctx: &mut Ctx<'_>, input: TemplatesInput) -> Result<TemplatesOutput> {
        let mut items: Vec<TemplateInfo> = templates()
            .into_iter()
            .filter(|t| {
                input
                    .country
                    .as_deref()
                    .is_none_or(|c| t.country.eq_ignore_ascii_case(c))
            })
            .map(|t| TemplateInfo {
                roles: t
                    .roles
                    .all()
                    .iter()
                    .map(|(role, code)| ((*role).to_owned(), (*code).to_owned()))
                    .collect(),
                accounts: t.accounts.len(),
                tax_codes: t.tax_codes.iter().map(|c| c.code.clone()).collect(),
                code: t.code,
                name: t.name,
                description: t.description,
                country: t.country,
                currency: t.currency,
                start_month: t.start_month,
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(TemplatesOutput { items })
    }
}

// ----------------------------------------------------------------- setup

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetupInput {
    /// The template to set up from; see setup.templates.
    template: String,
    /// The new company's code, unique among companies.
    code: String,
    /// The new company's name.
    name: String,
    /// The accounting currency, such as `EUR`. The template's when
    /// omitted.
    base_currency: Option<String>,
    /// The first fiscal year to open, named by the calendar year it starts
    /// in.
    first_year: i32,
    /// The month the fiscal year starts in, 1 to 12. The template's when
    /// omitted (January in the built-in templates).
    start_month: Option<u32>,
    /// How many fiscal years to open, 1 to 5. 1 when omitted.
    years: Option<u32>,
    /// `monthly` (the default): each fiscal year is opened as twelve
    /// periods, named YYYY-MM, so that a month can be closed on its own;
    /// `yearly`: one period per fiscal year.
    #[serde(default)]
    periods: Periods,
    /// The number of the bank account to create, in place of the
    /// template's.
    bank_account_code: Option<String>,
    /// The name of the bank account, in place of the template's, such as
    /// the bank's name.
    bank_account_name: Option<String>,
}

/// How the fiscal years are divided into periods.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Periods {
    #[default]
    Monthly,
    Yearly,
}

/// A fiscal year opened: what a year-end close of it names.
#[derive(Serialize, JsonSchema)]
struct FiscalYear {
    /// FY2026, or FY2026/27 for a year that does not start in January.
    name: String,
    /// Its first period: the close's `first_period_id` (the same as the
    /// last for a yearly period).
    first_period_id: Uuid,
    /// Its last period: the close's `fiscal_period_id`.
    last_period_id: Uuid,
}

#[derive(Serialize, JsonSchema)]
struct SetupOutput {
    /// The company as created.
    company: Value,
    /// The template it was set up from.
    template: String,
    /// How many accounts were created.
    accounts: usize,
    /// Which account does which job: role, then the account's code and id.
    roles: BTreeMap<String, RoleAccount>,
    /// The fiscal periods opened, by name: 2026-01 … 2026-12, or FY2026.
    fiscal_periods: Vec<String>,
    /// The fiscal years, each with its first and last period.
    fiscal_years: Vec<FiscalYear>,
    /// The tax codes set up for the company, created or found.
    tax_codes: Vec<String>,
    /// The asset classes created, by code.
    asset_classes: Vec<String>,
    /// The warehouse created, if the template has one.
    warehouse_id: Option<Uuid>,
    /// What was not set up, and why: a module that is not installed.
    notes: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
struct RoleAccount {
    /// The account's code.
    code: String,
    /// The account's id.
    id: Uuid,
}

struct Setup;

fn id_of(record: &Value) -> Result<Uuid> {
    record
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(|| Error::internal("a created record has no id"))
}

/// The months from `start` to `end`: the first and last day of each.
fn months_of(start: NaiveDate, end: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut months = Vec::new();
    let mut from = start;
    while from <= end {
        let last = from
            .with_day(1)
            .and_then(|first| first.checked_add_months(Months::new(1)))
            .and_then(|next| next.pred_opt())
            .unwrap_or(end)
            .min(end);
        months.push((from, last));
        match last.succ_opt() {
            Some(next) => from = next,
            None => break,
        }
    }
    months
}

/// The fiscal years to open: name, first and last day.
fn fiscal_years(
    first_year: i32,
    start_month: u32,
    years: u32,
) -> Result<Vec<(String, NaiveDate, NaiveDate)>> {
    let mut out = Vec::new();
    for n in 0..years {
        let year = first_year + n as i32;
        let start = NaiveDate::from_ymd_opt(year, start_month, 1)
            .ok_or_else(|| Error::field("first_year", "is not a year"))?;
        let end = start
            .checked_add_months(Months::new(12))
            .and_then(|next| next.pred_opt())
            .ok_or_else(|| Error::field("first_year", "is not a year"))?;
        let name = if start_month == 1 {
            format!("FY{year}")
        } else {
            format!("FY{year}/{:02}", end.year() % 100)
        };
        out.push((name, start, end));
    }
    Ok(out)
}

#[async_trait]
impl Command for Setup {
    type Input = SetupInput;
    type Output = SetupOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "company.setup",
            "Create a company ready to work, from a template (see setup.templates): the \
             company, its chart of accounts, its fiscal years, a warehouse, the purchasing, \
             sales and tax settings, its tax codes, and the account rules for inventory and \
             stock variance. All of it or nothing. Takes the permission company:setup for all \
             companies, and the right to create each kind of record it creates.",
        )
        .permission(PERMISSION)
        .label("Set up company")
        .resource("company")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SetupInput) -> Result<SetupOutput> {
        ctx.actor.require_global(PERMISSION)?;
        let template = templates()
            .into_iter()
            .find(|t| t.code == input.template)
            .ok_or_else(|| {
                let known: Vec<String> = templates().into_iter().map(|t| t.code).collect();
                Error::field(
                    "template",
                    format!(
                        "'{}' is not a setup template; there are: {}",
                        input.template,
                        known.join(", ")
                    ),
                )
            })?;
        let years = input.years.unwrap_or(1);
        if !(1..=MAX_YEARS).contains(&years) {
            return Err(Error::field("years", format!("must be 1 to {MAX_YEARS}")));
        }
        let start_month = input.start_month.unwrap_or(template.start_month);
        if !(1..=12).contains(&start_month) {
            return Err(Error::field("start_month", "must be 1 to 12"));
        }
        if !(1900..=9000).contains(&input.first_year) {
            return Err(Error::field("first_year", "is not a year"));
        }
        let periods = fiscal_years(input.first_year, start_month, years)?;
        let kernel = ctx.kernel;
        let has = |name: &str| kernel.registry().entity(name).is_ok();
        let mut notes = Vec::new();

        // The company. Its code is checked by the database, so a duplicate
        // is refused here, before anything else is written.
        let company = entity::create(
            ctx,
            "company",
            json!({
                "code": input.code,
                "name": input.name,
                "base_currency": input.base_currency.clone().unwrap_or(template.currency.clone()),
            }),
        )
        .await?;
        let company_id = id_of(&company)?;

        // The chart, groups before what they hold.
        let bank_role = template.roles.bank.clone();
        let has_bank_flag = kernel
            .registry()
            .entity("account")
            .is_ok_and(|a| a.fields.iter().any(|f| f.name == "is_bank"));
        let mut accounts: HashMap<String, Uuid> = HashMap::new();
        for spec in &template.accounts {
            let (code, name) = if spec.code == bank_role {
                (
                    input.bank_account_code.clone().unwrap_or(spec.code.clone()),
                    input.bank_account_name.clone().unwrap_or(spec.name.clone()),
                )
            } else {
                (spec.code.clone(), spec.name.clone())
            };
            if code != spec.code && template.accounts.iter().any(|a| a.code == code) {
                return Err(Error::field(
                    "bank_account_code",
                    format!("{code} is another account of the template's chart"),
                ));
            }
            let mut data = json!({
                "company_id": company_id,
                "code": code,
                "name": name,
                "account_type": spec.account_type,
                "is_group": spec.group,
            });
            if has_bank_flag {
                data["is_bank"] = json!(spec.bank || spec.code == bank_role);
            }
            if let Some(parent) = &spec.parent {
                data["parent_id"] = json!(accounts[parent]);
            }
            let created = entity::create(ctx, "account", data).await?;
            accounts.insert(spec.code.clone(), id_of(&created)?);
        }
        let account = |code: &str| accounts[code];
        let roles = &template.roles;

        let mut fiscal_periods = Vec::new();
        let mut fiscal_year_list = Vec::new();
        for (year, start, end) in &periods {
            let parts = match input.periods {
                Periods::Yearly => vec![(year.clone(), *start, *end)],
                Periods::Monthly => months_of(*start, *end)
                    .into_iter()
                    .map(|(from, to)| (from.format("%Y-%m").to_string(), from, to))
                    .collect(),
            };
            let mut ids = Vec::new();
            for (name, from, to) in parts {
                let created = entity::create(
                    ctx,
                    "fiscal_period",
                    json!({ "company_id": company_id, "name": name, "start_date": from, "end_date": to }),
                )
                .await?;
                ids.push(id_of(&created)?);
                fiscal_periods.push(name);
            }
            fiscal_year_list.push(FiscalYear {
                name: year.clone(),
                first_period_id: ids[0],
                last_period_id: ids[ids.len() - 1],
            });
        }

        let mut warehouse_id = None;
        if let Some(warehouse) = &template.warehouse {
            if has("warehouse") {
                let created = entity::create(
                    ctx,
                    "warehouse",
                    json!({ "company_id": company_id, "code": warehouse.code, "name": warehouse.name }),
                )
                .await?;
                warehouse_id = Some(id_of(&created)?);
            } else {
                notes.push("no warehouse: the stock module is not installed".into());
            }
        }

        // The accounts every posting module looks for.
        let settings: [(&str, Value); 2] = [
            (
                "purchasing_settings",
                json!({
                    "grni_account_id": account(&roles.grni),
                    "payable_account_id": account(&roles.payable),
                    "price_variance_account_id": account(&roles.price_variance),
                }),
            ),
            (
                "sales_settings",
                json!({
                    "receivable_account_id": account(&roles.receivable),
                    "revenue_account_id": account(&roles.revenue),
                    "cogs_account_id": account(&roles.cogs),
                }),
            ),
        ];
        for (name, mut data) in settings {
            if has(name) {
                data["company_id"] = json!(company_id);
                entity::create(ctx, name, data).await?;
            } else {
                notes.push(format!("no {name}: its module is not installed"));
            }
        }
        if has("account_rule") {
            for (purpose, code) in [
                ("inventory", &roles.inventory),
                ("stock_variance", &roles.stock_variance),
            ] {
                entity::create(
                    ctx,
                    "account_rule",
                    json!({ "company_id": company_id, "purpose": purpose, "account_id": account(code) }),
                )
                .await?;
            }
            // Where realized exchange differences and early-payment
            // discounts go, when the template says.
            for (purpose, code) in roles.rules() {
                if let Some(code) = code {
                    entity::create(
                        ctx,
                        "account_rule",
                        json!({ "company_id": company_id, "purpose": purpose, "account_id": account(code) }),
                    )
                    .await?;
                }
            }
        }

        // Tax codes are shared by all companies; their accounts are not.
        let mut tax_codes = Vec::new();
        let tax_installed = has("tax_code") && has("tax_settings");
        if !template.tax_codes.is_empty() && !tax_installed {
            notes.push("no tax codes: the tax module is not installed".into());
        } else {
            // The codes created or found so far, for the groups after them.
            let mut group_parts: std::collections::HashMap<String, Uuid> =
                std::collections::HashMap::new();
            for tax in &template.tax_codes {
                let rate: Decimal = tax.rate.parse().map_err(Error::internal)?;
                // An existing code is reused only if it is the same tax in
                // every respect the template states: rate, compounding,
                // authority, and being a group of the same components.
                let found: Option<(Uuid, Decimal, bool, Option<String>, Vec<Uuid>)> =
                    sqlx::query_as(
                        "SELECT c.id, c.rate, c.compound, c.authority,
                                COALESCE(array_agg(g.component_id ORDER BY g.position)
                                         FILTER (WHERE g.component_id IS NOT NULL), '{}')
                         FROM tax_code c
                         LEFT JOIN tax_code_component g ON g.group_id = c.id
                         WHERE c.code = $1 GROUP BY c.id",
                    )
                    .bind(&tax.code)
                    .fetch_optional(&mut *ctx.conn)
                    .await?;
                let wanted_parts: Vec<Uuid> =
                    tax.components.iter().map(|c| group_parts[c]).collect();
                let tax_code_id = match found {
                    Some((id, existing_rate, compound, authority, parts))
                        if existing_rate == rate
                            && compound == tax.compound
                            && authority
                                .as_deref()
                                .map(str::trim)
                                .filter(|a| !a.is_empty())
                                == tax.authority.as_deref().map(str::trim)
                            && parts == wanted_parts =>
                    {
                        // And of the VAT category the template states.
                        if let Some(wanted) = &tax.vat_category {
                            let category: Option<String> = sqlx::query_scalar(
                                "SELECT vat_category FROM tax_code WHERE id = $1",
                            )
                            .bind(id)
                            .fetch_one(&mut *ctx.conn)
                            .await?;
                            if category.as_ref().is_some_and(|c| c != wanted) {
                                return Err(Error::state(format!(
                                    "the tax code {} exists with another VAT category than \
                                     the template's {wanted}; change one of them first",
                                    tax.code
                                )));
                            }
                        }
                        id
                    }
                    Some((_, existing_rate, compound, authority, parts)) => {
                        // What the code is, only to someone who may read tax
                        // codes.
                        let detail = if ctx.actor.can_any("tax_code:read") {
                            format!(
                                " (it has rate {existing_rate}, compounding {compound}, \
                                 authority {}, {} components; the template's: rate {rate}, \
                                 compounding {}, authority {}, {} components)",
                                authority.as_deref().unwrap_or("none"),
                                parts.len(),
                                tax.compound,
                                tax.authority.as_deref().unwrap_or("none"),
                                wanted_parts.len(),
                            )
                        } else {
                            String::new()
                        };
                        return Err(Error::state(format!(
                            "the tax code {} exists and is not the template's tax{detail}; \
                             change one of them first",
                            tax.code
                        )));
                    }
                    None => {
                        let mut code = json!({
                            "code": tax.code, "name": tax.name, "rate": tax.rate,
                            "compound": tax.compound,
                        });
                        if let Some(authority) = &tax.authority {
                            code["authority"] = json!(authority);
                        }
                        for (field, value) in [
                            ("vat_category", &tax.vat_category),
                            ("exemption_reason_code", &tax.exemption_reason_code),
                            ("exemption_reason", &tax.exemption_reason),
                        ] {
                            if let Some(value) = value {
                                code[field] = json!(value);
                            }
                        }
                        let id = id_of(&entity::create(ctx, "tax_code", code).await?)?;
                        for (position, component) in tax.components.iter().enumerate() {
                            entity::create(
                                ctx,
                                "tax_code_component",
                                json!({
                                    "group_id": id,
                                    "component_id": group_parts[component],
                                    "position": position + 1,
                                }),
                            )
                            .await?;
                        }
                        id
                    }
                };
                group_parts.insert(tax.code.clone(), tax_code_id);
                // A group posts through its components.
                if let (Some(collected), Some(paid)) = (&tax.collected, &tax.paid) {
                    entity::create(
                        ctx,
                        "tax_settings",
                        json!({
                            "company_id": company_id,
                            "tax_code_id": tax_code_id,
                            "collected_account_id": account(collected),
                            "paid_account_id": account(paid),
                        }),
                    )
                    .await?;
                }
                tax_codes.push(tax.code.clone());
            }
        }

        // Asset classes and the accounts that hold them.
        let mut asset_classes = Vec::new();
        let assets_installed = has("asset_class") && has("account_rule");
        if !template.asset_classes.is_empty() && !assets_installed {
            notes.push("no asset classes: the assets module is not installed".into());
        } else {
            for spec in &template.asset_classes {
                let mut class = json!({
                    "company_id": company_id, "code": spec.code, "name": spec.name,
                    "method": spec.method,
                });
                if let Some(rate) = &spec.declining_rate {
                    class["declining_rate"] = json!(rate);
                }
                if let Some(months) = spec.useful_life_months {
                    class["useful_life_months"] = json!(months);
                }
                let class_id = id_of(&entity::create(ctx, "asset_class", class).await?)?;
                for (purpose, code) in [
                    ("asset_cost", &spec.cost),
                    ("accumulated_depreciation", &spec.accumulated),
                    ("depreciation_expense", &spec.expense),
                ] {
                    entity::create(
                        ctx,
                        "account_rule",
                        json!({ "company_id": company_id, "purpose": purpose,
                                "scope_type": "asset_class", "scope_id": class_id,
                                "account_id": account(code) }),
                    )
                    .await?;
                }
                asset_classes.push(spec.code.clone());
            }
        }

        // Payment terms are shared by all companies, like tax codes.
        if !template.payment_terms.is_empty() && !has("payment_terms") {
            notes.push("no payment terms: the open items module is not installed".into());
        } else {
            for spec in &template.payment_terms {
                let percent: Option<Decimal> = spec
                    .discount_percent
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(Error::internal)?;
                let found: Option<(i32, bool, Option<Decimal>, Option<i32>)> = sqlx::query_as(
                    "SELECT net_days, end_of_month, discount_percent, discount_days
                     FROM payment_terms WHERE code = $1",
                )
                .bind(&spec.code)
                .fetch_optional(&mut *ctx.conn)
                .await?;
                match found {
                    Some(same)
                        if same
                            == (
                                spec.net_days,
                                spec.end_of_month,
                                percent,
                                spec.discount_days,
                            ) => {}
                    Some(_) => {
                        return Err(Error::state(format!(
                            "the payment terms {} exist and are not the template's; change one \
                             of them first",
                            spec.code
                        )));
                    }
                    None => {
                        let mut terms = json!({
                            "code": spec.code, "name": spec.name, "net_days": spec.net_days,
                            "end_of_month": spec.end_of_month,
                        });
                        if let (Some(percent), Some(days)) =
                            (&spec.discount_percent, spec.discount_days)
                        {
                            terms["discount_percent"] = json!(percent);
                            terms["discount_days"] = json!(days);
                        }
                        entity::create(ctx, "payment_terms", terms).await?;
                    }
                }
            }
        }

        let roles = roles
            .every()
            .iter()
            .map(|(role, code)| {
                let created_code = if *code == bank_role {
                    input
                        .bank_account_code
                        .clone()
                        .unwrap_or((*code).to_owned())
                } else {
                    (*code).to_owned()
                };
                (
                    (*role).to_owned(),
                    RoleAccount {
                        code: created_code,
                        id: account(code),
                    },
                )
            })
            .collect();
        Ok(SetupOutput {
            company,
            template: template.code,
            accounts: accounts.len(),
            roles,
            fiscal_periods,
            fiscal_years: fiscal_year_list,
            tax_codes,
            asset_classes,
            warehouse_id,
            notes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_templates_hold_together() {
        let all = builtin();
        assert_eq!(all.len(), 6);
        for template in &all {
            template.check().unwrap();
        }
    }

    #[test]
    fn a_broken_template_is_refused() {
        let mut text: Value =
            serde_json::from_str(include_str!("templates/us_small_business.json")).unwrap();
        text["roles"]["bank"] = json!("9999");
        let error = SetupTemplate::from_json(&text.to_string()).unwrap_err();
        assert!(error.contains("bank is 9999"), "{error}");
        text["roles"]["bank"] = json!("1000");
        let error = SetupTemplate::from_json(&text.to_string()).unwrap_err();
        assert!(error.contains("group account"), "{error}");
        assert!(
            register(builtin().remove(0)).is_err(),
            "a code is registered once"
        );
    }

    #[test]
    fn fiscal_years_follow_the_start_month() {
        let years = fiscal_years(2026, 1, 2).unwrap();
        assert_eq!(years[0].0, "FY2026");
        assert_eq!(years[1].2, NaiveDate::from_ymd_opt(2027, 12, 31).unwrap());
        let years = fiscal_years(2026, 7, 1).unwrap();
        assert_eq!(years[0].0, "FY2026/27");
        assert_eq!(years[0].1, NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
        assert_eq!(years[0].2, NaiveDate::from_ymd_opt(2027, 6, 30).unwrap());
    }
}
