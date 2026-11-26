//! A partner's change to an order: an 860, an ORDCHG, or an 850 (or
//! ORDERS) that cancels or replaces an order sent before. It is proposed
//! against the sales order the first order became, line by line, and
//! applied only to a draft: once the order is approved, every change is a
//! finding for a person, who decides what follows (and answers it).

use erpcraft_kernel::document::{self, DocumentChange};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::inbound::blocking;
use crate::outbound;
use crate::partner::{self, Partner};
use crate::policy::ORDER_CHANGE;
use crate::proposal::{self, Choices, Finding, MessageRow, Severity, divide_price, fits};
use crate::x12::change860::{self, OrderChange};
use crate::x12::po850::Line;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OrderRef {
    pub id: Uuid,
    pub number: Option<String>,
    pub status: String,
    pub version: i32,
}

/// One change to one line of the order, in the item's own unit.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LineChange {
    /// The partner's line id.
    pub line_id: String,
    /// `add`, `delete`, `quantity`, `price`.
    pub what: String,
    /// The sales order's line it changes; none for a line added.
    pub order_line_id: Option<Uuid>,
    pub item_id: Option<Uuid>,
    pub qty_before: Option<Decimal>,
    pub qty_after: Option<Decimal>,
    pub price_before: Option<Decimal>,
    pub price_after: Option<Decimal>,
    /// As the partner stated them.
    pub partner_qty: Option<Decimal>,
    pub partner_uom: String,
    pub partner_price: Option<Decimal>,
    #[serde(skip)]
    #[schemars(skip)]
    added: Option<proposal::ProposedLine>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ChangeProposal {
    pub message_id: Uuid,
    pub state: String,
    /// The partner's order number the change is to.
    pub order_number: String,
    /// Whether the whole order is called off.
    pub cancels_order: bool,
    /// The sales order changed, if one was found.
    pub order: Option<OrderRef>,
    pub changes: Vec<LineChange>,
    pub findings: Vec<Finding>,
    /// Whether edi.apply_change would apply it as it stands.
    pub would_apply: bool,
}

/// The messages a change can come as.
pub fn is_change(row: &MessageRow, purpose: Option<&str>) -> bool {
    row.direction == "in"
        && match row.message.as_str() {
            "860" | "ORDCHG" => true,
            "850" | "ORDERS" => matches!(purpose, Some("01" | "05")),
            _ => false,
        }
}

/// The change a message states, read again from its bytes.
async fn change_of(ctx: &mut Ctx<'_>, row: &MessageRow) -> Result<OrderChange> {
    if matches!(row.message.as_str(), "850" | "ORDERS") {
        let order = proposal::order_of(ctx, row).await?;
        return Ok(change860::of_order(&order));
    }
    let (Some(interchange), Some(position)) = (row.interchange_id, row.position) else {
        return Err(Error::internal("a change received has no interchange"));
    };
    let payload: Vec<u8> = sqlx::query_scalar("SELECT payload FROM edi_interchange WHERE id = $1")
        .bind(interchange)
        .fetch_one(&mut *ctx.conn)
        .await?;
    let index = (position as usize).saturating_sub(1);
    let edifact = row.message == "ORDCHG";
    blocking(move || -> Option<OrderChange> {
        if edifact {
            let interchange = crate::edifact::read(&payload).ok()?.pop()?;
            let d = interchange.delimiters;
            let message = interchange.messages.into_iter().nth(index)?;
            Some(crate::edifact::ordchg::read(&message, &d).0)
        } else {
            let set = crate::x12::read(&payload)
                .ok()?
                .pop()?
                .groups
                .into_iter()
                .flat_map(|g| g.sets)
                .nth(index)?;
            Some(change860::read(&set).0)
        }
    })
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::internal("the change is no longer where it was read"))
}

async fn purpose_of(ctx: &mut Ctx<'_>, id: Uuid) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT purpose FROM edi_message WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *ctx.conn)
            .await?,
    )
}

/// What a change would do, without doing it.
pub async fn propose(ctx: &mut Ctx<'_>, message_id: Uuid, lock: bool) -> Result<ChangeProposal> {
    let row = proposal::message_row(ctx, message_id, lock).await?;
    ctx.require("edi_message:read", row.company_id)?;
    let purpose = purpose_of(ctx, row.id).await?;
    if !is_change(&row, purpose.as_deref()) {
        return Err(Error::validation(
            "this message is not an order change received: an 860, an ORDCHG, or an order that \
             cancels or replaces one",
        ));
    }
    let partner = partner::load(&mut *ctx.conn, row.partner_id).await?;
    let change = change_of(ctx, &row).await?;
    let mut findings: Vec<Finding> =
        serde_json::from_value(row.findings.clone()).unwrap_or_default();
    match row.state.as_str() {
        "proposed" => {}
        "drafted" => findings.push(Finding::blocking(
            "applied",
            None,
            "This change was applied already.",
        )),
        "dismissed" => findings.push(Finding::blocking(
            "dismissed",
            None,
            "This change was dismissed.",
        )),
        other => findings.push(Finding::blocking(
            "state",
            None,
            format!("A change in the state {other} is not applied."),
        )),
    }
    for (position, tag) in &change.unhandled {
        findings.push(Finding::note(
            "unhandled",
            None,
            format!("Segment {tag} at position {position} has no field here and is not applied."),
        ));
    }
    let cancels_order = change.purpose == change860::CANCEL;
    let mut out = ChangeProposal {
        message_id: row.id,
        state: row.state.clone(),
        order_number: change.order_number.clone(),
        cancels_order,
        order: None,
        changes: vec![],
        findings: vec![],
        would_apply: false,
    };

    // The order the partner's first order became.
    let order_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT document_id FROM edi_document
         WHERE doc_type = 'sales_order' AND partner_id = $1 AND order_number = $2
         ORDER BY sequence DESC LIMIT 1",
    )
    .bind(partner.id)
    .bind(&change.order_number)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let Some(order_id) = order_id else {
        findings.push(Finding::blocking(
            "order_unknown",
            None,
            format!(
                "No sales order was drafted from the partner's order {}: there is nothing to \
                 change. Draft the order first, or dismiss the change.",
                change.order_number
            ),
        ));
        out.findings = findings;
        return Ok(out);
    };
    let view = document::get(ctx, "sales_order", order_id).await?;
    out.order = Some(OrderRef {
        id: order_id,
        number: view.header.number.clone(),
        status: view.header.status.clone(),
        version: view.header.version,
    });
    if view.header.status != "draft" {
        findings.push(Finding::blocking(
            "order_not_draft",
            None,
            format!(
                "Sales order {} is {}: a change to it is never applied by itself. A person \
                 decides what follows: change or cancel the order by hand, and answer the \
                 partner.",
                view.header.number.clone().unwrap_or_default(),
                view.header.status
            ),
        ));
    }
    if cancels_order {
        findings.push(Finding::blocking(
            "order_cancelled",
            None,
            format!(
                "The partner cancels its order {}: cancel the sales order by hand \
                 (sales_order.cancel), or delete the draft.",
                change.order_number
            ),
        ));
    }
    let sent = outbound::sent_order(ctx, order_id)
        .await?
        .ok_or_else(|| Error::internal("an order from EDI without its record"))?;
    // The order's lines, with the partner's line each answers.
    let lines = document::body::lines(&view.body, "lines").to_vec();
    let order_line = |partner_line: &str| {
        sent.lines
            .iter()
            .find(|(_, l)| l.partner_line_id == partner_line)
            .map(|(id, l)| (*id, l.clone()))
    };
    let body_line = |id: Uuid| {
        lines
            .iter()
            .find(|l| document::body::uuid(&l["id"]) == Some(id))
            .cloned()
    };

    let spec = crate::guide::spec_for(&mut *ctx.conn, &partner, &row.message, "in").await?;
    let mut named: Vec<String> = vec![];
    for line in &change.lines {
        let id = line.line_id.as_str();
        named.push(line.line_id.clone());
        let existing = order_line(id);
        let mut lc = LineChange {
            line_id: line.line_id.clone(),
            what: String::new(),
            order_line_id: existing.as_ref().map(|e| e.0),
            item_id: None,
            qty_before: None,
            qty_after: None,
            price_before: None,
            price_after: None,
            partner_qty: line.quantity,
            partner_uom: line.uom.clone(),
            partner_price: line.unit_price,
            added: None,
        };
        let current = existing
            .as_ref()
            .and_then(|(order_line, _)| body_line(*order_line));
        if let Some(current) = &current {
            lc.item_id = document::body::uuid(&current["item_id"]);
            lc.qty_before = document::body::decimal(&current["qty"]);
            lc.price_before = document::body::decimal(&current["unit_price"]);
        }
        match (line.change.as_str(), &existing, &current) {
            (change860::ADD, None, _) => {
                let synthetic = Line {
                    line_id: line.line_id.clone(),
                    quantity: line.quantity,
                    uom: line.uom.clone(),
                    unit_price: line.unit_price,
                    product_ids: line.product_ids.clone(),
                    ..Line::default()
                };
                let (proposed, _) = proposal::propose_lines(
                    ctx,
                    &partner,
                    &spec,
                    std::slice::from_ref(&synthetic),
                    &Choices::default(),
                    &mut findings,
                )
                .await?;
                let proposed = proposed.into_iter().next();
                lc.what = "add".into();
                lc.item_id = proposed.as_ref().and_then(|p| p.item_id);
                lc.qty_after = proposed.as_ref().and_then(|p| p.qty);
                lc.price_after = proposed.as_ref().and_then(|p| p.unit_price);
                lc.added = proposed;
                out.changes.push(lc);
            }
            (change860::ADD, Some(_), _) if !change.replaces_all => {
                findings.push(Finding::blocking(
                    "line_exists",
                    Some(id),
                    format!("The change adds line {id}, which the order has already."),
                ));
            }
            (change860::DELETE, Some(_), Some(_)) => {
                lc.what = "delete".into();
                out.changes.push(lc);
            }
            (_, None, _) if change.replaces_all => {
                // A replacement names a line the order does not have: it
                // is added.
                let synthetic = Line {
                    line_id: line.line_id.clone(),
                    quantity: line.quantity,
                    uom: line.uom.clone(),
                    unit_price: line.unit_price,
                    product_ids: line.product_ids.clone(),
                    ..Line::default()
                };
                let (proposed, _) = proposal::propose_lines(
                    ctx,
                    &partner,
                    &spec,
                    std::slice::from_ref(&synthetic),
                    &Choices::default(),
                    &mut findings,
                )
                .await?;
                let proposed = proposed.into_iter().next();
                lc.what = "add".into();
                lc.item_id = proposed.as_ref().and_then(|p| p.item_id);
                lc.qty_after = proposed.as_ref().and_then(|p| p.qty);
                lc.price_after = proposed.as_ref().and_then(|p| p.unit_price);
                lc.added = proposed;
                out.changes.push(lc);
            }
            (
                change860::QUANTITY_UP
                | change860::QUANTITY_DOWN
                | change860::PRICE
                | change860::CHANGES
                | change860::ADD,
                Some((_, sent_line)),
                Some(_),
            ) => {
                if !line.uom.is_empty() && line.uom != sent_line.partner_uom {
                    findings.push(Finding::blocking(
                        "unit_changed",
                        Some(id),
                        format!(
                            "Line {id} is changed in {}, and was ordered in {}: a person \
                             decides.",
                            line.uom, sent_line.partner_uom
                        ),
                    ));
                    continue;
                }
                let mut changed = false;
                if let Some(q) = line.quantity
                    && q != sent_line.partner_qty
                {
                    match q.checked_mul(sent_line.units_per).filter(|q| fits(*q)) {
                        Some(qty) if qty > Decimal::ZERO => {
                            lc.qty_after = Some(qty.normalize());
                            changed = true;
                        }
                        _ => findings.push(Finding::blocking(
                            "quantity",
                            Some(id),
                            format!(
                                "Line {id} is changed to {q} {}, which is no quantity a sales \
                                 order line holds (delete the line instead of ordering none).",
                                sent_line.partner_uom
                            ),
                        )),
                    }
                }
                if let Some(p) = line.unit_price
                    && Some(p) != sent_line.partner_price
                {
                    match divide_price(p, sent_line.units_per) {
                        Some(unit) => {
                            lc.price_after = Some(unit);
                            changed = true;
                        }
                        None => findings.push(Finding::blocking(
                            "price_does_not_divide",
                            Some(id),
                            format!(
                                "Line {id} is changed to {p} per {}, which does not divide into \
                                 {} of the item's unit without rounding.",
                                sent_line.partner_uom, sent_line.units_per
                            ),
                        )),
                    }
                }
                if changed {
                    lc.what = if lc.qty_after.is_some() {
                        "quantity"
                    } else {
                        "price"
                    }
                    .into();
                    out.changes.push(lc);
                }
            }
            (code, None, _) | (code, Some(_), None) => findings.push(Finding::blocking(
                "line_unknown",
                Some(id),
                format!("The change ({code}) is to line {id}, which the order does not have."),
            )),
            (code, _, _) => findings.push(Finding::blocking(
                "change_unknown",
                Some(id),
                format!(
                    "Line {id} is changed with the code {code}, which this module does not \
                     apply: a person decides."
                ),
            )),
        }
    }
    // A replacement deletes what it leaves out.
    if change.replaces_all {
        for (order_line, sent_line) in &sent.lines {
            if !named.contains(&sent_line.partner_line_id) && body_line(*order_line).is_some() {
                out.changes.push(LineChange {
                    line_id: sent_line.partner_line_id.clone(),
                    what: "delete".into(),
                    order_line_id: Some(*order_line),
                    item_id: None,
                    qty_before: None,
                    qty_after: None,
                    price_before: None,
                    price_after: None,
                    partner_qty: None,
                    partner_uom: sent_line.partner_uom.clone(),
                    partner_price: None,
                    added: None,
                });
            }
        }
    }
    if out.changes.is_empty() && !cancels_order {
        findings.push(Finding::note(
            "no_change",
            None,
            "The change changes nothing the order does not say already.",
        ));
    }
    out.would_apply = !findings.iter().any(|f| f.severity == Severity::Blocking);
    out.findings = findings;
    Ok(out)
}

/// Applies a change to the draft it changes.
pub async fn apply(ctx: &mut Ctx<'_>, message_id: Uuid, accept_notes: bool) -> Result<Value> {
    let proposal = propose(ctx, message_id, true).await?;
    let row = proposal::message_row(ctx, message_id, false).await?;
    ctx.require(crate::inbound::PERMISSION, row.company_id)?;
    let blocking: Vec<&Finding> = proposal
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Blocking)
        .collect();
    if let Some(first) = blocking.first() {
        return Err(Error::validation(format!(
            "the change is not applied: {}",
            first.message
        )));
    }
    let notes = proposal
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Note)
        .count();
    if notes > 0 && !accept_notes {
        return Err(Error::validation(format!(
            "the change has {notes} note(s); read them with edi.change_proposal and apply with \
             accept_notes: true"
        )));
    }
    let order = proposal
        .order
        .clone()
        .ok_or_else(|| Error::internal("a change that would apply names no order"))?;
    let partner: Partner = partner::load(&mut *ctx.conn, row.partner_id).await?;
    let count = |what: &str| proposal.changes.iter().filter(|c| c.what == what).count();
    ctx.decide(
        ORDER_CHANGE,
        partner.company_id,
        json!({
            "partner_code": partner.code,
            "order_number": proposal.order_number,
            "order_status": order.status,
            "lines_added": count("add"),
            "lines_deleted": count("delete"),
            "lines_changed": count("quantity") + count("price"),
        }),
    )
    .await?;

    let view = document::get(ctx, "sales_order", order.id).await?;
    let mut body = view.body.clone();
    let warehouse = partner.warehouse_id;
    let mut lines: Vec<Value> = document::body::lines(&body, "lines").to_vec();
    for c in &proposal.changes {
        let Some(at) = c.order_line_id.and_then(|id| {
            lines
                .iter()
                .position(|l| document::body::uuid(&l["id"]) == Some(id))
        }) else {
            continue;
        };
        match c.what.as_str() {
            "delete" => {
                lines.remove(at);
            }
            _ => {
                if let Some(q) = c.qty_after {
                    lines[at]["qty"] = json!(q);
                }
                if let Some(p) = c.price_after {
                    lines[at]["unit_price"] = json!(p);
                }
            }
        }
    }
    let first_new = lines.len();
    let mut added = vec![];
    for c in proposal.changes.iter().filter(|c| c.what == "add") {
        let (Some(item), Some(qty), Some(price)) = (c.item_id, c.qty_after, c.price_after) else {
            return Err(Error::internal(
                "an added line that would apply is not complete",
            ));
        };
        let warehouse = lines
            .first()
            .and_then(|l| document::body::uuid(&l["warehouse_id"]))
            .or(warehouse);
        lines.push(
            json!({ "item_id": item, "warehouse_id": warehouse, "qty": qty,
                           "unit_price": price }),
        );
        added.push(c);
    }
    body["lines"] = json!(lines);
    let updated = document::update(
        ctx,
        DocumentChange {
            doc_type: "sales_order".into(),
            id: order.id,
            version: order.version,
            doc_date: None,
            memo: None,
            body: Some(body),
        },
    )
    .await?;

    // What the partner sent, for what is answered later.
    for c in &proposal.changes {
        match (c.what.as_str(), c.order_line_id) {
            ("delete", Some(line)) => {
                sqlx::query(
                    "DELETE FROM edi_document_line
                     WHERE doc_type = 'sales_order' AND document_id = $1 AND line_id = $2",
                )
                .bind(order.id)
                .bind(line)
                .execute(&mut *ctx.conn)
                .await?;
            }
            ("quantity" | "price", Some(line)) => {
                sqlx::query(
                    "UPDATE edi_document_line
                     SET partner_qty = COALESCE($3, partner_qty),
                         partner_price = COALESCE($4, partner_price)
                     WHERE doc_type = 'sales_order' AND document_id = $1 AND line_id = $2",
                )
                .bind(order.id)
                .bind(line)
                .bind(c.partner_qty)
                .bind(c.partner_price)
                .execute(&mut *ctx.conn)
                .await?;
            }
            _ => {}
        }
    }
    let new_ids: Vec<Uuid> = document::body::lines(&updated.body, "lines")
        .iter()
        .skip(first_new)
        .filter_map(|l| document::body::uuid(&l["id"]))
        .collect();
    for (c, line_id) in added.iter().zip(new_ids) {
        let proposed = c.added.as_ref();
        sqlx::query(
            "INSERT INTO edi_document_line (doc_type, document_id, line_id, partner_line_id,
                 product_ids, partner_uom, partner_qty, partner_price, units_per)
             VALUES ('sales_order', $1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(order.id)
        .bind(line_id)
        .bind(&c.line_id)
        .bind(proposed.map(|p| p.product_ids.clone()).unwrap_or(json!([])))
        .bind(&c.partner_uom)
        .bind(c.partner_qty.unwrap_or_default())
        .bind(c.partner_price)
        .bind(proposed.and_then(|p| p.units_per).unwrap_or(Decimal::ONE))
        .execute(&mut *ctx.conn)
        .await?;
    }
    sqlx::query(
        "UPDATE edi_message SET state = 'drafted', doc_type = 'sales_order', document_id = $2
         WHERE id = $1",
    )
    .bind(message_id)
    .bind(order.id)
    .execute(&mut *ctx.conn)
    .await?;
    ctx.touch("edi_message", message_id);
    Ok(json!(updated))
}

// ---------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeInput {
    /// A change received: an 860, an ORDCHG, or an order that cancels or
    /// replaces one.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "edi_message", "key": "id" }))]
    message_id: Uuid,
}

pub struct ProposeChange;

#[async_trait]
impl Command for ProposeChange {
    type Input = ChangeInput;
    type Output = ChangeProposal;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi.change_proposal",
            "What a partner's change to an order would do, without doing it: the sales order \
             its first order became, and per line what is added, deleted, or changed in \
             quantity or price, in the item's own unit. A change to an order that is no longer \
             a draft is a blocking finding: a person decides what follows. A cancellation is \
             one too.",
        )
        .permission("edi_message:read")
        .label("Change proposal")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ChangeInput) -> Result<ChangeProposal> {
        propose(ctx, input.message_id, false).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyInput {
    /// A change received, proposed.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "edi_message", "key": "id" }))]
    message_id: Uuid,
    /// Apply although the proposal has notes, having read them.
    #[serde(default)]
    accept_notes: bool,
}

pub struct ApplyChange;

#[async_trait]
impl Command for ApplyChange {
    type Input = ApplyInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi.apply_change",
            "Apply a partner's change to the draft sales order it changes, through the order's \
             own update, as edi.change_proposal shows it: refused while a blocking finding \
             stands (an order that is no longer a draft among them), and while notes stand \
             unless accept_notes. The policy edi_order.change is asked first. Returns the \
             order.",
        )
        .permission(crate::inbound::PERMISSION)
        .label("Apply change")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ApplyInput) -> Result<Value> {
        apply(ctx, input.message_id, input.accept_notes).await
    }
}
