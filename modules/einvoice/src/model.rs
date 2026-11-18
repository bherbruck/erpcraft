//! The part of the EN 16931 semantic model this module reads and writes,
//! named by its business terms (BT-n) and groups (BG-n), and the rules of
//! the standard and of XRechnung that can be checked on it.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use erpcraft_kernel::Decimal;
use serde::Serialize;

/// The XRechnung 3.0 specification identifier (BT-24), as KoSIT publishes
/// it for the current version.
pub const XRECHNUNG: &str = "urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0";
/// The business process (BT-23) XRechnung asks for.
pub const PROCESS: &str = "urn:fdc:peppol.eu:2017:poacc:billing:01:1.0";
/// The identifier of EN 16931 itself: every compliant specification
/// identifier starts with it.
pub const EN16931: &str = "urn:cen.eu:en16931:2017";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Syntax {
    /// OASIS UBL 2.1.
    Ubl,
    /// UN/CEFACT Cross Industry Invoice (D16B), as ZUGFeRD and Factur-X
    /// embed it.
    Cii,
}

impl Syntax {
    pub fn as_str(self) -> &'static str {
        match self {
            Syntax::Ubl => "ubl",
            Syntax::Cii => "cii",
        }
    }
}

/// A seller (BG-4) or a buyer (BG-7).
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct Party {
    /// BT-27 / BT-44.
    pub name: String,
    /// BT-28 / BT-45.
    pub trading_name: Option<String>,
    /// BT-29 / BT-46.
    pub identifier: Option<String>,
    /// BT-30 / BT-47.
    pub legal_registration: Option<String>,
    /// BT-31 / BT-48.
    pub vat_id: Option<String>,
    /// BT-32 (seller only).
    pub tax_number: Option<String>,
    /// BT-33 (seller only).
    pub legal_info: Option<String>,
    /// BT-35 / BT-50.
    pub street: Option<String>,
    /// BT-36 / BT-51.
    pub additional_street: Option<String>,
    /// BT-37 / BT-52.
    pub city: Option<String>,
    /// BT-38 / BT-53.
    pub postcode: Option<String>,
    /// BT-40 / BT-55.
    pub country: Option<String>,
    /// BT-34 / BT-49, with its scheme.
    pub electronic_address: Option<String>,
    pub electronic_address_scheme: Option<String>,
    /// BT-41 / BT-56.
    pub contact_name: Option<String>,
    /// BT-42 / BT-57.
    pub contact_phone: Option<String>,
    /// BT-43 / BT-58.
    pub contact_email: Option<String>,
}

/// A postal address: the deliver to address (BG-15).
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct Address {
    /// BT-75, BT-76.
    pub street: Option<String>,
    pub additional_street: Option<String>,
    /// BT-77, BT-78.
    pub city: Option<String>,
    pub postcode: Option<String>,
    /// BT-80.
    pub country: String,
}

/// An allowance or a charge on a line (BG-27, BG-28) or on the document
/// (BG-20, BG-21).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AllowanceCharge {
    /// True for a charge, false for an allowance.
    pub charge: bool,
    pub amount: Decimal,
    pub base_amount: Option<Decimal>,
    pub percent: Option<Decimal>,
    pub reason: Option<String>,
    pub reason_code: Option<String>,
    /// For a document level one: its VAT category and rate.
    pub vat_category: Option<String>,
    pub vat_rate: Option<Decimal>,
}

/// An invoice line (BG-25).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Line {
    /// BT-126.
    pub id: String,
    /// BT-127.
    pub note: Option<String>,
    /// BT-129, and its unit (BT-130, UN/ECE Recommendation 20).
    pub quantity: Decimal,
    pub unit: String,
    /// BT-131.
    pub net_amount: Decimal,
    /// BT-132: the line of the buyer's purchase order.
    pub order_line_reference: Option<String>,
    /// BT-153, BT-154.
    pub name: String,
    pub description: Option<String>,
    /// BT-155, BT-156.
    pub seller_item_id: Option<String>,
    pub buyer_item_id: Option<String>,
    /// BT-146, per BT-149 units (1 unless given).
    pub price: Decimal,
    pub base_quantity: Option<Decimal>,
    pub allowances: Vec<AllowanceCharge>,
    /// BT-151, BT-152.
    pub vat_category: String,
    pub vat_rate: Option<Decimal>,
}

/// One VAT category and rate of the breakdown (BG-23).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct VatBreakdown {
    /// BT-118, BT-119.
    pub category: String,
    pub rate: Option<Decimal>,
    /// BT-116, BT-117.
    pub taxable: Decimal,
    pub tax: Decimal,
    /// BT-120, BT-121.
    pub exemption_reason: Option<String>,
    pub exemption_reason_code: Option<String>,
}

/// The document totals (BG-22).
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct Totals {
    /// BT-106.
    pub line_total: Decimal,
    /// BT-107, BT-108.
    pub allowance_total: Option<Decimal>,
    pub charge_total: Option<Decimal>,
    /// BT-109.
    pub tax_exclusive: Decimal,
    /// BT-110, and BT-111 in the VAT accounting currency (BT-6).
    pub tax_total: Decimal,
    pub tax_total_accounting: Option<Decimal>,
    /// BT-112.
    pub tax_inclusive: Decimal,
    /// BT-113, BT-114.
    pub prepaid: Option<Decimal>,
    pub rounding: Option<Decimal>,
    /// BT-115.
    pub payable: Decimal,
}

/// Payment instructions (BG-16): the means and the account paid into.
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct Payment {
    /// BT-81, UNTDID 4461: 58 for a SEPA credit transfer.
    pub means_code: String,
    /// BT-83.
    pub remittance: Option<String>,
    /// BT-84, BT-85, BT-86.
    pub iban: Option<String>,
    pub account_name: Option<String>,
    pub bic: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Invoice {
    pub syntax: Syntax,
    /// BT-24.
    pub specification: String,
    /// BT-23.
    pub process: Option<String>,
    /// BT-1.
    pub number: String,
    /// BT-2.
    pub issue_date: NaiveDate,
    /// BT-3: 380 an invoice, 381 a credit note.
    pub type_code: String,
    /// BT-5, and BT-6 when VAT is accounted in another currency.
    pub currency: String,
    pub tax_currency: Option<String>,
    /// BT-9.
    pub due_date: Option<NaiveDate>,
    /// BT-10.
    pub buyer_reference: Option<String>,
    /// BT-13.
    pub order_reference: Option<String>,
    /// BT-25, BT-26: the invoice a credit note credits.
    pub preceding_invoice: Option<String>,
    pub preceding_invoice_date: Option<NaiveDate>,
    /// BT-22.
    pub notes: Vec<String>,
    /// BT-72.
    pub delivery_date: Option<NaiveDate>,
    /// BT-73, BT-74: the invoicing period (BG-14).
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    /// BG-15: the address the goods were delivered to.
    pub delivery_address: Option<Address>,
    pub seller: Party,
    pub buyer: Party,
    pub payment: Option<Payment>,
    /// BT-20.
    pub payment_terms: Option<String>,
    pub allowances: Vec<AllowanceCharge>,
    pub lines: Vec<Line>,
    pub vat: Vec<VatBreakdown>,
    pub totals: Totals,
}

impl Invoice {
    pub fn is_credit_note(&self) -> bool {
        self.type_code == "381"
    }

    pub fn is_xrechnung(&self) -> bool {
        self.specification.contains("xrechnung")
    }
}

/// A rule an invoice breaks.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Finding {
    /// The rule's identifier, as the standard or XRechnung names it.
    pub rule: String,
    pub message: String,
    /// A fatal rule makes the invoice invalid; the others are advice.
    pub fatal: bool,
}

fn finding(rule: &str, message: impl Into<String>) -> Finding {
    Finding {
        rule: rule.into(),
        message: message.into(),
        fatal: true,
    }
}

fn advice(rule: &str, message: impl Into<String>) -> Finding {
    Finding {
        rule: rule.into(),
        message: message.into(),
        fatal: false,
    }
}

/// The largest magnitude a number of an invoice may have: what the
/// database's `numeric(20, 6)` holds, and far beyond any real invoice.
/// Bounded, a product of two numbers cannot overflow.
pub const MAX_NUMBER: i64 = 100_000_000_000_000;
/// The most decimal places a number of an invoice may have.
pub const MAX_PLACES: u32 = 10;

/// Read a number of an invoice, refusing one out of bounds.
pub fn bounded(text: &str) -> Result<Decimal, &'static str> {
    let text = text.trim();
    if text.len() > 40 {
        return Err("is not a number this system holds");
    }
    let d: Decimal = text.parse().map_err(|_| "is not a number")?;
    if d.abs() >= Decimal::from(MAX_NUMBER) {
        return Err("is too large");
    }
    if d.normalize().scale() > MAX_PLACES {
        return Err("has more than 10 decimal places");
    }
    Ok(d)
}

fn cents(d: Decimal) -> Decimal {
    d.round_dp_with_strategy(2, rust_decimal::RoundingStrategy::MidpointAwayFromZero)
}

/// `percent` % of `base`, or nothing where it cannot be computed.
pub fn percent_of(base: Decimal, percent: Decimal) -> Option<Decimal> {
    base.checked_mul(percent)?.checked_div(Decimal::ONE_HUNDRED)
}

/// Whether an amount has at most two decimals (BR-DEC-*).
fn two_places(d: Decimal) -> bool {
    d.normalize().scale() <= 2
}

/// Whether an IBAN's check digits are right (ISO 13616, mod 97).
pub fn iban_ok(iban: &str) -> bool {
    let iban: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
    if iban.len() < 15 || iban.len() > 34 || !iban.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let rearranged = format!("{}{}", &iban[4..], &iban[..4]);
    let mut rest: u32 = 0;
    for c in rearranged.chars() {
        let value = match c.to_digit(36) {
            Some(v) => v,
            None => return false,
        };
        let digits = if value >= 10 { 100 } else { 10 };
        rest = (rest * digits + value) % 97;
    }
    rest == 1
}

/// The VAT categories EN 16931 knows (UNTDID 5305, as restricted).
pub const CATEGORIES: &[&str] = &["S", "Z", "E", "AE", "K", "G", "O", "L", "M"];

/// Check an invoice against the rules this module can check: the
/// mandatory terms, the arithmetic of lines, VAT breakdown and totals, the
/// decimal places, and, for an XRechnung, the German rules that concern
/// its content. The checks follow the rules' texts and tolerances in the
/// official Schematron (CEN validation artefacts 1.3.17, XRechnung
/// Schematron 2.6.0); see the module's README for what is not checked.
pub fn check(invoice: &Invoice) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut need = |rule: &str, present: bool, what: &str| {
        if !present {
            out.push(finding(rule, format!("{what} is missing")));
        }
    };
    let has = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    need(
        "BR-01",
        !invoice.specification.is_empty(),
        "the specification identifier (BT-24)",
    );
    need(
        "BR-02",
        !invoice.number.trim().is_empty(),
        "the invoice number (BT-1)",
    );
    need(
        "BR-04",
        !invoice.type_code.is_empty(),
        "the invoice type code (BT-3)",
    );
    need(
        "BR-05",
        !invoice.currency.is_empty(),
        "the invoice currency (BT-5)",
    );
    need(
        "BR-06",
        !invoice.seller.name.trim().is_empty(),
        "the seller's name (BT-27)",
    );
    need(
        "BR-07",
        !invoice.buyer.name.trim().is_empty(),
        "the buyer's name (BT-44)",
    );
    need(
        "BR-09",
        has(&invoice.seller.country),
        "the seller's country (BT-40)",
    );
    need(
        "BR-11",
        has(&invoice.buyer.country),
        "the buyer's country (BT-55)",
    );
    need(
        "BR-16",
        !invoice.lines.is_empty(),
        "an invoice line (BG-25)",
    );
    need(
        "BR-CO-18",
        !invoice.vat.is_empty(),
        "a VAT breakdown (BG-23)",
    );
    need(
        "BR-CO-26",
        has(&invoice.seller.identifier)
            || has(&invoice.seller.legal_registration)
            || has(&invoice.seller.vat_id),
        "a seller identifier, legal registration or VAT identifier (BT-29, BT-30, BT-31)",
    );
    if invoice.is_credit_note() {
        // Not a rule of the standard for every credit note, but the
        // invoice credited is what a buyer matches it with.
        if !has(&invoice.preceding_invoice) {
            out.push(advice(
                "BG-3",
                "the invoice credited (BT-25) is not referred to",
            ));
        }
    }

    // Lines.
    let mut by_category: BTreeMap<(String, String), Decimal> = BTreeMap::new();
    let mut line_sum = Decimal::ZERO;
    for line in &invoice.lines {
        let at = |what: &str| format!("line {}: {what}", line.id);
        if line.id.trim().is_empty() {
            out.push(finding("BR-21", "a line has no identifier (BT-126)"));
        }
        if line.name.trim().is_empty() {
            out.push(finding("BR-25", at("the item name (BT-153) is missing")));
        }
        if line.unit.trim().is_empty() {
            out.push(finding(
                "BR-23",
                at("the unit of quantity (BT-130) is missing"),
            ));
        }
        if line.price < Decimal::ZERO {
            out.push(finding(
                "BR-27",
                at("the item net price (BT-146) is below zero"),
            ));
        }
        if !CATEGORIES.contains(&line.vat_category.as_str()) {
            out.push(finding(
                "BR-CO-4",
                at(&format!(
                    "VAT category '{}' is not one of EN 16931",
                    line.vat_category
                )),
            ));
        }
        if !two_places(line.net_amount) {
            out.push(finding(
                "BR-DEC-23",
                at("the net amount has more than two decimals"),
            ));
        }
        let mut allowances = Decimal::ZERO;
        let mut charges = Decimal::ZERO;
        for a in &line.allowances {
            if !two_places(a.amount) {
                out.push(finding(
                    "BR-DEC-24",
                    at("an allowance has more than two decimals"),
                ));
            }
            if a.reason.is_none() && a.reason_code.is_none() {
                out.push(finding(
                    if a.charge { "BR-CO-24" } else { "BR-CO-23" },
                    at("an allowance or charge gives no reason"),
                ));
            }
            if let (Some(base), Some(percent)) = (a.base_amount, a.percent)
                && percent_of(base, percent)
                    .is_none_or(|p| (a.amount - p).abs() > Decimal::new(2, 2))
            {
                out.push(finding(
                    "PEPPOL-EN16931-R040",
                    at("an allowance is not its base amount times its percentage"),
                ));
            }
            if a.charge {
                charges += a.amount;
            } else {
                allowances += a.amount;
            }
        }
        let base = line.base_quantity.unwrap_or(Decimal::ONE);
        if base <= Decimal::ZERO {
            out.push(finding(
                "PEPPOL-EN16931-R121",
                at("the base quantity is not above zero"),
            ));
        } else {
            let expected = line
                .quantity
                .checked_mul(line.price)
                .and_then(|g| g.checked_div(base))
                .and_then(|g| g.checked_add(cents(charges)))
                .and_then(|g| g.checked_sub(cents(allowances)));
            let Some(expected) = expected else {
                out.push(finding(
                    "PEPPOL-EN16931-R120",
                    at("quantity times price is beyond what can be computed"),
                ));
                continue;
            };
            if (line.net_amount - expected).abs() > Decimal::new(2, 2) {
                out.push(finding(
                    "PEPPOL-EN16931-R120",
                    at(&format!(
                        "the net amount {} is not quantity times price plus charges less \
                         allowances ({})",
                        line.net_amount,
                        cents(expected)
                    )),
                ));
            }
        }
        line_sum += line.net_amount;
        let rate = rate_key(line.vat_rate);
        *by_category
            .entry((line.vat_category.clone(), rate))
            .or_default() += line.net_amount;
    }
    for a in &invoice.allowances {
        let rate = rate_key(a.vat_rate);
        let key = (a.vat_category.clone().unwrap_or_default(), rate);
        let signed = if a.charge { a.amount } else { -a.amount };
        *by_category.entry(key).or_default() += signed;
        if a.reason.is_none() && a.reason_code.is_none() {
            out.push(finding(
                if a.charge { "BR-CO-22" } else { "BR-CO-21" },
                "a document level allowance or charge gives no reason",
            ));
        }
    }

    // Totals.
    let t = &invoice.totals;
    for (rule, value) in [
        ("BR-DEC-09", Some(t.line_total)),
        ("BR-DEC-10", t.allowance_total),
        ("BR-DEC-11", t.charge_total),
        ("BR-DEC-12", Some(t.tax_exclusive)),
        ("BR-DEC-13", Some(t.tax_total)),
        ("BR-DEC-14", Some(t.tax_inclusive)),
        ("BR-DEC-16", t.prepaid),
        ("BR-DEC-17", t.rounding),
        ("BR-DEC-18", Some(t.payable)),
    ] {
        if value.is_some_and(|v| !two_places(v)) {
            out.push(finding(rule, "a total has more than two decimals"));
        }
    }
    if t.line_total != cents(line_sum) {
        out.push(finding(
            "BR-CO-10",
            format!(
                "the sum of line net amounts (BT-106) is {}, the lines add up to {}",
                t.line_total,
                cents(line_sum)
            ),
        ));
    }
    let doc_allowances: Decimal = invoice
        .allowances
        .iter()
        .filter(|a| !a.charge)
        .map(|a| a.amount)
        .sum();
    let doc_charges: Decimal = invoice
        .allowances
        .iter()
        .filter(|a| a.charge)
        .map(|a| a.amount)
        .sum();
    if t.allowance_total.unwrap_or_default() != doc_allowances {
        out.push(finding(
            "BR-CO-11",
            "the sum of allowances (BT-107) is not the document level allowances",
        ));
    }
    if t.charge_total.unwrap_or_default() != doc_charges {
        out.push(finding(
            "BR-CO-12",
            "the sum of charges (BT-108) is not the document level charges",
        ));
    }
    let exclusive = cents(
        t.line_total - t.allowance_total.unwrap_or_default() + t.charge_total.unwrap_or_default(),
    );
    if t.tax_exclusive != exclusive {
        out.push(finding(
            "BR-CO-13",
            format!(
                "the total without VAT (BT-109) is {}, lines less allowances plus charges come \
                 to {exclusive}",
                t.tax_exclusive
            ),
        ));
    }
    let vat_sum: Decimal = invoice.vat.iter().map(|v| v.tax).sum();
    if t.tax_total != vat_sum {
        out.push(finding(
            "BR-CO-14",
            format!(
                "the VAT total (BT-110) is {}, the breakdown adds up to {vat_sum}",
                t.tax_total
            ),
        ));
    }
    if t.tax_inclusive != cents(t.tax_exclusive + t.tax_total) {
        out.push(finding(
            "BR-CO-15",
            "the total with VAT (BT-112) is not the total without VAT plus the VAT",
        ));
    }
    if t.payable != t.tax_inclusive - t.prepaid.unwrap_or_default() + t.rounding.unwrap_or_default()
    {
        out.push(finding(
            "BR-CO-16",
            "the amount due (BT-115) is not the total with VAT less paid plus rounding",
        ));
    }

    // The VAT breakdown, category by category.
    let mut seen = Vec::new();
    for v in &invoice.vat {
        let rate_text = rate_key(v.rate);
        let key = (v.category.clone(), rate_text.clone());
        let what = format!("VAT category {} at {}%", v.category, rate_text);
        if seen.contains(&key) {
            out.push(finding(
                "BR-CO-18",
                format!("{what} is in the breakdown twice"),
            ));
        }
        seen.push(key.clone());
        if !two_places(v.taxable) || !two_places(v.tax) {
            out.push(finding(
                "BR-DEC-19",
                format!("{what}: more than two decimals"),
            ));
        }
        // BR-S-08 and the like: the taxable amount is what the lines and
        // document allowances and charges of the category come to, within
        // one unit, as the official Schematron allows.
        let expected = by_category.get(&key).copied().unwrap_or_default();
        if (v.taxable - expected).abs() >= Decimal::ONE {
            out.push(finding(
                &format!("BR-{}-08", rule_part(&v.category)),
                format!(
                    "{what}: taxable amount {} but the lines come to {expected}",
                    v.taxable
                ),
            ));
        }
        // BR-CO-17: the tax is the taxable amount times the rate, within
        // one unit (the Schematron's tolerance, which lets per-line
        // rounding stand).
        let rate = v.rate.unwrap_or_default();
        let computed = percent_of(v.taxable.abs(), rate).map(cents);
        if computed.is_none_or(|c| (v.tax.abs() - c).abs() >= Decimal::ONE) {
            let computed = computed.map_or("beyond what can be computed".into(), |c| c.to_string());
            out.push(finding(
                "BR-CO-17",
                format!("{what}: tax {} but taxable times rate is {computed}", v.tax),
            ));
        }
        if v.category == "S" && rate <= Decimal::ZERO {
            out.push(finding(
                "BR-S-05",
                format!("{what}: a standard rate is above zero"),
            ));
        }
        if matches!(v.category.as_str(), "Z" | "E" | "AE" | "K" | "G" | "O") && !rate.is_zero() {
            out.push(finding(
                &format!("BR-{}-05", rule_part(&v.category)),
                format!("{what}: the rate must be zero"),
            ));
        }
        if matches!(v.category.as_str(), "Z" | "E" | "AE" | "K" | "G" | "O") && !v.tax.is_zero() {
            out.push(finding(
                &format!("BR-{}-09", rule_part(&v.category)),
                format!("{what}: the VAT amount must be zero, not {}", v.tax),
            ));
        }
        if matches!(v.category.as_str(), "E" | "AE" | "K" | "G" | "O")
            && v.exemption_reason.is_none()
            && v.exemption_reason_code.is_none()
        {
            out.push(finding(
                &format!("BR-{}-10", rule_part(&v.category)),
                format!("{what}: an exemption reason (BT-120 or BT-121) is needed"),
            ));
        }
        if matches!(v.category.as_str(), "S" | "Z")
            && (v.exemption_reason.is_some() || v.exemption_reason_code.is_some())
        {
            out.push(finding(
                &format!("BR-{}-10", v.category),
                format!("{what}: has an exemption reason, which this category may not"),
            ));
        }
    }

    // What the categories require of the parties and the delivery
    // (BR-AE-02 to 04, BR-IC-02 to 04, BR-IC-11, BR-IC-12, BR-G-02 to 04,
    // BR-O-02 to 04, BR-O-11). The tax representative (BG-11) is not
    // read, so the seller's own identifiers must be there.
    let used = |c: &str| {
        invoice.lines.iter().any(|l| l.vat_category == c)
            || invoice
                .allowances
                .iter()
                .any(|a| a.vat_category.as_deref() == Some(c))
            || invoice.vat.iter().any(|v| v.category == c)
    };
    let (s, b) = (&invoice.seller, &invoice.buyer);
    if used("AE") {
        if !(has(&s.vat_id) || has(&s.tax_number)) {
            out.push(finding(
                "BR-AE-02",
                "a reverse charge (AE) needs the seller's VAT identifier (BT-31) or tax \
                 registration (BT-32)",
            ));
        }
        if !(has(&b.vat_id) || has(&b.legal_registration)) {
            out.push(finding(
                "BR-AE-02",
                "a reverse charge (AE) needs the buyer's VAT identifier (BT-48) or legal \
                 registration (BT-47)",
            ));
        }
    }
    if used("K") {
        if !has(&s.vat_id) {
            out.push(finding(
                "BR-IC-02",
                "an intra-Community supply (K) needs the seller's VAT identifier (BT-31)",
            ));
        }
        if !has(&b.vat_id) {
            out.push(finding(
                "BR-IC-02",
                "an intra-Community supply (K) needs the buyer's VAT identifier (BT-48)",
            ));
        }
        if invoice.delivery_date.is_none()
            && invoice.period_start.is_none()
            && invoice.period_end.is_none()
        {
            out.push(finding(
                "BR-IC-11",
                "an intra-Community supply (K) needs the delivery date (BT-72) or the \
                 invoicing period (BG-14)",
            ));
        }
        if invoice
            .delivery_address
            .as_ref()
            .is_none_or(|a| a.country.trim().is_empty())
        {
            out.push(finding(
                "BR-IC-12",
                "an intra-Community supply (K) needs the country delivered to (BT-80)",
            ));
        }
    }
    if used("G") && !has(&s.vat_id) {
        out.push(finding(
            "BR-G-02",
            "an export outside the EU (G) needs the seller's VAT identifier (BT-31)",
        ));
    }
    if used("O") {
        if has(&s.vat_id) || has(&b.vat_id) {
            out.push(finding(
                "BR-O-02",
                "an invoice not subject to VAT (O) may not carry the seller's or the buyer's \
                 VAT identifier (BT-31, BT-48)",
            ));
        }
        if invoice.vat.iter().any(|v| v.category != "O")
            || invoice.lines.iter().any(|l| l.vat_category != "O")
        {
            out.push(finding(
                "BR-O-11",
                "an invoice with VAT category O (not subject to VAT) may have no other category",
            ));
        }
    }
    for key in by_category.keys() {
        if !seen.contains(key) {
            out.push(finding(
                &format!("BR-{}-01", rule_part(&key.0)),
                format!(
                    "lines of VAT category {} at {}% have no VAT breakdown",
                    key.0, key.1
                ),
            ));
        }
    }
    if invoice.vat.iter().any(|v| v.category != "O")
        && !(has(&invoice.seller.vat_id) || has(&invoice.seller.tax_number))
    {
        let rule = if invoice.is_xrechnung() {
            "BR-DE-16"
        } else {
            "BR-S-02"
        };
        out.push(finding(
            rule,
            "the seller's VAT identifier (BT-31) or tax number (BT-32) is missing",
        ));
    }

    if let Some(payment) = &invoice.payment {
        if payment.means_code == "58" && !payment.iban.as_deref().is_some_and(iban_ok) {
            out.push(finding(
                "BR-DE-19",
                "a SEPA credit transfer (58) needs a correct IBAN (BT-84)",
            ));
        }
        if matches!(payment.means_code.as_str(), "30" | "58") && payment.iban.is_none() {
            out.push(finding(
                "BR-50",
                "a credit transfer needs the account paid into (BT-84)",
            ));
        }
    }

    if let Some(a) = &invoice.delivery_address
        && a.country.trim().is_empty()
    {
        out.push(finding(
            "BR-57",
            "the deliver to address has no country (BT-80)",
        ));
    }

    if invoice.is_xrechnung() {
        let s = &invoice.seller;
        if let Some(a) = &invoice.delivery_address {
            need_de(
                &mut out,
                "BR-DE-10",
                has(&a.city),
                "the deliver to city (BT-77)",
            );
            need_de(
                &mut out,
                "BR-DE-11",
                has(&a.postcode),
                "the deliver to post code (BT-78)",
            );
        }
        let b = &invoice.buyer;
        if !invoice.specification.starts_with(EN16931) {
            out.push(finding(
                "BR-DE-21",
                "the specification identifier is not XRechnung's",
            ));
        }
        need_de(
            &mut out,
            "BR-DE-1",
            invoice.payment.is_some(),
            "the payment instructions (BG-16)",
        );
        need_de(
            &mut out,
            "BR-DE-15",
            has(&invoice.buyer_reference),
            "the buyer reference (BT-10), for a public buyer its Leitweg-ID",
        );
        need_de(
            &mut out,
            "BR-DE-3",
            has(&s.city),
            "the seller's city (BT-37)",
        );
        need_de(
            &mut out,
            "BR-DE-4",
            has(&s.postcode),
            "the seller's post code (BT-38)",
        );
        need_de(
            &mut out,
            "BR-DE-5",
            has(&s.contact_name),
            "the seller's contact point (BT-41)",
        );
        need_de(
            &mut out,
            "BR-DE-6",
            has(&s.contact_phone),
            "the seller's contact telephone (BT-42)",
        );
        need_de(
            &mut out,
            "BR-DE-7",
            has(&s.contact_email),
            "the seller's contact email (BT-43)",
        );
        need_de(
            &mut out,
            "BR-DE-8",
            has(&b.city),
            "the buyer's city (BT-52)",
        );
        need_de(
            &mut out,
            "BR-DE-9",
            has(&b.postcode),
            "the buyer's post code (BT-53)",
        );
        need_de(
            &mut out,
            "PEPPOL-EN16931-R020",
            has(&s.electronic_address),
            "the seller's electronic address (BT-34)",
        );
        need_de(
            &mut out,
            "PEPPOL-EN16931-R010",
            has(&b.electronic_address),
            "the buyer's electronic address (BT-49)",
        );
        if !["326", "380", "384", "389", "381", "875", "876", "877"]
            .contains(&invoice.type_code.as_str())
        {
            out.push(finding(
                "BR-DE-17",
                format!(
                    "type code {} is not one XRechnung allows",
                    invoice.type_code
                ),
            ));
        }
        if invoice.vat.iter().any(|v| v.rate.is_none()) {
            out.push(finding(
                "BR-DE-14",
                "a VAT category rate (BT-119) is missing",
            ));
        }
        if let Some(phone) = &s.contact_phone
            && phone.chars().filter(char::is_ascii_digit).count() < 3
        {
            out.push(advice(
                "BR-DE-27",
                "the seller's telephone has fewer than three digits",
            ));
        }
        if let Some(email) = &s.contact_email
            && !email_ok(email)
        {
            out.push(advice(
                "BR-DE-28",
                "the seller's contact email does not look like one",
            ));
        }
        if let Some(terms) = &invoice.payment_terms {
            for line in terms.lines().filter(|l| l.trim_start().starts_with('#')) {
                if !skonto_ok(line.trim()) {
                    out.push(finding(
                        "BR-DE-18",
                        format!("payment terms line '{line}' is not #SKONTO#TAGE=n#PROZENT=n.nn#"),
                    ));
                }
            }
        }
        if invoice.delivery_date.is_none() {
            out.push(advice(
                "BR-DE-TMP-32",
                "no delivery date (BT-72) or invoicing period is given",
            ));
        }
    }
    out
}

/// How the rules of a category are named: "IC" for K, as the standard
/// names them, the category itself otherwise.
fn rule_part(category: &str) -> &str {
    match category {
        "K" => "IC",
        other => other,
    }
}

/// A rate as a key: a missing rate is zero, as for category O.
fn rate_key(rate: Option<Decimal>) -> String {
    let r = rate.unwrap_or_default().normalize();
    if r.is_zero() {
        "0".into()
    } else {
        r.to_string()
    }
}

fn need_de(out: &mut Vec<Finding>, rule: &str, present: bool, what: &str) {
    if !present {
        out.push(finding(rule, format!("{what} is missing")));
    }
}

fn email_ok(email: &str) -> bool {
    let mut parts = email.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => {
            local.len() >= 2
                && domain.len() >= 2
                && !email.contains(' ')
                && !local.ends_with('.')
                && !domain.starts_with('.')
                && !email.starts_with('.')
                && !email.ends_with('.')
        }
        _ => false,
    }
}

/// `#SKONTO#TAGE=14#PROZENT=2.00#`, optionally `#BASISBETRAG=n#` after.
pub fn skonto_ok(line: &str) -> bool {
    let parts: Vec<&str> = line.split('#').collect();
    if parts.len() < 5 || !parts[0].is_empty() || !parts[parts.len() - 1].is_empty() {
        return false;
    }
    let days = parts[2]
        .strip_prefix("TAGE=")
        .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
    let percent = parts[3].strip_prefix("PROZENT=").is_some_and(|p| {
        p.split_once('.').is_some_and(|(a, b)| {
            !a.is_empty()
                && a.chars().all(|c| c.is_ascii_digit())
                && b.len() == 2
                && b.chars().all(|c| c.is_ascii_digit())
        })
    });
    let rest_ok = match parts.len() {
        5 => true,
        6 => parts[4]
            .strip_prefix("BASISBETRAG=")
            .is_some_and(|b| b.parse::<Decimal>().is_ok()),
        _ => false,
    };
    parts[1] == "SKONTO" && days && percent && rest_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibans_and_skonto() {
        assert!(iban_ok("DE79000000001234567890"));
        assert!(iban_ok("DE89 3704 0044 0532 0130 00"));
        assert!(!iban_ok("DE89370400440532013001"));
        assert!(skonto_ok("#SKONTO#TAGE=14#PROZENT=2.00#"));
        assert!(skonto_ok("#SKONTO#TAGE=7#PROZENT=3.00#BASISBETRAG=100.00#"));
        assert!(!skonto_ok("#SKONTO#TAGE=14#PROZENT=2#"));
        assert!(!skonto_ok("#skonto#TAGE=14#PROZENT=2.00#"));
    }
}
