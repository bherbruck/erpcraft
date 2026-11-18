//! A posted sales invoice or sales credit note as an EN 16931 invoice.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use erpcraft_kernel::document;
use erpcraft_kernel::{
    Ctx, Decimal, Error, JsonSchema, PgConnection, Result, round_money, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::model::{
    self, AllowanceCharge, Finding, Invoice, Line, Party, Payment, Syntax, Totals, VatBreakdown,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocType {
    SalesInvoice,
    SalesCreditNote,
}

impl DocType {
    fn as_str(self) -> &'static str {
        match self {
            DocType::SalesInvoice => "sales_invoice",
            DocType::SalesCreditNote => "sales_credit_note",
        }
    }
}

/// Which rules the invoice is made to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// XRechnung 3.0: EN 16931 with the German rules (buyer reference,
    /// seller contact, electronic addresses, payment instructions).
    #[default]
    Xrechnung,
    /// EN 16931 alone, as ZUGFeRD / Factur-X's EN 16931 profile and other
    /// countries use it.
    En16931,
}

/// A seller or buyer's data as the invoice uses it, read from the party
/// module, where addresses, tax identifiers and contacts are kept.
#[derive(Default)]
struct PartyRow {
    legal_name: Option<String>,
    trading_name: Option<String>,
    legal_registration: Option<String>,
    legal_info: Option<String>,
    vat_id: Option<String>,
    tax_number: Option<String>,
    street: Option<String>,
    additional_street: Option<String>,
    city: Option<String>,
    postcode: Option<String>,
    country_code: Option<String>,
    contact_name: Option<String>,
    contact_phone: Option<String>,
    contact_email: Option<String>,
    electronic_address: Option<String>,
    electronic_address_scheme: String,
    buyer_reference: Option<String>,
    iban: Option<String>,
    bic: Option<String>,
    account_name: Option<String>,
}

/// The party as the document shows it: what it kept when it was posted,
/// else the party's data now.
async fn party_row(
    conn: &mut PgConnection,
    document_id: Uuid,
    role: erpcraft_party::Role,
    kind: &str,
    id: Uuid,
) -> Result<PartyRow> {
    let d = erpcraft_party::load_for_document(&mut *conn, document_id, role, kind, id).await?;
    let address = d.address.clone();
    let mut row = PartyRow {
        legal_name: d.legal_name.clone(),
        trading_name: d.trading_name.clone(),
        legal_registration: d.legal_registration.clone(),
        legal_info: d.legal_info.clone(),
        vat_id: d.tax_id("eu_vat"),
        tax_number: d.tax_id("tax_number"),
        street: address.as_ref().and_then(|a| a.street.clone()),
        additional_street: address.as_ref().and_then(|a| a.additional_street.clone()),
        city: address.as_ref().and_then(|a| a.city.clone()),
        postcode: address.as_ref().and_then(|a| a.postcode.clone()),
        country_code: address.as_ref().map(|a| a.country_code.clone()),
        contact_name: d.contact_name.clone(),
        contact_phone: d.contact_phone.clone(),
        contact_email: d.contact_email.clone(),
        electronic_address: d.electronic_address.as_ref().map(|(_, v)| v.clone()),
        electronic_address_scheme: d
            .electronic_address
            .as_ref()
            .map_or("EM".into(), |(s, _)| s.clone()),
        buyer_reference: d.invoice_reference.clone(),
        ..PartyRow::default()
    };
    // The account paid into is known by its numbers in banking, the one
    // place bank account numbers are kept.
    if let Some(account) = d.payment_account_id
        && let Some(numbers) = erpcraft_banking::account_number(&mut *conn, account).await?
    {
        row.iban = numbers.iban;
        row.bic = numbers.bic;
        row.account_name = numbers.account_name;
    }
    Ok(row)
}

/// UN/ECE Recommendation 20 for the units of measure items commonly
/// carry; "C62" (one) for anything else.
fn unit_code(uom: Option<&str>) -> &'static str {
    match uom.map(|u| u.trim().to_ascii_lowercase()).as_deref() {
        Some("pc" | "pcs" | "piece" | "pieces" | "ea" | "each" | "stk" | "stück") => "H87",
        Some("kg") => "KGM",
        Some("g") => "GRM",
        Some("t") => "TNE",
        Some("l" | "ltr") => "LTR",
        Some("m") => "MTR",
        Some("m2" | "sqm") => "MTK",
        Some("m3") => "MTQ",
        Some("h" | "hr" | "hour" | "std") => "HUR",
        Some("day" | "d") => "DAY",
        _ => "C62",
    }
}

/// What a person must set before the invoice can be made, and where.
struct Missing(Vec<String>);

impl Missing {
    fn need(&mut self, ok: bool, what: &str, where_: &str) {
        if !ok {
            self.0.push(format!("{what}: set {where_}"));
        }
    }
}

fn has(v: &Option<String>) -> bool {
    v.as_deref().is_some_and(|s| !s.trim().is_empty())
}

struct LineData {
    id: Uuid,
    line_no: i64,
    qty: Decimal,
    unit_price: Decimal,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
    tax_code_id: Option<Uuid>,
    net: Decimal,
    tax: Option<Decimal>,
}

fn decimal(v: &Value) -> Option<Decimal> {
    document::body::decimal(v)
}

/// Build the invoice of a posted sales invoice or credit note.
pub async fn build(
    ctx: &mut Ctx<'_>,
    doc_type: DocType,
    id: Uuid,
    profile: Profile,
    syntax: Syntax,
) -> Result<Invoice> {
    let view = document::get(ctx, doc_type.as_str(), id).await?;
    let doc = &view.header;
    if doc.status != "posted" {
        return Err(Error::state(format!(
            "a {} {} is not an invoice yet: only a posted one is sent",
            doc.status,
            doc_type.as_str().replace('_', " ")
        )));
    }
    let credit = doc_type == DocType::SalesCreditNote;
    let conn: &mut PgConnection = &mut *ctx.conn;
    let company_name: String = sqlx::query_scalar("SELECT name FROM company WHERE id = $1")
        .bind(doc.company_id)
        .fetch_one(&mut *conn)
        .await?;
    let base = erpcraft_kernel::company::base_currency(&mut *conn, doc.company_id).await?;
    let currency = doc.summary["currency"]
        .as_str()
        .map_or(base.clone(), str::to_owned);
    let decimals = erpcraft_gl::currency_decimals(&mut *conn, &currency).await?;
    if decimals > 2 {
        return Err(Error::validation(format!(
            "{currency} has {decimals} decimal places; EN 16931 amounts have at most two"
        )));
    }

    let seller = party_row(
        &mut *conn,
        doc.id,
        erpcraft_party::Role::Seller,
        "company",
        doc.company_id,
    )
    .await?;
    let customer_id = doc.summary["customer_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| Error::internal("the document names no customer"))?;
    let (customer_code, customer_name): (String, String) =
        sqlx::query_as("SELECT code, name FROM customer WHERE id = $1")
            .bind(customer_id)
            .fetch_one(&mut *conn)
            .await?;
    let buyer = party_row(
        &mut *conn,
        doc.id,
        erpcraft_party::Role::Buyer,
        "customer",
        customer_id,
    )
    .await?;

    // Everything the invoice needs that a person has to set, and where:
    // the party module's records of the company and the customer.
    let xr = profile == Profile::Xrechnung;
    let mut missing = Missing(Vec::new());
    let us = "the company";
    let address = |who: &str| format!("a billing address of {who} (party.set)");
    missing.need(
        has(&seller.country_code),
        "the seller's country (BT-40)",
        &address(us),
    );
    missing.need(
        has(&seller.vat_id) || has(&seller.tax_number),
        "the seller's VAT identifier or tax number (BT-31, BT-32)",
        "a tax identifier of the company, scheme eu_vat or tax_number (party.set)",
    );
    if xr {
        missing.need(has(&seller.city), "the seller's city (BT-37)", &address(us));
        missing.need(
            has(&seller.postcode),
            "the seller's post code (BT-38)",
            &address(us),
        );
        let contact = "a contact of the company with a name, telephone and email (party.set)";
        missing.need(
            has(&seller.contact_name),
            "the seller's contact (BT-41)",
            contact,
        );
        missing.need(
            has(&seller.contact_phone),
            "the seller's telephone (BT-42)",
            contact,
        );
        missing.need(
            has(&seller.contact_email),
            "the seller's contact email (BT-43)",
            contact,
        );
        missing.need(
            has(&seller.electronic_address),
            "the seller's electronic address (BT-34)",
            "an electronic address of the company (party.set)",
        );
        if !credit {
            missing.need(
                has(&seller.iban),
                "the IBAN customers pay into (BT-84)",
                "payment_account_id in the company's particulars (party.set), to a bank \
                 account whose IBAN is recorded as its bank_account_number",
            );
        }
    }
    let them = format!("customer {customer_code}");
    missing.need(
        has(&buyer.country_code),
        "the buyer's country (BT-55)",
        &address(&them),
    );
    if xr {
        missing.need(
            has(&buyer.city),
            "the buyer's city (BT-52)",
            &address(&them),
        );
        missing.need(
            has(&buyer.postcode),
            "the buyer's post code (BT-53)",
            &address(&them),
        );
        missing.need(
            has(&buyer.electronic_address),
            "the buyer's electronic address (BT-49)",
            &format!("an electronic address of {them} (party.set)"),
        );
        missing.need(
            has(&buyer.buyer_reference),
            "the buyer reference (BT-10), for a German public buyer its Leitweg-ID",
            &format!("invoice_reference in the particulars of {them} (party.set)"),
        );
    }

    // Lines, with what they are and how they are taxed.
    let mut lines_data = Vec::new();
    for line in document::body::lines(&view.body, "lines") {
        let qty = decimal(&line["qty"]).unwrap_or_default();
        let unit_price = decimal(&line["unit_price"]).unwrap_or_default();
        let discount_percent = decimal(&line["discount_percent"]).filter(|d| !d.is_zero());
        let discount_amount = decimal(&line["discount_amount"]).filter(|d| !d.is_zero());
        let net = match decimal(&line["net_amount"]) {
            Some(net) => net,
            None => {
                erpcraft_tax::line_net(qty, unit_price, discount_percent, discount_amount, decimals)
                    .ok_or_else(|| Error::internal("a credited line's discounts exceed it"))?
            }
        };
        lines_data.push(LineData {
            id: document::body::uuid(&line["id"]).unwrap_or_default(),
            line_no: line["line_no"].as_i64().unwrap_or_default(),
            qty,
            unit_price,
            discount_percent,
            discount_amount,
            tax_code_id: document::body::uuid(&line["tax_code_id"]),
            net,
            tax: decimal(&line["tax_amount"]),
        });
    }
    if lines_data.is_empty() {
        return Err(Error::validation("the document has no lines"));
    }
    let mut lines = Vec::new();
    let mut breakdown: BTreeMap<(String, String), VatBreakdown> = BTreeMap::new();
    for l in &lines_data {
        let what: Option<(String, Option<String>, Option<String>)> = if credit {
            sqlx::query_as(
                "SELECT COALESCE(i.description, o.description, it.name, ch.name, a.name, ''),
                        it.code, it.uom
                 FROM sales_credit_note_line c
                 JOIN sales_invoice_line i ON i.id = c.invoice_line_id
                 LEFT JOIN sales_order_line o ON o.id = i.order_line_id
                 LEFT JOIN item it ON it.id = o.item_id
                 LEFT JOIN charge ch ON ch.id = COALESCE(i.charge_id, o.charge_id)
                 LEFT JOIN account a ON a.id = COALESCE(i.account_id, o.account_id)
                 WHERE c.id = $1",
            )
        } else {
            sqlx::query_as(
                "SELECT COALESCE(i.description, o.description, it.name, ch.name, a.name, ''),
                        it.code, it.uom
                 FROM sales_invoice_line i
                 LEFT JOIN sales_order_line o ON o.id = i.order_line_id
                 LEFT JOIN item it ON it.id = o.item_id
                 LEFT JOIN charge ch ON ch.id = COALESCE(i.charge_id, o.charge_id)
                 LEFT JOIN account a ON a.id = COALESCE(i.account_id, o.account_id)
                 WHERE i.id = $1",
            )
        }
        .bind(l.id)
        .fetch_optional(&mut *conn)
        .await?;
        let (name, item_code, uom) = what.unwrap_or_default();

        // The VAT category and rate of the line's tax code.
        let Some(code_id) = l.tax_code_id else {
            missing.0.push(format!(
                "line {}: a VAT category and rate (BT-151, BT-152): give the line a tax code",
                l.line_no
            ));
            continue;
        };
        #[allow(clippy::type_complexity)]
        let (code, rate, parts, category, reason, reason_code, self_assessed): (
            String,
            Decimal,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        ) = sqlx::query_as(
            "SELECT c.code, c.rate,
                    (SELECT count(*) FROM tax_code_component g WHERE g.group_id = c.id),
                    t.category, t.exemption_reason, t.exemption_reason_code, c.self_assessed
             FROM tax_code c LEFT JOIN einvoice_tax_category t ON t.tax_code_id = c.id
             WHERE c.id = $1",
        )
        .bind(code_id)
        .fetch_one(&mut *conn)
        .await?;
        if parts > 0 {
            missing.0.push(format!(
                "line {}: tax code {code} is a group of several taxes; EN 16931 has one VAT \
                 rate per line",
                l.line_no
            ));
            continue;
        }
        // Tax the buyer accounts for: reverse charge, at 0 % on the invoice,
        // unless the code is named otherwise.
        let (rate, reason, reason_code) = if self_assessed {
            (
                Decimal::ZERO,
                reason.or_else(|| Some("Reverse charge".into())),
                reason_code.or_else(|| Some("VATEX-EU-AE".into())),
            )
        } else {
            (rate, reason, reason_code)
        };
        let category = match category {
            Some(c) => c,
            None if self_assessed => "AE".to_owned(),
            None if rate > Decimal::ZERO => "S".to_owned(),
            None => {
                missing.0.push(format!(
                    "line {}: tax code {code} is at 0 %, which EN 16931 must name (zero rated, \
                     exempt, reverse charge, ...): set it in an einvoice_tax_category for {code}",
                    l.line_no
                ));
                continue;
            }
        };
        let Some(tax) = l.tax else {
            return Err(Error::internal("a posted line without its tax"));
        };

        let gross = round_money(l.qty * l.unit_price, decimals);
        let mut allowances = Vec::new();
        if let Some(percent) = l.discount_percent {
            allowances.push(AllowanceCharge {
                charge: false,
                amount: round_money(gross * percent / Decimal::ONE_HUNDRED, decimals),
                base_amount: Some(gross),
                percent: Some(percent),
                reason: Some("Discount".into()),
                reason_code: Some("95".into()),
                vat_category: None,
                vat_rate: None,
            });
        }
        if let Some(amount) = l.discount_amount {
            allowances.push(AllowanceCharge {
                charge: false,
                amount,
                base_amount: None,
                percent: None,
                reason: Some("Discount".into()),
                reason_code: Some("95".into()),
                vat_category: None,
                vat_rate: None,
            });
        }
        let rate_key = rate.normalize().to_string();
        let entry = breakdown
            .entry((category.clone(), rate_key))
            .or_insert_with(|| VatBreakdown {
                category: category.clone(),
                rate: Some(rate.normalize()),
                taxable: Decimal::ZERO,
                tax: Decimal::ZERO,
                exemption_reason: reason.clone(),
                exemption_reason_code: reason_code.clone(),
            });
        entry.taxable += l.net;
        entry.tax += tax;
        lines.push(Line {
            id: l.line_no.to_string(),
            note: None,
            quantity: l.qty,
            unit: unit_code(uom.as_deref()).into(),
            net_amount: l.net,
            order_line_reference: None,
            name: if name.is_empty() {
                format!("Line {}", l.line_no)
            } else {
                name
            },
            description: None,
            seller_item_id: item_code,
            buyer_item_id: None,
            price: l.unit_price,
            base_quantity: None,
            allowances,
            vat_category: category,
            vat_rate: Some(rate.normalize()),
        });
    }
    if !missing.0.is_empty() {
        return Err(Error::validation(format!(
            "the {} cannot be sent as an e-invoice yet. Missing: {}",
            doc_type.as_str().replace('_', " "),
            missing.0.join("; ")
        )));
    }

    // Due date and terms, from the open item the document opened.
    let item: Option<(Option<NaiveDate>, Option<NaiveDate>)> = sqlx::query_as(
        "SELECT i.due_date, i.discount_date FROM open_item i JOIN posting p ON p.id = i.posting_id
         WHERE p.document_id = $1 AND p.reverses_id IS NULL LIMIT 1",
    )
    .bind(doc.id)
    .fetch_optional(&mut *conn)
    .await?;
    let (due_date, discount_date) = item.unwrap_or_default();
    let terms_id = document::body::uuid(&view.body["payment_terms_id"]);
    let terms: Option<(String, Option<Decimal>)> = match terms_id {
        Some(id) => {
            sqlx::query_as("SELECT name, discount_percent FROM payment_terms WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?
        }
        None => None,
    };
    let payment_terms = match (&terms, discount_date) {
        (Some((name, Some(percent))), Some(until)) if !credit => {
            let days = (until - doc.doc_date).num_days().max(0);
            let mut p = *percent;
            p.rescale(2);
            Some(format!("{name}\n#SKONTO#TAGE={days}#PROZENT={p}#\n"))
        }
        (Some((name, _)), _) if !credit => Some(name.clone()),
        _ => None,
    };

    // When the goods went out: the last delivery of the order by the
    // document's date.
    let order = document::body::uuid(&view.body["sales_order_id"]);
    let delivery_date: Option<NaiveDate> = match order {
        Some(order) => sqlx::query_scalar(
            "SELECT max(d.doc_date) FROM sales_delivery s JOIN document d ON d.id = s.document_id
             WHERE s.sales_order_id = $1 AND d.status = 'posted' AND d.doc_date <= $2",
        )
        .bind(order)
        .bind(doc.doc_date)
        .fetch_one(&mut *conn)
        .await?,
        None => None,
    };
    let preceding: Option<(Option<String>, NaiveDate)> = if credit {
        match document::body::uuid(&view.body["sales_invoice_id"]) {
            Some(invoice) => {
                sqlx::query_as("SELECT number, doc_date FROM document WHERE id = $1")
                    .bind(invoice)
                    .fetch_optional(&mut *conn)
                    .await?
            }
            None => None,
        }
    } else {
        None
    };

    let vat: Vec<VatBreakdown> = breakdown.into_values().collect();
    let line_total: Decimal = lines.iter().map(|l| l.net_amount).sum();
    let tax_total: Decimal = vat.iter().map(|v| v.tax).sum();
    let foreign = currency != base;
    let number = doc.number.clone().unwrap_or_default();
    let payment = if credit {
        // Money goes back to the buyer: settled between the parties.
        Some(Payment {
            means_code: "97".into(),
            remittance: Some(number.clone()),
            ..Payment::default()
        })
    } else {
        seller.iban.as_ref().map(|iban| Payment {
            means_code: "58".into(),
            remittance: Some(number.clone()),
            iban: Some(iban.clone()),
            account_name: seller.account_name.clone(),
            bic: seller.bic.clone(),
        })
    };
    Ok(Invoice {
        syntax,
        specification: match profile {
            Profile::Xrechnung => model::XRECHNUNG.into(),
            Profile::En16931 => model::EN16931.into(),
        },
        process: Some(model::PROCESS.into()),
        number,
        issue_date: doc.doc_date,
        type_code: if credit { "381" } else { "380" }.into(),
        currency: currency.clone(),
        tax_currency: foreign.then(|| base.clone()),
        due_date: if credit { None } else { due_date },
        buyer_reference: buyer.buyer_reference.clone(),
        order_reference: None,
        preceding_invoice: preceding.as_ref().and_then(|(n, _)| n.clone()),
        preceding_invoice_date: preceding.map(|(_, d)| d),
        notes: doc.memo.iter().cloned().collect(),
        delivery_date,
        seller: Party {
            name: seller.legal_name.clone().unwrap_or(company_name),
            trading_name: seller.trading_name.clone(),
            identifier: None,
            legal_registration: seller.legal_registration.clone(),
            vat_id: seller.vat_id.clone(),
            tax_number: seller.tax_number.clone(),
            legal_info: seller.legal_info.clone(),
            street: seller.street.clone(),
            additional_street: seller.additional_street.clone(),
            city: seller.city.clone(),
            postcode: seller.postcode.clone(),
            country: seller.country_code.clone(),
            electronic_address: seller.electronic_address.clone(),
            electronic_address_scheme: Some(seller.electronic_address_scheme.clone()),
            contact_name: seller.contact_name.clone(),
            contact_phone: seller.contact_phone.clone(),
            contact_email: seller.contact_email.clone(),
        },
        buyer: Party {
            name: buyer.legal_name.clone().unwrap_or(customer_name),
            trading_name: None,
            identifier: Some(customer_code),
            legal_registration: buyer.legal_registration.clone(),
            vat_id: buyer.vat_id.clone(),
            tax_number: None,
            legal_info: None,
            street: buyer.street.clone(),
            additional_street: buyer.additional_street.clone(),
            city: buyer.city.clone(),
            postcode: buyer.postcode.clone(),
            country: buyer.country_code.clone(),
            electronic_address: buyer.electronic_address.clone(),
            electronic_address_scheme: Some(buyer.electronic_address_scheme.clone()),
            contact_name: buyer.contact_name.clone(),
            contact_phone: buyer.contact_phone.clone(),
            contact_email: buyer.contact_email.clone(),
        },
        payment,
        payment_terms,
        allowances: vec![],
        lines,
        vat,
        totals: Totals {
            line_total,
            allowance_total: None,
            charge_total: None,
            tax_exclusive: line_total,
            tax_total,
            tax_total_accounting: if foreign {
                decimal(&doc.summary["total_tax_accounting"])
            } else {
                None
            },
            tax_inclusive: line_total + tax_total,
            prepaid: None,
            rounding: None,
            payable: line_total + tax_total,
        },
    })
}

/// What `einvoice.export` returns.
#[derive(Serialize, JsonSchema)]
pub struct ExportOutput {
    /// A file name for the XML: the document number and the syntax.
    pub filename: String,
    /// `application/xml`.
    pub media_type: String,
    pub syntax: Syntax,
    /// The specification identifier (BT-24) the invoice claims.
    pub specification: String,
    /// The invoice itself.
    pub xml: String,
    /// Whether every rule checked here holds. The official validator
    /// checks more; see the module's README.
    pub valid: bool,
    /// Advice: rules that do not make the invoice invalid, such as a
    /// missing delivery date.
    pub findings: Vec<Finding>,
}

/// Write the invoice, read it back, and check what was read.
pub fn render(invoice: &Invoice) -> Result<ExportOutput> {
    let xml = match invoice.syntax {
        Syntax::Ubl => crate::ubl::write(invoice),
        Syntax::Cii => crate::cii::write(invoice),
    };
    let reread = crate::read_xml(&xml)
        .map_err(|e| Error::internal(format!("the invoice written could not be read back: {e}")))?;
    let findings = model::check(&reread);
    let fatal: Vec<String> = findings
        .iter()
        .filter(|f| f.fatal)
        .map(|f| format!("[{}] {}", f.rule, f.message))
        .collect();
    if !fatal.is_empty() {
        return Err(Error::validation(format!(
            "the invoice would break rules of the standard: {}",
            fatal.join("; ")
        )));
    }
    let safe: String = invoice
        .number
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(ExportOutput {
        filename: format!("{safe}-{}.xml", invoice.syntax.as_str()),
        media_type: "application/xml".into(),
        syntax: invoice.syntax,
        specification: invoice.specification.clone(),
        xml,
        valid: true,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    pub(crate) fn sample() -> Invoice {
        let party = |name: &str| Party {
            name: name.into(),
            vat_id: Some("DE123456789".into()),
            city: Some("München".into()),
            postcode: Some("80331".into()),
            country: Some("DE".into()),
            electronic_address: Some("a@b.example".into()),
            electronic_address_scheme: Some("EM".into()),
            contact_name: Some("Buchhaltung".into()),
            contact_phone: Some("+49 89 123".into()),
            contact_email: Some("a@b.example".into()),
            ..Party::default()
        };
        Invoice {
            syntax: Syntax::Ubl,
            specification: model::XRECHNUNG.into(),
            process: Some(model::PROCESS.into()),
            number: "SI-1".into(),
            issue_date: "2026-04-03".parse().unwrap(),
            type_code: "380".into(),
            currency: "EUR".into(),
            tax_currency: None,
            due_date: Some("2026-05-03".parse().unwrap()),
            buyer_reference: Some("04011000-12345-03".into()),
            order_reference: None,
            preceding_invoice: None,
            preceding_invoice_date: None,
            notes: vec![],
            delivery_date: Some("2026-04-02".parse().unwrap()),
            seller: party("Muster GmbH"),
            buyer: party("Kunde AG"),
            payment: Some(Payment {
                means_code: "58".into(),
                remittance: Some("SI-1".into()),
                iban: Some("DE89370400440532013000".into()),
                account_name: None,
                bic: Some("COBADEFFXXX".into()),
            }),
            payment_terms: Some("30 Tage\n#SKONTO#TAGE=14#PROZENT=2.00#\n".into()),
            allowances: vec![],
            lines: vec![Line {
                id: "1".into(),
                note: None,
                quantity: d("10"),
                unit: "H87".into(),
                net_amount: d("225.00"),
                order_line_reference: None,
                name: "Widget".into(),
                description: None,
                seller_item_id: Some("WIDGET".into()),
                buyer_item_id: None,
                price: d("25.00"),
                base_quantity: None,
                allowances: vec![AllowanceCharge {
                    charge: false,
                    amount: d("25.00"),
                    base_amount: Some(d("250.00")),
                    percent: Some(d("10")),
                    reason: Some("Discount".into()),
                    reason_code: Some("95".into()),
                    vat_category: None,
                    vat_rate: None,
                }],
                vat_category: "S".into(),
                vat_rate: Some(d("19")),
            }],
            vat: vec![VatBreakdown {
                category: "S".into(),
                rate: Some(d("19")),
                taxable: d("225.00"),
                tax: d("42.75"),
                exemption_reason: None,
                exemption_reason_code: None,
            }],
            totals: Totals {
                line_total: d("225.00"),
                tax_exclusive: d("225.00"),
                tax_total: d("42.75"),
                tax_inclusive: d("267.75"),
                payable: d("267.75"),
                ..Totals::default()
            },
        }
    }

    #[test]
    fn written_and_read_back_in_both_syntaxes() {
        for syntax in [Syntax::Ubl, Syntax::Cii] {
            let mut invoice = sample();
            invoice.syntax = syntax;
            let out = render(&invoice).unwrap_or_else(|e| panic!("{syntax:?}: {e:?}"));
            let back = crate::read_xml(&out.xml).unwrap();
            assert_eq!(back.totals.payable, d("267.75"));
            assert_eq!(back.lines[0].allowances[0].percent, Some(d("10")));
            assert_eq!(
                back.payment_terms.as_deref(),
                invoice.payment_terms.as_deref().map(str::trim)
            );
            assert!(out.xml.contains("PROZENT=2.00#\n<"));
        }
        let mut credit = sample();
        credit.type_code = "381".into();
        credit.preceding_invoice = Some("SI-0".into());
        let out = render(&credit).unwrap();
        assert!(out.xml.contains("<ubl:CreditNote"));
        assert_eq!(
            crate::read_xml(&out.xml)
                .unwrap()
                .preceding_invoice
                .as_deref(),
            Some("SI-0")
        );
    }
}
