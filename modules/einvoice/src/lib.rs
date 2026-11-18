//! Electronic invoices after the European standard EN 16931: XRechnung (in
//! UBL 2.1 or CII syntax) and the XML of ZUGFeRD / Factur-X.
//!
//! `einvoice.export` writes a posted sales invoice or sales credit note as
//! an EN 16931 invoice, XRechnung 3.0 by default; `einvoice.read` reads an
//! incoming one (XML, or a ZUGFeRD / Factur-X PDF), checks it and proposes
//! the vendor, order and lines it belongs to; `einvoice.import` drafts the
//! vendor invoice. The seller's and buyer's addresses, VAT identifiers,
//! electronic addresses and the buyer reference are party data, the VAT
//! category of a tax code is the tax code's own, and the IBAN of the
//! account paid into is banking's; this module keeps the record of the
//! invoices it imported. See README.md for the sources and for what is and
//! is not checked.

use std::sync::Arc;

use base64::Engine;
use erpcraft_kernel::document::DocumentView;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, DynCommand, EntityMeta, Error, FieldMeta, FieldType, JsonSchema,
    Migration, Module, Result, async_trait, command, schemars,
};
use serde::Deserialize;
use uuid::Uuid;

pub mod cii;
pub mod export;
pub mod facturx;
pub mod import;
pub mod model;
pub mod pdf;
pub mod ubl;
pub mod xml;

use model::{Invoice, Syntax};

pub struct EinvoiceModule;

impl Module for EinvoiceModule {
    fn name(&self) -> &'static str {
        "einvoice"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "tax", "sales", "purchasing", "banking", "party"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "einvoice",
                sql: include_str!("../migrations/0001_einvoice.sql"),
            },
            Migration {
                version: 2,
                name: "bank_numbers_in_banking",
                sql: include_str!("../migrations/0002_bank_numbers_in_banking.sql"),
            },
            Migration {
                version: 3,
                name: "party_data_in_party",
                sql: include_str!("../migrations/0003_party_data_in_party.sql"),
            },
            Migration {
                version: 4,
                name: "vat_category_on_tax_code",
                sql: include_str!("../migrations/0004_vat_category_on_tax_code.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![import_entity()]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Export),
            command(ExportPdf),
            command(Read),
            command(Import),
        ]
    }
}

// --------------------------------------------------------------- entities

fn created() -> FieldMeta {
    FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly()
}

fn import_entity() -> EntityMeta {
    EntityMeta::new("einvoice_import", "Imported e-invoice")
        .field(FieldMeta::new("company_id", "Company", FieldType::link("company")).readonly())
        .field(FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor")).readonly())
        .field(FieldMeta::new("invoice_number", "Invoice number", FieldType::Text).readonly())
        .field(FieldMeta::new("issue_date", "Issued", FieldType::Date).readonly())
        .field(FieldMeta::new("syntax", "Syntax", FieldType::options(&["ubl", "cii"])).readonly())
        .field(FieldMeta::new("sha256", "SHA-256", FieldType::Text).readonly())
        .field(
            FieldMeta::new(
                "document_id",
                "Vendor invoice",
                FieldType::document("vendor_invoice"),
            )
            .readonly(),
        )
        .field(created())
        .order_by(&["created_at"])
        .search(&["invoice_number"])
        .display(&["invoice_number"])
        .company_field("company_id")
        .read_only()
}

// ------------------------------------------------------------------- read

/// Read an EN 16931 invoice in either syntax from XML text.
pub fn read_xml(content: &str) -> std::result::Result<Invoice, String> {
    let root = xml::parse(content)?;
    let ns = root.namespace().unwrap_or_default();
    let invoice = match (root.name.as_str(), ns) {
        ("Invoice", ubl::INVOICE_NS) | ("CreditNote", ubl::CREDIT_NOTE_NS) => ubl::read(&root)?,
        ("CrossIndustryInvoice", cii::RSM) => cii::read(&root)?,
        (name, ns) => {
            return Err(format!(
                "this is not an EN 16931 invoice: its root element is {name} in namespace \
                 '{ns}', not a UBL 2.1 Invoice or CreditNote or a CII CrossIndustryInvoice"
            ));
        }
    };
    if !invoice.specification.starts_with(model::EN16931) {
        return Err(format!(
            "this is not an EN 16931 invoice: its specification identifier (BT-24) is '{}' \
             (ZUGFeRD's MINIMUM and BASIC WL profiles are not invoices under EN 16931)",
            invoice.specification
        ));
    }
    Ok(invoice)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContentInput {
    /// The company the invoice is for.
    company_id: Uuid,
    /// The invoice as XML text (XRechnung, UBL or CII). Give this or
    /// `content_base64`.
    content: Option<String>,
    /// The file, base64-encoded: XML, or a ZUGFeRD / Factur-X PDF whose
    /// embedded XML is read. At most 20 MiB.
    content_base64: Option<String>,
}

/// The invoice in a request, and where it came from.
async fn invoice_of(input: &ContentInput) -> Result<(Invoice, &'static str, String)> {
    let (bytes, field): (Vec<u8>, &str) = match (&input.content, &input.content_base64) {
        (Some(text), None) => (text.as_bytes().to_vec(), "content"),
        (None, Some(encoded)) => {
            if encoded.len() > pdf::MAX_PDF_BYTES * 4 / 3 + 4 {
                return Err(Error::field(
                    "content_base64",
                    "the file is larger than 20 MiB",
                ));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .map_err(|_| Error::field("content_base64", "is not base64"))?;
            (bytes, "content_base64")
        }
        _ => {
            return Err(Error::validation(
                "give the invoice as content (XML text) or as content_base64 (an XML or PDF file)",
            ));
        }
    };
    let hash = import::sha256(&bytes);
    let (xml, container) = if pdf::is_pdf(&bytes) {
        if field == "content" {
            return Err(Error::field(
                "content",
                "is a PDF: send it as content_base64",
            ));
        }
        (
            pdf::embedded_xml(bytes)
                .await
                .map_err(|e| Error::field(field, e))?,
            "pdf",
        )
    } else {
        (
            String::from_utf8(bytes).map_err(|_| Error::field(field, "is not UTF-8 text"))?,
            "xml",
        )
    };
    let invoice =
        read_xml(&xml).map_err(|e| Error::field(field, format!("cannot be read: {e}")))?;
    Ok((invoice, container, hash))
}

// --------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportInput {
    /// What to send: `sales_invoice` or `sales_credit_note`.
    doc_type: export::DocType,
    /// The posted document.
    id: Uuid,
    /// `ubl` (UBL 2.1, the default) or `cii` (UN/CEFACT CII, the syntax
    /// ZUGFeRD / Factur-X embed).
    syntax: Option<Syntax>,
    /// `xrechnung` (the default: EN 16931 with the German rules) or
    /// `en16931` (the standard alone).
    profile: Option<export::Profile>,
}

struct Export;

#[async_trait]
impl Command for Export {
    type Input = ExportInput;
    type Output = export::ExportOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "einvoice.export",
            "Write a posted sales invoice or sales credit note as an EN 16931 electronic \
             invoice: XRechnung 3.0 by default, in UBL 2.1 or CII syntax. The XML is read back \
             and checked against the rules this module checks (mandatory terms, line and VAT \
             arithmetic, totals, XRechnung's German rules) before it is returned. Refused with \
             what is missing and where to set it when the company's or the customer's party \
             records (address, tax identifier, contact, electronic address) or a tax code's \
             VAT category lack something the \
             invoice needs. Takes einvoice:export and the right to read the document.",
        )
        .permission("einvoice:export")
        .label("E-invoice")
        .resource("einvoice")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ExportInput) -> Result<export::ExportOutput> {
        let syntax = input.syntax.unwrap_or(Syntax::Ubl);
        let invoice = export::build(
            ctx,
            input.doc_type,
            input.id,
            input.profile.unwrap_or_default(),
            syntax,
        )
        .await?;
        export::render(&invoice)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportPdfInput {
    /// What to send: `sales_invoice` or `sales_credit_note`.
    doc_type: export::DocType,
    /// The posted document.
    id: Uuid,
    /// `en16931` (the default: Factur-X's EN 16931 profile) or `xrechnung`
    /// (for a German public buyer).
    profile: Option<export::Profile>,
}

struct ExportPdf;

#[async_trait]
impl Command for ExportPdf {
    type Input = ExportPdfInput;
    type Output = facturx::FacturxOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "einvoice.export_pdf",
            "Write a posted sales invoice or sales credit note as a ZUGFeRD / Factur-X invoice: \
             the printed PDF (PDF/A-3b) with the invoice's CII XML inside as factur-x.xml and \
             the Factur-X XMP metadata, profile EN 16931 by default or XRechnung. Refused, with \
             what is missing, when the invoice does not hold, as einvoice.export would say. \
             Takes einvoice:export and the right to read the document.",
        )
        .permission("einvoice:export")
        .label("ZUGFeRD PDF")
        .resource("einvoice")
        .target(false)
    }

    async fn run(
        &self,
        ctx: &mut Ctx<'_>,
        input: ExportPdfInput,
    ) -> Result<facturx::FacturxOutput> {
        facturx::facturx(
            ctx,
            input.doc_type,
            input.id,
            input.profile.unwrap_or(export::Profile::En16931),
        )
        .await
    }
}

struct Read;

#[async_trait]
impl Command for Read {
    type Input = ContentInput;
    type Output = import::ReadOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "einvoice.read",
            "Read an incoming EN 16931 invoice (XRechnung in UBL or CII, or the XML of a \
             ZUGFeRD / Factur-X PDF) without importing it: what it says, the rules it breaks, \
             the vendor its seller's VAT identifier names (or the vendors it may be, by name), \
             the open purchase order its order reference names, and the order line and tax \
             code each line would take. A DTD, an entity, JavaScript in a PDF and files over \
             the size limits are refused.",
        )
        .permission("vendor_invoice:create")
        .label("Read e-invoice")
        .resource("einvoice")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ContentInput) -> Result<import::ReadOutput> {
        ctx.require("vendor_invoice:create", input.company_id)?;
        let (invoice, container, _) = invoice_of(&input).await?;
        import::read(
            ctx,
            input.company_id,
            invoice,
            container,
            &std::collections::BTreeMap::new(),
        )
        .await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportInput {
    /// The company the invoice is for.
    company_id: Uuid,
    /// The invoice as XML text. Give this or `content_base64`.
    content: Option<String>,
    /// The file, base64-encoded: XML or a ZUGFeRD / Factur-X PDF.
    content_base64: Option<String>,
    /// The vendor, when the seller's VAT identifier does not name one:
    /// one of the candidates `einvoice.read` proposes.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "vendor", "key": "id" }))]
    vendor_id: Option<Uuid>,
    /// The account for lines that invoice no order line (a service, a
    /// direct invoice).
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "account", "key": "id" }))]
    account_id: Option<Uuid>,
    /// The tax code for a line, by the line's identifier (BT-126), where
    /// `einvoice.read` found none or several for its VAT category and
    /// rate.
    line_tax_codes: Option<std::collections::BTreeMap<String, Uuid>>,
    /// Draft it although the draft would not say what the invoice says
    /// (`differences` of `einvoice.read`: totals that differ by rounding,
    /// an amount already paid). The differences are written into the
    /// draft's memo.
    #[serde(default)]
    accept_differences: bool,
}

struct Import;

#[async_trait]
impl Command for Import {
    type Input = ImportInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "einvoice.import",
            "Draft a vendor invoice from an incoming EN 16931 invoice. The vendor is the one \
             whose EU VAT identifier (party.set) is the seller's, or the one given as \
             vendor_id; nothing is guessed from a name. When the buyer's order reference \
             (BT-13) is the number of an open purchase order of that vendor, the invoice is \
             drafted against it and each line against the order line its line reference or \
             item code names; other lines, and a direct invoice, go to account_id. An invoice \
             that breaks a rule, a credit note, and one imported before are refused. The draft \
             is checked and posted like one entered by hand.",
        )
        .permission("vendor_invoice:create")
        .label("Import e-invoice")
        .resource("einvoice")
        .target(false)
        .creates("vendor_invoice")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ImportInput) -> Result<DocumentView> {
        ctx.require("vendor_invoice:create", input.company_id)?;
        let content = ContentInput {
            company_id: input.company_id,
            content: input.content,
            content_base64: input.content_base64,
        };
        let (invoice, container, hash) = invoice_of(&content).await?;
        let chosen = input.line_tax_codes.unwrap_or_default();
        let read = import::read(ctx, input.company_id, invoice, container, &chosen).await?;
        import::import(
            ctx,
            input.company_id,
            read,
            hash,
            import::Choices {
                vendor_id: input.vendor_id,
                account_id: input.account_id,
                line_tax_codes: chosen,
                accept_differences: input.accept_differences,
            },
        )
        .await
    }
}
