//! Collections: chasing what customers owe.
//!
//! Customer statements (`customer_statement`) show a customer its opening
//! balance, what happened in a period, what is still open and how old it
//! is. Dunning levels say when an overdue item is reminded and what that
//! costs; a dunning run proposes the items that reached a new level, a
//! person reviews it, and posting it makes one letter per customer, which
//! posts its fee and interest and is sent by mail. See "Collections:
//! statements and dunning" in ARCHITECTURE.md.

use std::sync::Arc;

use erpcraft_kernel::document::DocumentHandler;
use erpcraft_kernel::{
    DynCommand, EntityMeta, FieldMeta, FieldType, Migration, Module, PolicyPoint, command,
};

mod dunning;
mod statement;

/// The policy point asked when a run charges less than it proposed.
pub const WAIVE: &str = "dunning.waive";

pub struct CollectionsModule;

impl Module for CollectionsModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("de", include_str!("../i18n/de.json")),
            ("fr", include_str!("../i18n/fr.json")),
        ]
    }

    fn name(&self) -> &'static str {
        "collections"
    }

    fn label(&self) -> String {
        "Collections".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["sales", "open_items", "tax", "party", "printing", "mail"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "collections",
                sql: include_str!("../migrations/0001_collections.sql"),
            },
            Migration {
                version: 2,
                name: "fee_decimals",
                sql: include_str!("../migrations/0002_fee_decimals.sql"),
            },
            Migration {
                version: 3,
                name: "fee_on_change",
                sql: include_str!("../migrations/0003_fee_on_change.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let company = || {
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable()
        };
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        let item = || {
            FieldMeta::new(
                "open_item_id",
                "Open item",
                FieldType::Lookup(
                    erpcraft_openitems::lookup()
                        .field("company_id", "company_id")
                        .value("kind", "receivable"),
                ),
            )
        };
        vec![
            EntityMeta::new("dunning_level", "Dunning level")
                .field(company())
                .field(
                    FieldMeta::new("level", "Level", FieldType::Int)
                        .required()
                        .help("1 for the first reminder, up to 9."),
                )
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new("days_overdue", "Days overdue", FieldType::Int)
                        .required()
                        .help("An item reaches the level this many days after it fell due."),
                )
                .field(
                    FieldMeta::new("fee", "Fee", FieldType::Decimal)
                        .money()
                        .help("Per letter, in the accounting currency."),
                )
                .field(
                    FieldMeta::new("fee_account_id", "Fee account", FieldType::link("account"))
                        .help("Where the fee is posted: an account a plain line may post to."),
                )
                .field(
                    FieldMeta::new("fee_taxable", "Fee is taxed", FieldType::Bool).help(
                        "Tax the fee with the code the company's tax rules propose for the \
                         customer and its address.",
                    ),
                )
                .field(
                    FieldMeta::new("interest_rate", "Interest a year", FieldType::Decimal)
                        .percent(6)
                        .help("On what is overdue, per day on a 365-day year. Not taxed."),
                )
                .field(FieldMeta::new(
                    "interest_account_id",
                    "Interest account",
                    FieldType::link("account"),
                ))
                .field(
                    FieldMeta::new("pay_within_days", "Pay within days", FieldType::Int)
                        .help("What the letter charges is due this many days after it."),
                )
                .field(FieldMeta::new("subject", "Subject", FieldType::Text))
                .field(
                    FieldMeta::new("text", "Text", FieldType::Text)
                        .help("Printed on the letter and sent as the message."),
                )
                .field(
                    FieldMeta::new(
                        "print_template_id",
                        "Letter template",
                        FieldType::link("print_template"),
                    )
                    .help("A print template for dunning letters; empty: the default."),
                )
                .field(created())
                .order_by(&["level"])
                .display(&["level", "name"])
                .company_field("company_id"),
            EntityMeta::new("dunning_hold", "Dunning hold")
                .field(company())
                .field(
                    FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                        .help("Dun nothing of this customer."),
                )
                .field(item().help("Or not this one item."))
                .field(FieldMeta::new("reason", "Reason", FieldType::Text).required())
                .field(
                    FieldMeta::new("active", "Active", FieldType::Bool)
                        .help("Lift the hold by making it inactive."),
                )
                .field(created())
                .order_by(&["created_at"])
                .search(&["reason"])
                .display(&["reason"])
                .company_field("company_id"),
            EntityMeta::new("payment_plan", "Payment plan")
                .field(company())
                .field(
                    FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                        .required(),
                )
                .field(FieldMeta::new("starts_on", "Starts on", FieldType::Date).required())
                .field(
                    FieldMeta::new("ends_on", "Ends on", FieldType::Date)
                        .help("Empty: until it is made inactive."),
                )
                .field(FieldMeta::new("note", "Agreement", FieldType::Text))
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["starts_on"])
                .company_field("company_id"),
            EntityMeta::new("dunning_history", "Dunning history")
                .field(company())
                .field(item().readonly())
                .field(
                    FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                        .readonly(),
                )
                .field(
                    FieldMeta::new("letter_id", "Letter", FieldType::document("dunning_letter"))
                        .readonly(),
                )
                .field(
                    FieldMeta::new("run_id", "Run", FieldType::document("dunning_run")).readonly(),
                )
                .field(FieldMeta::new("level", "Level", FieldType::Int).readonly())
                .field(FieldMeta::new("dunned_on", "Dunned on", FieldType::Date).readonly())
                .field(
                    FieldMeta::new("interest", "Interest", FieldType::Decimal)
                        .money()
                        .readonly(),
                )
                .field(FieldMeta::new("interest_from", "Interest from", FieldType::Date).readonly())
                .field(FieldMeta::new("interest_to", "Interest to", FieldType::Date).readonly())
                .field(created())
                .order_by(&["dunned_on", "level"])
                .read_only()
                .company_field("company_id"),
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                WAIVE,
                "Asked when a dunning run is posted, for each letter whose fee or interest is \
                 below what was proposed: may it be waived? Unless configured otherwise it may \
                 not; a company allows it for those who decide, for example when \
                 `user.department == 'credit'`, or asks for approval.",
            )
            .number("fee_waived", "The fee left out, in the accounting currency")
            .as_money(None)
            .number("interest_waived", "The interest left out")
            .as_money(None)
            .number("level", "The letter's dunning level")
            .text("customer", "The customer's code")
            .refuse_when("facts.fee_waived > 0 || facts.interest_waived > 0")
            .refusal("only someone the company allows may waive dunning fees and interest"),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![
            Arc::new(statement::Statement),
            Arc::new(dunning::Run),
            Arc::new(dunning::Letter),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(statement::Issue),
            command(statement::IssueAll),
            command(dunning::Propose),
            command(dunning::Post),
            command(dunning::Cancel),
            command(dunning::Send),
        ]
    }
}
