//! Printing: documents rendered to HTML a browser prints, from templates
//! an administrator can change without a rebuild.
//!
//! A template is a record of `print_template` for one document type, for
//! one company or for all. `document.render` picks the one asked for, else
//! the company's default, else the default for all companies, else the
//! one built into this crate, and renders it with what the caller may read
//! of the document. The browser prints the page; `document.render_pdf`
//! makes a PDF/A-3 of it on the server ([`pdf`]).
//!
//! What a template can see is exactly what `document.get` returns to the
//! caller, plus the company's code, name and currency, plus what the
//! caller may read of the records the document refers to, read through
//! the kernel's entity and document reads so that row and field rules
//! apply. See [`engine`] for what a template can do.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use erpcraft_kernel::registry::Registry;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, DocTypeMeta, DocumentView, DynCommand, EntityMeta, Error, FieldMeta,
    FieldType, JsonSchema, Migration, Module, Result, async_trait, command, company, document,
    entity, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

mod builtin;
mod engine;
mod facts;
mod letterhead;
pub mod pdf;
pub mod xmp;

pub struct PrintingModule;

impl Module for PrintingModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![("de", include_str!("../i18n/de.json"))]
    }

    fn name(&self) -> &'static str {
        "printing"
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "printing",
                sql: include_str!("../migrations/0001_printing.sql"),
            },
            Migration {
                version: 2,
                name: "company_logo",
                sql: include_str!("../migrations/0002_company_logo.sql"),
            },
            Migration {
                version: 3,
                name: "locale",
                sql: include_str!("../migrations/0003_locale.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![
            EntityMeta::new("print_template", "Print template")
                .field(
                    FieldMeta::new("doc_type", "Document type", FieldType::Text)
                        .required()
                        .help("The document type the template prints, such as sales_invoice."),
                )
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .help("Only for this company's documents. Empty: for every company's."),
                )
                .field(
                    FieldMeta::new("body", "Template", FieldType::Text)
                        .required()
                        .help("HTML with MiniJinja tags. See the built-in templates for the data."),
                )
                .field(
                    FieldMeta::new("is_default", "Default", FieldType::Bool).help(
                        "Used when no template is asked for. One per document type and company, \
                         and one for all companies.",
                    ),
                )
                .field(FieldMeta::new("locale", "Language", FieldType::Text).help(
                    "The language it is written in, such as de: documents to a party of \
                         that language print with it. Empty: any language.",
                ))
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
                .order_by(&["doc_type", "name"])
                .search(&["name", "doc_type"])
                .validator(validate),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Render),
            command(RenderPdf),
            command(Preview),
            command(Builtin),
            command(letterhead::SetLogo),
            command(letterhead::GetLogo),
            command(letterhead::RemoveLogo),
        ]
    }
}

/// The checks of a template beyond its fields' types: the document type
/// exists, and the template compiles and holds nothing active.
fn validate(registry: &Registry, data: &Map<String, Value>) -> Result<()> {
    if let Some(doc_type) = data.get("doc_type") {
        let doc_type = doc_type.as_str().unwrap_or_default();
        if registry.doc_type(doc_type).is_err() {
            return Err(Error::field(
                "doc_type",
                format!("'{doc_type}' is not a document type"),
            ));
        }
    }
    if let Some(body) = data.get("body") {
        let body = body.as_str().unwrap_or_default();
        if body.trim().is_empty() {
            return Err(Error::field("body", "is empty"));
        }
        engine::check_source(body)
            .map_err(|problem| Error::field("body", format!("cannot be used: {problem}")))?;
    }
    Ok(())
}

// ------------------------------------------------------------------- render

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenderInput {
    /// Document type, for example `sales_invoice`.
    doc_type: String,
    /// Id of the document.
    id: Uuid,
    /// The print template to use. Omit for the default of the document's
    /// company, else the default for all companies, else the built-in one.
    template_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema)]
struct Rendered {
    /// The page: a whole HTML document with its own print stylesheet. Show
    /// it in a sandboxed iframe (`sandbox` without `allow-scripts`) and
    /// print from there.
    html: String,
    /// The print template used; null for the built-in one.
    template_id: Option<Uuid>,
    /// What the page is called: the document type and number.
    title: String,
}

struct Render;

#[async_trait]
impl Command for Render {
    type Input = RenderInput;
    type Output = Rendered;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "document.render",
            "Render a document as an HTML page to print, or to save as a PDF from the browser, \
             with a print template: the one given, else the default of the document's company, \
             else the default for all companies, else the built-in one. The page shows only \
             what the caller may read. Show it in a sandboxed iframe without scripts.",
        )
        .label("Print")
        .resource("document")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RenderInput) -> Result<Rendered> {
        render(
            ctx,
            &input.doc_type,
            input.id,
            Source::Stored(input.template_id),
        )
        .await
    }
}

#[derive(Serialize, JsonSchema)]
pub struct RenderedPdf {
    /// The PDF, in base64: PDF/A-3b, A4, with its fonts embedded.
    pub data: String,
    pub content_type: String,
    /// A file name for it: the document's type and number.
    pub filename: String,
    pub title: String,
    pub template_id: Option<Uuid>,
}

struct RenderPdf;

#[async_trait]
impl Command for RenderPdf {
    type Input = RenderInput;
    type Output = RenderedPdf;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "document.render_pdf",
            "Render a document as a PDF on the server (PDF/A-3b, A4), from the same print \
             template and with the same data as document.render: the letterhead, the parties, \
             the lines, the totals. The layout follows the built-in stylesheet; styles a \
             template sets on its own elements show in the HTML page only. The PDF is returned \
             in base64.",
        )
        .label("PDF")
        .resource("document")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RenderInput) -> Result<RenderedPdf> {
        pdf_of(
            ctx,
            &input.doc_type,
            input.id,
            input.template_id,
            vec![],
            None,
        )
        .await
    }
}

/// How long a PDF may take to lay out.
const PDF_TIME: std::time::Duration = std::time::Duration::from_secs(20);

/// A document as a PDF, with `attachments` carried inside: for a module
/// that sends it, or makes an e-invoice of it.
pub async fn pdf_of(
    ctx: &mut Ctx<'_>,
    doc_type: &str,
    id: Uuid,
    template_id: Option<Uuid>,
    attachments: Vec<pdf::Attachment>,
    extension: Option<&xmp::XmpExtension>,
) -> Result<RenderedPdf> {
    use base64::Engine;
    let rendered = render(ctx, doc_type, id, Source::Stored(template_id)).await?;
    // Laid out off the request's thread, and given a time it must finish
    // in: a page is bounded, but its layout is work.
    let (html, title, extension) = (
        rendered.html.clone(),
        rendered.title.clone(),
        extension.cloned(),
    );
    let job = tokio::task::spawn_blocking(move || {
        pdf::render_with(&html, &title, attachments, extension.as_ref())
    });
    let bytes = tokio::time::timeout(PDF_TIME, job)
        .await
        .map_err(|_| {
            Error::validation(format!(
                "the PDF took longer than {} seconds to lay out",
                PDF_TIME.as_secs()
            ))
        })?
        .map_err(Error::internal)?
        .map_err(|e| Error::validation(format!("the PDF cannot be made: {e}")))?;
    let filename = format!(
        "{}.pdf",
        rendered
            .title
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            })
            .collect::<String>()
    );
    Ok(RenderedPdf {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        content_type: "application/pdf".into(),
        filename,
        title: rendered.title,
        template_id: rendered.template_id,
    })
}

/// Which template a render uses.
enum Source {
    /// The one with this id, or else the default.
    Stored(Option<Uuid>),
    /// One being written, not saved.
    Draft(String),
}

async fn render(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid, source: Source) -> Result<Rendered> {
    // The one path by which a document is read: its permissions hold.
    let view = document::get(ctx, doc_type, id).await?;
    let meta = ctx.kernel.registry().doc_type(doc_type)?.meta.clone();
    let (template_id, body, field) = match source {
        Source::Stored(asked) => {
            let locale = language_of(ctx, &view).await?;
            let (template_id, body) = template_for(ctx, &view, asked, &locale).await?;
            (template_id, body, "template_id")
        }
        Source::Draft(body) => {
            // Exactly the checks of saving one.
            engine::check_source(&body)
                .map_err(|problem| Error::field("body", format!("cannot be used: {problem}")))?;
            (None, body, "body")
        }
    };
    let locale = language_of(ctx, &view).await?;
    let meta = translated_meta(ctx, &meta, &locale);
    let title = title(&meta, &view, &locale);
    let context = context(ctx, &meta, &view, &title, &locale).await?;
    let html = engine::render(&body, &context).map_err(|problem| {
        Error::validation(format!("the print template cannot be rendered: {problem}"))
            .with_field(field)
    })?;
    // The logo goes in after the page was sanitised, from bytes checked
    // when it was uploaded.
    let logo = letterhead::logo(&mut *ctx.conn, view.header.company_id).await?;
    let html = letterhead::place_logo(&html, logo.as_ref(), 2 * engine::MAX_OUTPUT)?;
    Ok(Rendered {
        html,
        template_id,
        title,
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PreviewInput {
    /// Document type, for example `sales_invoice`.
    doc_type: String,
    /// Id of the document to show the template with.
    id: Uuid,
    /// The template's source as it is being written.
    body: String,
}

struct Preview;

#[async_trait]
impl Command for Preview {
    type Input = PreviewInput;
    type Output = Rendered;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "print_template.preview",
            "Render a document with a print template that is not saved, as the template editor \
             shows it. The source gets exactly the checks of saving one and the same limits, \
             and nothing is stored. Takes the right to create or change print templates.",
        )
        .label("Preview")
        .resource("print_template")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PreviewInput) -> Result<Rendered> {
        // Running a template one may not save would be saving it by
        // another name.
        if !ctx.actor.can_global("print_template:create")
            && !ctx.actor.can_global("print_template:update")
        {
            return Err(Error::forbidden(
                "previewing a print template takes print_template:create or \
                 print_template:update for all companies",
            ));
        }
        render(ctx, &input.doc_type, input.id, Source::Draft(input.body)).await
    }
}

fn title(meta: &DocTypeMeta, view: &DocumentView, locale: &str) -> String {
    match &view.header.number {
        Some(number) => format!("{} {number}", meta.label),
        None if locale == "de" => format!("{} (Entwurf)", meta.label),
        None => format!("{} (draft)", meta.label),
    }
}

/// The language a document prints in: that of the party it is to (the
/// customer of a sales document, the vendor of a purchase document), else
/// the company's, else English.
async fn language_of(ctx: &mut Ctx<'_>, view: &DocumentView) -> Result<String> {
    let summary = &view.header.summary;
    let id = |field: &str| {
        summary[field]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    let party = id("customer_id")
        .map(|c| ("customer", c))
        .or_else(|| id("vendor_id").map(|v| ("vendor", v)))
        .or_else(|| {
            let kind = summary["party_type"].as_str()?;
            let kind = match kind {
                "customer" => "customer",
                "vendor" => "vendor",
                _ => return None,
            };
            id("party_id").map(|p| (kind, p))
        });
    erpcraft_party::language(&mut *ctx.conn, view.header.company_id, party).await
}

/// The document type's description in the page's language, as
/// `meta.describe` gives it: its labels, plurals and help.
fn translated_meta(ctx: &Ctx<'_>, meta: &DocTypeMeta, locale: &str) -> DocTypeMeta {
    let mut meta = meta.clone();
    if locale == "en" {
        return meta;
    }
    let catalogues = ctx.kernel.registry().catalogues();
    let module = meta.module.clone();
    let tr = |text: &mut String| {
        if let Some(t) = catalogues.lookup(locale, &module, text) {
            *text = t.to_owned();
        }
    };
    let field = |f: &mut erpcraft_kernel::FieldMeta| {
        tr(&mut f.label);
        if let Some(help) = &mut f.help {
            tr(help);
        }
    };
    tr(&mut meta.label);
    tr(&mut meta.plural);
    meta.fields.iter_mut().for_each(field);
    meta.summary.iter_mut().for_each(field);
    for collection in &mut meta.collections {
        tr(&mut collection.label);
        collection.fields.iter_mut().for_each(field);
    }
    for state in &mut meta.states {
        tr(&mut state.label);
    }
    meta
}

async fn template_for(
    ctx: &mut Ctx<'_>,
    view: &DocumentView,
    asked: Option<Uuid>,
    locale: &str,
) -> Result<(Option<Uuid>, String)> {
    let doc = &view.header;
    if let Some(id) = asked {
        let found: Option<(String, Option<Uuid>, bool, String)> = sqlx::query_as(
            "SELECT doc_type, company_id, active, body FROM print_template WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        // One answer for every template that cannot print this document,
        // so that asking says nothing about templates one may not read.
        let usable = found.and_then(|(doc_type, company_id, active, body)| {
            (doc_type == doc.doc_type
                && active
                && company_id.is_none_or(|company| company == doc.company_id))
            .then_some(body)
        });
        let Some(body) = usable else {
            return Err(Error::field(
                "template_id",
                "is not an active print template for this document",
            ));
        };
        return Ok((Some(id), body));
    }
    let default: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, body FROM print_template
         WHERE doc_type = $1 AND is_default AND active
           AND (company_id = $2 OR company_id IS NULL)
           AND (locale = $3 OR locale IS NULL)
         ORDER BY locale IS NULL, company_id IS NULL LIMIT 1",
    )
    .bind(&doc.doc_type)
    .bind(doc.company_id)
    .bind(locale)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    Ok(match default {
        Some((id, body)) => (Some(id), body),
        None => (None, builtin::template(&doc.doc_type, locale)),
    })
}

/// What a template is given:
///
/// - `doc`: the document exactly as `document.get` returns it to the
///   caller: header, `summary`, `created_by_name`, `body`, `postings`,
///   `links`;
/// - `meta`: the document type's description: `label`, `fields`,
///   `collections`, `summary`, so a template can label and format what it
///   prints;
/// - `company`: `id`, `code`, `name`, `currency` and the currency's
///   `decimals`;
/// - `refs`: for every id the document refers to that the caller may read,
///   `{display, record}`: what tells a person which it is, and the record
///   as the caller may read it. For a line of another document, `record`
///   is that line;
/// - `parties`: the seller and the buyer with their addresses, VAT
///   identifiers and contacts, as far as the caller may read them, when a
///   module answers `document.parties` (the e-invoicing module does);
///   otherwise null. See that command's output for the fields;
/// - `title`.
async fn context(
    ctx: &mut Ctx<'_>,
    meta: &DocTypeMeta,
    view: &DocumentView,
    title: &str,
    locale: &str,
) -> Result<Value> {
    let company_id = view.header.company_id;
    let (code, name, currency): (String, String, String) =
        sqlx::query_as("SELECT code, name, base_currency FROM company WHERE id = $1")
            .bind(company_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
    let decimals = company::currency_decimals(&mut *ctx.conn, company_id).await?;
    let doc = serde_json::to_value(view).map_err(Error::internal)?;
    let refs = Refs::resolve(ctx, meta, &doc).await?;
    // Addresses are kept by whichever module answers `document.parties`;
    // printing names no module. Anything it cannot read prints as nothing.
    let parties = match ctx.kernel.registry().command("document.parties") {
        Ok(entry) => {
            let handler = entry.handler.clone();
            handler
                .run_json(
                    ctx,
                    json!({ "doc_type": view.header.doc_type, "id": view.header.id }),
                )
                .await
                .unwrap_or(Value::Null)
        }
        Err(_) => Value::Null,
    };
    Ok(json!({
        "doc": doc,
        "meta": {
            "name": meta.name,
            "label": meta.label,
            "fields": meta.fields,
            // Each collection with `subject`, the first field that refers
            // to something: printed in full, the other references by code.
            "collections": meta.collections.iter().map(|c| {
                let subject = c.fields.iter().find(|f| f.ty.is_reference()).map(|f| f.name.clone());
                let mut value = json!(c);
                value["subject"] = json!(subject);
                value
            }).collect::<Vec<_>>(),
            "summary": meta.summary,
        },
        "company": {
            "id": company_id, "code": code, "name": name,
            "currency": currency, "decimals": decimals,
        },
        "refs": refs.found,
        "parties": parties,
        "letterhead": letterhead::letterhead(ctx, company_id, &name).await?,
        "title": title,
        "locale": locale,
        "facts": facts::of(ctx, view).await?,
    }))
}

/// The most distinct records one render resolves. Beyond, references
/// print as nothing: a document of tens of thousands of lines, each with
/// another item, is not printed one entity read per line.
const MAX_REFERENCES: usize = 2_000;

/// The records a document refers to, as far as the caller may read them.
#[derive(Default)]
struct Refs {
    found: BTreeMap<String, Value>,
    /// Records already asked for that the caller may not read: asked once.
    unreadable: std::collections::HashSet<Uuid>,
    /// Documents read so far, by id; `None` when the caller may not.
    documents: HashMap<Uuid, Option<Value>>,
}

/// Whether an error of a read means only that the caller may not see the
/// record, which then prints as nothing.
fn unreadable(error: &Error) -> bool {
    matches!(error, Error::NotFound(_) | Error::Forbidden(_))
}

fn uuid_of(value: Option<&Value>) -> Option<Uuid> {
    value
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

impl Refs {
    async fn resolve(ctx: &mut Ctx<'_>, meta: &DocTypeMeta, doc: &Value) -> Result<Refs> {
        let mut refs = Refs::default();
        let body = &doc["body"];
        for field in &meta.fields {
            refs.field(ctx, field, body, body).await?;
        }
        for field in &meta.summary {
            refs.field(ctx, field, &doc["summary"], body).await?;
        }
        for collection in &meta.collections {
            let Some(lines) = body[&collection.name].as_array() else {
                continue;
            };
            for line in lines {
                for field in &collection.fields {
                    refs.field(ctx, field, line, body).await?;
                }
            }
        }
        Ok(refs)
    }

    /// Resolve `field` of `record`; `body` is the document's, for a line
    /// link that names a field of it.
    async fn field(
        &mut self,
        ctx: &mut Ctx<'_>,
        field: &FieldMeta,
        record: &Value,
        body: &Value,
    ) -> Result<()> {
        let Some(id) = uuid_of(record.get(&field.name)) else {
            return Ok(());
        };
        if self.found.contains_key(&id.to_string())
            || self.unreadable.contains(&id)
            || self.found.len() + self.unreadable.len() >= MAX_REFERENCES
        {
            return Ok(());
        }
        match &field.ty {
            FieldType::Link { entity, key, .. } if key == "id" => {
                self.entity(ctx, entity, id).await?;
            }
            FieldType::EntityLink { entity_field } => {
                if let Some(entity) = record.get(entity_field).and_then(Value::as_str) {
                    let entity = entity.to_owned();
                    if ctx.kernel.registry().entity(&entity).is_ok() {
                        self.entity(ctx, &entity, id).await?;
                    }
                }
            }
            FieldType::DocumentLink { doc_type, .. } => {
                if let Some(linked) = self.document(ctx, doc_type, id).await? {
                    let display = linked["number"].as_str().unwrap_or("(draft)").to_owned();
                    let header: Map<String, Value> = linked
                        .as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(k, _)| !matches!(k.as_str(), "body" | "postings" | "links"))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    self.found.insert(
                        id.to_string(),
                        json!({ "display": display, "short": display, "record": header }),
                    );
                } else {
                    self.unreadable.insert(id);
                }
            }
            FieldType::LineLink {
                document_field,
                doc_type,
                collection,
            } => {
                let Some(source) = uuid_of(body.get(document_field)) else {
                    return Ok(());
                };
                let Some(linked) = self.document(ctx, doc_type, source).await? else {
                    return Ok(());
                };
                let line = linked["body"][collection]
                    .as_array()
                    .and_then(|lines| lines.iter().find(|l| uuid_of(l.get("id")) == Some(id)))
                    .cloned();
                let Some(line) = line else {
                    return Ok(());
                };
                // The line is shown by what it refers to: the item of an
                // order line.
                let fields = ctx
                    .kernel
                    .registry()
                    .doc_type(doc_type)
                    .ok()
                    .and_then(|t| t.meta.collections.iter().find(|c| &c.name == collection))
                    .map(|c| c.fields.clone())
                    .unwrap_or_default();
                let mut shown = Vec::new();
                for inner in &fields {
                    if !matches!(&inner.ty, FieldType::Link { key, .. } if key == "id") {
                        continue;
                    }
                    Box::pin(self.field(ctx, inner, &line, &linked["body"])).await?;
                    if let Some(display) = uuid_of(line.get(&inner.name))
                        .and_then(|inner_id| self.found.get(&inner_id.to_string()))
                        .and_then(|found| found["display"].as_str())
                    {
                        shown.push(display.to_owned());
                        // The first reference says what the line is about.
                        break;
                    }
                }
                let display = shown.join(" ");
                let short = fields
                    .iter()
                    .filter_map(|inner| uuid_of(line.get(&inner.name)))
                    .find_map(|inner_id| {
                        self.found
                            .get(&inner_id.to_string())
                            .and_then(|found| found["short"].as_str())
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| display.clone());
                self.found.insert(
                    id.to_string(),
                    json!({ "display": display, "short": short, "record": line }),
                );
            }
            _ => {}
        }
        Ok(())
    }

    /// Read a record through the kernel's entity read, so that row and
    /// field rules apply. A record the caller may not read is left out.
    async fn entity(&mut self, ctx: &mut Ctx<'_>, entity_name: &str, id: Uuid) -> Result<()> {
        let record = match entity::get(ctx, entity_name, id).await {
            Ok(record) => record,
            Err(error) if unreadable(&error) => {
                self.unreadable.insert(id);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let display_fields = ctx
            .kernel
            .registry()
            .entity(entity_name)
            .map(|meta| meta.display.clone())
            .unwrap_or_default();
        // A withheld field is not in the record, so it shows nowhere.
        let display: Vec<String> = display_fields
            .iter()
            .filter_map(|field| match record.get(field) {
                Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
                Some(Value::Number(n)) => Some(n.to_string()),
                _ => None,
            })
            .collect();
        self.found.insert(
            id.to_string(),
            json!({
                "display": display.join(" "),
                "short": display.first().cloned().unwrap_or_default(),
                "record": record,
            }),
        );
        Ok(())
    }

    /// Read a document through `document.get`'s path, once.
    async fn document(
        &mut self,
        ctx: &mut Ctx<'_>,
        doc_type: &str,
        id: Uuid,
    ) -> Result<Option<Value>> {
        if let Some(known) = self.documents.get(&id) {
            return Ok(known.clone());
        }
        let found = match document::get(ctx, doc_type, id).await {
            Ok(view) => Some(serde_json::to_value(view).map_err(Error::internal)?),
            Err(error) if unreadable(&error) => None,
            Err(error) => return Err(error),
        };
        self.documents.insert(id, found.clone());
        Ok(found)
    }
}

// ------------------------------------------------------------ the built-ins

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BuiltinInput {
    /// Document type, for example `sales_invoice`. The generic template is
    /// returned for a type without one of its own.
    doc_type: String,
    /// The language: `de` for the German one; English when omitted.
    locale: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct BuiltinOutput {
    /// The source of the built-in template, to start a stored one from.
    body: String,
    /// Whether the type has a template of its own; false for the generic one.
    specific: bool,
}

struct Builtin;

#[async_trait]
impl Command for Builtin {
    type Input = BuiltinInput;
    type Output = BuiltinOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "print_template.builtin",
            "The source of the print template built into the server for a document type, to \
             start a template of your own from. It shows the data a template is given.",
        )
        .permission("print_template:read")
        .label("Built-in template")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: BuiltinInput) -> Result<BuiltinOutput> {
        ctx.actor.require_any("print_template:read")?;
        ctx.kernel.registry().doc_type(&input.doc_type)?;
        Ok(BuiltinOutput {
            body: builtin::template(&input.doc_type, input.locale.as_deref().unwrap_or("en")),
            specific: builtin::has_own(&input.doc_type, input.locale.as_deref().unwrap_or("en")),
        })
    }
}
