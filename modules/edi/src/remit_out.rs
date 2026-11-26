//! The remittance advice sent to a vendor (820, or REMADV for an EDIFACT
//! partner): what a posted payment pays, each of the vendor's invoices by
//! the vendor's own number. Made for every payment a posted payment run
//! makes to a vendor whose partner wants one (`remittance_out`), or on
//! demand.

use chrono::Utc;
use erpcraft_kernel::document;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, entity, schemars,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::outbound::{self, Generated};
use crate::partner::{self, Partner};
use crate::proposal::Finding;
use crate::standalone::{self, Standalone};
use crate::x12::po850::Party;
use crate::x12::remit820::{INVOICE, RemitItem, Remittance};

#[derive(sqlx::FromRow)]
struct Paid {
    amount: Decimal,
    discount: Decimal,
    vendor_id: Uuid,
    invoiced: Decimal,
    reference: Option<String>,
    number: Option<String>,
}

/// The partner a payment's vendor trades through, if it wants advice.
pub async fn partner_of_payment(
    conn: &mut erpcraft_kernel::PgConnection,
    payment: Uuid,
) -> Result<Option<Partner>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT p.id FROM payment_allocation a
         JOIN open_item i ON i.id = a.open_item_id
         JOIN document d ON d.id = a.document_id
         JOIN edi_partner p ON p.vendor_id = i.party_id AND p.company_id = d.company_id
         WHERE a.document_id = $1 AND i.party_type = 'vendor' AND p.active AND p.remittance_out
         ORDER BY p.created_at LIMIT 1",
    )
    .bind(payment)
    .fetch_optional(&mut *conn)
    .await?;
    match id {
        Some(id) => Ok(Some(partner::load(conn, id).await?)),
        None => Ok(None),
    }
}

/// Makes, checks and (unless a preview) stores the advice of a payment.
pub async fn generate(ctx: &mut Ctx<'_>, payment_id: Uuid, preview: bool) -> Result<Generated> {
    let view = document::get(ctx, "payment", payment_id).await?;
    ctx.require(outbound::PERMISSION, view.header.company_id)?;
    if view.header.status != "posted" {
        return Err(Error::state(
            "a remittance advice is made for a posted payment",
        ));
    }
    let partner = partner_of_payment(&mut *ctx.conn, payment_id)
        .await?
        .ok_or_else(|| {
            Error::validation(
                "the payment's vendor has no EDI partner that wants remittance advice \
                 (remittance_out)",
            )
        })?;
    let made: Option<String> = sqlx::query_scalar(
        "SELECT state FROM edi_message
         WHERE direction = 'out' AND doc_type = 'payment' AND document_id = $1
           AND state NOT IN ('invalid', 'rejected', 'failed') AND superseded_by IS NULL
         LIMIT 1",
    )
    .bind(payment_id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    if let Some(state) = made
        && !preview
    {
        return Err(Error::state(format!(
            "a remittance advice was made for this payment already and is {state}: send the \
             same bytes again with edi_message.resend"
        )));
    }
    let paid: Vec<Paid> = sqlx::query_as(
        "SELECT a.amount, a.discount, i.party_id AS vendor_id,
                COALESCE(i.txn_amount, i.amount) AS invoiced, v.vendor_reference AS reference,
                d.number
         FROM payment_allocation a
         JOIN open_item i ON i.id = a.open_item_id
         JOIN posting p ON p.id = i.posting_id
         JOIN document d ON d.id = p.document_id
         LEFT JOIN vendor_invoice v ON v.document_id = d.id
         WHERE a.document_id = $1 ORDER BY a.line_no",
    )
    .bind(payment_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let mut findings = vec![];
    let mut items = vec![];
    for p in &paid {
        if Some(p.vendor_id) != partner.vendor_id {
            continue;
        }
        let reference = p
            .reference
            .clone()
            .filter(|r| !r.trim().is_empty())
            .or_else(|| p.number.clone())
            .unwrap_or_default();
        if p.reference.as_deref().is_none_or(|r| r.trim().is_empty()) {
            findings.push(Finding::note(
                "no_vendor_reference",
                None,
                format!(
                    "An invoice paid has no vendor's number: it is named by ours, {reference}."
                ),
            ));
        }
        items.push(RemitItem {
            qualifier: INVOICE.into(),
            reference,
            paid: Some(p.amount.normalize()),
            invoiced: Some(p.invoiced.abs().normalize()),
            discount: (!p.discount.is_zero()).then(|| p.discount.normalize()),
            adjustments: vec![],
        });
    }
    let total = items
        .iter()
        .filter_map(|i| i.paid)
        .fold(Decimal::ZERO, |t, a| t + a);
    let us = entity::get(ctx, "company", view.header.company_id)
        .await
        .ok()
        .and_then(|c| c["name"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let vendor = match partner.vendor_id {
        Some(v) => entity::get(ctx, "vendor", v)
            .await
            .ok()
            .and_then(|c| c["name"].as_str().map(str::to_owned))
            .unwrap_or_default(),
        None => String::new(),
    };
    let remit = Remittance {
        handling: "C".into(),
        amount: Some(total.normalize()),
        credit_debit: "C".into(),
        method: "ACH".into(),
        date: Some(view.header.doc_date),
        trace: view.header.number.clone().unwrap_or_default(),
        parties: vec![
            Party {
                role: "PR".into(),
                name: us,
                ..Party::default()
            },
            Party {
                role: "PE".into(),
                name: vendor,
                ..Party::default()
            },
        ],
        items,
        ..Remittance::default()
    };
    let edifact = partner.is_edifact();
    let d = if edifact {
        partner
            .edifact_envelope(Utc::now().naive_utc(), 0)?
            .delimiters
    } else {
        crate::Delimiters::X12
    };
    let body = if edifact {
        crate::edifact::remadv::body(&remit, &d)
    } else {
        remit.body()
    };
    let meant = remit.clone();
    let reads_back = move |bytes: &[u8]| -> bool {
        if edifact {
            let Some(interchange) = crate::edifact::read(bytes).ok().and_then(|mut i| i.pop())
            else {
                return false;
            };
            let Some(message) = interchange.messages.first() else {
                return false;
            };
            let (read, verdict) = crate::edifact::remadv::read(message, &interchange.delimiters);
            interchange.errors.is_empty()
                && message.errors.is_empty()
                && verdict.is_clean()
                && read == crate::edifact::remadv::carried(&meant)
        } else {
            let Some(set) = crate::x12::read(bytes)
                .ok()
                .and_then(|mut i| i.pop())
                .filter(|i| i.errors.is_empty())
                .and_then(|i| i.groups.into_iter().next())
                .and_then(|g| g.sets.into_iter().next())
                .filter(|s| s.errors.is_empty())
            else {
                return false;
            };
            let (read, verdict) = crate::x12::remit820::read(&set);
            verdict == Default::default() && read == meant
        }
    };
    standalone::send(
        ctx,
        Standalone {
            code: if edifact { "REMADV" } else { "820" }.into(),
            functional_id: "RA",
            body: Some(body),
            findings,
            business_key: view.header.number.clone(),
            doc_type: Some("payment".into()),
            document_id: Some(payment_id),
            total,
            partner,
        },
        preview,
        reads_back,
    )
    .await
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemitOutInput {
    /// A posted payment to a vendor.
    #[schemars(extend("x-field-type" = { "kind": "document", "doc_type": "payment" }))]
    payment_id: Uuid,
    /// Show the advice and what is wrong with it; store nothing.
    #[serde(default)]
    preview: bool,
}

pub struct AdviseRemittance;

#[async_trait]
impl Command for AdviseRemittance {
    type Input = RemitOutInput;
    type Output = Generated;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "edi.remittance_advice",
            "Make the remittance advice (an 820, or REMADV for an EDIFACT partner) of a posted \
             payment to a vendor whose EDI partner wants one: each invoice paid by the \
             vendor's own number, with what is paid, what it came to and the discount kept \
             back. It is read back before it is kept. A posted payment run makes one for each \
             such vendor by itself.",
        )
        .permission(outbound::PERMISSION)
        .label("Remittance advice")
        .resource("edi_message")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RemitOutInput) -> Result<Generated> {
        generate(ctx, input.payment_id, input.preview).await
    }
}
