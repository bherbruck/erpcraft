//! EN 16931 in OASIS UBL 2.1 syntax: an `Invoice`, or a `CreditNote` for
//! type code 381, in the element order the UBL 2.1 schemas require, as the
//! syntax binding (CEN/TS 16931-3-2) and XRechnung map the business terms.

use chrono::NaiveDate;
use erpcraft_kernel::Decimal;

use crate::model::{
    Address, AllowanceCharge, Invoice, Line, Party, Payment, Syntax, Totals, VatBreakdown,
};
use crate::xml::{Node, Writer};

pub const INVOICE_NS: &str = "urn:oasis:names:specification:ubl:schema:xsd:Invoice-2";
pub const CREDIT_NOTE_NS: &str = "urn:oasis:names:specification:ubl:schema:xsd:CreditNote-2";
const CAC: &str = "urn:oasis:names:specification:ubl:schema:xsd:CommonAggregateComponents-2";
const CBC: &str = "urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2";

/// An amount with two decimals, as BR-DEC-* require.
pub fn amount(d: Decimal) -> String {
    let mut d = d.round_dp_with_strategy(2, rust_decimal::RoundingStrategy::MidpointAwayFromZero);
    d.rescale(2);
    d.to_string()
}

/// A quantity, price or rate: as exact as it is, without trailing zeros.
pub fn number(d: Decimal) -> String {
    let d = d.normalize();
    if d.is_zero() {
        "0".into()
    } else {
        d.to_string()
    }
}

fn date(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn party(w: &mut Writer, p: &Party, seller: bool) {
    w.open("cac:Party", &[]);
    if let Some(address) = &p.electronic_address {
        let scheme = p.electronic_address_scheme.as_deref().unwrap_or("EM");
        w.leaf("cbc:EndpointID", &[("schemeID", scheme)], address);
    }
    if let Some(id) = &p.identifier {
        w.open("cac:PartyIdentification", &[]);
        w.leaf("cbc:ID", &[], id);
        w.close();
    }
    if let Some(name) = &p.trading_name {
        w.open("cac:PartyName", &[]);
        w.leaf("cbc:Name", &[], name);
        w.close();
    }
    w.open("cac:PostalAddress", &[]);
    w.opt("cbc:StreetName", p.street.as_deref());
    w.opt("cbc:AdditionalStreetName", p.additional_street.as_deref());
    w.opt("cbc:CityName", p.city.as_deref());
    w.opt("cbc:PostalZone", p.postcode.as_deref());
    if let Some(country) = &p.country {
        w.open("cac:Country", &[]);
        w.leaf("cbc:IdentificationCode", &[], country);
        w.close();
    }
    w.close();
    for (id, scheme) in [(&p.vat_id, "VAT"), (&p.tax_number, "FC")] {
        if let Some(id) = id
            && (scheme == "VAT" || seller)
        {
            w.open("cac:PartyTaxScheme", &[]);
            w.leaf("cbc:CompanyID", &[], id);
            w.open("cac:TaxScheme", &[]);
            w.leaf("cbc:ID", &[], scheme);
            w.close();
            w.close();
        }
    }
    w.open("cac:PartyLegalEntity", &[]);
    w.leaf("cbc:RegistrationName", &[], &p.name);
    w.opt("cbc:CompanyID", p.legal_registration.as_deref());
    if seller {
        w.opt("cbc:CompanyLegalForm", p.legal_info.as_deref());
    }
    w.close();
    if p.contact_name.is_some() || p.contact_phone.is_some() || p.contact_email.is_some() {
        w.open("cac:Contact", &[]);
        w.opt("cbc:Name", p.contact_name.as_deref());
        w.opt("cbc:Telephone", p.contact_phone.as_deref());
        w.opt("cbc:ElectronicMail", p.contact_email.as_deref());
        w.close();
    }
    w.close();
}

fn allowance(w: &mut Writer, a: &AllowanceCharge, currency: &str) {
    w.open("cac:AllowanceCharge", &[]);
    w.leaf(
        "cbc:ChargeIndicator",
        &[],
        if a.charge { "true" } else { "false" },
    );
    w.opt("cbc:AllowanceChargeReasonCode", a.reason_code.as_deref());
    w.opt("cbc:AllowanceChargeReason", a.reason.as_deref());
    if let Some(percent) = a.percent {
        w.leaf("cbc:MultiplierFactorNumeric", &[], &number(percent));
    }
    w.leaf("cbc:Amount", &[("currencyID", currency)], &amount(a.amount));
    if let Some(base) = a.base_amount {
        w.leaf("cbc:BaseAmount", &[("currencyID", currency)], &amount(base));
    }
    if let Some(category) = &a.vat_category {
        w.open("cac:TaxCategory", &[]);
        w.leaf("cbc:ID", &[], category);
        if let Some(rate) = a.vat_rate {
            w.leaf("cbc:Percent", &[], &number(rate));
        }
        w.open("cac:TaxScheme", &[]);
        w.leaf("cbc:ID", &[], "VAT");
        w.close();
        w.close();
    }
    w.close();
}

fn line(w: &mut Writer, l: &Line, credit: bool, currency: &str) {
    w.open(
        if credit {
            "cac:CreditNoteLine"
        } else {
            "cac:InvoiceLine"
        },
        &[],
    );
    w.leaf("cbc:ID", &[], &l.id);
    w.opt("cbc:Note", l.note.as_deref());
    w.leaf(
        if credit {
            "cbc:CreditedQuantity"
        } else {
            "cbc:InvoicedQuantity"
        },
        &[("unitCode", &l.unit)],
        &number(l.quantity),
    );
    w.leaf(
        "cbc:LineExtensionAmount",
        &[("currencyID", currency)],
        &amount(l.net_amount),
    );
    if let Some(reference) = &l.order_line_reference {
        w.open("cac:OrderLineReference", &[]);
        w.leaf("cbc:LineID", &[], reference);
        w.close();
    }
    for a in &l.allowances {
        allowance(w, a, currency);
    }
    w.open("cac:Item", &[]);
    w.opt("cbc:Description", l.description.as_deref());
    w.leaf("cbc:Name", &[], &l.name);
    if let Some(id) = &l.buyer_item_id {
        w.open("cac:BuyersItemIdentification", &[]);
        w.leaf("cbc:ID", &[], id);
        w.close();
    }
    if let Some(id) = &l.seller_item_id {
        w.open("cac:SellersItemIdentification", &[]);
        w.leaf("cbc:ID", &[], id);
        w.close();
    }
    w.open("cac:ClassifiedTaxCategory", &[]);
    w.leaf("cbc:ID", &[], &l.vat_category);
    if let Some(rate) = l.vat_rate {
        w.leaf("cbc:Percent", &[], &number(rate));
    }
    w.open("cac:TaxScheme", &[]);
    w.leaf("cbc:ID", &[], "VAT");
    w.close();
    w.close();
    w.close();
    w.open("cac:Price", &[]);
    w.leaf(
        "cbc:PriceAmount",
        &[("currencyID", currency)],
        &number(l.price),
    );
    if let Some(base) = l.base_quantity {
        w.leaf("cbc:BaseQuantity", &[("unitCode", &l.unit)], &number(base));
    }
    w.close();
    w.close();
}

/// Write an invoice, or a credit note, in UBL 2.1.
pub fn write(inv: &Invoice) -> String {
    let credit = inv.is_credit_note();
    let cur = inv.currency.as_str();
    let mut w = Writer::new();
    let (root, ns) = if credit {
        ("ubl:CreditNote", CREDIT_NOTE_NS)
    } else {
        ("ubl:Invoice", INVOICE_NS)
    };
    w.open(
        root,
        &[("xmlns:ubl", ns), ("xmlns:cac", CAC), ("xmlns:cbc", CBC)],
    );
    w.leaf("cbc:CustomizationID", &[], &inv.specification);
    w.opt("cbc:ProfileID", inv.process.as_deref());
    w.leaf("cbc:ID", &[], &inv.number);
    w.leaf("cbc:IssueDate", &[], &date(inv.issue_date));
    if !credit && let Some(due) = inv.due_date {
        w.leaf("cbc:DueDate", &[], &date(due));
    }
    w.leaf(
        if credit {
            "cbc:CreditNoteTypeCode"
        } else {
            "cbc:InvoiceTypeCode"
        },
        &[],
        &inv.type_code,
    );
    for note in &inv.notes {
        w.leaf("cbc:Note", &[], note);
    }
    w.leaf("cbc:DocumentCurrencyCode", &[], cur);
    w.opt("cbc:TaxCurrencyCode", inv.tax_currency.as_deref());
    w.opt("cbc:BuyerReference", inv.buyer_reference.as_deref());
    if inv.period_start.is_some() || inv.period_end.is_some() {
        w.open("cac:InvoicePeriod", &[]);
        if let Some(d) = inv.period_start {
            w.leaf("cbc:StartDate", &[], &date(d));
        }
        if let Some(d) = inv.period_end {
            w.leaf("cbc:EndDate", &[], &date(d));
        }
        w.close();
    }
    if let Some(order) = &inv.order_reference {
        w.open("cac:OrderReference", &[]);
        w.leaf("cbc:ID", &[], order);
        w.close();
    }
    if let Some(preceding) = &inv.preceding_invoice {
        w.open("cac:BillingReference", &[]);
        w.open("cac:InvoiceDocumentReference", &[]);
        w.leaf("cbc:ID", &[], preceding);
        if let Some(d) = inv.preceding_invoice_date {
            w.leaf("cbc:IssueDate", &[], &date(d));
        }
        w.close();
        w.close();
    }
    w.open("cac:AccountingSupplierParty", &[]);
    party(&mut w, &inv.seller, true);
    w.close();
    w.open("cac:AccountingCustomerParty", &[]);
    party(&mut w, &inv.buyer, false);
    w.close();
    if inv.delivery_date.is_some() || inv.delivery_address.is_some() {
        w.open("cac:Delivery", &[]);
        if let Some(d) = inv.delivery_date {
            w.leaf("cbc:ActualDeliveryDate", &[], &date(d));
        }
        if let Some(a) = &inv.delivery_address {
            w.open("cac:DeliveryLocation", &[]);
            w.open("cac:Address", &[]);
            w.opt("cbc:StreetName", a.street.as_deref());
            w.opt("cbc:AdditionalStreetName", a.additional_street.as_deref());
            w.opt("cbc:CityName", a.city.as_deref());
            w.opt("cbc:PostalZone", a.postcode.as_deref());
            w.open("cac:Country", &[]);
            w.leaf("cbc:IdentificationCode", &[], &a.country);
            w.close();
            w.close();
            w.close();
        }
        w.close();
    }
    if let Some(p) = &inv.payment {
        w.open("cac:PaymentMeans", &[]);
        w.leaf("cbc:PaymentMeansCode", &[], &p.means_code);
        if credit && let Some(due) = inv.due_date {
            w.leaf("cbc:PaymentDueDate", &[], &date(due));
        }
        w.opt("cbc:PaymentID", p.remittance.as_deref());
        if let Some(iban) = &p.iban {
            w.open("cac:PayeeFinancialAccount", &[]);
            w.leaf("cbc:ID", &[], iban);
            w.opt("cbc:Name", p.account_name.as_deref());
            if let Some(bic) = &p.bic {
                w.open("cac:FinancialInstitutionBranch", &[]);
                w.leaf("cbc:ID", &[], bic);
                w.close();
            }
            w.close();
        }
        w.close();
    }
    if let Some(terms) = &inv.payment_terms {
        w.open("cac:PaymentTerms", &[]);
        w.leaf("cbc:Note", &[], terms);
        w.close();
    }
    for a in &inv.allowances {
        allowance(&mut w, a, cur);
    }
    w.open("cac:TaxTotal", &[]);
    w.leaf(
        "cbc:TaxAmount",
        &[("currencyID", cur)],
        &amount(inv.totals.tax_total),
    );
    for v in &inv.vat {
        w.open("cac:TaxSubtotal", &[]);
        w.leaf(
            "cbc:TaxableAmount",
            &[("currencyID", cur)],
            &amount(v.taxable),
        );
        w.leaf("cbc:TaxAmount", &[("currencyID", cur)], &amount(v.tax));
        w.open("cac:TaxCategory", &[]);
        w.leaf("cbc:ID", &[], &v.category);
        if let Some(rate) = v.rate {
            w.leaf("cbc:Percent", &[], &number(rate));
        }
        w.opt(
            "cbc:TaxExemptionReasonCode",
            v.exemption_reason_code.as_deref(),
        );
        w.opt("cbc:TaxExemptionReason", v.exemption_reason.as_deref());
        w.open("cac:TaxScheme", &[]);
        w.leaf("cbc:ID", &[], "VAT");
        w.close();
        w.close();
        w.close();
    }
    w.close();
    if let (Some(tax_currency), Some(accounting)) =
        (&inv.tax_currency, inv.totals.tax_total_accounting)
    {
        w.open("cac:TaxTotal", &[]);
        w.leaf(
            "cbc:TaxAmount",
            &[("currencyID", tax_currency)],
            &amount(accounting),
        );
        w.close();
    }
    let t = &inv.totals;
    w.open("cac:LegalMonetaryTotal", &[]);
    w.leaf(
        "cbc:LineExtensionAmount",
        &[("currencyID", cur)],
        &amount(t.line_total),
    );
    w.leaf(
        "cbc:TaxExclusiveAmount",
        &[("currencyID", cur)],
        &amount(t.tax_exclusive),
    );
    w.leaf(
        "cbc:TaxInclusiveAmount",
        &[("currencyID", cur)],
        &amount(t.tax_inclusive),
    );
    for (name, value) in [
        ("cbc:AllowanceTotalAmount", t.allowance_total),
        ("cbc:ChargeTotalAmount", t.charge_total),
        ("cbc:PrepaidAmount", t.prepaid),
        ("cbc:PayableRoundingAmount", t.rounding),
    ] {
        if let Some(value) = value {
            w.leaf(name, &[("currencyID", cur)], &amount(value));
        }
    }
    w.leaf(
        "cbc:PayableAmount",
        &[("currencyID", cur)],
        &amount(t.payable),
    );
    w.close();
    for l in &inv.lines {
        line(&mut w, l, credit, cur);
    }
    w.finish()
}

// ------------------------------------------------------------------ read

fn decimal(node: Option<&Node>, what: &str) -> Result<Decimal, String> {
    let node = node.ok_or_else(|| format!("{what} is missing"))?;
    crate::model::bounded(&node.text)
        .map_err(|why| format!("line {}: {what} '{}' {why}", node.line, node.text))
}

fn opt_decimal(node: Option<&Node>, what: &str) -> Result<Option<Decimal>, String> {
    match node {
        Some(n) if !n.text.is_empty() => decimal(Some(n), what).map(Some),
        _ => Ok(None),
    }
}

fn read_date(node: Option<&Node>, what: &str) -> Result<Option<NaiveDate>, String> {
    match node {
        Some(n) if !n.text.is_empty() => xs_date(&n.text)
            .map(Some)
            .ok_or_else(|| format!("line {}: {what} '{}' is not a date", n.line, n.text)),
        _ => Ok(None),
    }
}

/// An `xs:date`: `2026-04-05`, optionally with a time zone, `Z` or an
/// offset such as `+02:00`. The zone does not change the day.
pub fn xs_date(text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    let (day, zone) = text.split_at_checked(10)?;
    let zone_ok = zone.is_empty()
        || zone == "Z"
        || (zone.len() == 6
            && matches!(zone.as_bytes()[0], b'+' | b'-')
            && zone.as_bytes()[3] == b':'
            && zone[1..3].parse::<u8>().is_ok_and(|h| h <= 14)
            && zone[4..6].parse::<u8>().is_ok_and(|m| m < 60));
    if !zone_ok {
        return None;
    }
    NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()
}

fn read_party(node: &Node) -> Party {
    let p = node.child("Party").unwrap_or(node);
    let address = p.child("PostalAddress");
    let tax = |scheme: &str| {
        p.all("PartyTaxScheme")
            .find(|t| t.text_at(&["TaxScheme", "ID"]).as_deref() == Some(scheme))
            .and_then(|t| t.text_at(&["CompanyID"]))
    };
    Party {
        name: p
            .text_at(&["PartyLegalEntity", "RegistrationName"])
            .or_else(|| p.text_at(&["PartyName", "Name"]))
            .unwrap_or_default(),
        trading_name: p.text_at(&["PartyName", "Name"]),
        identifier: p.text_at(&["PartyIdentification", "ID"]),
        legal_registration: p.text_at(&["PartyLegalEntity", "CompanyID"]),
        vat_id: tax("VAT"),
        tax_number: tax("FC"),
        legal_info: p.text_at(&["PartyLegalEntity", "CompanyLegalForm"]),
        street: address.and_then(|a| a.text_at(&["StreetName"])),
        additional_street: address.and_then(|a| a.text_at(&["AdditionalStreetName"])),
        city: address.and_then(|a| a.text_at(&["CityName"])),
        postcode: address.and_then(|a| a.text_at(&["PostalZone"])),
        country: address.and_then(|a| a.text_at(&["Country", "IdentificationCode"])),
        electronic_address: p.text_at(&["EndpointID"]),
        electronic_address_scheme: p
            .child("EndpointID")
            .and_then(|e| e.attr("schemeID"))
            .map(str::to_owned),
        contact_name: p.text_at(&["Contact", "Name"]),
        contact_phone: p.text_at(&["Contact", "Telephone"]),
        contact_email: p.text_at(&["Contact", "ElectronicMail"]),
    }
}

fn read_allowance(a: &Node) -> Result<AllowanceCharge, String> {
    Ok(AllowanceCharge {
        charge: a.text_at(&["ChargeIndicator"]).as_deref() == Some("true"),
        amount: decimal(a.child("Amount"), "an allowance or charge amount")?,
        base_amount: opt_decimal(a.child("BaseAmount"), "a base amount")?,
        percent: opt_decimal(a.child("MultiplierFactorNumeric"), "a percentage")?,
        reason: a.text_at(&["AllowanceChargeReason"]),
        reason_code: a.text_at(&["AllowanceChargeReasonCode"]),
        vat_category: a.text_at(&["TaxCategory", "ID"]),
        vat_rate: opt_decimal(a.at(&["TaxCategory", "Percent"]), "a VAT rate")?,
    })
}

/// Read a UBL `Invoice` or `CreditNote` into the semantic model.
pub fn read(root: &Node) -> Result<Invoice, String> {
    let credit = root.name == "CreditNote";
    let line_name = if credit {
        "CreditNoteLine"
    } else {
        "InvoiceLine"
    };
    let mut lines = Vec::new();
    for l in root.all(line_name) {
        let quantity_node = l.child(if credit {
            "CreditedQuantity"
        } else {
            "InvoicedQuantity"
        });
        let item = l.child("Item");
        lines.push(Line {
            id: l.text_at(&["ID"]).unwrap_or_default(),
            note: l.text_at(&["Note"]),
            quantity: decimal(quantity_node, "a line's quantity (BT-129)")?,
            unit: quantity_node
                .and_then(|q| q.attr("unitCode"))
                .unwrap_or_default()
                .to_owned(),
            net_amount: decimal(
                l.child("LineExtensionAmount"),
                "a line's net amount (BT-131)",
            )?,
            order_line_reference: l.text_at(&["OrderLineReference", "LineID"]),
            name: item.and_then(|i| i.text_at(&["Name"])).unwrap_or_default(),
            description: item.and_then(|i| i.text_at(&["Description"])),
            seller_item_id: item.and_then(|i| i.text_at(&["SellersItemIdentification", "ID"])),
            buyer_item_id: item.and_then(|i| i.text_at(&["BuyersItemIdentification", "ID"])),
            price: decimal(l.at(&["Price", "PriceAmount"]), "a line's price (BT-146)")?,
            base_quantity: opt_decimal(l.at(&["Price", "BaseQuantity"]), "a base quantity")?,
            allowances: l
                .all("AllowanceCharge")
                .map(read_allowance)
                .collect::<Result<_, _>>()?,
            vat_category: item
                .and_then(|i| i.text_at(&["ClassifiedTaxCategory", "ID"]))
                .unwrap_or_default(),
            vat_rate: opt_decimal(
                item.and_then(|i| i.at(&["ClassifiedTaxCategory", "Percent"])),
                "a line's VAT rate (BT-152)",
            )?,
        });
    }
    let currency = root.text_at(&["DocumentCurrencyCode"]).unwrap_or_default();
    let tax_currency = root.text_at(&["TaxCurrencyCode"]);
    let mut vat = Vec::new();
    let mut tax_total = None;
    let mut tax_total_accounting = None;
    for total in root.all("TaxTotal") {
        let amount_node = total.child("TaxAmount");
        let in_currency = amount_node.and_then(|a| a.attr("currencyID"));
        if total.child("TaxSubtotal").is_some() || in_currency == Some(currency.as_str()) {
            tax_total = Some(decimal(amount_node, "the VAT total (BT-110)")?);
            for s in total.all("TaxSubtotal") {
                vat.push(VatBreakdown {
                    category: s.text_at(&["TaxCategory", "ID"]).unwrap_or_default(),
                    rate: opt_decimal(s.at(&["TaxCategory", "Percent"]), "a VAT rate (BT-119)")?,
                    taxable: decimal(s.child("TaxableAmount"), "a taxable amount (BT-116)")?,
                    tax: decimal(s.child("TaxAmount"), "a VAT category amount (BT-117)")?,
                    exemption_reason: s.text_at(&["TaxCategory", "TaxExemptionReason"]),
                    exemption_reason_code: s.text_at(&["TaxCategory", "TaxExemptionReasonCode"]),
                });
            }
        } else {
            tax_total_accounting =
                opt_decimal(amount_node, "the VAT total in accounting currency (BT-111)")?;
        }
    }
    let m = root
        .child("LegalMonetaryTotal")
        .ok_or("the document totals (BG-22) are missing")?;
    let totals = Totals {
        line_total: decimal(
            m.child("LineExtensionAmount"),
            "the sum of line net amounts (BT-106)",
        )?,
        allowance_total: opt_decimal(m.child("AllowanceTotalAmount"), "BT-107")?,
        charge_total: opt_decimal(m.child("ChargeTotalAmount"), "BT-108")?,
        tax_exclusive: decimal(
            m.child("TaxExclusiveAmount"),
            "the total without VAT (BT-109)",
        )?,
        tax_total: tax_total.ok_or("the VAT total (BT-110) is missing")?,
        tax_total_accounting,
        tax_inclusive: decimal(m.child("TaxInclusiveAmount"), "the total with VAT (BT-112)")?,
        prepaid: opt_decimal(m.child("PrepaidAmount"), "BT-113")?,
        rounding: opt_decimal(m.child("PayableRoundingAmount"), "BT-114")?,
        payable: decimal(m.child("PayableAmount"), "the amount due (BT-115)")?,
    };
    let means = root.child("PaymentMeans");
    let payment = means.map(|p| Payment {
        means_code: p.text_at(&["PaymentMeansCode"]).unwrap_or_default(),
        remittance: p.text_at(&["PaymentID"]),
        iban: p.text_at(&["PayeeFinancialAccount", "ID"]),
        account_name: p.text_at(&["PayeeFinancialAccount", "Name"]),
        bic: p.text_at(&["PayeeFinancialAccount", "FinancialInstitutionBranch", "ID"]),
    });
    let due = read_date(root.child("DueDate"), "the due date (BT-9)")?.or(read_date(
        means.and_then(|p| p.child("PaymentDueDate")),
        "the due date (BT-9)",
    )?);
    Ok(Invoice {
        syntax: Syntax::Ubl,
        specification: root.text_at(&["CustomizationID"]).unwrap_or_default(),
        process: root.text_at(&["ProfileID"]),
        number: root.text_at(&["ID"]).unwrap_or_default(),
        issue_date: read_date(root.child("IssueDate"), "the issue date (BT-2)")?
            .ok_or("the issue date (BT-2) is missing")?,
        type_code: root
            .text_at(&[if credit {
                "CreditNoteTypeCode"
            } else {
                "InvoiceTypeCode"
            }])
            .unwrap_or_default(),
        currency,
        tax_currency,
        due_date: due,
        buyer_reference: root.text_at(&["BuyerReference"]),
        order_reference: root.text_at(&["OrderReference", "ID"]),
        preceding_invoice: root.text_at(&["BillingReference", "InvoiceDocumentReference", "ID"]),
        preceding_invoice_date: read_date(
            root.at(&["BillingReference", "InvoiceDocumentReference", "IssueDate"]),
            "the preceding invoice's date (BT-26)",
        )?,
        notes: root.all("Note").map(|n| n.text.clone()).collect(),
        delivery_date: read_date(
            root.at(&["Delivery", "ActualDeliveryDate"]),
            "the delivery date (BT-72)",
        )?,
        period_start: read_date(
            root.at(&["InvoicePeriod", "StartDate"]),
            "the invoicing period's start (BT-73)",
        )?,
        period_end: read_date(
            root.at(&["InvoicePeriod", "EndDate"]),
            "the invoicing period's end (BT-74)",
        )?,
        delivery_address: root
            .at(&["Delivery", "DeliveryLocation", "Address"])
            .map(|a| Address {
                street: a.text_at(&["StreetName"]),
                additional_street: a.text_at(&["AdditionalStreetName"]),
                city: a.text_at(&["CityName"]),
                postcode: a.text_at(&["PostalZone"]),
                country: a
                    .text_at(&["Country", "IdentificationCode"])
                    .unwrap_or_default(),
            }),
        seller: root
            .child("AccountingSupplierParty")
            .map(read_party)
            .unwrap_or_default(),
        buyer: root
            .child("AccountingCustomerParty")
            .map(read_party)
            .unwrap_or_default(),
        payment,
        payment_terms: root.text_at(&["PaymentTerms", "Note"]),
        allowances: root
            .all("AllowanceCharge")
            .map(read_allowance)
            .collect::<Result<_, _>>()?,
        lines,
        vat,
        totals,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates_with_a_time_zone() {
        for ok in [
            "2026-04-05",
            "2026-04-05Z",
            "2026-04-05+02:00",
            "2026-04-05-05:00",
        ] {
            assert_eq!(
                super::xs_date(ok).unwrap().to_string(),
                "2026-04-05",
                "{ok}"
            );
        }
        for bad in [
            "2026-04-05T10:00",
            "2026-4-5",
            "2026-04-05+2",
            "2026-04-05+25:00",
            "",
        ] {
            assert!(super::xs_date(bad).is_none(), "{bad}");
        }
    }
}
