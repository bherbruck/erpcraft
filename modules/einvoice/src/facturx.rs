//! A ZUGFeRD / Factur-X invoice: the PDF printing makes of a posted sales
//! invoice or credit note, PDF/A-3b, with the invoice's CII XML carried
//! inside as `factur-x.xml` (relationship Alternative) and the Factur-X
//! XMP extension schema saying what it is.

use erpcraft_kernel::{Ctx, Error, JsonSchema, Result};
use erpcraft_printing::pdf::Attachment;
use erpcraft_printing::xmp::{XmpExtension, XmpProperty};
use serde::Serialize;
use uuid::Uuid;

use crate::export::{self, DocType, Profile};
use crate::model::{Finding, Syntax};

/// The namespace of Factur-X's XMP properties.
pub const NAMESPACE: &str = "urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#";
/// The name the XML carries in the PDF.
pub const FILENAME: &str = "factur-x.xml";

/// The conformance level a profile is, as Factur-X names it.
pub fn conformance(profile: Profile) -> &'static str {
    match profile {
        Profile::En16931 => "EN 16931",
        Profile::Xrechnung => "XRECHNUNG",
    }
}

/// The XMP extension of a Factur-X PDF.
pub fn extension(profile: Profile) -> XmpExtension {
    let property = |name: &str, value: &str, description: &str| XmpProperty {
        name: name.into(),
        value: value.into(),
        value_type: "Text".into(),
        category: "external".into(),
        description: description.into(),
    };
    XmpExtension {
        schema: "Factur-X PDFA Extension Schema".into(),
        namespace: NAMESPACE.into(),
        prefix: "fx".into(),
        properties: vec![
            property(
                "DocumentFileName",
                FILENAME,
                "name of the embedded XML invoice file",
            ),
            property("DocumentType", "INVOICE", "INVOICE"),
            property(
                "Version",
                "1.0",
                "The actual version of the Factur-X XML schema",
            ),
            property(
                "ConformanceLevel",
                conformance(profile),
                "The conformance level of the embedded Factur-X data",
            ),
        ],
    }
}

/// What `einvoice.export_pdf` returns.
#[derive(Serialize, JsonSchema)]
pub struct FacturxOutput {
    /// The PDF, in base64.
    pub data: String,
    pub content_type: String,
    pub filename: String,
    pub title: String,
    /// The specification identifier (BT-24) the embedded invoice claims.
    pub specification: String,
    /// The Factur-X conformance level: `EN 16931` or `XRECHNUNG`.
    pub conformance_level: String,
    /// Advice on the embedded invoice that does not make it invalid.
    pub findings: Vec<Finding>,
}

/// The Factur-X PDF of a posted sales invoice or credit note. Refused when
/// the invoice does not hold, as `einvoice.export` would say.
pub async fn facturx(
    ctx: &mut Ctx<'_>,
    doc_type: DocType,
    id: Uuid,
    profile: Profile,
) -> Result<FacturxOutput> {
    let invoice = export::build(ctx, doc_type, id, profile, Syntax::Cii).await?;
    let xml = export::render(&invoice)?;
    if !xml.valid {
        let reasons: Vec<String> = xml
            .findings
            .iter()
            .filter(|f| f.fatal)
            .map(|f| format!("{}: {}", f.rule, f.message))
            .collect();
        return Err(Error::validation(format!(
            "the e-invoice does not hold, so no ZUGFeRD PDF is made: {}",
            reasons.join("; ")
        )));
    }
    let pdf = erpcraft_printing::pdf_of(
        ctx,
        doc_type.as_str(),
        id,
        None,
        vec![Attachment {
            name: FILENAME.into(),
            mime_type: "text/xml".into(),
            description: "Factur-X/ZUGFeRD invoice".into(),
            data: xml.xml.into_bytes(),
        }],
        Some(&extension(profile)),
    )
    .await?;
    Ok(FacturxOutput {
        data: pdf.data,
        content_type: pdf.content_type,
        filename: pdf.filename,
        title: pdf.title,
        specification: xml.specification,
        conformance_level: conformance(profile).into(),
        findings: xml.findings,
    })
}
