//! The commands of partners' guides: how a sample reads under a guide
//! (`edi_guide.check`), what samples suggest a guide says
//! (`edi_guide.derive`), and a guide as one file to share
//! (`edi_guide.export`, `edi_guide.import`).

use std::collections::BTreeMap;

use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Error, JsonSchema, Result, async_trait, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::commands::bytes_of;
use crate::guide::{self, FileMessage, GuideFile, ItemKind, ItemMatch, Options, Spec};
use crate::inbound::blocking;
use crate::segment::{MAX_BYTES, Segment};
use crate::x12::po850::PurchaseOrder;

fn guide_link() -> Value {
    json!({ "kind": "link", "entity": "edi_guide", "key": "id" })
}

/// One message of a sample file, as read.
struct Sample {
    message: String,
    control: String,
    segments: Vec<Segment>,
    order: Option<PurchaseOrder>,
}

/// The messages of a sample file, and its syntax and version.
fn samples(bytes: &[u8]) -> std::result::Result<(String, String, Vec<Sample>), String> {
    match crate::syntax_of(bytes) {
        Some(crate::Syntax::X12) => {
            let interchanges = crate::x12::read(bytes).map_err(|e| e.to_string())?;
            let mut out = vec![];
            let mut version = String::new();
            for interchange in interchanges {
                for group in interchange.groups {
                    if version.is_empty() {
                        version = group.version.clone();
                    }
                    for set in group.sets {
                        let order = (set.id == "850").then(|| crate::x12::po850::read(&set).0);
                        out.push(Sample {
                            message: set.id.clone(),
                            control: set.control.clone(),
                            segments: set.segments,
                            order,
                        });
                    }
                }
            }
            Ok(("x12".into(), version, out))
        }
        Some(crate::Syntax::Edifact) => {
            let interchanges = crate::edifact::read(bytes).map_err(|e| e.to_string())?;
            let mut out = vec![];
            let mut version = String::new();
            for interchange in interchanges {
                let d = interchange.delimiters;
                for m in interchange.messages {
                    if version.is_empty() {
                        version = format!("{}{}", m.version, m.release);
                    }
                    let order =
                        (m.kind == "ORDERS").then(|| crate::edifact::orders::read(&m, &d).0);
                    out.push(Sample {
                        message: m.kind.clone(),
                        control: m.reference.clone(),
                        segments: m.segments,
                        order,
                    });
                }
            }
            Ok(("edifact".into(), version, out))
        }
        None => Err("the file is neither X12 nor EDIFACT".into()),
    }
}

async fn read_samples(bytes: Vec<u8>) -> Result<(String, String, Vec<Sample>)> {
    blocking(move || samples(&bytes))
        .await
        .and_then(|r| r)
        .map_err(|why| Error::field("content", format!("cannot be read: {why}")))
}

fn direction_of(syntax: &str, message: &str) -> Option<&'static str> {
    guide::MESSAGES
        .iter()
        .find(|(s, m, _)| *s == syntax && *m == message)
        .map(|(_, _, d)| *d)
}

#[derive(sqlx::FromRow)]
struct GuideRow {
    company_id: Uuid,
    code: String,
    syntax: String,
    version: String,
}

async fn guide_row(ctx: &mut Ctx<'_>, id: Uuid) -> Result<GuideRow> {
    let row: GuideRow =
        sqlx::query_as("SELECT company_id, code, syntax, version FROM edi_guide WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *ctx.conn)
            .await?
            .ok_or_else(|| Error::not_found("edi_guide"))?;
    ctx.require("edi_guide:read", row.company_id)?;
    Ok(row)
}

// ------------------------------------------------------------------- check

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckInput {
    /// The guide to read the sample under.
    #[schemars(extend("x-field-type" = guide_link()))]
    guide_id: Uuid,
    /// A sample file as text. Give this or `content_base64`.
    content: Option<String>,
    /// A sample file, base64-encoded.
    content_base64: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Checked {
    pub guide_code: String,
    pub messages: Vec<CheckedMessage>,
}

#[derive(Serialize, JsonSchema)]
pub struct CheckedMessage {
    /// Its place in the file, from 1.
    pub position: usize,
    /// Such as 850 or ORDERS.
    pub message: String,
    /// ST02 or the UNH reference.
    pub control: String,
    /// `in` or `out`; none for a message no guide describes.
    pub direction: Option<String>,
    /// For an order: what it reads as.
    pub order: Option<Value>,
    /// Every constraint of the guide the message breaks.
    pub violations: Vec<String>,
}

pub struct Check;

#[async_trait]
impl Command for Check {
    type Input = CheckInput;
    type Output = Checked;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi_guide.check",
            "How a sample file from a partner reads under a guide, without keeping anything: \
             each message with its type and number, an order as it would be read, and every \
             constraint of the guide (with the module's base and the guide's base) it breaks. \
             Use it on the samples a partner's guide comes with before trading.",
        )
        .permission("edi_guide:read")
        .label("Check")
        .resource("edi_guide")
        .redact(&["content", "content_base64"])
        .max_input_bytes(3 * MAX_BYTES)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CheckInput) -> Result<Checked> {
        let row = guide_row(ctx, input.guide_id).await?;
        let bytes = bytes_of(input.content, input.content_base64)?;
        let (syntax, _, found) = read_samples(bytes).await?;
        if syntax != row.syntax {
            return Err(Error::field(
                "content",
                format!("the file is {syntax}, and the guide is for {}", row.syntax),
            ));
        }
        let mut messages = vec![];
        for (index, sample) in found.into_iter().enumerate() {
            let direction = direction_of(&syntax, &sample.message);
            let violations = match direction {
                Some(direction) => {
                    let spec = guide::spec_of(
                        &mut *ctx.conn,
                        &row.syntax,
                        &row.version,
                        Some(input.guide_id),
                        &sample.message,
                        direction,
                    )
                    .await?;
                    guide::violations(&spec.constraints, &sample.segments)
                }
                None => vec![],
            };
            messages.push(CheckedMessage {
                position: index + 1,
                message: sample.message,
                control: sample.control,
                direction: direction.map(str::to_owned),
                order: sample.order.map(|o| json!(o)),
                violations,
            });
        }
        Ok(Checked {
            guide_code: row.code,
            messages,
        })
    }
}

// ------------------------------------------------------------------ derive

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeriveInput {
    /// Sample files as text: each is read, and what they share proposed.
    #[serde(default)]
    content: Vec<String>,
    /// Sample files, base64-encoded.
    #[serde(default)]
    content_base64: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Derived {
    pub syntax: String,
    /// As the files' envelopes name it, such as 004010 or D96A.
    pub version: String,
    /// The messages proposed, to review and save as a guide's.
    pub messages: Vec<FileMessage>,
    /// Product id qualifiers seen that the module does not know the kind
    /// of: say in `item_match` what they are, or leave them.
    pub unknown_qualifiers: Vec<String>,
    /// How many messages each proposal rests on.
    pub samples: BTreeMap<String, usize>,
}

/// What orders received suggest: the product qualifiers by kind, the
/// ship-to's role and qualifier, the dates and references used, and the
/// segments every sample has as required.
fn derive_order(orders: &[(&PurchaseOrder, &[Segment])], unknown: &mut Vec<String>) -> Spec {
    let mut by_kind: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut dates: Vec<String> = vec![];
    let mut references: Vec<String> = vec![];
    let mut ship_to: Option<guide::ShipTo> = None;
    let push = |list: &mut Vec<String>, value: &str| {
        if !list.iter().any(|v| v == value) {
            list.push(value.to_owned());
        }
    };
    for (order, _) in orders {
        for line in &order.lines {
            for id in &line.product_ids {
                match crate::proposal::kind_of(&id.qualifier) {
                    Some(kind) => push(by_kind.entry(kind).or_default(), &id.qualifier),
                    None => push(unknown, &id.qualifier),
                }
            }
        }
        for d in &order.dates {
            push(&mut dates, &d.qualifier);
        }
        for r in &order.references {
            push(&mut references, &r.qualifier);
        }
        if ship_to.is_none()
            && let Some(p) = order.parties.iter().find(|p| p.role == "ST")
        {
            ship_to = Some(guide::ShipTo {
                role: p.role.clone(),
                qualifier: Some(p.id_qualifier.clone()).filter(|q| !q.is_empty()),
            });
        }
    }
    let item_match: Vec<ItemMatch> = [
        (ItemKind::Gtin, "gtin"),
        (ItemKind::SellerItem, "seller_item"),
        (ItemKind::BuyerItem, "buyer_item"),
    ]
    .into_iter()
    .filter_map(|(kind, code)| {
        by_kind
            .remove(code)
            .map(|qualifiers| ItemMatch { kind, qualifiers })
    })
    .collect();
    let ship: Vec<String> = ["002", "010"]
        .into_iter()
        .filter(|q| dates.iter().any(|d| d == q))
        .map(str::to_owned)
        .collect();
    // The segments every sample has.
    let mut every: Option<Vec<String>> = None;
    for (_, segments) in orders {
        let mut tags: Vec<String> = segments
            .iter()
            .skip(1)
            .take(segments.len().saturating_sub(2))
            .map(|s| s.tag.clone())
            .collect();
        tags.sort();
        tags.dedup();
        every = Some(match every {
            None => tags,
            Some(before) => before.into_iter().filter(|t| tags.contains(t)).collect(),
        });
    }
    Spec {
        options: Options {
            item_match: Some(item_match).filter(|m| !m.is_empty()),
            ship_to,
            dates: (!ship.is_empty()).then_some(guide::Dates { ship, cancel: None }),
            references: Some(references).filter(|r| !r.is_empty()),
            ..Options::default()
        },
        constraints: every
            .unwrap_or_default()
            .into_iter()
            .map(|segment| guide::Constraint {
                segment,
                required: true,
                ..guide::Constraint::default()
            })
            .collect(),
        extra: vec![],
    }
}

pub struct Derive;

#[async_trait]
impl Command for Derive {
    type Input = DeriveInput;
    type Output = Derived;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi_guide.derive",
            "Proposes what a guide says from a partner's sample orders, without keeping \
             anything: which product id qualifiers carry which kind of item number, the \
             ship-to's role and qualifier, the dates and references used, and the segments \
             every sample has as required. Review it, then save it as a guide's message \
             (edi_guide_message) or in a file for edi_guide.import.",
        )
        .permission("edi_guide:read")
        .label("Derive")
        .resource("edi_guide")
        .target(false)
        .redact(&["content", "content_base64"])
        .max_input_bytes(3 * MAX_BYTES)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DeriveInput) -> Result<Derived> {
        ctx.actor.require_any("edi_guide:read")?;
        let files: Vec<Vec<u8>> = input
            .content
            .into_iter()
            .map(|c| bytes_of(Some(c), None))
            .chain(
                input
                    .content_base64
                    .into_iter()
                    .map(|c| bytes_of(None, Some(c))),
            )
            .collect::<Result<_>>()?;
        if files.is_empty() || files.len() > 20 {
            return Err(Error::validation("give one to 20 sample files"));
        }
        let mut syntax = String::new();
        let mut version = String::new();
        let mut all: Vec<Sample> = vec![];
        for file in files {
            let (s, v, found) = read_samples(file).await?;
            if !syntax.is_empty() && s != syntax {
                return Err(Error::validation("the samples are not all of one syntax"));
            }
            syntax = s;
            if version.is_empty() {
                version = v;
            }
            all.extend(found);
        }
        let order_kind = if syntax == "x12" { "850" } else { "ORDERS" };
        let orders: Vec<(&PurchaseOrder, &[Segment])> = all
            .iter()
            .filter_map(|s| s.order.as_ref().map(|o| (o, s.segments.as_slice())))
            .collect();
        let mut samples = BTreeMap::new();
        for s in &all {
            *samples.entry(s.message.clone()).or_insert(0) += 1;
        }
        let mut unknown = vec![];
        let mut messages = vec![];
        if !orders.is_empty() {
            let spec = derive_order(&orders, &mut unknown);
            guide::check(&syntax, order_kind, "in", &spec).map_err(Error::internal)?;
            messages.push(FileMessage {
                message: order_kind.into(),
                direction: "in".into(),
                spec,
            });
        }
        Ok(Derived {
            syntax,
            version,
            messages,
            unknown_qualifiers: unknown,
            samples,
        })
    }
}

// ------------------------------------------------------------------ export

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportInput {
    /// The guide to write as a file.
    #[schemars(extend("x-field-type" = guide_link()))]
    guide_id: Uuid,
}

pub struct Export;

#[async_trait]
impl Command for Export {
    type Input = ExportInput;
    type Output = GuideFile;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi_guide.export",
            "A guide as one JSON file: its code, name, syntax and version, the code of the \
             guide it builds on, and the spec of each message. Imported with \
             edi_guide.import, in this company or another.",
        )
        .permission("edi_guide:read")
        .label("Export")
        .resource("edi_guide")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ExportInput) -> Result<GuideFile> {
        guide_row(ctx, input.guide_id).await?;
        guide::export(&mut *ctx.conn, input.guide_id).await
    }
}

// ------------------------------------------------------------------ import

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportInput {
    /// The company the guide is saved for.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "company", "key": "id" }))]
    company_id: Uuid,
    /// The file edi_guide.export wrote.
    file: GuideFile,
    /// Replace the messages of the company's guide of the same code. Left
    /// out, a guide of that code already there is refused.
    #[serde(default)]
    replace: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct Imported {
    pub guide_id: Uuid,
    pub created: bool,
    pub messages: usize,
}

pub struct Import;

#[async_trait]
impl Command for Import {
    type Input = ImportInput;
    type Output = Imported;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi_guide.import",
            "Saves a guide from the file edi_guide.export wrote, each message's spec checked \
             as when it is saved by hand. The guide it builds on is found by its code in the \
             company. A guide of the same code is replaced only when asked (replace).",
        )
        .permission("edi_guide:create")
        .label("Import")
        .resource("edi_guide")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ImportInput) -> Result<Imported> {
        ctx.require("edi_guide:create", input.company_id)?;
        let file = input.file;
        guide::check_file(&file).map_err(|why| Error::field("file", why))?;
        let base: Option<Uuid> = match &file.base {
            Some(code) => Some(
                sqlx::query_scalar("SELECT id FROM edi_guide WHERE company_id = $1 AND code = $2")
                    .bind(input.company_id)
                    .bind(code)
                    .fetch_optional(&mut *ctx.conn)
                    .await?
                    .ok_or_else(|| {
                        Error::field(
                            "file",
                            format!("the guide it builds on, {code}, is not one of the company's"),
                        )
                    })?,
            ),
            None => None,
        };
        let existing: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM edi_guide WHERE company_id = $1 AND code = $2")
                .bind(input.company_id)
                .bind(&file.code)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        let (id, created) = match existing {
            Some(_) if !input.replace => {
                return Err(Error::conflict(format!(
                    "the company has a guide {} already: import it with replace to replace it",
                    file.code
                )));
            }
            Some(id) => {
                ctx.require("edi_guide:update", input.company_id)?;
                sqlx::query(
                    "UPDATE edi_guide SET name = $2, syntax = $3, version = $4,
                         base_guide_id = $5, notes = $6 WHERE id = $1",
                )
                .bind(id)
                .bind(&file.name)
                .bind(&file.syntax)
                .bind(&file.version)
                .bind(base)
                .bind(&file.notes)
                .execute(&mut *ctx.conn)
                .await?;
                sqlx::query("DELETE FROM edi_guide_message WHERE guide_id = $1")
                    .bind(id)
                    .execute(&mut *ctx.conn)
                    .await?;
                (id, false)
            }
            None => {
                let id = new_id();
                sqlx::query(
                    "INSERT INTO edi_guide (id, company_id, code, name, syntax, version,
                         base_guide_id, notes)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(id)
                .bind(input.company_id)
                .bind(&file.code)
                .bind(&file.name)
                .bind(&file.syntax)
                .bind(&file.version)
                .bind(base)
                .bind(&file.notes)
                .execute(&mut *ctx.conn)
                .await?;
                (id, true)
            }
        };
        ctx.touch("edi_guide", id);
        for m in &file.messages {
            let message_id = new_id();
            sqlx::query(
                "INSERT INTO edi_guide_message (id, company_id, guide_id, message, direction,
                     spec)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(message_id)
            .bind(input.company_id)
            .bind(id)
            .bind(&m.message)
            .bind(&m.direction)
            .bind(json!(m.spec))
            .execute(&mut *ctx.conn)
            .await?;
            ctx.touch("edi_guide_message", message_id);
        }
        Ok(Imported {
            guide_id: id,
            created,
            messages: file.messages.len(),
        })
    }
}
