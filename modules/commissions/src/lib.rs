//! Commissions: plans that pay salespeople a percent of what they sold
//! (net or margin, with tiers, on invoicing or on payment), runs that
//! work it out per period and accrue it, and a statement per salesperson.
//! See "Salespeople and commissions" in ARCHITECTURE.md.

use std::sync::Arc;

use erpcraft_kernel::{
    DocumentHandler, DynCommand, EntityMeta, FieldMeta, FieldType, Migration, Module, PolicyPoint,
    command,
};

mod plan;
mod run;

/// Asked before a run is posted.
pub const POST: &str = "commission_run.post";

pub struct CommissionsModule;

impl Module for CommissionsModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("de", include_str!("../i18n/de.json")),
            ("fr", include_str!("../i18n/fr.json")),
        ]
    }

    fn name(&self) -> &'static str {
        "commissions"
    }

    fn label(&self) -> String {
        "Commissions".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec![
            "gl",
            "open_items",
            "sales",
            "purchasing",
            "pricing",
            "payments",
            "analysis",
        ]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "commissions",
            sql: include_str!("../migrations/0001_commissions.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        let account = |name: &str, label: &str, help: &str| {
            FieldMeta::new(name, label, FieldType::link("account"))
                .required()
                .help(help)
        };
        vec![
            EntityMeta::new("commission_settings", "Commission settings")
                .plural("Commission settings")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(account(
                    "expense_account_id",
                    "Commission expense account",
                    "Debited with what salespeople earn.",
                ))
                .field(account(
                    "payable_account_id",
                    "Commission payable account",
                    "Credited with it, until it is paid.",
                ))
                .field(created())
                .order_by(&["company_id"])
                .company_field("company_id"),
            EntityMeta::new("commission_payee", "Commission payee")
                .field(
                    FieldMeta::new("sales_rep_id", "Salesperson", FieldType::link("sales_rep"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor"))
                        .required()
                        .help(
                            "The vendor the salesperson is paid as: its commission is an open \
                             payable to it.",
                        ),
                )
                .field(created())
                .order_by(&["sales_rep_id"])
                .child_of("sales_rep", "sales_rep_id"),
            EntityMeta::new("commission_plan", "Commission plan")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new(
                        "basis",
                        "Basis",
                        FieldType::labelled(&[("net", "Net sales"), ("margin", "Margin")]),
                    )
                    .help("What the percent is of: net sales, or net less the cost posted."),
                )
                .field(
                    FieldMeta::new(
                        "trigger",
                        "Earned on",
                        FieldType::labelled(&[("invoice", "Invoice"), ("payment", "Payment")]),
                    )
                    .help(
                        "When it is earned: when the invoice is posted, or as it is paid. A \
                         credit takes back when it is posted, either way.",
                    ),
                )
                .field(
                    FieldMeta::new("sales_rep_id", "Salesperson", FieldType::link("sales_rep"))
                        .help("Only this salesperson; every one when empty."),
                )
                .field(
                    FieldMeta::new(
                        "item_group_id",
                        "Item group",
                        FieldType::Link {
                            entity: "price_group".into(),
                            key: "id".into(),
                            filters: [("kind".to_owned(), "item".to_owned())].into(),
                        },
                    )
                    .help("Only items of this group; every line when empty."),
                )
                .field(FieldMeta::new("valid_from", "Valid from", FieldType::Date))
                .field(FieldMeta::new("valid_to", "Valid to", FieldType::Date))
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .active_field("active")
                .company_field("company_id"),
            EntityMeta::new("commission_tier", "Commission tier")
                .field(
                    FieldMeta::new("plan_id", "Plan", FieldType::link("commission_plan"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("from_amount", "From", FieldType::Decimal)
                        .money()
                        .help(
                            "Where the band starts, in the salesperson's basis in one run: \
                             what lies above it, up to the next tier, earns its percent.",
                        ),
                )
                .field(
                    FieldMeta::new("percent", "Percent", FieldType::Decimal)
                        .percent(4)
                        .required(),
                )
                .field(created())
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .immutable()
                        .help("The plan's company; filled in from it."),
                )
                .order_by(&["plan_id", "from_amount"])
                .child_of("commission_plan", "plan_id")
                .company_field("company_id"),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(run::Run), Arc::new(run::Statement)]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                POST,
                "Asked before a commission run is posted: may it accrue what it says the \
                 salespeople earned? Unless configured otherwise it may. A rule can ask for \
                 approval, for example when `facts.total > 10000`.",
            )
            .number("total", "The commission of the run")
            .as_money(Some("currency"))
            .text("currency", "The accounting currency")
            .number("salespeople", "How many salespeople it pays")
            .refusal("this commission run may not be posted"),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(run::Propose),
            command(run::Post),
            command(run::Reverse),
            command(run::Cancel),
        ]
    }
}
