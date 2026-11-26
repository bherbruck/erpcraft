//! The module's commands for answering a partner: making the 855, 856
//! and 810, packing and shipping, labels, what waits, and resending.

use chrono::{DateTime, NaiveDate, Utc};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::inbound::{self, InterchangeOut, MESSAGE_COLUMNS, MessageOut};
use crate::label::{self, Label};
use crate::outbound::{self, Generated, Message};
use crate::shipment::{self, Carrier, PackInput, ShipmentOut, Shipped};

// ---------------------------------------------------------------- generate

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerateInput {
    /// `sales_order` for an 855, `sales_delivery` for an 856,
    /// `sales_invoice` or `sales_credit_note` for an 810.
    doc_type: String,
    /// The approved order or posted document.
    id: Uuid,
    /// `855`, `856` or `810`.
    message: String,
    /// Show the message and what is wrong with it; store nothing.
    #[serde(default)]
    preview: bool,
}

pub struct Generate;

#[async_trait]
impl Command for Generate {
    type Input = GenerateInput;
    type Output = Generated;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi.generate",
            "Make the message a partner is owed for a document: the 855 for an approved sales \
             order, the 856 for a posted and packed delivery, the 810 for a posted invoice or \
             credit note. It quotes the partner's order number, line ids, item numbers and \
             units as they were received. It is read back and checked against the books (the \
             quantities of the delivery, the invoice's total to the cent); one that fails is \
             kept as invalid with what is wrong, and no bytes are fixed for it. Otherwise its \
             control numbers are taken and its bytes stored, ready to send. A message rejected \
             by the partner is replaced by a new one. The policies edi.send and \
             edi_invoice.before_asn decide whether it may go.",
        )
        .permission(outbound::PERMISSION)
        .label("Make EDI message")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: GenerateInput) -> Result<Generated> {
        let message = Message::parse(&input.message)?;
        outbound::generate(ctx, &input.doc_type, input.id, message, input.preview).await
    }
}

// ---------------------------------------------------------------- shipping

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackCommandInput {
    /// A sales delivery for an order that came by EDI.
    #[schemars(extend("x-field-type" = { "kind": "document", "doc_type": "sales_delivery" }))]
    delivery_id: Uuid,
    #[serde(flatten)]
    carrier: Carrier,
    /// The pallets and cartons, in order. Each gets an SSCC.
    packs: Vec<PackInput>,
}

pub struct Pack;

#[async_trait]
impl Command for Pack {
    type Input = PackCommandInput;
    type Output = ShipmentOut;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi_shipment.pack",
            "Record how a delivery to an EDI partner is packed: the carrier and its papers, \
             the pallets and the cartons on them, and which delivery line's goods are in which \
             carton. Each pallet and carton gets an SSCC from the company's GS1 prefix \
             (edi_settings), never issued twice. Packing again replaces what was recorded; the \
             old SSCCs are not reused. The shipment is packed when its cartons hold exactly \
             what the delivery delivers.",
        )
        .permission(shipment::PERMISSION)
        .label("Pack")
        .resource("sales_delivery")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PackCommandInput) -> Result<ShipmentOut> {
        shipment::pack(ctx, input.delivery_id, input.carrier, input.packs).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryInput {
    /// A sales delivery for an order that came by EDI.
    #[schemars(extend("x-field-type" = { "kind": "document", "doc_type": "sales_delivery" }))]
    delivery_id: Uuid,
}

pub struct AutoPack;

#[async_trait]
impl Command for AutoPack {
    type Input = DeliveryInput;
    type Output = ShipmentOut;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi_shipment.auto_pack",
            "Pack a delivery one carton per case of the partner's unit (a case of 6 is one \
             carton of 6), for goods that ship in their own cases. Refused for a line that is \
             no whole number of cases: pack that one with edi_shipment.pack.",
        )
        .permission(shipment::PERMISSION)
        .label("Pack by case")
        .resource("sales_delivery")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DeliveryInput) -> Result<ShipmentOut> {
        shipment::auto_pack(ctx, input.delivery_id).await
    }
}

pub struct Ship;

#[async_trait]
impl Command for Ship {
    type Input = DeliveryInput;
    type Output = Shipped;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi_shipment.ship",
            "Ship a packed delivery to an EDI partner: check that its cartons hold exactly what \
             it delivers, post it with sales_delivery.post (every check of a delivery applies), \
             and make its ship notice (856), all in one transaction. If anything fails, nothing \
             is posted.",
        )
        .permission(shipment::PERMISSION)
        .label("Ship")
        .resource("sales_delivery")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DeliveryInput) -> Result<Shipped> {
        shipment::ship(ctx, input.delivery_id).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackIdInput {
    /// A pallet or carton.
    pack_id: Uuid,
}

pub struct LabelOf;

#[async_trait]
impl Command for LabelOf {
    type Input = PackIdInput;
    type Output = Label;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi.label",
            "The GS1-128 label of a pallet or carton: its SSCC, where it goes, the carrier, \
             the partner's order number, which carton of how many, and the label as ZPL for a \
             label printer.",
        )
        .permission(shipment::PERMISSION)
        .label("Label")
        .resource("sales_delivery")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PackIdInput) -> Result<Label> {
        label::label(ctx, input.pack_id).await
    }
}

// ----------------------------------------------------------------- pending

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PendingInput {
    /// The company.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "company", "key": "id" }))]
    company_id: Uuid,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub struct PendingMessage {
    /// The message.
    pub message_id: Uuid,
    /// The partner's code.
    pub partner_code: String,
    /// The message's kind, such as 850 or 810.
    pub message: String,
    /// The partner's order number.
    pub order_number: Option<String>,
    /// The message's state.
    pub state: String,
    /// When it was received or made.
    pub created_at: DateTime<Utc>,
    /// When the partner's acknowledgement was due.
    pub ack_due_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub struct PendingDelivery {
    /// The delivery.
    pub delivery_id: Uuid,
    /// The delivery's number.
    pub number: Option<String>,
    /// The delivery's date.
    pub doc_date: NaiveDate,
    /// The partner's code.
    pub partner_code: String,
    /// The partner's order number.
    pub order_number: String,
    /// Whether its packing is recorded and complete.
    pub packed: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PendingOut {
    /// Orders received and not yet drafted or dismissed.
    pub proposals: Vec<PendingMessage>,
    /// Messages to send that failed their checks.
    pub invalid: Vec<PendingMessage>,
    /// Messages the partner rejected and nothing has replaced.
    pub rejected: Vec<PendingMessage>,
    /// Messages sent and not acknowledged in the partner's hours.
    pub overdue: Vec<PendingMessage>,
    /// Ready to send, waiting for someone to send them.
    pub ready: Vec<PendingMessage>,
    /// Deliveries of EDI orders posted with no ship notice made.
    pub deliveries_without_notice: Vec<PendingDelivery>,
}

pub struct Pending;

#[async_trait]
impl Command for Pending {
    type Input = PendingInput;
    type Output = PendingOut;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi.pending",
            "What EDI waits for somebody on: orders received and not drafted, messages to send \
             that failed their checks, messages the partner rejected, messages sent and not \
             acknowledged within the partner's hours, messages ready and not sent, and \
             deliveries of EDI orders posted without a ship notice.",
        )
        .permission("edi_message:read")
        .label("EDI pending")
        .resource("edi_message")
        .target(false)
        .report("EDI pending")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PendingInput) -> Result<PendingOut> {
        ctx.require("edi_message:read", input.company_id)?;
        let list = |condition: &str| {
            format!(
                "SELECT m.id AS message_id, p.code AS partner_code, m.message,
                        m.business_key AS order_number, m.state, m.created_at, m.ack_due_at
                 FROM edi_message m JOIN edi_partner p ON p.id = m.partner_id
                 WHERE m.company_id = $1 AND m.superseded_by IS NULL AND {condition}
                 ORDER BY m.created_at, m.id LIMIT 500"
            )
        };
        let mut rows = Vec::new();
        for condition in [
            "m.direction = 'in' AND m.state = 'proposed'",
            "m.direction = 'out' AND m.state = 'invalid'",
            "m.direction = 'out' AND m.state = 'rejected'",
            "m.direction = 'out' AND m.state = 'sent' AND m.ack_due_at < now()",
            "m.direction = 'out' AND m.state = 'ready'",
        ] {
            let found: Vec<PendingMessage> = sqlx::query_as(&list(condition))
                .bind(input.company_id)
                .fetch_all(&mut *ctx.conn)
                .await?;
            rows.push(found);
        }
        let deliveries: Vec<PendingDelivery> = sqlx::query_as(
            "SELECT d.id AS delivery_id, d.number, d.doc_date, p.code AS partner_code,
                    e.order_number, COALESCE(s.state = 'packed', false) AS packed
             FROM document d
             JOIN sales_delivery sd ON sd.document_id = d.id
             JOIN edi_document e ON e.doc_type = 'sales_order'
                                AND e.document_id = sd.sales_order_id
             JOIN edi_partner p ON p.id = e.partner_id
             LEFT JOIN edi_shipment s ON s.delivery_id = d.id
             WHERE d.company_id = $1 AND d.doc_type = 'sales_delivery' AND d.status = 'posted'
               AND NOT EXISTS (
                   SELECT 1 FROM edi_message m
                   WHERE m.doc_type = 'sales_delivery' AND m.document_id = d.id
                     AND m.message IN ('856', 'DESADV') AND m.superseded_by IS NULL
                     AND m.state NOT IN ('invalid', 'rejected', 'failed'))
             ORDER BY d.doc_date, d.id LIMIT 500",
        )
        .bind(input.company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut rows = rows.into_iter();
        let mut next = || rows.next().unwrap_or_default();
        Ok(PendingOut {
            proposals: next(),
            invalid: next(),
            rejected: next(),
            overdue: next(),
            ready: next(),
            deliveries_without_notice: deliveries,
        })
    }
}

// ------------------------------------------------------------------ resend

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResendInput {
    /// A message sent.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "edi_message", "key": "id" }))]
    id: Uuid,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Resent {
    pub interchange: InterchangeOut,
    pub message: MessageOut,
}

pub struct Resend;

#[async_trait]
impl Command for Resend {
    type Input = ResendInput;
    type Output = Resent;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi_message.resend",
            "Send a message again: the same bytes with the same control numbers, so the \
             partner's own duplicate check works. Its interchange is ready to send once more. \
             A corrected message is not a resend: make a new one with edi.generate.",
        )
        .permission(outbound::PERMISSION)
        .label("Resend")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ResendInput) -> Result<Resent> {
        let row: Option<(Uuid, String, Option<Uuid>, String)> = sqlx::query_as(
            "SELECT company_id, direction, interchange_id, state FROM edi_message
             WHERE id = $1 FOR UPDATE",
        )
        .bind(input.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let (company, direction, interchange, state) =
            row.ok_or_else(|| Error::not_found("edi_message"))?;
        ctx.require(outbound::PERMISSION, company)?;
        ctx.touch("edi_message", input.id);
        let Some(interchange) = interchange.filter(|_| direction == "out") else {
            return Err(Error::state(
                "only a message sent, with its bytes, is sent again",
            ));
        };
        if !matches!(state.as_str(), "ready" | "sent" | "failed") {
            return Err(Error::state(format!(
                "this message is {state}: a message the partner answered is not sent again"
            )));
        }
        sqlx::query("UPDATE edi_interchange SET state = 'ready' WHERE id = $1")
            .bind(interchange)
            .execute(&mut *ctx.conn)
            .await?;
        sqlx::query(
            "UPDATE edi_message SET state = 'ready', ack_due_at = NULL
             WHERE interchange_id = $1 AND state IN ('ready', 'sent', 'failed')",
        )
        .bind(interchange)
        .execute(&mut *ctx.conn)
        .await?;
        let message = sqlx::query_as(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM edi_message WHERE id = $1"
        ))
        .bind(input.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        Ok(Resent {
            interchange: inbound::interchange_out(&mut *ctx.conn, interchange).await?,
            message,
        })
    }
}
