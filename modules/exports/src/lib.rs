//! What a company's outside accountant needs from it: DATEV files for a
//! German tax adviser, the 1099 and T5018 information returns, and the
//! general ledger as CSV for any other tool. See "Accountant exports" in
//! ARCHITECTURE.md, and docs/ops/gobd.md.

use std::sync::Arc;

use erpcraft_kernel::{DynCommand, EntityMeta, FieldMeta, FieldType, Migration, Module, command};

mod datev;
mod file;
mod gdpdu;
mod gl;
mod returns;
mod zip;

pub struct ExportsModule;

impl Module for ExportsModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("de", include_str!("../i18n/de.json")),
            ("fr", include_str!("../i18n/fr.json")),
        ]
    }

    fn name(&self) -> &'static str {
        "exports"
    }

    fn label(&self) -> String {
        "Accountant exports".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec![
            "gl",
            "tax",
            "party",
            "open_items",
            "sales",
            "purchasing",
            "payments",
        ]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "exports",
            sql: include_str!("../migrations/0001_exports.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        vec![
            EntityMeta::new("datev_settings", "DATEV settings")
                .plural("DATEV settings")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("consultant_number", "Consultant number", FieldType::Int)
                        .required()
                        .help("The Beraternummer, 1001 to 9999999."),
                )
                .field(
                    FieldMeta::new("client_number", "Client number", FieldType::Int)
                        .required()
                        .help("The Mandantennummer, 1 to 99999."),
                )
                .field(
                    FieldMeta::new("account_length", "Account length", FieldType::Int).help(
                        "The Sachkontenlänge, 4 to 8 (4 when empty); personal accounts are one \
                         digit longer.",
                    ),
                )
                .field(
                    FieldMeta::new("chart", "Chart of accounts", FieldType::Text)
                        .help("03 for SKR 03 (the default), 04 for SKR 04."),
                )
                .field(
                    FieldMeta::new(
                        "fiscal_year_start_month",
                        "Fiscal year starts",
                        FieldType::Int,
                    )
                    .help("The month, 1 (January, the default) to 12."),
                )
                .field(
                    FieldMeta::new("dictation_code", "Dictation code", FieldType::Text)
                        .help("The Diktatkürzel: two letters or digits."),
                )
                .field(
                    FieldMeta::new("kost1_dimension_id", "KOST1", FieldType::link("dimension"))
                        .help("The dimension whose values are cost center 1."),
                )
                .field(
                    FieldMeta::new("kost2_dimension_id", "KOST2", FieldType::link("dimension"))
                        .help("The dimension whose values are cost center 2."),
                )
                .field(created())
                .order_by(&["company_id"])
                .company_field("company_id"),
            EntityMeta::new("datev_tax_key", "DATEV tax key")
                .field(
                    FieldMeta::new("tax_code_id", "Tax code", FieldType::link("tax_code"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("sales_key", "Key on sales", FieldType::Text)
                        .help("The BU-Schlüssel of the code on sales, such as 3 for 19 %."),
                )
                .field(
                    FieldMeta::new("purchase_key", "Key on purchases", FieldType::Text)
                        .help("The BU-Schlüssel of the code on purchases, such as 9 for 19 %."),
                )
                .field(created())
                .order_by(&["tax_code_id"]),
            EntityMeta::new("datev_account_setting", "DATEV account setting")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("account_id", "Account", FieldType::link("account"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("automatic", "Automatic account", FieldType::Bool)
                        .required()
                        .help(
                            "DATEV works out the tax from the account: lines on it carry no \
                             key.",
                        ),
                )
                .field(created())
                .order_by(&["account_id"])
                .company_field("company_id"),
            EntityMeta::new("datev_party_account", "DATEV personal account")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new(
                        "party_type",
                        "Party type",
                        FieldType::labelled(&[("customer", "Customer"), ("vendor", "Vendor")]),
                    )
                    .required()
                    .immutable(),
                )
                .field(
                    FieldMeta::new(
                        "party_id",
                        "Party",
                        FieldType::entity_named_by("party_type"),
                    )
                    .required()
                    .immutable(),
                )
                .field(
                    FieldMeta::new("account_number", "Account number", FieldType::Int)
                        .required()
                        .help("The Debitor or Kreditor number in DATEV."),
                )
                .field(created())
                .order_by(&["account_number"])
                .company_field("company_id"),
            EntityMeta::new("vendor_information_return", "Information return")
                .field(
                    FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new(
                        "form",
                        "Form",
                        FieldType::labelled(&[
                            ("us_1099", "Form 1099 (US)"),
                            ("ca_t5018", "T5018 (Canada)"),
                        ]),
                    )
                    .required(),
                )
                .field(
                    FieldMeta::new(
                        "box",
                        "Box",
                        FieldType::labelled(&[
                            ("nec_1", "1099-NEC box 1: nonemployee compensation"),
                            ("misc_1", "1099-MISC box 1: rents"),
                            ("misc_2", "1099-MISC box 2: royalties"),
                            ("misc_3", "1099-MISC box 3: other income"),
                            (
                                "misc_6",
                                "1099-MISC box 6: medical and health care payments",
                            ),
                            (
                                "misc_10",
                                "1099-MISC box 10: gross proceeds paid to an attorney",
                            ),
                        ]),
                    )
                    .help("For a 1099: the box the vendor's payments are reported in."),
                )
                .field(created())
                .order_by(&["vendor_id"])
                .child_of("vendor", "vendor_id"),
            EntityMeta::new("export_log", "Export")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company")).immutable(),
                )
                .field(FieldMeta::new("kind", "Kind", FieldType::Text))
                .field(FieldMeta::new("from_date", "From", FieldType::Date))
                .field(FieldMeta::new("to_date", "To", FieldType::Date))
                .field(FieldMeta::new("rows", "Rows", FieldType::Int))
                .field(FieldMeta::new("filename", "File", FieldType::Text))
                .field(FieldMeta::new("sha256", "SHA-256", FieldType::Text))
                .field(FieldMeta::new("fixed", "Fixed", FieldType::Bool))
                .field(FieldMeta::new(
                    "created_by",
                    "Created by",
                    FieldType::link("user"),
                ))
                .field(created())
                .order_by(&["created_at"])
                .company_field("company_id")
                .read_only(),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(datev::ExportPostings),
            command(datev::ExportParties),
            command(datev::ExportAccounts),
            command(returns::Report),
            command(returns::Export),
            command(gl::Export),
            command(gdpdu::Export),
        ]
    }
}
