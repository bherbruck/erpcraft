//! EN 16931 in UN/CEFACT Cross Industry Invoice syntax (D16B, as the
//! syntax binding CEN/TS 16931-3-3, XRechnung and ZUGFeRD / Factur-X use
//! it), in the element order of the schema.

use chrono::NaiveDate;
use erpcraft_kernel::Decimal;

use crate::model::{
    Address, AllowanceCharge, Invoice, Line, Party, Payment, Syntax, Totals, VatBreakdown,
};
use crate::ubl::{amount, number};
use crate::xml::{Node, Writer};

pub const RSM: &str = "urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100";
const RAM: &str =
    "urn:un:unece:uncefact:data:standard:ReusableAggregateBusinessInformationEntity:100";
const QDT: &str = "urn:un:unece:uncefact:data:standard:QualifiedDataType:100";
const UDT: &str = "urn:un:unece:uncefact:data:standard:UnqualifiedDataType:100";

fn date_time(w: &mut Writer, outer: &str, d: NaiveDate) {
    w.open(outer, &[]);
    w.leaf(
        "udt:DateTimeString",
        &[("format", "102")],
        &d.format("%Y%m%d").to_string(),
    );
    w.close();
}

fn party(w: &mut Writer, name: &str, p: &Party, seller: bool) {
    w.open(name, &[]);
    w.opt("ram:ID", p.identifier.as_deref());
    w.leaf("ram:Name", &[], &p.name);
    if seller {
        w.opt("ram:Description", p.legal_info.as_deref());
    }
    if p.legal_registration.is_some() || p.trading_name.is_some() {
        w.open("ram:SpecifiedLegalOrganization", &[]);
        w.opt("ram:ID", p.legal_registration.as_deref());
        w.opt("ram:TradingBusinessName", p.trading_name.as_deref());
        w.close();
    }
    if p.contact_name.is_some() || p.contact_phone.is_some() || p.contact_email.is_some() {
        w.open("ram:DefinedTradeContact", &[]);
        w.opt("ram:PersonName", p.contact_name.as_deref());
        if let Some(phone) = &p.contact_phone {
            w.open("ram:TelephoneUniversalCommunication", &[]);
            w.leaf("ram:CompleteNumber", &[], phone);
            w.close();
        }
        if let Some(email) = &p.contact_email {
            w.open("ram:EmailURIUniversalCommunication", &[]);
            w.leaf("ram:URIID", &[], email);
            w.close();
        }
        w.close();
    }
    w.open("ram:PostalTradeAddress", &[]);
    w.opt("ram:PostcodeCode", p.postcode.as_deref());
    w.opt("ram:LineOne", p.street.as_deref());
    w.opt("ram:LineTwo", p.additional_street.as_deref());
    w.opt("ram:CityName", p.city.as_deref());
    w.opt("ram:CountryID", p.country.as_deref());
    w.close();
    if let Some(address) = &p.electronic_address {
        w.open("ram:URIUniversalCommunication", &[]);
        let scheme = p.electronic_address_scheme.as_deref().unwrap_or("EM");
        w.leaf("ram:URIID", &[("schemeID", scheme)], address);
        w.close();
    }
    for (id, scheme) in [(&p.vat_id, "VA"), (&p.tax_number, "FC")] {
        if let Some(id) = id
            && (scheme == "VA" || seller)
        {
            w.open("ram:SpecifiedTaxRegistration", &[]);
            w.leaf("ram:ID", &[("schemeID", scheme)], id);
            w.close();
        }
    }
    w.close();
}

fn allowance(w: &mut Writer, a: &AllowanceCharge) {
    w.open("ram:SpecifiedTradeAllowanceCharge", &[]);
    w.open("ram:ChargeIndicator", &[]);
    w.leaf(
        "udt:Indicator",
        &[],
        if a.charge { "true" } else { "false" },
    );
    w.close();
    if let Some(percent) = a.percent {
        w.leaf("ram:CalculationPercent", &[], &number(percent));
    }
    if let Some(base) = a.base_amount {
        w.leaf("ram:BasisAmount", &[], &amount(base));
    }
    w.leaf("ram:ActualAmount", &[], &amount(a.amount));
    w.opt("ram:ReasonCode", a.reason_code.as_deref());
    w.opt("ram:Reason", a.reason.as_deref());
    if let Some(category) = &a.vat_category {
        w.open("ram:CategoryTradeTax", &[]);
        w.leaf("ram:TypeCode", &[], "VAT");
        w.leaf("ram:CategoryCode", &[], category);
        if let Some(rate) = a.vat_rate {
            w.leaf("ram:RateApplicablePercent", &[], &number(rate));
        }
        w.close();
    }
    w.close();
}

fn line(w: &mut Writer, l: &Line) {
    w.open("ram:IncludedSupplyChainTradeLineItem", &[]);
    w.open("ram:AssociatedDocumentLineDocument", &[]);
    w.leaf("ram:LineID", &[], &l.id);
    if let Some(note) = &l.note {
        w.open("ram:IncludedNote", &[]);
        w.leaf("ram:Content", &[], note);
        w.close();
    }
    w.close();
    w.open("ram:SpecifiedTradeProduct", &[]);
    w.opt("ram:SellerAssignedID", l.seller_item_id.as_deref());
    w.opt("ram:BuyerAssignedID", l.buyer_item_id.as_deref());
    w.leaf("ram:Name", &[], &l.name);
    w.opt("ram:Description", l.description.as_deref());
    w.close();
    w.open("ram:SpecifiedLineTradeAgreement", &[]);
    if let Some(reference) = &l.order_line_reference {
        w.open("ram:BuyerOrderReferencedDocument", &[]);
        w.leaf("ram:LineID", &[], reference);
        w.close();
    }
    w.open("ram:NetPriceProductTradePrice", &[]);
    w.leaf("ram:ChargeAmount", &[], &number(l.price));
    if let Some(base) = l.base_quantity {
        w.leaf("ram:BasisQuantity", &[("unitCode", &l.unit)], &number(base));
    }
    w.close();
    w.close();
    w.open("ram:SpecifiedLineTradeDelivery", &[]);
    w.leaf(
        "ram:BilledQuantity",
        &[("unitCode", &l.unit)],
        &number(l.quantity),
    );
    w.close();
    w.open("ram:SpecifiedLineTradeSettlement", &[]);
    w.open("ram:ApplicableTradeTax", &[]);
    w.leaf("ram:TypeCode", &[], "VAT");
    w.leaf("ram:CategoryCode", &[], &l.vat_category);
    if let Some(rate) = l.vat_rate {
        w.leaf("ram:RateApplicablePercent", &[], &number(rate));
    }
    w.close();
    for a in &l.allowances {
        allowance(w, a);
    }
    w.open("ram:SpecifiedTradeSettlementLineMonetarySummation", &[]);
    w.leaf("ram:LineTotalAmount", &[], &amount(l.net_amount));
    w.close();
    w.close();
    w.close();
}

/// Write an invoice or a credit note in CII.
pub fn write(inv: &Invoice) -> String {
    let mut w = Writer::new();
    w.open(
        "rsm:CrossIndustryInvoice",
        &[
            ("xmlns:rsm", RSM),
            ("xmlns:ram", RAM),
            ("xmlns:qdt", QDT),
            ("xmlns:udt", UDT),
        ],
    );
    w.open("rsm:ExchangedDocumentContext", &[]);
    if let Some(process) = &inv.process {
        w.open("ram:BusinessProcessSpecifiedDocumentContextParameter", &[]);
        w.leaf("ram:ID", &[], process);
        w.close();
    }
    w.open("ram:GuidelineSpecifiedDocumentContextParameter", &[]);
    w.leaf("ram:ID", &[], &inv.specification);
    w.close();
    w.close();
    w.open("rsm:ExchangedDocument", &[]);
    w.leaf("ram:ID", &[], &inv.number);
    w.leaf("ram:TypeCode", &[], &inv.type_code);
    date_time(&mut w, "ram:IssueDateTime", inv.issue_date);
    for note in &inv.notes {
        w.open("ram:IncludedNote", &[]);
        w.leaf("ram:Content", &[], note);
        w.close();
    }
    w.close();
    w.open("rsm:SupplyChainTradeTransaction", &[]);
    for l in &inv.lines {
        line(&mut w, l);
    }
    w.open("ram:ApplicableHeaderTradeAgreement", &[]);
    w.opt("ram:BuyerReference", inv.buyer_reference.as_deref());
    party(&mut w, "ram:SellerTradeParty", &inv.seller, true);
    party(&mut w, "ram:BuyerTradeParty", &inv.buyer, false);
    if let Some(order) = &inv.order_reference {
        w.open("ram:BuyerOrderReferencedDocument", &[]);
        w.leaf("ram:IssuerAssignedID", &[], order);
        w.close();
    }
    w.close();
    w.open("ram:ApplicableHeaderTradeDelivery", &[]);
    if let Some(a) = &inv.delivery_address {
        w.open("ram:ShipToTradeParty", &[]);
        w.open("ram:PostalTradeAddress", &[]);
        w.opt("ram:PostcodeCode", a.postcode.as_deref());
        w.opt("ram:LineOne", a.street.as_deref());
        w.opt("ram:LineTwo", a.additional_street.as_deref());
        w.opt("ram:CityName", a.city.as_deref());
        w.leaf("ram:CountryID", &[], &a.country);
        w.close();
        w.close();
    }
    if let Some(d) = inv.delivery_date {
        w.open("ram:ActualDeliverySupplyChainEvent", &[]);
        date_time(&mut w, "ram:OccurrenceDateTime", d);
        w.close();
    }
    w.close();
    w.open("ram:ApplicableHeaderTradeSettlement", &[]);
    if let Some(p) = &inv.payment {
        w.opt("ram:PaymentReference", p.remittance.as_deref());
    }
    w.opt("ram:TaxCurrencyCode", inv.tax_currency.as_deref());
    w.leaf("ram:InvoiceCurrencyCode", &[], &inv.currency);
    if let Some(p) = &inv.payment {
        w.open("ram:SpecifiedTradeSettlementPaymentMeans", &[]);
        w.leaf("ram:TypeCode", &[], &p.means_code);
        if let Some(iban) = &p.iban {
            w.open("ram:PayeePartyCreditorFinancialAccount", &[]);
            w.leaf("ram:IBANID", &[], iban);
            w.opt("ram:AccountName", p.account_name.as_deref());
            w.close();
        }
        if let Some(bic) = &p.bic {
            w.open("ram:PayeeSpecifiedCreditorFinancialInstitution", &[]);
            w.leaf("ram:BICID", &[], bic);
            w.close();
        }
        w.close();
    }
    for v in &inv.vat {
        w.open("ram:ApplicableTradeTax", &[]);
        w.leaf("ram:CalculatedAmount", &[], &amount(v.tax));
        w.leaf("ram:TypeCode", &[], "VAT");
        w.opt("ram:ExemptionReason", v.exemption_reason.as_deref());
        w.leaf("ram:BasisAmount", &[], &amount(v.taxable));
        w.leaf("ram:CategoryCode", &[], &v.category);
        w.opt(
            "ram:ExemptionReasonCode",
            v.exemption_reason_code.as_deref(),
        );
        if let Some(rate) = v.rate {
            w.leaf("ram:RateApplicablePercent", &[], &number(rate));
        }
        w.close();
    }
    if inv.period_start.is_some() || inv.period_end.is_some() {
        w.open("ram:BillingSpecifiedPeriod", &[]);
        if let Some(d) = inv.period_start {
            date_time(&mut w, "ram:StartDateTime", d);
        }
        if let Some(d) = inv.period_end {
            date_time(&mut w, "ram:EndDateTime", d);
        }
        w.close();
    }
    for a in &inv.allowances {
        allowance(&mut w, a);
    }
    if inv.payment_terms.is_some() || inv.due_date.is_some() {
        w.open("ram:SpecifiedTradePaymentTerms", &[]);
        w.opt("ram:Description", inv.payment_terms.as_deref());
        if let Some(due) = inv.due_date {
            date_time(&mut w, "ram:DueDateDateTime", due);
        }
        w.close();
    }
    let t = &inv.totals;
    w.open("ram:SpecifiedTradeSettlementHeaderMonetarySummation", &[]);
    w.leaf("ram:LineTotalAmount", &[], &amount(t.line_total));
    if let Some(v) = t.charge_total {
        w.leaf("ram:ChargeTotalAmount", &[], &amount(v));
    }
    if let Some(v) = t.allowance_total {
        w.leaf("ram:AllowanceTotalAmount", &[], &amount(v));
    }
    w.leaf("ram:TaxBasisTotalAmount", &[], &amount(t.tax_exclusive));
    w.leaf(
        "ram:TaxTotalAmount",
        &[("currencyID", &inv.currency)],
        &amount(t.tax_total),
    );
    if let (Some(tax_currency), Some(accounting)) = (&inv.tax_currency, t.tax_total_accounting) {
        w.leaf(
            "ram:TaxTotalAmount",
            &[("currencyID", tax_currency)],
            &amount(accounting),
        );
    }
    if let Some(v) = t.rounding {
        w.leaf("ram:RoundingAmount", &[], &amount(v));
    }
    w.leaf("ram:GrandTotalAmount", &[], &amount(t.tax_inclusive));
    if let Some(v) = t.prepaid {
        w.leaf("ram:TotalPrepaidAmount", &[], &amount(v));
    }
    w.leaf("ram:DuePayableAmount", &[], &amount(t.payable));
    w.close();
    if let Some(preceding) = &inv.preceding_invoice {
        w.open("ram:InvoiceReferencedDocument", &[]);
        w.leaf("ram:IssuerAssignedID", &[], preceding);
        if let Some(d) = inv.preceding_invoice_date {
            w.open("ram:FormattedIssueDateTime", &[]);
            w.leaf(
                "qdt:DateTimeString",
                &[("format", "102")],
                &d.format("%Y%m%d").to_string(),
            );
            w.close();
        }
        w.close();
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

/// A `DateTimeString` of format 102 (`CCYYMMDD`) inside `node`.
fn read_date(node: Option<&Node>, what: &str) -> Result<Option<NaiveDate>, String> {
    let Some(inner) = node.and_then(|n| n.child("DateTimeString")) else {
        return Ok(None);
    };
    if inner.attr("format").is_some_and(|f| f != "102") {
        return Err(format!(
            "line {}: {what} is not in format 102 (CCYYMMDD)",
            inner.line
        ));
    }
    NaiveDate::parse_from_str(&inner.text, "%Y%m%d")
        .map(Some)
        .map_err(|_| format!("line {}: {what} '{}' is not a date", inner.line, inner.text))
}

fn read_party(p: &Node) -> Party {
    let address = p.child("PostalTradeAddress");
    let tax = |scheme: &str| {
        p.all("SpecifiedTaxRegistration")
            .filter_map(|t| t.child("ID"))
            .find(|id| id.attr("schemeID") == Some(scheme))
            .map(|id| id.text.clone())
    };
    let uri = p.at(&["URIUniversalCommunication", "URIID"]);
    Party {
        name: p.text_at(&["Name"]).unwrap_or_default(),
        trading_name: p.text_at(&["SpecifiedLegalOrganization", "TradingBusinessName"]),
        identifier: p.text_at(&["ID"]),
        legal_registration: p.text_at(&["SpecifiedLegalOrganization", "ID"]),
        vat_id: tax("VA"),
        tax_number: tax("FC"),
        legal_info: p.text_at(&["Description"]),
        street: address.and_then(|a| a.text_at(&["LineOne"])),
        additional_street: address.and_then(|a| a.text_at(&["LineTwo"])),
        city: address.and_then(|a| a.text_at(&["CityName"])),
        postcode: address.and_then(|a| a.text_at(&["PostcodeCode"])),
        country: address.and_then(|a| a.text_at(&["CountryID"])),
        electronic_address: uri.map(|u| u.text.clone()).filter(|t| !t.is_empty()),
        electronic_address_scheme: uri.and_then(|u| u.attr("schemeID")).map(str::to_owned),
        contact_name: p.text_at(&["DefinedTradeContact", "PersonName"]),
        contact_phone: p.text_at(&[
            "DefinedTradeContact",
            "TelephoneUniversalCommunication",
            "CompleteNumber",
        ]),
        contact_email: p.text_at(&[
            "DefinedTradeContact",
            "EmailURIUniversalCommunication",
            "URIID",
        ]),
    }
}

fn read_allowance(a: &Node) -> Result<AllowanceCharge, String> {
    Ok(AllowanceCharge {
        charge: a.text_at(&["ChargeIndicator", "Indicator"]).as_deref() == Some("true"),
        amount: decimal(a.child("ActualAmount"), "an allowance or charge amount")?,
        base_amount: opt_decimal(a.child("BasisAmount"), "a base amount")?,
        percent: opt_decimal(a.child("CalculationPercent"), "a percentage")?,
        reason: a.text_at(&["Reason"]),
        reason_code: a.text_at(&["ReasonCode"]),
        vat_category: a.text_at(&["CategoryTradeTax", "CategoryCode"]),
        vat_rate: opt_decimal(
            a.at(&["CategoryTradeTax", "RateApplicablePercent"]),
            "a VAT rate",
        )?,
    })
}

/// Read a `CrossIndustryInvoice` into the semantic model.
pub fn read(root: &Node) -> Result<Invoice, String> {
    let context = root.child("ExchangedDocumentContext");
    let document = root
        .child("ExchangedDocument")
        .ok_or("the document header (ExchangedDocument) is missing")?;
    let tx = root
        .child("SupplyChainTradeTransaction")
        .ok_or("the trade transaction (SupplyChainTradeTransaction) is missing")?;
    let agreement = tx.child("ApplicableHeaderTradeAgreement");
    let settlement = tx
        .child("ApplicableHeaderTradeSettlement")
        .ok_or("the settlement (ApplicableHeaderTradeSettlement) is missing")?;
    let mut lines = Vec::new();
    for l in tx.all("IncludedSupplyChainTradeLineItem") {
        let product = l.child("SpecifiedTradeProduct");
        let price = l.at(&["SpecifiedLineTradeAgreement", "NetPriceProductTradePrice"]);
        let quantity = l.at(&["SpecifiedLineTradeDelivery", "BilledQuantity"]);
        let line_settlement = l.child("SpecifiedLineTradeSettlement");
        lines.push(Line {
            id: l
                .text_at(&["AssociatedDocumentLineDocument", "LineID"])
                .unwrap_or_default(),
            note: l.text_at(&["AssociatedDocumentLineDocument", "IncludedNote", "Content"]),
            quantity: decimal(quantity, "a line's quantity (BT-129)")?,
            unit: quantity
                .and_then(|q| q.attr("unitCode"))
                .unwrap_or_default()
                .to_owned(),
            net_amount: decimal(
                line_settlement.and_then(|s| {
                    s.at(&[
                        "SpecifiedTradeSettlementLineMonetarySummation",
                        "LineTotalAmount",
                    ])
                }),
                "a line's net amount (BT-131)",
            )?,
            order_line_reference: l.text_at(&[
                "SpecifiedLineTradeAgreement",
                "BuyerOrderReferencedDocument",
                "LineID",
            ]),
            name: product
                .and_then(|p| p.text_at(&["Name"]))
                .unwrap_or_default(),
            description: product.and_then(|p| p.text_at(&["Description"])),
            seller_item_id: product.and_then(|p| p.text_at(&["SellerAssignedID"])),
            buyer_item_id: product.and_then(|p| p.text_at(&["BuyerAssignedID"])),
            price: decimal(
                price.and_then(|p| p.child("ChargeAmount")),
                "a line's price (BT-146)",
            )?,
            base_quantity: opt_decimal(
                price.and_then(|p| p.child("BasisQuantity")),
                "a base quantity",
            )?,
            allowances: line_settlement
                .map(|s| {
                    s.all("SpecifiedTradeAllowanceCharge")
                        .map(read_allowance)
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default(),
            vat_category: line_settlement
                .and_then(|s| s.text_at(&["ApplicableTradeTax", "CategoryCode"]))
                .unwrap_or_default(),
            vat_rate: opt_decimal(
                line_settlement
                    .and_then(|s| s.at(&["ApplicableTradeTax", "RateApplicablePercent"])),
                "a line's VAT rate (BT-152)",
            )?,
        });
    }
    let currency = settlement
        .text_at(&["InvoiceCurrencyCode"])
        .unwrap_or_default();
    let tax_currency = settlement.text_at(&["TaxCurrencyCode"]);
    let mut vat = Vec::new();
    for t in settlement.all("ApplicableTradeTax") {
        vat.push(VatBreakdown {
            category: t.text_at(&["CategoryCode"]).unwrap_or_default(),
            rate: opt_decimal(t.child("RateApplicablePercent"), "a VAT rate (BT-119)")?,
            taxable: decimal(t.child("BasisAmount"), "a taxable amount (BT-116)")?,
            tax: decimal(
                t.child("CalculatedAmount"),
                "a VAT category amount (BT-117)",
            )?,
            exemption_reason: t.text_at(&["ExemptionReason"]),
            exemption_reason_code: t.text_at(&["ExemptionReasonCode"]),
        });
    }
    let m = settlement
        .child("SpecifiedTradeSettlementHeaderMonetarySummation")
        .ok_or("the document totals (BG-22) are missing")?;
    let tax_in = |c: &str| {
        m.all("TaxTotalAmount")
            .find(|t| t.attr("currencyID").is_none_or(|x| x == c))
    };
    let totals = Totals {
        line_total: decimal(
            m.child("LineTotalAmount"),
            "the sum of line net amounts (BT-106)",
        )?,
        allowance_total: opt_decimal(m.child("AllowanceTotalAmount"), "BT-107")?,
        charge_total: opt_decimal(m.child("ChargeTotalAmount"), "BT-108")?,
        tax_exclusive: decimal(
            m.child("TaxBasisTotalAmount"),
            "the total without VAT (BT-109)",
        )?,
        tax_total: decimal(tax_in(&currency), "the VAT total (BT-110)")?,
        tax_total_accounting: match &tax_currency {
            Some(c) if *c != currency => opt_decimal(
                m.all("TaxTotalAmount")
                    .find(|t| t.attr("currencyID") == Some(c.as_str())),
                "BT-111",
            )?,
            _ => None,
        },
        tax_inclusive: decimal(m.child("GrandTotalAmount"), "the total with VAT (BT-112)")?,
        prepaid: opt_decimal(m.child("TotalPrepaidAmount"), "BT-113")?,
        rounding: opt_decimal(m.child("RoundingAmount"), "BT-114")?,
        payable: decimal(m.child("DuePayableAmount"), "the amount due (BT-115)")?,
    };
    let means = settlement.child("SpecifiedTradeSettlementPaymentMeans");
    let payment = means.map(|p| Payment {
        means_code: p.text_at(&["TypeCode"]).unwrap_or_default(),
        remittance: settlement.text_at(&["PaymentReference"]),
        iban: p
            .text_at(&["PayeePartyCreditorFinancialAccount", "IBANID"])
            .or_else(|| p.text_at(&["PayeePartyCreditorFinancialAccount", "ProprietaryID"])),
        account_name: p.text_at(&["PayeePartyCreditorFinancialAccount", "AccountName"]),
        bic: p.text_at(&["PayeeSpecifiedCreditorFinancialInstitution", "BICID"]),
    });
    let terms = settlement.child("SpecifiedTradePaymentTerms");
    Ok(Invoice {
        syntax: Syntax::Cii,
        specification: context
            .and_then(|c| c.text_at(&["GuidelineSpecifiedDocumentContextParameter", "ID"]))
            .unwrap_or_default(),
        process: context
            .and_then(|c| c.text_at(&["BusinessProcessSpecifiedDocumentContextParameter", "ID"])),
        number: document.text_at(&["ID"]).unwrap_or_default(),
        issue_date: read_date(document.child("IssueDateTime"), "the issue date (BT-2)")?
            .ok_or("the issue date (BT-2) is missing")?,
        type_code: document.text_at(&["TypeCode"]).unwrap_or_default(),
        currency,
        tax_currency,
        due_date: read_date(
            terms.and_then(|t| t.child("DueDateDateTime")),
            "the due date (BT-9)",
        )?,
        buyer_reference: agreement.and_then(|a| a.text_at(&["BuyerReference"])),
        order_reference: agreement
            .and_then(|a| a.text_at(&["BuyerOrderReferencedDocument", "IssuerAssignedID"])),
        preceding_invoice: settlement.text_at(&["InvoiceReferencedDocument", "IssuerAssignedID"]),
        preceding_invoice_date: match settlement
            .at(&["InvoiceReferencedDocument", "FormattedIssueDateTime"])
        {
            Some(n) => read_date(Some(n), "the preceding invoice's date (BT-26)")?,
            None => None,
        },
        notes: document
            .all("IncludedNote")
            .filter_map(|n| n.text_at(&["Content"]))
            .collect(),
        delivery_date: read_date(
            tx.at(&[
                "ApplicableHeaderTradeDelivery",
                "ActualDeliverySupplyChainEvent",
                "OccurrenceDateTime",
            ]),
            "the delivery date (BT-72)",
        )?,
        period_start: read_date(
            settlement.at(&["BillingSpecifiedPeriod", "StartDateTime"]),
            "the invoicing period's start (BT-73)",
        )?,
        period_end: read_date(
            settlement.at(&["BillingSpecifiedPeriod", "EndDateTime"]),
            "the invoicing period's end (BT-74)",
        )?,
        delivery_address: tx
            .at(&[
                "ApplicableHeaderTradeDelivery",
                "ShipToTradeParty",
                "PostalTradeAddress",
            ])
            .map(|a| Address {
                street: a.text_at(&["LineOne"]),
                additional_street: a.text_at(&["LineTwo"]),
                city: a.text_at(&["CityName"]),
                postcode: a.text_at(&["PostcodeCode"]),
                country: a.text_at(&["CountryID"]).unwrap_or_default(),
            }),
        seller: agreement
            .and_then(|a| a.child("SellerTradeParty"))
            .map(read_party)
            .unwrap_or_default(),
        buyer: agreement
            .and_then(|a| a.child("BuyerTradeParty"))
            .map(read_party)
            .unwrap_or_default(),
        payment,
        payment_terms: terms.and_then(|t| t.text_at(&["Description"])),
        allowances: settlement
            .all("SpecifiedTradeAllowanceCharge")
            .map(read_allowance)
            .collect::<Result<_, _>>()?,
        lines,
        vat,
        totals,
    })
}
