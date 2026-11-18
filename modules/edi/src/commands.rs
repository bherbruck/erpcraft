//! The module's commands for receiving, proposing and drafting, and for
//! handing interchanges over by hand.

use std::collections::BTreeMap;

use base64::Engine;
use erpcraft_kernel::document::DocumentView;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::inbound::{self, InterchangeOut, ReceiveOutput, blocking};
use crate::proposal::{self, Choices, Proposal};
use crate::segment::{Charset, MAX_BYTES, Tokenizer, skip_filler};
use crate::{draft, explain, x12};

/// The bytes a request carries: text, or base64.
fn bytes_of(content: Option<String>, content_base64: Option<String>) -> Result<Vec<u8>> {
    match (content, content_base64) {
        (Some(text), None) => {
            if text.len() > MAX_BYTES {
                return Err(Error::field("content", "the file is larger than 4 MiB"));
            }
            Ok(text.into_bytes())
        }
        (None, Some(encoded)) => {
            if encoded.len() > MAX_BYTES / 3 * 4 + 8 {
                return Err(Error::field(
                    "content_base64",
                    "the file is larger than 4 MiB",
                ));
            }
            base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .map_err(|_| Error::field("content_base64", "is not base64"))
        }
        _ => Err(Error::validation(
            "give the file as content (text) or as content_base64",
        )),
    }
}

fn message_link() -> Value {
    serde_json::json!({ "kind": "link", "entity": "edi_message", "key": "id" })
}

// ----------------------------------------------------------------- receive

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiveInput {
    /// The interchange as text. Give this or `content_base64`.
    content: Option<String>,
    /// The file, base64-encoded: for bytes that are not UTF-8.
    content_base64: Option<String>,
    /// Only a partner of this company may receive it.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "company", "key": "id" }))]
    company_id: Option<Uuid>,
}

pub struct Receive;

#[async_trait]
impl Command for Receive {
    type Input = ReceiveInput;
    type Output = ReceiveOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi.receive",
            "Receive an X12 interchange from a trading partner, as a transport or a person \
             hands it over. The bytes are stored first and never change; then they are read, \
             routed to the partner their envelope names, and each functional group is \
             acknowledged by syntax alone (a 997 or 999, ready to fetch with \
             edi_interchange.content; a TA1 where the interchange asks for one). Each order \
             becomes a message proposed for drafting (edi.proposal, edi.draft). A file nobody \
             is found for is kept as unroutable, one that cannot be read as unreadable. The \
             same bytes again return the first result and do nothing twice.",
        )
        .permission(inbound::PERMISSION)
        .label("Receive")
        .resource("edi_interchange")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReceiveInput) -> Result<ReceiveOutput> {
        let bytes = bytes_of(input.content, input.content_base64)?;
        inbound::receive(ctx, bytes, input.company_id).await
    }
}

// ---------------------------------------------------------------- proposal

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageInput {
    /// A message received.
    #[schemars(extend("x-field-type" = message_link()))]
    message_id: Uuid,
}

pub struct Propose;

#[async_trait]
impl Command for Propose {
    type Input = MessageInput;
    type Output = Proposal;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi.proposal",
            "What a partner's order would become, without changing anything: the order as it \
             was sent, the customer, the ship-to and for each line the item, quantity and price \
             in the item's own unit, each with how it was found; and the findings. A blocking \
             finding (an item, unit or ship-to nobody knows, a price that does not divide into \
             the item's unit, an order number drafted before, test traffic) stops edi.draft \
             until a person decides; a note is for reading. Read with the caller's rights: a \
             customer or item the caller may not read is left out.",
        )
        .permission("edi_message:read")
        .label("Proposal")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: MessageInput) -> Result<Proposal> {
        proposal::propose(ctx, input.message_id, &Choices::default()).await
    }
}

// ------------------------------------------------------------------- draft

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftInput {
    /// A proposed order.
    #[schemars(extend("x-field-type" = message_link()))]
    message_id: Uuid,
    /// The item for a line, by the partner's line id (PO101), where the
    /// proposal found none or the wrong one.
    items: Option<BTreeMap<String, Uuid>>,
    /// The price per the partner's unit for a line, by its line id, where
    /// the order states none or one that does not divide into the item's
    /// unit. The partner is answered with it (855).
    prices: Option<BTreeMap<String, Decimal>>,
    /// One of the customer's addresses, where the order's ship-to is not
    /// known here.
    shipping_address_id: Option<Uuid>,
    /// The warehouse the goods leave from, where the partner names none.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "warehouse", "key": "id" }))]
    warehouse_id: Option<Uuid>,
    /// Save the items and ship-to chosen as the partner's cross-references.
    #[serde(default)]
    remember: bool,
    /// The proposal's notes were read: draft anyway. They are written into
    /// the draft's memo.
    #[serde(default)]
    accept_notes: bool,
    /// Drafted by automation as the partner's run-as user: the policy
    /// edi.auto_draft decides instead of accept_notes, and edi.auto_approve
    /// whether the draft is approved at once.
    #[serde(default)]
    automatic: bool,
}

pub struct Draft;

#[async_trait]
impl Command for Draft {
    type Input = DraftInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi.draft",
            "Draft the sales order a partner's order proposes, as a person entering it would: \
             under the caller's permissions and every check of a sales order, with the choices \
             given for whatever the proposal could not resolve. Refused while a blocking finding \
             stands, and while notes stand without accept_notes; an order number drafted before \
             is decided by the policy edi_order.duplicate. The partner's order number, line \
             ids, item numbers, units, quantities and prices are kept as sent, to be quoted \
             back. The draft is approved like any sales order.",
        )
        .permission(draft::PERMISSION)
        .label("Draft order")
        .resource("edi_message")
        .target(false)
        .creates("sales_order")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftInput) -> Result<DocumentView> {
        draft::draft(
            ctx,
            draft::Request {
                message_id: input.message_id,
                choices: Choices {
                    items: input.items.unwrap_or_default(),
                    prices: input.prices.unwrap_or_default(),
                    shipping_address_id: input.shipping_address_id,
                    warehouse_id: input.warehouse_id,
                },
                remember: input.remember,
                accept_notes: input.accept_notes,
                automatic: input.automatic,
            },
        )
        .await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DismissInput {
    /// A message received and not drafted.
    #[schemars(extend("x-field-type" = message_link()))]
    message_id: Uuid,
    /// Why nobody will draft it.
    reason: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Dismissed {
    id: Uuid,
    state: String,
    reason: String,
}

pub struct Dismiss;

#[async_trait]
impl Command for Dismiss {
    type Input = DismissInput;
    type Output = Dismissed;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi.dismiss",
            "End a message received that nobody will draft, saying why. It stays on record \
             with its bytes.",
        )
        .permission(draft::PERMISSION)
        .label("Dismiss")
        .resource("edi_message")
        .target(false)
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DismissInput) -> Result<Dismissed> {
        let out = draft::dismiss(ctx, input.message_id, &input.reason).await?;
        Ok(Dismissed {
            id: input.message_id,
            state: "dismissed".into(),
            reason: out["reason"].as_str().unwrap_or_default().to_owned(),
        })
    }
}

// ----------------------------------------------------------------- explain

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplainInput {
    /// A message on record: its set is explained.
    #[schemars(extend("x-field-type" = message_link()))]
    message_id: Option<Uuid>,
    /// Or a file as text: every segment of it.
    content: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExplainedSegment {
    /// For a message, the position in its set (the ST is 1), as an
    /// acknowledgement names it; for a file, the place in the file.
    pub position: usize,
    pub tag: String,
    /// The segment as it stands in the file.
    pub text: String,
    /// What it says, in a sentence.
    pub says: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Explained {
    pub segments: Vec<ExplainedSegment>,
}

/// Every segment of a file, or of its `set`-th transaction set (counted
/// from 1 over the whole interchange), explained.
fn explain_bytes(
    bytes: &[u8],
    set: Option<usize>,
) -> std::result::Result<Vec<ExplainedSegment>, String> {
    let interchanges = x12::read(bytes).map_err(|e| e.to_string())?;
    let first = interchanges
        .first()
        .ok_or_else(|| "the file holds no interchange".to_owned())?;
    let mut tokens = Tokenizer::new(
        bytes,
        skip_filler(bytes, 0, &first.isa.delimiters),
        first.isa.delimiters,
        Charset::of(bytes),
    );
    let mut budget = Default::default();
    let mut out = Vec::new();
    let mut sets = 0usize;
    let mut in_set = 0usize;
    let mut current = String::new();
    let mut index = 0usize;
    while let Some(segment) = tokens.next(&mut budget).map_err(|e| e.to_string())? {
        let raw = String::from_utf8_lossy(&bytes[segment.offset..tokens.position()])
            .trim_end()
            .to_owned();
        index += 1;
        if segment.tag == "ST" {
            sets += 1;
            in_set = 0;
            current = segment.get(1).to_owned();
        }
        let set_of = if segment.tag == "ST" || in_set > 0 {
            in_set += 1;
            Some(sets)
        } else {
            None
        };
        if segment.tag == "SE" {
            in_set = 0;
        }
        let end = segment.tag == "IEA";
        let keep = match set {
            Some(wanted) => set_of == Some(wanted),
            None => true,
        };
        if keep {
            out.push(ExplainedSegment {
                position: if set.is_some() { out.len() + 1 } else { index },
                says: explain::explain(&segment, &current),
                tag: segment.tag,
                text: raw,
            });
        }
        if end {
            break;
        }
    }
    Ok(out)
}

pub struct Explain;

#[async_trait]
impl Command for Explain {
    type Input = ExplainInput;
    type Output = Explained;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi.explain",
            "Each segment of a message on record, or of a file given as text, with what it \
             says in a sentence: for a person or an agent reading what a partner sent or what \
             is about to be sent. Segments this module does not act on are shown by their tag.",
        )
        .permission("edi_message:read")
        .label("Explain")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ExplainInput) -> Result<Explained> {
        let (bytes, set) = match (input.message_id, input.content) {
            (Some(id), None) => {
                let row = proposal::message_row(ctx, id, false).await?;
                ctx.require("edi_message:read", row.company_id)?;
                let Some(interchange) = row.interchange_id else {
                    return Err(Error::state(
                        "this message failed its checks and has no bytes; its findings say why",
                    ));
                };
                let payload: Vec<u8> =
                    sqlx::query_scalar("SELECT payload FROM edi_interchange WHERE id = $1")
                        .bind(interchange)
                        .fetch_one(&mut *ctx.conn)
                        .await?;
                let set = (row.message != "TA1").then_some(row.position.unwrap_or(1) as usize);
                (payload, set)
            }
            (None, Some(text)) => (bytes_of(Some(text), None)?, None),
            _ => {
                return Err(Error::validation(
                    "give a message_id or the content of a file",
                ));
            }
        };
        let segments = blocking(move || explain_bytes(&bytes, set))
            .await
            .and_then(|r| r)
            .map_err(|e| Error::field("content", format!("cannot be read: {e}")))?;
        Ok(Explained { segments })
    }
}

// ------------------------------------------------------------- interchange

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InterchangeInput {
    /// The interchange.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "edi_interchange", "key": "id" }))]
    id: Uuid,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Content {
    pub id: Uuid,
    pub direction: String,
    pub control: Option<String>,
    pub size: i32,
    pub sha256: String,
    /// The bytes, base64-encoded.
    pub content_base64: String,
    /// The bytes as text: UTF-8, else ISO 8859-1.
    pub content: String,
}

/// Loads an interchange the caller may read, with its company.
async fn readable_interchange(
    ctx: &mut Ctx<'_>,
    id: Uuid,
    permission: &str,
) -> Result<(Option<Uuid>, Vec<u8>, InterchangeOut)> {
    let found: Option<(Option<Uuid>, Vec<u8>)> =
        sqlx::query_as("SELECT company_id, payload FROM edi_interchange WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    let (company, payload) = found.ok_or_else(|| Error::not_found("edi_interchange"))?;
    match company {
        Some(company) => ctx.require(permission, company)?,
        // Bytes nobody could route belong to no company.
        None => ctx.actor.require_global(permission)?,
    }
    let out = inbound::interchange_out(&mut *ctx.conn, id).await?;
    Ok((company, payload, out))
}

pub struct ContentOf;

#[async_trait]
impl Command for ContentOf {
    type Input = InterchangeInput;
    type Output = Content;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi_interchange.content",
            "The bytes of an interchange, base64-encoded and as text: what to hand to the \
             partner while no transport carries it (a 997 to send, say), and the evidence of \
             what was exchanged.",
        )
        .permission("edi_interchange:read")
        .label("Content")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: InterchangeInput) -> Result<Content> {
        let (_, payload, out) = readable_interchange(ctx, input.id, "edi_interchange:read").await?;
        let content = match Charset::of(&payload) {
            Charset::Utf8 => String::from_utf8_lossy(&payload).into_owned(),
            Charset::Latin1 => payload.iter().map(|b| *b as char).collect(),
        };
        Ok(Content {
            id: out.id,
            direction: out.direction,
            control: out.control,
            size: out.size,
            sha256: out.sha256,
            content_base64: base64::engine::general_purpose::STANDARD.encode(&payload),
            content,
        })
    }
}

pub struct MarkSent;

#[async_trait]
impl Command for MarkSent {
    type Input = InterchangeInput;
    type Output = InterchangeOut;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi_interchange.mark_sent",
            "Record that an interchange ready to send was delivered to the partner by hand. \
             Its messages count as sent from now, and the partner's acknowledgement is due \
             within the partner's hours.",
        )
        .permission("edi:send")
        .label("Mark sent")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: InterchangeInput) -> Result<InterchangeOut> {
        let (company, _, out) = readable_interchange(ctx, input.id, "edi:send").await?;
        if company.is_none() || out.direction != "out" {
            return Err(Error::state("only an interchange to send is marked sent"));
        }
        let state: String =
            sqlx::query_scalar("SELECT state FROM edi_interchange WHERE id = $1 FOR UPDATE")
                .bind(input.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        ctx.touch("edi_interchange", input.id);
        if state == "sent" {
            ctx.no_change();
            return inbound::interchange_out(&mut *ctx.conn, input.id).await;
        }
        if state != "ready" {
            return Err(Error::state(format!(
                "this interchange is {state}: only one ready is marked sent"
            )));
        }
        sqlx::query("UPDATE edi_interchange SET state = 'sent', sent_at = now() WHERE id = $1")
            .bind(input.id)
            .execute(&mut *ctx.conn)
            .await?;
        // Acknowledgements are never acknowledged, so nothing is due for
        // them.
        sqlx::query(
            "UPDATE edi_message m SET state = 'sent',
                 ack_due_at = CASE WHEN m.message IN ('997', '999', 'TA1', 'CONTRL') THEN NULL
                     ELSE now() + make_interval(hours => p.ack_hours) END
             FROM edi_partner p
             WHERE m.interchange_id = $1 AND m.state = 'ready' AND p.id = m.partner_id",
        )
        .bind(input.id)
        .execute(&mut *ctx.conn)
        .await?;
        inbound::interchange_out(&mut *ctx.conn, input.id).await
    }
}
