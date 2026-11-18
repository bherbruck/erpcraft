//! Drafting the sales order a partner's order proposes.
//!
//! The order is drafted through `document::create`, as a person entering
//! it would, under the caller's permissions and every check of the sales
//! order. Nothing is posted and nothing is approved here. What the order
//! said that the sales order does not keep (the partner's order number and
//! line ids, its item numbers, unit, quantity and price as sent) goes into
//! `edi_document`, so every answer quotes it as it came.

use chrono::Utc;
use erpcraft_kernel::document::{self, DocumentView, NewDocument};
use erpcraft_kernel::{Ctx, Decimal, Error, Result, entity};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::partner;
use crate::policy::{AUTO_APPROVE, AUTO_DRAFT, DUPLICATE_ORDER};
use crate::proposal::{self, Choices, Finding, Proposal, Severity};
use crate::x12::po850::PurchaseOrder;

pub const PERMISSION: &str = "edi:draft";

pub struct Request {
    pub message_id: Uuid,
    pub choices: Choices,
    /// Save the items and ship-to chosen as cross-references.
    pub remember: bool,
    /// The notes were read; draft anyway.
    pub accept_notes: bool,
    /// Drafted without a person: the policy edi.auto_draft decides, and
    /// edi.auto_approve whether the draft is approved at once.
    pub automatic: bool,
}

fn listed(findings: &[&Finding]) -> String {
    findings
        .iter()
        .map(|f| f.message.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn draft(ctx: &mut Ctx<'_>, request: Request) -> Result<DocumentView> {
    let row = proposal::message_row(ctx, request.message_id, true).await?;
    ctx.require(PERMISSION, row.company_id)?;
    ctx.touch("edi_message", row.id);
    if row.state != "proposed" {
        return Err(Error::state(format!(
            "this message is {}: only a proposed order is drafted",
            row.state
        )));
    }
    let order = proposal::order_of(ctx, &row).await?;
    let partner = partner::load(&mut *ctx.conn, row.partner_id).await?;
    let proposal = proposal::build(ctx, &row, &partner, order, &request.choices).await?;

    let blocking: Vec<&Finding> = proposal
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Blocking && f.code != "duplicate_order")
        .collect();
    if !blocking.is_empty() {
        return Err(Error::validation(format!(
            "the order cannot be drafted yet: {}",
            listed(&blocking)
        )));
    }
    // A number drafted before: the policy decides, and may ask for an
    // approval.
    if proposal
        .findings
        .iter()
        .any(|f| f.code == "duplicate_order")
    {
        let drafted_before: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM edi_document WHERE partner_id = $1 AND order_number = $2",
        )
        .bind(partner.id)
        .bind(&proposal.order.number)
        .fetch_one(&mut *ctx.conn)
        .await?;
        ctx.decide(
            DUPLICATE_ORDER,
            partner.company_id,
            json!({
                "partner_code": partner.code,
                "order_number": proposal.order.number,
                "drafted_before": drafted_before,
            }),
        )
        .await?;
    }
    let notes: Vec<&Finding> = proposal
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Note && f.code != "duplicate_order")
        .collect();
    if request.automatic {
        // Whoever allowed this read the notes in the rule's condition.
        ctx.decide(
            AUTO_DRAFT,
            partner.company_id,
            json!({
                "partner_code": partner.code,
                "message": "850",
                "notes": notes.len(),
                "lines": proposal.lines.len(),
                "total_net": proposal.total_net.unwrap_or_default(),
            }),
        )
        .await?;
    } else if !notes.is_empty() && !request.accept_notes {
        return Err(Error::validation(format!(
            "the proposal has {} note(s); read them with edi.proposal and draft with \
             accept_notes: {}",
            notes.len(),
            listed(&notes)
        )));
    }

    let warehouse_id = proposal
        .warehouse_id
        .ok_or_else(|| Error::internal("a proposal that would draft has no warehouse"))?;
    let mut lines = Vec::new();
    for line in &proposal.lines {
        let (Some(item_id), Some(qty), Some(unit_price)) =
            (line.item_id, line.qty, line.unit_price)
        else {
            return Err(Error::internal(
                "a proposal that would draft has an open line",
            ));
        };
        lines.push(json!({
            "item_id": item_id,
            "warehouse_id": warehouse_id,
            "qty": qty,
            "unit_price": unit_price,
        }));
    }
    let customer_id = partner.customer_id;
    let body = json!({
        "customer_id": customer_id,
        "currency": proposal.currency,
        "shipping_address_id": proposal.ship_to.address_id,
        "lines": lines,
    });
    let doc_date = proposal
        .order
        .date
        .unwrap_or_else(|| Utc::now().date_naive());
    let view = document::create(
        ctx,
        NewDocument {
            doc_type: "sales_order".into(),
            company_id: partner.company_id,
            doc_date,
            memo: Some(memo(&partner.code, &proposal.order, &notes)),
            body,
        },
    )
    .await?;

    // The draft says what the order says, to the cent.
    let drafted = document::body::decimal(&view.header.summary["total_net"]);
    if let (Some(drafted), Some(sent)) = (drafted, proposal.total_net)
        && drafted != sent
    {
        return Err(Error::internal(format!(
            "the draft comes to {drafted} and the order to {sent}"
        )));
    }

    let line_ids = order_line_ids(ctx, &view).await?;
    if line_ids.len() != proposal.lines.len() {
        return Err(Error::internal(
            "the draft does not have a line for every line of the order",
        ));
    }
    keep(ctx, &proposal, &view, &line_ids).await?;
    if request.remember {
        remember(ctx, &partner, &proposal, &request.choices).await?;
    }
    sqlx::query(
        "UPDATE edi_message SET state = 'drafted', doc_type = 'sales_order', document_id = $2
         WHERE id = $1",
    )
    .bind(row.id)
    .bind(view.header.id)
    .execute(&mut *ctx.conn)
    .await?;
    if request.automatic {
        let approve = ctx
            .policy(AUTO_APPROVE, partner.company_id)
            .await?
            .evaluate(json!({
                "partner_code": partner.code,
                "lines": proposal.lines.len(),
                "total_net": proposal.total_net.unwrap_or_default(),
            }))?;
        if approve == erpcraft_kernel::Decision::Allowed {
            // The normal approval, with every policy of sales_order.approve,
            // then the 855 the approval owes the partner.
            let handler = ctx
                .kernel
                .registry()
                .command("sales_order.approve")?
                .handler
                .clone();
            handler
                .run_json(ctx, json!({ "id": view.header.id }))
                .await?;
            match crate::outbound::generate(
                ctx,
                "sales_order",
                view.header.id,
                crate::outbound::Message::Ack855,
                false,
            )
            .await
            {
                Ok(_) => {}
                Err(e @ Error::Internal(_)) => return Err(e),
                Err(e) => tracing::info!(error = %e, "no 855 for an order approved by itself"),
            }
            return document::get(ctx, "sales_order", view.header.id).await;
        }
    }
    Ok(view)
}

async fn order_line_ids(ctx: &mut Ctx<'_>, view: &DocumentView) -> Result<Vec<Uuid>> {
    let from_body: Vec<Uuid> = document::body::lines(&view.body, "lines")
        .iter()
        .filter_map(|l| document::body::uuid(&l["id"]))
        .collect();
    if !from_body.is_empty() {
        return Ok(from_body);
    }
    // The caller asked for the header alone, so the view has no body:
    // the lines as the sales order stores them, in their order.
    Ok(sqlx::query_scalar(
        "SELECT id FROM sales_order_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(view.header.id)
    .fetch_all(&mut *ctx.conn)
    .await?)
}

/// The memo of the draft: the partner's order number, its dates and what
/// a person accepted, until the sales order has fields of its own for
/// them.
fn memo(partner_code: &str, order: &PurchaseOrder, notes: &[&Finding]) -> String {
    let mut memo = format!("EDI order {} from partner {partner_code}", order.number);
    if !order.release.is_empty() {
        memo.push_str(&format!(", release {}", order.release));
    }
    if let Some(date) = order.date {
        memo.push_str(&format!(", dated {date}"));
    }
    memo.push('.');
    for date in &order.dates {
        let what = match date.qualifier.as_str() {
            "002" => "Delivery requested",
            "010" => "Ship requested",
            "037" => "Ship not before",
            "038" => "Ship no later than",
            "001" => "Cancel after",
            other => {
                memo.push_str(&format!(" Date {other}: {}.", date.date));
                continue;
            }
        };
        memo.push_str(&format!(" {what}: {}.", date.date));
    }
    if !notes.is_empty() {
        memo.push_str(" Accepted notes:");
        for note in notes {
            memo.push(' ');
            memo.push_str(&note.message);
        }
    }
    memo
}

async fn keep(
    ctx: &mut Ctx<'_>,
    proposal: &Proposal,
    view: &DocumentView,
    line_ids: &[Uuid],
) -> Result<()> {
    let order = &proposal.order;
    sqlx::query(
        "INSERT INTO edi_document (doc_type, document_id, partner_id, message_id, order_number,
             release, order_date, header, sequence)
         VALUES ('sales_order', $1, $2, $3, $4, $5, $6, $7,
                 (SELECT count(*) + 1 FROM edi_document WHERE partner_id = $2
                    AND order_number = $4))",
    )
    .bind(view.header.id)
    .bind(proposal.partner.id)
    .bind(proposal.message_id)
    .bind(&order.number)
    .bind(Some(order.release.clone()).filter(|r| !r.is_empty()))
    .bind(order.date)
    .bind(json!({
        "purpose": order.purpose,
        "order_type": order.order_type,
        "currency": order.currency,
        "references": order.references,
        "dates": order.dates,
        "terms": order.terms,
        "freight_terms": order.freight_terms,
        "parties": order.parties,
    }))
    .execute(&mut *ctx.conn)
    .await?;
    for (line, line_id) in proposal.lines.iter().zip(line_ids) {
        sqlx::query(
            "INSERT INTO edi_document_line (doc_type, document_id, line_id, partner_line_id,
                 product_ids, partner_uom, partner_qty, partner_price, units_per)
             VALUES ('sales_order', $1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(view.header.id)
        .bind(line_id)
        .bind(&line.line_id)
        .bind(&line.product_ids)
        .bind(&line.partner_uom)
        .bind(line.partner_qty)
        .bind(line.partner_price)
        .bind(line.units_per.unwrap_or(Decimal::ONE))
        .execute(&mut *ctx.conn)
        .await?;
    }
    Ok(())
}

/// Saves what a person chose as cross-references, through the generic
/// entity commands' checks and permissions, so that the next order of the
/// partner resolves by itself. A number that already names an item is
/// left as it is.
async fn remember(
    ctx: &mut Ctx<'_>,
    partner: &partner::Partner,
    proposal: &Proposal,
    choices: &Choices,
) -> Result<()> {
    for line in &proposal.lines {
        let Some(item_id) = choices.items.get(&line.line_id) else {
            continue;
        };
        let ids: Vec<(String, String)> = line
            .product_ids
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some((
                    p["qualifier"].as_str()?.to_owned(),
                    p["value"].as_str()?.to_owned(),
                ))
            })
            .collect();
        // The most lasting number the line has: a GTIN, the seller's
        // number, the buyer's.
        let pick = ["gtin", "seller_item", "buyer_item"]
            .iter()
            .find_map(|kind| {
                ids.iter()
                    .find(|(q, _)| proposal::kind_of(q) == Some(*kind))
                    .map(|(_, v)| (*kind, v.clone()))
            });
        let Some((kind, value)) = pick else {
            continue;
        };
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM edi_item_xref
                            WHERE partner_id = $1 AND kind = $2 AND value = $3)",
        )
        .bind(partner.id)
        .bind(kind)
        .bind(&value)
        .fetch_one(&mut *ctx.conn)
        .await?;
        if taken {
            continue;
        }
        entity::create(
            ctx,
            "edi_item_xref",
            json!({ "company_id": partner.company_id, "partner_id": partner.id,
                    "item_id": item_id, "kind": kind, "value": value }),
        )
        .await?;
    }
    if let (Some(address), "chosen", Some(sent)) = (
        proposal.ship_to.address_id,
        proposal.ship_to.how.as_str(),
        proposal.ship_to.sent.as_ref(),
    ) {
        let (qualifier, code) = (
            sent["id_qualifier"].as_str().unwrap_or_default(),
            sent["id"].as_str().unwrap_or_default(),
        );
        if !qualifier.is_empty() && !code.is_empty() {
            let taken: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM edi_location_xref
                                WHERE partner_id = $1 AND qualifier = $2 AND code = $3)",
            )
            .bind(partner.id)
            .bind(qualifier)
            .bind(code)
            .fetch_one(&mut *ctx.conn)
            .await?;
            if !taken {
                entity::create(
                    ctx,
                    "edi_location_xref",
                    json!({ "company_id": partner.company_id, "partner_id": partner.id,
                            "qualifier": qualifier, "code": code, "address_id": address }),
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Ends a message nobody will draft.
pub async fn dismiss(ctx: &mut Ctx<'_>, message_id: Uuid, reason: &str) -> Result<Value> {
    let row = proposal::message_row(ctx, message_id, true).await?;
    ctx.require(PERMISSION, row.company_id)?;
    ctx.touch("edi_message", row.id);
    if row.direction != "in" || !matches!(row.state.as_str(), "proposed" | "test" | "received") {
        return Err(Error::state(format!(
            "this message is {}: only a message received and not drafted is dismissed",
            row.state
        )));
    }
    if reason.trim().is_empty() {
        return Err(Error::field("reason", "say why the message is dismissed"));
    }
    sqlx::query("UPDATE edi_message SET state = 'dismissed', reason = $2 WHERE id = $1")
        .bind(row.id)
        .bind(reason.trim())
        .execute(&mut *ctx.conn)
        .await?;
    Ok(json!({ "id": row.id, "state": "dismissed", "reason": reason.trim() }))
}
