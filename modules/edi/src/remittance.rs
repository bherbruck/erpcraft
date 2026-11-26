//! A customer's remittance advice (820, or REMADV): what its payment
//! pays. Proposed like a bank line: the open items it names by invoice
//! number, each with what is paid and the discount taken, drafted as a
//! payment. What it deducts (a chargeback) is never booked by itself: the
//! policy `edi_remittance.deduction` refuses it unless configured, so each
//! deduction is a note a person reads, and the invoice keeps it open.

use std::collections::BTreeSet;

use chrono::{NaiveDate, Utc};
use erpcraft_kernel::document::{self, NewDocument};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, company, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::inbound::blocking;
use crate::partner;
use crate::policy::DEDUCTION;
use crate::proposal::{self, Finding, MessageRow, Severity};
use crate::x12::remit820::{self, Remittance};

/// One document the remittance pays.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemitLine {
    /// The document's number as the partner names it.
    pub reference: String,
    /// The open item found for it, if any.
    pub open_item_id: Option<Uuid>,
    /// What is open on it now, in its currency.
    pub open: Option<Decimal>,
    pub paid: Decimal,
    pub discount: Decimal,
    /// What the partner deducts from it.
    pub deducted: Decimal,
}

/// One amount the partner deducts.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Deduction {
    /// The document it is taken off; none for the whole payment.
    pub reference: Option<String>,
    pub amount: Decimal,
    pub reason: String,
    /// Whether the policy takes it as it is.
    pub allowed: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemitProposal {
    pub message_id: Uuid,
    pub state: String,
    /// The payment's trace number.
    pub trace: String,
    /// The day the money is paid: the payment's date.
    pub date: NaiveDate,
    pub amount: Option<Decimal>,
    pub currency: Option<String>,
    pub lines: Vec<RemitLine>,
    pub deductions: Vec<Deduction>,
    pub findings: Vec<Finding>,
    /// Whether edi.draft_remittance would draft it as it stands.
    pub would_draft: bool,
}

async fn remittance_of(ctx: &mut Ctx<'_>, row: &MessageRow) -> Result<Remittance> {
    if row.direction != "in" || !matches!(row.message.as_str(), "820" | "REMADV") {
        return Err(Error::validation(
            "this message is not a remittance received: only an 820 or a REMADV is proposed as a \
             payment",
        ));
    }
    let (Some(interchange), Some(position)) = (row.interchange_id, row.position) else {
        return Err(Error::internal("a remittance received has no interchange"));
    };
    let payload: Vec<u8> = sqlx::query_scalar("SELECT payload FROM edi_interchange WHERE id = $1")
        .bind(interchange)
        .fetch_one(&mut *ctx.conn)
        .await?;
    let index = (position as usize).saturating_sub(1);
    let edifact = row.message == "REMADV";
    blocking(move || -> Option<Remittance> {
        if edifact {
            let interchange = crate::edifact::read(&payload).ok()?.pop()?;
            let d = interchange.delimiters;
            let message = interchange.messages.into_iter().nth(index)?;
            Some(crate::edifact::remadv::read(&message, &d).0)
        } else {
            let set = crate::x12::read(&payload)
                .ok()?
                .pop()?
                .groups
                .into_iter()
                .flat_map(|g| g.sets)
                .nth(index)?;
            Some(remit820::read(&set).0)
        }
    })
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::internal("the remittance is no longer where it was read"))
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: Uuid,
    open: Decimal,
    currency: String,
    doc_date: NaiveDate,
}

/// What a remittance would become, without doing anything.
pub async fn propose(ctx: &mut Ctx<'_>, message_id: Uuid, lock: bool) -> Result<RemitProposal> {
    let row = proposal::message_row(ctx, message_id, lock).await?;
    ctx.require("edi_message:read", row.company_id)?;
    ctx.require("open_item:read", row.company_id)?;
    let remit = remittance_of(ctx, &row).await?;
    let partner = partner::load(&mut *ctx.conn, row.partner_id).await?;
    let mut findings: Vec<Finding> = serde_json::from_value::<Vec<Finding>>(row.findings.clone())
        .unwrap_or_default()
        .into_iter()
        // That the total does not agree is blocking here, below.
        .filter(|f| f.code != "count")
        .collect();
    match row.state.as_str() {
        "proposed" => {}
        "drafted" => findings.push(Finding::blocking(
            "drafted",
            None,
            "A payment was drafted from this remittance already.",
        )),
        other => findings.push(Finding::blocking(
            "state",
            None,
            format!("A remittance in the state {other} is not drafted."),
        )),
    }
    for (position, tag) in &remit.unhandled {
        findings.push(Finding::note(
            "unhandled",
            None,
            format!("Segment {tag} at position {position} has no field here."),
        ));
    }
    let date = remit.date.unwrap_or_else(|| Utc::now().date_naive());
    let customer = partner.customer()?;
    let base = company::base_currency(&mut *ctx.conn, row.company_id).await?;

    let mut lines = vec![];
    let mut deductions = vec![];
    let mut currencies = BTreeSet::new();
    let mut seen: Vec<Uuid> = vec![];
    for item in &remit.items {
        let reference = item.reference.as_str();
        let deducted: Decimal = item
            .adjustments
            .iter()
            .filter_map(|a| a.amount)
            .filter(|a| *a < Decimal::ZERO)
            .map(|a| -a)
            .sum();
        let mut line = RemitLine {
            reference: item.reference.clone(),
            open_item_id: None,
            open: None,
            paid: item.paid.unwrap_or_default(),
            discount: item.discount.unwrap_or_default(),
            deducted,
        };
        if item.qualifier != remit820::INVOICE {
            findings.push(Finding::blocking(
                "document_kind",
                Some(reference),
                format!(
                    "The remittance pays {reference} as a document of kind {}, not an invoice: a \
                     person decides.",
                    item.qualifier
                ),
            ));
        }
        let found: Vec<ItemRow> = sqlx::query_as(
            "SELECT i.id, COALESCE(i.txn_amount, i.amount) - COALESCE(i.txn_settled, i.settled)
                        AS open,
                    COALESCE(i.currency, $4) AS currency, i.doc_date
             FROM open_item i JOIN posting p ON p.id = i.posting_id
             JOIN document d ON d.id = p.document_id
             WHERE i.company_id = $1 AND i.kind = 'receivable' AND i.party_type = 'customer'
               AND i.party_id = $2 AND d.number = $3
             ORDER BY i.doc_date, i.id",
        )
        .bind(row.company_id)
        .bind(customer)
        .bind(reference)
        .bind(&base)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let open_items: Vec<&ItemRow> = found.iter().filter(|i| !i.open.is_zero()).collect();
        match open_items.as_slice() {
            [] => findings.push(Finding::blocking(
                "invoice_unknown",
                Some(reference),
                if found.is_empty() {
                    format!("No invoice {reference} of the partner's customer is known.")
                } else {
                    format!("Invoice {reference} is paid already: nothing is open on it.")
                },
            )),
            [found] => {
                line.open_item_id = Some(found.id);
                line.open = Some(found.open);
                currencies.insert(found.currency.clone());
                if seen.contains(&found.id) {
                    findings.push(Finding::blocking(
                        "invoice_twice",
                        Some(reference),
                        format!("The remittance pays invoice {reference} twice."),
                    ));
                }
                seen.push(found.id);
                if found.doc_date > date {
                    findings.push(Finding::blocking(
                        "paid_before_invoice",
                        Some(reference),
                        format!(
                            "Invoice {reference} is dated {}, after the payment ({date}): a \
                             payment settles what exists by its date. Take the money on account \
                             and apply it once the invoice exists.",
                            found.doc_date
                        ),
                    ));
                }
                if line.paid < Decimal::ZERO || line.discount < Decimal::ZERO {
                    findings.push(Finding::blocking(
                        "amount",
                        Some(reference),
                        format!("The remittance pays {reference} less than nothing."),
                    ));
                } else if line.paid + line.discount > found.open {
                    findings.push(Finding::blocking(
                        "overpaid",
                        Some(reference),
                        format!(
                            "The remittance pays {} and takes {} discount on invoice \
                             {reference}, which has {} open.",
                            line.paid, line.discount, found.open
                        ),
                    ));
                }
                if !line.discount.is_zero() {
                    findings.push(Finding::note(
                        "discount",
                        Some(reference),
                        format!(
                            "The partner takes {} discount on invoice {reference}: check that its \
                             terms allow it.",
                            line.discount
                        ),
                    ));
                }
            }
            _ => findings.push(Finding::blocking(
                "invoice_ambiguous",
                Some(reference),
                format!("More than one open item of the customer is numbered {reference}."),
            )),
        }
        lines.push(line);
        for a in &item.adjustments {
            deductions.push((Some(item.reference.clone()), a.clone()));
        }
    }
    for a in &remit.adjustments {
        deductions.push((None, a.clone()));
    }

    // Deductions: never booked; a person reads each, unless the policy
    // takes it as it is.
    let mut out_deductions = vec![];
    for (reference, a) in deductions {
        let Some(amount) = a.amount.filter(|a| !a.is_zero()) else {
            continue;
        };
        let decision = ctx
            .policy(DEDUCTION, partner.company_id)
            .await?
            .evaluate(json!({
                "partner_code": partner.code,
                "amount": (-amount).to_string(),
                "reason": a.reason,
            }))?;
        let allowed = matches!(decision, erpcraft_kernel::Decision::Allowed);
        let on = reference
            .as_deref()
            .map(|r| format!(" from invoice {r}"))
            .unwrap_or_default();
        let message = format!(
            "The partner deducts {}{on} (reason {}{}). Nothing is booked for it: the invoice \
             keeps it open until a person settles it, with a credit note or by asking for the \
             money.",
            -amount,
            if a.reason.is_empty() {
                "none given"
            } else {
                &a.reason
            },
            if a.reference.is_empty() {
                String::new()
            } else {
                format!(", reference {}", a.reference)
            }
        );
        findings.push(Finding::note(
            if allowed {
                "deduction_allowed"
            } else {
                "deduction"
            },
            reference.as_deref(),
            message,
        ));
        out_deductions.push(Deduction {
            reference,
            amount: -amount,
            reason: a.reason.clone(),
            allowed,
        });
    }

    let paid: Decimal = lines.iter().map(|l| l.paid).sum();
    if let Some(amount) = remit.amount
        && amount != paid
    {
        findings.push(Finding::blocking(
            "total",
            None,
            format!(
                "The payment is {amount} and the documents it pays come to {paid}: a person \
                 decides what was paid."
            ),
        ));
    }
    if currencies.len() > 1 {
        findings.push(Finding::blocking(
            "currency",
            None,
            "The invoices paid are in more than one currency: one payment pays one currency.",
        ));
    }
    if lines.iter().all(|l| l.paid.is_zero()) {
        findings.push(Finding::blocking(
            "nothing_paid",
            None,
            "The remittance pays nothing.",
        ));
    }
    let would_draft = !findings.iter().any(|f| f.severity == Severity::Blocking);
    Ok(RemitProposal {
        message_id: row.id,
        state: row.state.clone(),
        trace: remit.trace.clone(),
        date,
        amount: remit.amount,
        currency: currencies.into_iter().next(),
        lines,
        deductions: out_deductions,
        findings,
        would_draft,
    })
}

/// Drafts the payment a remittance proposes.
pub async fn draft(
    ctx: &mut Ctx<'_>,
    message_id: Uuid,
    bank_account_id: Uuid,
    accept_notes: bool,
) -> Result<Value> {
    let p = propose(ctx, message_id, true).await?;
    let row = proposal::message_row(ctx, message_id, false).await?;
    if let Some(first) = p.findings.iter().find(|f| f.severity == Severity::Blocking) {
        return Err(Error::validation(format!(
            "the remittance is not drafted: {}",
            first.message
        )));
    }
    let notes: Vec<&Finding> = p
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Note && f.code != "deduction_allowed")
        .collect();
    if !notes.is_empty() && !accept_notes {
        return Err(Error::validation(format!(
            "the remittance has {} note(s), deductions among them; read them with \
             edi.remittance_proposal and draft with accept_notes: true",
            notes.len()
        )));
    }
    let partner = partner::load(&mut *ctx.conn, row.partner_id).await?;
    let allocations: Vec<Value> = p
        .lines
        .iter()
        .filter(|l| !l.paid.is_zero() || !l.discount.is_zero())
        .map(
            |l| json!({ "open_item_id": l.open_item_id, "amount": l.paid, "discount": l.discount }),
        )
        .collect();
    let mut memo = format!("EDI remittance {} from partner {}.", p.trace, partner.code);
    for d in &p.deductions {
        memo.push_str(&format!(
            " Deducted {}{} (reason {}), not booked.",
            d.amount,
            d.reference
                .as_deref()
                .map(|r| format!(" from {r}"))
                .unwrap_or_default(),
            d.reason
        ));
    }
    let view = document::create(
        ctx,
        NewDocument {
            doc_type: "payment".into(),
            company_id: row.company_id,
            doc_date: p.date,
            memo: Some(memo),
            body: json!({
                "direction": "incoming",
                "bank_account_id": bank_account_id,
                "currency": p.currency,
                "allocations": allocations,
            }),
        },
    )
    .await?;
    sqlx::query(
        "UPDATE edi_message SET state = 'drafted', doc_type = 'payment', document_id = $2
         WHERE id = $1",
    )
    .bind(message_id)
    .bind(view.header.id)
    .execute(&mut *ctx.conn)
    .await?;
    ctx.touch("edi_message", message_id);
    Ok(json!(view))
}

// ---------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemitInput {
    /// A remittance received: an 820 or a REMADV.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "edi_message", "key": "id" }))]
    message_id: Uuid,
}

pub struct ProposeRemittance;

#[async_trait]
impl Command for ProposeRemittance {
    type Input = RemitInput;
    type Output = RemitProposal;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "edi.remittance_proposal",
            "What a customer's remittance advice (820 or REMADV) would become, without changing \
             anything: per invoice it names, the open item found, what is open, paid and taken \
             as discount; and every amount it deducts, with its reason code. An invoice that is \
             not found or not open, one dated after the payment, an amount more than is open, \
             or a total that does not agree is blocking. A deduction is never booked: it is a \
             note for a person unless the policy edi_remittance.deduction takes it.",
        )
        .permission("edi_message:read")
        .label("Remittance proposal")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RemitInput) -> Result<RemitProposal> {
        propose(ctx, input.message_id, false).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftRemitInput {
    /// A remittance received, proposed.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "edi_message", "key": "id" }))]
    message_id: Uuid,
    /// The bank account the money arrives in.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "account", "key": "id" }))]
    bank_account_id: Uuid,
    /// Draft although the proposal has notes (deductions among them),
    /// having read them.
    #[serde(default)]
    accept_notes: bool,
}

pub struct DraftRemittance;

#[async_trait]
impl Command for DraftRemittance {
    type Input = DraftRemitInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi.draft_remittance",
            "Draft the incoming payment a customer's remittance advice proposes: one allocation \
             per invoice it pays, with the amount paid and the discount taken, dated the day the \
             money is paid, into the bank account given. Refused while a blocking finding \
             stands, and while notes (deductions) stand unless accept_notes. A deduction is not \
             booked: the invoice keeps it open. The payment is a draft: post it with \
             payment.post once the money is in.",
        )
        .permission(crate::inbound::PERMISSION)
        .label("Draft payment")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftRemitInput) -> Result<Value> {
        draft(
            ctx,
            input.message_id,
            input.bank_account_id,
            input.accept_notes,
        )
        .await
    }
}
