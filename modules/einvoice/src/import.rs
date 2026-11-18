//! An incoming electronic invoice: read, checked, matched to a vendor and
//! an order, and drafted as a vendor invoice.
//!
//! What the caller learns goes through the kernel's reads with the
//! caller's rights: a vendor, a party record or an order the caller may
//! not read is not proposed. Before a draft is made, what it will come to
//! is worked out as posting will work it out and set against the invoice's
//! totals; a difference is not drafted silently.

use std::collections::BTreeMap;

use erpcraft_kernel::document::{self, DocumentView, NewDocument};
use erpcraft_kernel::{Ctx, Decimal, Error, JsonSchema, Result, entity, round_money, schemars};
use rust_decimal::RoundingStrategy;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::Digest;
use uuid::Uuid;

use crate::model::{Finding, Invoice, Line, Syntax};

/// A vendor the invoice may be from.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Candidate {
    pub vendor_id: Uuid,
    pub code: Option<String>,
    pub name: Option<String>,
    /// `vat_id` when the seller's VAT identifier (BT-31) is the vendor's;
    /// `name` when only the name is alike.
    pub matched_by: String,
}

/// A tax code a line could take.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TaxCandidate {
    pub tax_code_id: Uuid,
    pub code: String,
}

/// What an invoice line would become.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LineProposal {
    /// The invoice line's identifier (BT-126).
    pub line: String,
    pub name: String,
    pub quantity: Decimal,
    pub net_amount: Decimal,
    /// The purchase order line it invoices, when one was found.
    pub order_line_id: Option<Uuid>,
    /// `line_reference` (BT-132 names the order line), `item` (the buyer's
    /// item identifier, BT-156, is the item's code), or null: then the line
    /// goes to the account given to `einvoice.import`.
    pub matched_by: Option<String>,
    /// The tax code the line takes: the one chosen in `line_tax_codes`, or
    /// the company's only code for the line's VAT category and rate.
    pub tax_code_id: Option<Uuid>,
    /// The company's codes for the line's category and rate. Where there
    /// is more than one, choose with `line_tax_codes`.
    pub tax_candidates: Vec<TaxCandidate>,
    /// The unit price and the discount the draft line takes, so that its
    /// net is the invoice line's to the cent.
    pub unit_price: Decimal,
    pub discount_amount: Option<Decimal>,
}

/// What the draft will come to, worked out as posting works it out.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct DraftTotals {
    pub net: Decimal,
    pub tax: Decimal,
    pub gross: Decimal,
}

/// What `einvoice.read` returns.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReadOutput {
    pub syntax: Syntax,
    /// Where the XML came from: `xml`, or `pdf` for ZUGFeRD / Factur-X.
    pub container: String,
    pub invoice: Invoice,
    /// Whether every rule checked here holds.
    pub valid: bool,
    pub findings: Vec<Finding>,
    /// The vendor, when the seller's VAT identifier names exactly one.
    pub vendor_id: Option<Uuid>,
    /// Every vendor it may be that the caller may read, best first.
    /// Confirm one with `vendor_id` when importing if the VAT identifier
    /// did not decide.
    pub candidates: Vec<Candidate>,
    /// The open purchase order the buyer's order reference (BT-13) names,
    /// of that vendor, when the caller may read it.
    pub purchase_order_id: Option<Uuid>,
    pub lines: Vec<LineProposal>,
    /// What the draft will come to, once the vendor is known.
    pub draft: Option<DraftTotals>,
    /// What stops the import until it is put right: a line charged VAT
    /// without a tax code, for one.
    pub problems: Vec<String>,
    /// Where the draft would not say what the invoice says: totals that
    /// differ, an amount already paid, a buyer the invoice does not name.
    /// `einvoice.import` refuses them unless `accept_differences` is given.
    pub differences: Vec<String>,
    /// The vendor invoice this invoice was imported as before.
    pub imported_as: Option<Uuid>,
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// A record as the caller may read it; nothing if they may not.
async fn readable(ctx: &mut Ctx<'_>, entity_name: &str, id: Uuid) -> Result<Option<Value>> {
    match entity::get(ctx, entity_name, id).await {
        Ok(record) => Ok(Some(record)),
        Err(Error::NotFound(_) | Error::Forbidden(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn text(record: &Value, field: &str) -> Option<String> {
    record[field].as_str().map(str::to_owned)
}

/// The unit price and discount a draft line takes so that its net,
/// worked out as posting works it out (quantity times unit price, rounded,
/// less the discount), is the invoice line's net to the cent. The unit
/// price has at most six decimals, rounded up, so the discount is never
/// below zero; allowances and charges on the line are in the discount.
pub fn line_terms(
    line: &Line,
    decimals: u32,
) -> std::result::Result<(Decimal, Option<Decimal>), String> {
    let out_of_range = || {
        format!(
            "line {}: its amounts are beyond what can be computed",
            line.id
        )
    };
    let base = line.base_quantity.unwrap_or(Decimal::ONE);
    if base <= Decimal::ZERO || line.quantity <= Decimal::ZERO {
        return Err(format!(
            "line {}: a quantity and base quantity above zero are needed",
            line.id
        ));
    }
    if line.quantity.normalize().scale() > 6 {
        return Err(format!(
            "line {}: the quantity has more than six decimals",
            line.id
        ));
    }
    let per_unit = line.price.checked_div(base).ok_or_else(out_of_range)?;
    let net_per_unit = line
        .net_amount
        .checked_div(line.quantity)
        .ok_or_else(out_of_range)?;
    let unit = per_unit
        .max(net_per_unit)
        .round_dp_with_strategy(6, RoundingStrategy::ToPositiveInfinity);
    let gross = round_money(
        line.quantity.checked_mul(unit).ok_or_else(out_of_range)?,
        decimals,
    );
    let discount = gross - line.net_amount;
    if discount < Decimal::ZERO {
        return Err(out_of_range());
    }
    Ok((unit.normalize(), (!discount.is_zero()).then_some(discount)))
}

/// Read, check and match an incoming invoice for a company, without
/// creating anything.
pub async fn read(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    invoice: Invoice,
    container: &str,
    chosen: &BTreeMap<String, Uuid>,
) -> Result<ReadOutput> {
    let findings = crate::model::check(&invoice);
    let mut differences = Vec::new();
    let mut extra = Vec::new();

    // Addressed to this company? By VAT identifier where both have one;
    // otherwise by name or electronic address; and said when nothing
    // identifies the company.
    let own = erpcraft_party::load(&mut *ctx.conn, "company", company_id).await?;
    let own_vat = own.tax_id("eu_vat");
    let own_legal = own.legal_name.clone();
    let own_address = own.electronic_address.as_ref().map(|(_, v)| v.clone());
    let company_name: String = sqlx::query_scalar("SELECT name FROM company WHERE id = $1")
        .bind(company_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
    let buyer = &invoice.buyer;
    match (&own_vat, &buyer.vat_id) {
        (Some(own), Some(theirs)) if normalized(own) != normalized(theirs) => {
            extra.push(Finding {
                rule: "buyer".into(),
                message: format!(
                    "the invoice is addressed to VAT identifier {theirs}, not the company's {own}"
                ),
                fatal: true,
            });
        }
        (Some(_), Some(_)) => {}
        _ => {
            let names = [Some(company_name.clone()), own_legal.clone()];
            let by_name = names
                .iter()
                .flatten()
                .any(|n| normalized(n) == normalized(&buyer.name));
            let by_address = match (&own_address, &buyer.electronic_address) {
                (Some(own), Some(theirs)) => own.trim().eq_ignore_ascii_case(theirs.trim()),
                _ => false,
            };
            if !by_name && !by_address {
                differences.push(format!(
                    "nothing on the invoice identifies the company as its buyer: it names \
                     '{}'{}, without a VAT identifier to compare",
                    buyer.name,
                    buyer
                        .electronic_address
                        .as_ref()
                        .map(|a| format!(" at {a}"))
                        .unwrap_or_default()
                ));
            }
        }
    }

    // The vendor: by VAT identifier, else proposals by name, each only if
    // the caller may read the records it rests on.
    let mut candidates: Vec<Candidate> = Vec::new();
    if let Some(vat) = invoice.seller.vat_id.as_deref().map(normalized) {
        let rows = erpcraft_party::vendors_by_tax_id(&mut *ctx.conn, "eu_vat", &vat).await?;
        for (_, vendor) in rows {
            // A vendor's tax identifier is as readable as the vendor: matched
            // only on one the caller may read.
            if let Some(v) = readable(ctx, "vendor", vendor).await? {
                candidates.push(Candidate {
                    vendor_id: vendor,
                    code: text(&v, "code"),
                    name: text(&v, "name"),
                    matched_by: "vat_id".into(),
                });
            }
        }
    }
    let by_vat = candidates.len();
    let name = invoice.seller.name.trim().to_lowercase();
    if !name.is_empty() {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM vendor
             WHERE lower(name) = $1 OR lower(name) LIKE '%' || $2 || '%'
                OR $1 LIKE '%' || lower(name) || '%'
             ORDER BY (lower(name) = $1) DESC, code LIMIT 25",
        )
        .bind(&name)
        .bind(name.replace(['%', '_'], ""))
        .fetch_all(&mut *ctx.conn)
        .await?;
        for id in ids {
            if candidates.len() >= by_vat + 10 || candidates.iter().any(|c| c.vendor_id == id) {
                continue;
            }
            // Proposed by name only to someone who may read that name.
            if let Some(v) = readable(ctx, "vendor", id).await?
                && v["name"].as_str().is_some()
            {
                candidates.push(Candidate {
                    vendor_id: id,
                    code: text(&v, "code"),
                    name: text(&v, "name"),
                    matched_by: "name".into(),
                });
            }
        }
    }
    let vendor_id = (by_vat == 1).then(|| candidates[0].vendor_id);

    let imported_as: Option<Uuid> = match vendor_id {
        Some(vendor) => {
            sqlx::query_scalar(
                "SELECT document_id FROM einvoice_import
                 WHERE company_id = $1 AND vendor_id = $2 AND invoice_number = $3",
            )
            .bind(company_id)
            .bind(vendor)
            .bind(&invoice.number)
            .fetch_optional(&mut *ctx.conn)
            .await?
        }
        None => None,
    };
    let mut findings = findings;
    findings.extend(extra);
    let mut output = ReadOutput {
        syntax: invoice.syntax,
        container: container.into(),
        valid: !findings.iter().any(|f| f.fatal),
        findings,
        vendor_id,
        candidates,
        purchase_order_id: None,
        lines: vec![],
        draft: None,
        problems: vec![],
        differences,
        imported_as,
        invoice,
    };
    if let Some(vendor) = vendor_id {
        propose(ctx, company_id, vendor, chosen, &mut output).await?;
    }
    Ok(output)
}

/// Find the order, each line's order line and tax code, and what the
/// draft will come to, for a vendor.
pub async fn propose(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    vendor_id: Uuid,
    chosen: &BTreeMap<String, Uuid>,
    output: &mut ReadOutput,
) -> Result<()> {
    let invoice = output.invoice.clone();
    // The order, if the caller may read it.
    let order_id: Option<Uuid> = match &invoice.order_reference {
        Some(number) => {
            sqlx::query_scalar(
                "SELECT d.id FROM document d JOIN purchase_order o ON o.document_id = d.id
                 WHERE d.company_id = $1 AND d.doc_type = 'purchase_order' AND d.number = $2
                   AND d.status = 'open' AND o.vendor_id = $3",
            )
            .bind(company_id)
            .bind(number.trim())
            .bind(vendor_id)
            .fetch_optional(&mut *ctx.conn)
            .await?
        }
        None => None,
    };
    let order = match order_id {
        Some(id) => match document::get(ctx, "purchase_order", id).await {
            Ok(view) => Some(view),
            Err(Error::NotFound(_) | Error::Forbidden(_)) => None,
            Err(e) => return Err(e),
        },
        None => None,
    };
    let mut order_lines: Vec<(Uuid, i64, Option<String>)> = Vec::new();
    if let Some(view) = &order {
        for l in document::body::lines(&view.body, "lines") {
            let Some(id) = document::body::uuid(&l["id"]) else {
                continue;
            };
            let code: Option<String> = match document::body::uuid(&l["item_id"]) {
                Some(item) => {
                    sqlx::query_scalar("SELECT code FROM item WHERE id = $1")
                        .bind(item)
                        .fetch_optional(&mut *ctx.conn)
                        .await?
                }
                None => None,
            };
            order_lines.push((id, l["line_no"].as_i64().unwrap_or_default(), code));
        }
    }
    let decimals = erpcraft_gl::currency_decimals(&mut *ctx.conn, &invoice.currency).await?;
    let mut lines = Vec::new();
    let mut problems = Vec::new();
    let mut totals = DraftTotals::default();
    for line in &invoice.lines {
        let mut found = None;
        if let Some(reference) = line.order_line_reference.as_deref().map(str::trim)
            && let Some((id, _, _)) = order_lines
                .iter()
                .find(|(id, no, _)| reference == no.to_string() || reference == id.to_string())
        {
            found = Some((*id, "line_reference"));
        }
        if found.is_none()
            && let Some(code) = &line.buyer_item_id
        {
            let same: Vec<_> = order_lines
                .iter()
                .filter(|(_, _, c)| c.as_deref() == Some(code.as_str()))
                .collect();
            if let [only] = same.as_slice() {
                found = Some((only.0, "item"));
            }
        }
        // The company's tax codes for the category and rate. Where the
        // vendor charges none because the company accounts for the tax
        // (a reverse charge, AE, or an intra-Community supply, K, which is
        // the company's acquisition), only a code the company assesses
        // itself, at its own rate, is one: a 0 % code of the same category
        // is for the company's own sales and would leave the tax unassessed.
        let rate = line.vat_rate.unwrap_or_default();
        let candidates: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT c.id, c.code FROM tax_code c
             JOIN tax_settings s ON s.tax_code_id = c.id AND s.company_id = $1
             WHERE c.active
               AND CASE WHEN $3 IN ('AE', 'K') THEN c.self_assessed AND $2 = 0
                        ELSE c.rate = $2 END
               AND NOT EXISTS (SELECT 1 FROM tax_code_component g WHERE g.group_id = c.id)
               AND COALESCE(c.vat_category,
                            CASE WHEN c.rate > 0 AND NOT c.self_assessed THEN 'S' END) = $3
             ORDER BY c.code",
        )
        .bind(company_id)
        .bind(rate)
        .bind(&line.vat_category)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let tax_code_id = match chosen.get(&line.id) {
            Some(code) => {
                let ours: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM tax_settings WHERE company_id = $1
                                    AND tax_code_id = $2)",
                )
                .bind(company_id)
                .bind(code)
                .fetch_one(&mut *ctx.conn)
                .await?;
                if !ours {
                    problems.push(format!(
                        "line {}: the tax code chosen is not one the company posts",
                        line.id
                    ));
                }
                Some(*code)
            }
            None if candidates.len() == 1 => Some(candidates[0].0),
            None => None,
        };
        if tax_code_id.is_none() && rate > Decimal::ZERO {
            problems.push(format!(
                "line {}: the invoice charges {}% VAT ({}), and {}; choose one with \
                 line_tax_codes",
                line.id,
                rate.normalize(),
                line.vat_category,
                if candidates.is_empty() {
                    "the company has no tax code for it".to_owned()
                } else {
                    format!(
                        "the company has several codes for it: {}",
                        candidates
                            .iter()
                            .map(|(_, c)| c.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ));
        }
        let (unit_price, discount_amount) = match line_terms(line, decimals) {
            Ok(terms) => terms,
            Err(why) => {
                problems.push(why);
                (line.price, None)
            }
        };
        // What posting will charge on the line.
        let tax = match tax_code_id {
            Some(code) => erpcraft_tax::estimate(
                &mut *ctx.conn,
                company_id,
                code,
                erpcraft_tax::Direction::Paid,
                line.net_amount,
                None,
                None,
            )
            .await
            .map(|t| erpcraft_tax::Tax::total(&t))
            .unwrap_or_default(),
            None => Decimal::ZERO,
        };
        totals.net += line.net_amount;
        totals.tax += tax;
        lines.push(LineProposal {
            line: line.id.clone(),
            name: line.name.clone(),
            quantity: line.quantity,
            net_amount: line.net_amount,
            order_line_id: found.map(|(id, _)| id),
            matched_by: found.map(|(_, by)| by.to_owned()),
            tax_code_id,
            tax_candidates: candidates
                .into_iter()
                .map(|(tax_code_id, code)| TaxCandidate { tax_code_id, code })
                .collect(),
            unit_price,
            discount_amount,
        });
    }
    totals.gross = totals.net + totals.tax;

    // The draft against what the invoice says, to the cent.
    let t = &invoice.totals;
    if totals.tax != t.tax_total {
        output.differences.push(format!(
            "the invoice's VAT (BT-110) is {}, the draft's would be {}",
            t.tax_total, totals.tax
        ));
    }
    if totals.gross != t.tax_inclusive {
        output.differences.push(format!(
            "the invoice's total with VAT (BT-112) is {}, the draft's would be {}",
            t.tax_inclusive, totals.gross
        ));
    }
    if let Some(paid) = t.prepaid.filter(|p| !p.is_zero()) {
        output.differences.push(format!(
            "the invoice says {paid} was paid already (BT-113) and {} is due (BT-115); the \
             draft owes the full {}: record the payment against it",
            t.payable, totals.gross
        ));
    } else if t.payable != totals.gross {
        output.differences.push(format!(
            "the invoice's amount due (BT-115) is {}, the draft would owe {}",
            t.payable, totals.gross
        ));
    }
    output.purchase_order_id = order.map(|o| o.header.id);
    output.lines = lines;
    output.problems = problems;
    output.draft = Some(totals);
    Ok(())
}

pub fn sha256(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the caller says when importing.
pub struct Choices {
    pub vendor_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
    pub line_tax_codes: BTreeMap<String, Uuid>,
    pub accept_differences: bool,
}

/// Draft the vendor invoice an incoming invoice describes.
pub async fn import(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    mut read: ReadOutput,
    sha256: String,
    choices: Choices,
) -> Result<DocumentView> {
    let fatal: Vec<String> = read
        .findings
        .iter()
        .filter(|f| f.fatal)
        .map(|f| format!("[{}] {}", f.rule, f.message))
        .collect();
    if !fatal.is_empty() {
        return Err(Error::field(
            "content",
            format!("the invoice breaks rules of EN 16931: {}", fatal.join("; ")),
        ));
    }
    if read.invoice.is_credit_note() {
        return Err(Error::field(
            "content",
            "is a credit note: enter it as a vendor credit note against the invoice it credits",
        ));
    }
    if !read.invoice.allowances.is_empty() {
        return Err(Error::field(
            "content",
            "has allowances or charges on the document level, which are not imported yet: \
             enter the invoice by hand",
        ));
    }
    // The vendor: the one the VAT identifier names, or the one confirmed.
    let vendor = match (read.vendor_id, choices.vendor_id) {
        (Some(found), Some(given)) if found != given => {
            return Err(Error::field(
                "vendor_id",
                "is not the vendor the seller's VAT identifier belongs to",
            ));
        }
        (Some(found), _) => found,
        (None, Some(given)) => {
            if readable(ctx, "vendor", given).await?.is_none() {
                return Err(Error::field("vendor_id", "must be a vendor"));
            }
            given
        }
        (None, None) => {
            let names: Vec<String> = read
                .candidates
                .iter()
                .map(|c| {
                    format!(
                        "{} {}",
                        c.code.as_deref().unwrap_or_default(),
                        c.name.as_deref().unwrap_or_default()
                    )
                })
                .collect();
            return Err(Error::field(
                "vendor_id",
                if names.is_empty() {
                    "is needed: no vendor has the seller's VAT identifier or name; create the \
                     vendor (and record its VAT identifier with party.set) or name one"
                        .to_owned()
                } else {
                    format!(
                        "is needed: the seller's VAT identifier names no vendor; it may be {}",
                        names.join(", ")
                    )
                },
            ));
        }
    };
    if read.vendor_id.is_none() {
        propose(ctx, company_id, vendor, &choices.line_tax_codes, &mut read).await?;
    }
    let before: Option<Option<String>> = sqlx::query_scalar(
        "SELECT d.number FROM einvoice_import i JOIN document d ON d.id = i.document_id
         WHERE i.company_id = $1 AND i.vendor_id = $2 AND i.invoice_number = $3",
    )
    .bind(company_id)
    .bind(vendor)
    .bind(&read.invoice.number)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    if let Some(number) = before {
        return Err(Error::field(
            "content",
            format!(
                "invoice {} of this vendor was imported before, as {}",
                read.invoice.number,
                number.unwrap_or_else(|| "a draft vendor invoice".into())
            ),
        ));
    }
    if !read.problems.is_empty() {
        return Err(Error::field(
            "content",
            format!("cannot be drafted yet: {}", read.problems.join("; ")),
        ));
    }
    if !read.differences.is_empty() && !choices.accept_differences {
        return Err(Error::field(
            "accept_differences",
            format!(
                "the draft would not say what the invoice says: {}. Give accept_differences to \
                 draft it all the same",
                read.differences.join("; ")
            ),
        ));
    }
    let unmatched: Vec<&str> = read
        .lines
        .iter()
        .filter(|l| l.order_line_id.is_none())
        .map(|l| l.line.as_str())
        .collect();
    if !unmatched.is_empty() && choices.account_id.is_none() {
        return Err(Error::field(
            "account_id",
            format!(
                "is needed for the lines that invoice no order line ({}): the account they \
                 are booked to",
                unmatched.join(", ")
            ),
        ));
    }
    let lines: Vec<Value> = read
        .lines
        .iter()
        .map(|p| {
            let mut l = json!({
                "description": p.name,
                "qty": p.quantity,
                "unit_price": p.unit_price,
                "discount_amount": p.discount_amount,
                "tax_code_id": p.tax_code_id,
            });
            match p.order_line_id {
                Some(order_line) => l["order_line_id"] = json!(order_line),
                None => l["account_id"] = json!(choices.account_id),
            }
            l
        })
        .collect();
    let mut body = json!({
        "vendor_reference": read.invoice.number,
        "lines": lines,
    });
    match read.purchase_order_id {
        Some(order) => body["purchase_order_id"] = json!(order),
        None => {
            body["vendor_id"] = json!(vendor);
            body["currency"] = json!(read.invoice.currency);
        }
    }
    let mut memo = format!(
        "Electronic invoice {} ({}, SHA-256 {})",
        read.invoice.number,
        read.syntax.as_str().to_uppercase(),
        &sha256[..16]
    );
    if !read.differences.is_empty() {
        memo.push_str(&format!(
            ". Differences accepted: {}",
            read.differences.join("; ")
        ));
    }
    let created = document::create(
        ctx,
        NewDocument {
            doc_type: "vendor_invoice".into(),
            company_id,
            doc_date: read.invoice.issue_date,
            memo: Some(memo),
            body,
        },
    )
    .await?;
    // The draft as it was saved must come to what was worked out.
    let expected = read.draft.clone().unwrap_or_default();
    let summary = &created.header.summary;
    let saved_net = document::body::decimal(&summary["total_net"]);
    if saved_net != Some(expected.net) {
        return Err(Error::internal(format!(
            "the draft comes to {saved_net:?} net, not the {} worked out",
            expected.net
        )));
    }
    sqlx::query(
        "INSERT INTO einvoice_import (id, company_id, vendor_id, invoice_number, issue_date,
             syntax, sha256, document_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(erpcraft_kernel::new_id())
    .bind(company_id)
    .bind(vendor)
    .bind(&read.invoice.number)
    .bind(read.invoice.issue_date)
    .bind(read.syntax.as_str())
    .bind(&sha256)
    .bind(created.header.id)
    .execute(&mut *ctx.conn)
    .await?;
    Ok(created)
}
