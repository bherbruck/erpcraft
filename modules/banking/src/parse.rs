//! Reading what banks export: CSV with a mapping, ISO 20022 camt.053, and
//! OFX. Each reader returns the same [`Parsed`] statement, or a refusal
//! that names the line (and for XML the element) it is about.

use std::collections::HashMap;
use std::str::FromStr;

use chrono::NaiveDate;
use erpcraft_kernel::Decimal;
use quick_xml::events::Event;

/// The most a file may weigh.
pub(crate) const MAX_BYTES: usize = 5 * 1024 * 1024;
/// The most transactions one statement may hold.
pub(crate) const MAX_LINES: usize = 10_000;
/// The longest text kept in a field: references can be long, not endless.
const MAX_TEXT: usize = 500;
/// How deep elements of an XML or OFX file may nest.
pub(crate) const MAX_DEPTH: usize = 64;
/// The most elements (tags) one file may hold.
pub(crate) const MAX_ELEMENTS: usize = 500_000;
/// The longest piece of the file an error message repeats.
const MAX_QUOTED: usize = 80;

/// The largest amount a statement holds: what `numeric(20, 4)` keeps.
/// Below it, any sum of at most [`MAX_LINES`] amounts fits a decimal.
pub(crate) fn max_amount() -> Decimal {
    Decimal::from_i128_with_scale(99_999_999_999_999_999_999, 4)
}

/// `text` as an error message repeats it: trimmed, and cut short.
pub(crate) fn quoted(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_QUOTED {
        return text.to_owned();
    }
    let short: String = text.chars().take(MAX_QUOTED).collect();
    format!("{short}…")
}

/// The line a byte position is on, from 1, counted as the file is read:
/// each byte is looked at once, however many positions are asked for.
struct LineCounter<'a> {
    content: &'a [u8],
    position: usize,
    line: usize,
}

impl<'a> LineCounter<'a> {
    fn new(content: &'a str) -> Self {
        LineCounter {
            content: content.as_bytes(),
            position: 0,
            line: 1,
        }
    }

    fn at(&mut self, position: usize) -> usize {
        let end = position.min(self.content.len());
        if end < self.position {
            // Only for an error behind the last event: count it afresh.
            return self.content[..end].iter().filter(|b| **b == b'\n').count() + 1;
        }
        self.line += self.content[self.position..end]
            .iter()
            .filter(|b| **b == b'\n')
            .count();
        self.position = end;
        self.line
    }
}

/// An amount within what a statement holds.
fn capped(value: Decimal, text: &str) -> Result<Decimal, String> {
    if value.abs() > max_amount() {
        return Err(format!("'{}' is more than an amount can be", quoted(text)));
    }
    Ok(value)
}

/// Digits, then a point and 1 to 4 digits or not: `1234.5`.
fn plain_decimal(text: &str) -> bool {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && (!text.contains('.') || (1..=4).contains(&fraction.len()))
        && fraction.bytes().all(|b| b.is_ascii_digit())
}

/// An amount of camt.053: unsigned (the sign is `CdtDbtInd`), with a
/// point before the fraction and nothing between thousands, as its schema
/// has it.
pub(crate) fn camt_amount(text: &str) -> Result<Decimal, String> {
    let trimmed = text.trim();
    if !plain_decimal(trimmed) {
        return Err(format!(
            "'{}' is not an amount as camt.053 writes one: digits, a point and at most 4 \
             decimals, no sign and no thousands separator",
            quoted(text)
        ));
    }
    let value =
        Decimal::from_str(trimmed).map_err(|_| format!("'{}' is not an amount", quoted(text)))?;
    capped(value, text)
}

/// An amount of OFX: a leading sign or not, digits, and a point before the
/// fraction. A comma is read as the decimal mark only where there is no
/// point and it has 1 to 4 digits after it (`-12,50`); any other comma,
/// and a trailing sign, is refused rather than guessed at.
pub(crate) fn ofx_amount(text: &str) -> Result<Decimal, String> {
    let trimmed = text.trim();
    let (sign, unsigned) = match trimmed.as_bytes().first() {
        Some(b'-') => ("-", &trimmed[1..]),
        Some(b'+') => ("", &trimmed[1..]),
        _ => ("", trimmed),
    };
    let unsigned = match unsigned.split_once(',') {
        Some((whole, fraction)) if !unsigned.contains('.') && !fraction.contains(',') => {
            format!("{whole}.{fraction}")
        }
        _ => unsigned.to_owned(),
    };
    if !plain_decimal(&unsigned) {
        return Err(format!(
            "'{}' is not an amount as OFX writes one: a sign or not, digits, a point and at \
             most 4 decimals",
            quoted(text)
        ));
    }
    let value = Decimal::from_str(&format!("{sign}{unsigned}"))
        .map_err(|_| format!("'{}' is not an amount", quoted(text)))?;
    capped(value, text)
}

/// `CRDT` is a credit, `DBIT` a debit; nothing else is read as either.
fn credit_debit(value: &str) -> Result<bool, String> {
    match value {
        "CRDT" => Ok(true),
        "DBIT" => Ok(false),
        other => Err(format!(
            "CdtDbtInd is '{}', not CRDT or DBIT",
            quoted(other)
        )),
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct ParsedLine {
    pub booking_date: Option<NaiveDate>,
    pub value_date: Option<NaiveDate>,
    pub amount: Decimal,
    pub counterparty_name: Option<String>,
    pub counterparty_account: Option<String>,
    pub reference: Option<String>,
    pub transaction_id: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Parsed {
    pub currency: Option<String>,
    /// The account as the file names it (an IBAN, an account number).
    pub account: Option<String>,
    pub opening: Option<Decimal>,
    pub closing: Option<Decimal>,
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    pub lines: Vec<ParsedLine>,
}

fn clip(text: &str) -> Option<String> {
    let text: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect();
    (!text.is_empty()).then_some(text)
}

fn too_long(lines: usize) -> Result<(), String> {
    if lines > MAX_LINES {
        return Err(format!(
            "the file holds more than {MAX_LINES} transactions; split it"
        ));
    }
    Ok(())
}

/// A decimal as banks write it, with `.` or (`decimal_comma`) `,` before
/// the fraction and spaces, apostrophes or the other mark between thousands.
pub(crate) fn amount(text: &str, decimal_comma: bool) -> Result<Decimal, String> {
    let mut cleaned: String = text
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\'' && *c != '\u{a0}')
        .collect();
    if decimal_comma {
        cleaned = cleaned.replace('.', "").replace(',', ".");
    } else {
        cleaned = cleaned.replace(',', "");
    }
    // A trailing sign, as some banks write debits: 12.50-
    if let Some(number) = cleaned.strip_suffix('-') {
        cleaned = format!("-{number}");
    }
    let value =
        Decimal::from_str(&cleaned).map_err(|_| format!("'{}' is not an amount", quoted(text)))?;
    if value.scale() > 4 {
        return Err(format!("'{}' has more than 4 decimal places", quoted(text)));
    }
    capped(value, text)
}

// -------------------------------------------------------------------- CSV

/// How one bank's CSV export reads. A column is a header name, or a
/// position from 1 when the file has no header.
#[derive(Debug, Clone)]
pub(crate) struct CsvMapping {
    pub delimiter: u8,
    pub has_header: bool,
    pub date_format: String,
    pub decimal_comma: bool,
    pub booking_date: String,
    pub value_date: Option<String>,
    pub amount: Option<String>,
    pub debit: Option<String>,
    pub credit: Option<String>,
    pub counterparty_name: Option<String>,
    pub counterparty_account: Option<String>,
    pub reference: Option<String>,
    pub transaction_id: Option<String>,
}

pub(crate) fn csv(content: &str, mapping: &CsvMapping) -> Result<Parsed, String> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(mapping.delimiter)
        .has_headers(mapping.has_header)
        .flexible(false)
        .from_reader(content.as_bytes());
    let headers: Vec<String> = if mapping.has_header {
        reader
            .headers()
            .map_err(|e| format!("line 1: {e}"))?
            .iter()
            .map(|h| h.trim().trim_start_matches('\u{feff}').to_owned())
            .collect()
    } else {
        vec![]
    };
    // Where each mapped column is.
    let position = |column: &str| -> Result<usize, String> {
        if mapping.has_header {
            headers
                .iter()
                .position(|h| h.eq_ignore_ascii_case(column.trim()))
                .ok_or_else(|| {
                    format!(
                        "the file has no column '{}'; its columns are: {}",
                        quoted(column),
                        quoted(&headers.join(", "))
                    )
                })
        } else {
            column
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1)
                .map(|n| n - 1)
                .ok_or_else(|| format!("'{}' is not a column number from 1", quoted(column)))
        }
    };
    let optional = |column: &Option<String>| column.as_deref().map(position).transpose();
    let booking = position(&mapping.booking_date)?;
    let value_date = optional(&mapping.value_date)?;
    let amount_at = optional(&mapping.amount)?;
    let debit_at = optional(&mapping.debit)?;
    let credit_at = optional(&mapping.credit)?;
    let name_at = optional(&mapping.counterparty_name)?;
    let account_at = optional(&mapping.counterparty_account)?;
    let reference_at = optional(&mapping.reference)?;
    let id_at = optional(&mapping.transaction_id)?;
    if amount_at.is_none() && (debit_at.is_none() || credit_at.is_none()) {
        return Err("the mapping needs an amount column, or a debit and a credit column".into());
    }

    let mut parsed = Parsed::default();
    // Rows without a transaction id of the bank are told apart by what
    // they say, and by how often the same thing came before.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for record in reader.records() {
        let record = record.map_err(|e| {
            let line = e.position().map_or(0, |p| p.line());
            format!("line {line}: {e}")
        })?;
        let line = record.position().map_or(0, |p| p.line());
        let at = |what: &str| format!("line {line}: {what}");
        let cell = |i: usize| record.get(i).map(str::trim).unwrap_or_default();
        if record.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        too_long(parsed.lines.len() + 1)?;
        let date = |i: usize| -> Result<Option<NaiveDate>, String> {
            let text = cell(i);
            if text.is_empty() {
                return Ok(None);
            }
            NaiveDate::parse_from_str(text, &mapping.date_format)
                .map(Some)
                .map_err(|_| {
                    at(&format!(
                        "'{}' is not a date in the format {}",
                        quoted(text),
                        quoted(&mapping.date_format)
                    ))
                })
        };
        let booking_date = date(booking)?.ok_or_else(|| at("the booking date is empty"))?;
        let value = match amount_at {
            Some(i) => amount(cell(i), mapping.decimal_comma).map_err(|e| at(&e))?,
            None => {
                let side = |i: Option<usize>| -> Result<Decimal, String> {
                    match i.map(cell).filter(|t| !t.is_empty()) {
                        Some(text) => amount(text, mapping.decimal_comma)
                            .map(|v| v.abs())
                            .map_err(|e| at(&e)),
                        None => Ok(Decimal::ZERO),
                    }
                };
                side(credit_at)?
                    .checked_sub(side(debit_at)?)
                    .ok_or_else(|| at("the amount is more than an amount can be"))?
            }
        };
        if value.is_zero() {
            continue;
        }
        let text = |i: Option<usize>| i.map(cell).and_then(clip);
        let mut row = ParsedLine {
            booking_date: Some(booking_date),
            value_date: value_date.map(date).transpose()?.flatten(),
            amount: value,
            counterparty_name: text(name_at),
            counterparty_account: text(account_at),
            reference: text(reference_at),
            transaction_id: text(id_at),
        };
        if row.transaction_id.is_none() {
            let key = format!(
                "{}|{}|{}|{}",
                booking_date,
                value.normalize(),
                row.counterparty_name.as_deref().unwrap_or_default(),
                row.reference.as_deref().unwrap_or_default()
            );
            let n = seen.entry(key.clone()).or_default();
            *n += 1;
            row.transaction_id = clip(&format!("row:{key}|{n}"));
        }
        parsed.lines.push(row);
    }
    if parsed.lines.is_empty() {
        return Err("the file holds no transactions".into());
    }
    Ok(parsed)
}

// --------------------------------------------------------------- camt.053

#[derive(Default)]
struct Balance {
    code: Option<String>,
    amount: Option<String>,
    credit: Option<bool>,
}

#[derive(Default)]
struct Entry {
    line: usize,
    amount: Option<String>,
    credit: Option<bool>,
    booking: Option<String>,
    value: Option<String>,
    id: Option<String>,
    debtor: Option<String>,
    creditor: Option<String>,
    debtor_account: Option<String>,
    creditor_account: Option<String>,
    remittance: Vec<String>,
    info: Option<String>,
}

fn iso_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok()
}

/// An ISO 20022 bank-to-customer statement (camt.053, any version from
/// .001.02 on). One statement per file. Namespaces are ignored: elements
/// are known by their names. A DTD or an entity is refused.
pub(crate) fn camt053(content: &str) -> Result<Parsed, String> {
    let mut reader = quick_xml::Reader::from_str(content);
    // Not trimmed per event: an entity splits text into several events,
    // and the spaces around it belong to the text. Values are trimmed
    // whole, at their end tag.
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut parsed = Parsed::default();
    let mut statements = 0;
    let mut balance: Option<Balance> = None;
    let mut entry: Option<Entry> = None;
    let mut entries: Vec<Entry> = Vec::new();
    let mut currency_attr: Option<String> = None;
    // Every currency an amount of the statement names, with its line.
    let mut currencies: Vec<(usize, String)> = Vec::new();
    let mut lines = LineCounter::new(content);
    let mut elements = 0usize;

    loop {
        let position = reader.buffer_position() as usize;
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(e) => {
                let line = lines.at(reader.error_position() as usize);
                return Err(format!(
                    "line {line}: the file is not well-formed XML: {}",
                    quoted(&e.to_string())
                ));
            }
        };
        let line = lines.at(position);
        let at = |path: &[String], what: &str| {
            format!(
                "line {line}, element {}: {what}",
                quoted(path.last().map(String::as_str).unwrap_or("?"))
            )
        };
        match event {
            Event::DocType(_) => {
                return Err(format!(
                    "line {line}: a document type declaration is not accepted"
                ));
            }
            Event::Start(start) => {
                elements += 1;
                if elements > MAX_ELEMENTS {
                    return Err(format!(
                        "line {line}: the file holds more than {MAX_ELEMENTS} elements"
                    ));
                }
                if path.len() >= MAX_DEPTH {
                    return Err(format!(
                        "line {line}: elements nest deeper than {MAX_DEPTH} levels"
                    ));
                }
                let name = start.local_name().as_ref().to_owned();
                text.clear();
                if name == "Amt" {
                    currency_attr = start.try_get_attribute("Ccy").ok().flatten().and_then(|a| {
                        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .ok()
                            .map(|v| v.into_owned())
                    });
                }
                path.push(name.clone());
                let parent = path.len().checked_sub(2).map(|i| path[i].as_str());
                match (name.as_str(), parent) {
                    ("Stmt", _) => {
                        statements += 1;
                        if statements > 1 {
                            return Err(at(
                                &path,
                                "the file holds more than one statement; import one at a time",
                            ));
                        }
                    }
                    ("Bal", Some("Stmt")) => balance = Some(Balance::default()),
                    ("Ntry", Some("Stmt")) => {
                        too_long(entries.len() + 1)?;
                        entry = Some(Entry {
                            line,
                            ..Entry::default()
                        });
                    }
                    _ => {}
                }
            }
            Event::Empty(_) => {
                elements += 1;
                if elements > MAX_ELEMENTS {
                    return Err(format!(
                        "line {line}: the file holds more than {MAX_ELEMENTS} elements"
                    ));
                }
            }
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::CData(c) => text.push_str(&c.into_inner()),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c),
                    _ => match r.into_inner().as_ref() {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                match resolved {
                    Some(c) => text.push(c),
                    None => return Err(at(&path, "an unknown entity is not accepted")),
                }
            }
            Event::End(_) => {
                let value = text.trim().to_owned();
                // The path ends with these names. At most MAX_DEPTH long.
                let has = |suffix: &[&str]| {
                    path.len() >= suffix.len()
                        && path[path.len() - suffix.len()..]
                            .iter()
                            .zip(suffix)
                            .all(|(a, b)| a == b)
                };
                let within = |name: &str| path.iter().any(|p| p == name);
                let in_stmt = within("Stmt");
                // The amount of a balance or an entry. Amounts of the
                // details (an instructed amount) may be in another.
                if in_stmt
                    && (has(&["Bal", "Amt"]) || has(&["Ntry", "Amt"]))
                    && let Some(ccy) = &currency_attr
                {
                    currencies.push((line, ccy.clone()));
                }
                if in_stmt && entry.is_none() && balance.is_none() {
                    if has(&["Stmt", "Acct", "Id", "IBAN"])
                        || has(&["Stmt", "Acct", "Id", "Othr", "Id"])
                    {
                        parsed.account = clip(&value);
                    } else if has(&["Stmt", "Acct", "Ccy"]) {
                        parsed.currency = clip(&value);
                    } else if has(&["FrToDt", "FrDtTm"]) || has(&["FrToDt", "FrDt"]) {
                        parsed.period_start = iso_date(&value);
                    } else if has(&["FrToDt", "ToDtTm"]) || has(&["FrToDt", "ToDt"]) {
                        parsed.period_end = iso_date(&value);
                    }
                }
                if let Some(b) = balance.as_mut() {
                    if has(&["Bal", "Tp", "CdOrPrtry", "Cd"]) {
                        b.code = Some(value.clone());
                    } else if has(&["Bal", "Amt"]) {
                        b.amount = Some(value.clone());
                    } else if has(&["Bal", "CdtDbtInd"]) {
                        b.credit = Some(credit_debit(&value).map_err(|m| at(&path, &m))?);
                    }
                }
                if let Some(e) = entry.as_mut() {
                    let in_details = within("NtryDtls");
                    if has(&["Ntry", "Amt"]) {
                        e.amount = Some(value.clone());
                    } else if has(&["Ntry", "CdtDbtInd"]) {
                        e.credit = Some(credit_debit(&value).map_err(|m| at(&path, &m))?);
                    } else if has(&["BookgDt", "Dt"]) || has(&["BookgDt", "DtTm"]) {
                        e.booking = Some(value.clone());
                    } else if has(&["ValDt", "Dt"]) || has(&["ValDt", "DtTm"]) {
                        e.value = Some(value.clone());
                    } else if has(&["Ntry", "AcctSvcrRef"])
                        || (e.id.is_none() && has(&["Refs", "AcctSvcrRef"]))
                    {
                        e.id = clip(&value);
                    } else if in_details && (has(&["Dbtr", "Nm"]) || has(&["Dbtr", "Pty", "Nm"])) {
                        e.debtor = clip(&value);
                    } else if in_details && (has(&["Cdtr", "Nm"]) || has(&["Cdtr", "Pty", "Nm"])) {
                        e.creditor = clip(&value);
                    } else if in_details && has(&["DbtrAcct", "Id", "IBAN"]) {
                        e.debtor_account = clip(&value);
                    } else if in_details && has(&["CdtrAcct", "Id", "IBAN"]) {
                        e.creditor_account = clip(&value);
                    } else if has(&["RmtInf", "Ustrd"]) || has(&["CdtrRefInf", "Ref"]) {
                        if let Some(v) = clip(&value) {
                            e.remittance.push(v);
                        }
                    } else if has(&["Ntry", "AddtlNtryInf"]) {
                        e.info = clip(&value);
                    }
                }
                let closing = path.pop().unwrap_or_default();
                text.clear();
                if closing == "Bal" && path.last().map(String::as_str) == Some("Stmt") {
                    let b = balance.take().unwrap_or_default();
                    let value = match (&b.amount, b.credit) {
                        (Some(a), Some(credit)) => {
                            let v = camt_amount(a).map_err(|e| at(&path, &e))?;
                            if credit { v } else { -v }
                        }
                        _ => return Err(at(&path, "a balance needs Amt and CdtDbtInd")),
                    };
                    match b.code.as_deref() {
                        Some("OPBD") | Some("PRCD") => {
                            parsed.opening.get_or_insert(value);
                        }
                        Some("CLBD") => parsed.closing = Some(value),
                        _ => {}
                    }
                }
                if closing == "Ntry" && path.last().map(String::as_str) == Some("Stmt") {
                    entries.push(entry.take().unwrap_or_default());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if statements == 0 {
        return Err("the file holds no camt.053 statement (no Stmt element)".into());
    }
    // The statement's currency: its account's, else that of its first
    // amount. Every amount is in it, wherever in the file it stands.
    if parsed.currency.is_none() {
        parsed.currency = currencies.first().map(|(_, c)| c.clone());
    }
    let Some(currency) = parsed.currency.clone() else {
        return Err("the file names no currency: neither Acct/Ccy nor a Ccy of an amount".into());
    };
    if let Some((line, other)) = currencies.iter().find(|(_, c)| *c != currency) {
        return Err(format!(
            "line {line}, element Amt: the amount is in {}, the statement in {}",
            quoted(other),
            quoted(&currency)
        ));
    }
    for e in entries {
        let at = |what: &str| format!("line {}, element Ntry: {what}", e.line);
        let credit = e.credit.ok_or_else(|| at("CdtDbtInd is missing"))?;
        let value = camt_amount(e.amount.as_deref().ok_or_else(|| at("Amt is missing"))?)
            .map_err(|m| at(&m))?;
        let booking_date = e
            .booking
            .as_deref()
            .and_then(iso_date)
            .ok_or_else(|| at("BookgDt is missing or not a date"))?;
        let mut reference = e.remittance.join(" ");
        if reference.is_empty() {
            reference = e.info.clone().unwrap_or_default();
        }
        parsed.lines.push(ParsedLine {
            booking_date: Some(booking_date),
            value_date: e.value.as_deref().and_then(iso_date),
            amount: if credit { value } else { -value },
            // Who the money came from, or went to.
            counterparty_name: if credit { e.debtor } else { e.creditor },
            counterparty_account: if credit {
                e.debtor_account
            } else {
                e.creditor_account
            },
            reference: clip(&reference),
            transaction_id: e.id,
        });
    }
    Ok(parsed)
}

// -------------------------------------------------------------------- OFX

/// An OFX bank statement, version 1 (SGML, tags left open) or 2 (XML).
/// One statement per file.
pub(crate) fn ofx(content: &str) -> Result<Parsed, String> {
    let start = content
        .find("<OFX>")
        .or_else(|| content.find("<ofx>"))
        .ok_or("the file is not OFX: it has no <OFX> element")?;
    let mut parsed = Parsed::default();
    let mut path: Vec<String> = Vec::new();
    let mut txn: Option<(usize, HashMap<String, String>)> = None;
    let mut statements = 0;
    let mut closed = false;
    let mut elements = 0usize;
    let mut lines = LineCounter::new(content);
    let mut rest = &content[start..];
    let mut offset = start;
    while let Some(open) = rest.find('<') {
        let line = lines.at(offset + open);
        elements += 1;
        if elements > MAX_ELEMENTS {
            return Err(format!(
                "line {line}: the file holds more than {MAX_ELEMENTS} elements"
            ));
        }
        let close = rest[open..]
            .find('>')
            .ok_or_else(|| format!("line {line}: a tag is not closed"))?;
        let tag = rest[open + 1..open + close].trim().to_ascii_uppercase();
        let after = &rest[open + close + 1..];
        let value_end = after.find('<').unwrap_or(after.len());
        let value = after[..value_end].trim();
        let at = |what: &str| format!("line {line}, element {}: {what}", quoted(&tag));
        if closed {
            return Err(at("the file goes on after </OFX>"));
        }
        if let Some(name) = tag.strip_prefix('/') {
            // Close the aggregate; a closing tag of a value (OFX 2) is
            // not on the path.
            if let Some(i) = path.iter().rposition(|p| p == name) {
                // A transaction ends with its own closing tag, before the
                // list it is in.
                if txn.is_some() && name != "STMTTRN" && path[i..].iter().any(|p| p == "STMTTRN") {
                    return Err(at("a STMTTRN is not closed before it"));
                }
                path.truncate(i);
                if name == "OFX" {
                    closed = true;
                }
                if name == "STMTTRN" {
                    let (tline, fields) = txn.take().unwrap_or_default();
                    let at = |what: &str| format!("line {tline}, element STMTTRN: {what}");
                    let value = ofx_amount(
                        fields
                            .get("TRNAMT")
                            .ok_or_else(|| at("TRNAMT is missing"))?,
                    )
                    .map_err(|m| at(&m))?;
                    let booking_date = fields
                        .get("DTPOSTED")
                        .and_then(|d| ofx_date(d))
                        .ok_or_else(|| at("DTPOSTED is missing or not a date"))?;
                    let reference = [
                        fields.get("MEMO"),
                        fields.get("CHECKNUM"),
                        fields.get("REFNUM"),
                    ]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                    too_long(parsed.lines.len() + 1)?;
                    parsed.lines.push(ParsedLine {
                        booking_date: Some(booking_date),
                        value_date: fields.get("DTUSER").and_then(|d| ofx_date(d)),
                        amount: value,
                        counterparty_name: fields.get("NAME").and_then(|n| clip(n)),
                        counterparty_account: None,
                        reference: clip(&reference),
                        transaction_id: fields.get("FITID").and_then(|f| clip(f)),
                    });
                }
            }
        } else if tag.starts_with('?') || tag.starts_with('!') {
            // A processing instruction or a declaration of OFX 2.
        } else if value.is_empty() {
            if path.len() >= MAX_DEPTH {
                return Err(at(&format!("elements nest deeper than {MAX_DEPTH} levels")));
            }
            if tag == "STMTTRN" && txn.is_some() {
                return Err(at("a STMTTRN begins before the one before it is closed"));
            }
            path.push(tag.clone());
            if tag == "STMTRS" || tag == "CCSTMTRS" {
                statements += 1;
                if statements > 1 {
                    return Err(at(
                        "the file holds more than one statement; import one at a time",
                    ));
                }
            }
            if tag == "STMTTRN" {
                txn = Some((line, HashMap::new()));
            }
        } else {
            let parent = path.last().map(String::as_str);
            if let Some((_, fields)) = txn.as_mut() {
                // NAME may sit in a PAYEE aggregate.
                fields
                    .entry(tag.clone())
                    .or_insert_with(|| value.to_owned());
            } else {
                match (tag.as_str(), parent) {
                    ("CURDEF", _) => parsed.currency = clip(value),
                    ("ACCTID", Some("BANKACCTFROM")) | ("ACCTID", Some("CCACCTFROM")) => {
                        parsed.account = clip(value)
                    }
                    ("DTSTART", Some("BANKTRANLIST")) => parsed.period_start = ofx_date(value),
                    ("DTEND", Some("BANKTRANLIST")) => parsed.period_end = ofx_date(value),
                    ("BALAMT", Some("LEDGERBAL")) => {
                        parsed.closing = Some(ofx_amount(value).map_err(|m| at(&m))?)
                    }
                    _ => {}
                }
            }
        }
        let consumed = open + close + 1;
        rest = &rest[consumed..];
        offset += consumed;
    }
    if statements == 0 {
        return Err("the file holds no bank statement (no STMTRS)".into());
    }
    if let Some((tline, _)) = txn {
        return Err(format!(
            "line {tline}, element STMTTRN: the transaction is not closed; the file is cut off"
        ));
    }
    if !closed {
        return Err("the file ends before </OFX>: it is cut off".into());
    }
    // OFX states the balance at the end only, and the opening balance is
    // worked out from it: without it the file is incomplete.
    let closing = parsed
        .closing
        .ok_or("the file states no closing balance (LEDGERBAL); it is incomplete")?;
    let moved = parsed
        .lines
        .iter()
        .try_fold(Decimal::ZERO, |sum, l| sum.checked_add(l.amount))
        .ok_or("the transactions add up to more than an amount can be")?;
    parsed.opening = Some(
        closing
            .checked_sub(moved)
            .ok_or("the opening balance is more than an amount can be")?,
    );
    Ok(parsed)
}

/// `YYYYMMDD`, with time and zone after it or not.
fn ofx_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim().get(..8)?, "%Y%m%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    #[test]
    fn amounts_read_as_banks_write_them() {
        assert_eq!(amount("1,234.50", false).unwrap(), d("1234.50"));
        assert_eq!(amount("1.234,50", true).unwrap(), d("1234.50"));
        assert_eq!(amount("-12,5", true).unwrap(), d("-12.5"));
        assert_eq!(amount("12.50-", false).unwrap(), d("-12.50"));
        assert_eq!(amount(" 1 000.00 ", false).unwrap(), d("1000.00"));
        assert!(amount("12.345678", false).is_err());
        assert!(amount("abc", false).is_err());
    }

    #[test]
    fn a_csv_row_without_a_bank_id_still_has_one() {
        let mapping = CsvMapping {
            delimiter: b';',
            has_header: true,
            date_format: "%d.%m.%Y".into(),
            decimal_comma: true,
            booking_date: "Buchungstag".into(),
            value_date: None,
            amount: Some("Betrag".into()),
            debit: None,
            credit: None,
            counterparty_name: Some("Name".into()),
            counterparty_account: None,
            reference: Some("Verwendungszweck".into()),
            transaction_id: None,
        };
        let parsed = csv(
            "Buchungstag;Name;Verwendungszweck;Betrag\n01.03.2026;A;x;-1,00\n01.03.2026;A;x;-1,00\n",
            &mapping,
        )
        .unwrap();
        assert_eq!(parsed.lines.len(), 2);
        assert_ne!(
            parsed.lines[0].transaction_id,
            parsed.lines[1].transaction_id
        );
        let error = csv(
            "Buchungstag;Name;Verwendungszweck;Betrag\n31.02.2026;A;x;1,00\n",
            &mapping,
        )
        .unwrap_err();
        assert!(error.starts_with("line 2:"), "{error}");
        let error = csv("Datum;Betrag\n", &mapping).unwrap_err();
        assert!(error.contains("no column 'Buchungstag'"), "{error}");
    }

    #[test]
    fn camt_and_ofx_amounts_are_read_strictly() {
        assert_eq!(camt_amount("1234.5").unwrap(), d("1234.5"));
        for bad in [
            "1,234.50", "-12.50", "12.50-", "+1", "1e5", "12.", ".5", "1.23456", "",
        ] {
            assert!(camt_amount(bad).is_err(), "{bad}");
        }
        assert_eq!(ofx_amount("-12,50").unwrap(), d("-12.50"));
        assert_eq!(ofx_amount("+3.5").unwrap(), d("3.5"));
        assert_eq!(ofx_amount("-1003.50").unwrap(), d("-1003.50"));
        for bad in ["-1,003.50", "1,000,000", "12.50-", "1,23456", "12,"] {
            assert!(ofx_amount(bad).is_err(), "{bad}");
        }
        // Up to what numeric(20, 4) keeps, and not beyond.
        assert!(camt_amount("9999999999999999.9999").is_ok());
        assert!(camt_amount("10000000000000000").is_err());
        assert!(ofx_amount("-79000000000000000000000000000").is_err());
        assert!(amount("79000000000000000000000000000", false).is_err());
        // A long cell is not repeated whole.
        let error = amount(&"x".repeat(10_000), false).unwrap_err();
        assert!(error.len() < 200, "{error}");
    }

    #[test]
    fn credit_and_debit_are_said_exactly() {
        let statement = |indicator: &str| {
            format!(
                "<Document><BkToCstmrStmt><Stmt><Acct><Ccy>USD</Ccy></Acct>
                 <Ntry><Amt Ccy=\"USD\">1.00</Amt><CdtDbtInd>{indicator}</CdtDbtInd>
                 <BookgDt><Dt>2026-03-01</Dt></BookgDt></Ntry></Stmt></BkToCstmrStmt></Document>"
            )
        };
        assert_eq!(
            camt053(&statement("DBIT")).unwrap().lines[0].amount,
            d("-1.00")
        );
        assert_eq!(
            camt053(&statement("CRDT")).unwrap().lines[0].amount,
            d("1.00")
        );
        let error = camt053(&statement("CRDTX")).unwrap_err();
        assert!(error.contains("not CRDT or DBIT"), "{error}");
        // An entry in another currency, before the statement names its own.
        let early = "<Document><Stmt>
            <Ntry><Amt Ccy=\"EUR\">1.00</Amt><CdtDbtInd>CRDT</CdtDbtInd>
            <BookgDt><Dt>2026-03-01</Dt></BookgDt></Ntry>
            <Bal><Tp><CdOrPrtry><Cd>CLBD</Cd></CdOrPrtry></Tp><Amt Ccy=\"USD\">1.00</Amt>
            <CdtDbtInd>CRDT</CdtDbtInd></Bal><Acct><Ccy>USD</Ccy></Acct></Stmt></Document>";
        let error = camt053(early).unwrap_err();
        assert!(error.contains("in EUR, the statement in USD"), "{error}");
        let none = "<Document><Stmt><Ntry><Amt>1.00</Amt><CdtDbtInd>CRDT</CdtDbtInd>
            <BookgDt><Dt>2026-03-01</Dt></BookgDt></Ntry></Stmt></Document>";
        assert!(camt053(none).unwrap_err().contains("no currency"));
    }

    #[test]
    fn large_files_are_read_in_linear_time_and_bounded() {
        // Many elements on many lines: each byte is counted once.
        let filler = "<X/>\n".repeat(400_000);
        let file = format!(
            "<Document><Stmt><Acct><Ccy>USD</Ccy></Acct>{filler}
             <Ntry><Amt Ccy=\"USD\">1.00</Amt><CdtDbtInd>CRDT</CdtDbtInd>
             <BookgDt><Dt>2026-03-01</Dt></BookgDt></Ntry></Stmt></Document>"
        );
        let started = std::time::Instant::now();
        assert_eq!(camt053(&file).unwrap().lines.len(), 1);
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
        let error = camt053(&"<X/>".repeat(MAX_ELEMENTS + 1)).unwrap_err();
        assert!(error.contains("more than"), "{error}");
        let error = camt053(&"<a>".repeat(MAX_DEPTH + 1)).unwrap_err();
        assert!(error.contains("deeper than"), "{error}");
        // OFX: a million tags that never close.
        let started = std::time::Instant::now();
        let error = ofx(&format!("<OFX>{}", "<A>".repeat(1_000_000))).unwrap_err();
        assert!(error.contains("deeper than"), "{error}");
        let error = ofx(&format!("<OFX>{}", "<A>x".repeat(1_000_000))).unwrap_err();
        assert!(error.contains("more than"), "{error}");
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    }

    #[test]
    fn a_cut_off_ofx_file_is_refused() {
        let whole = "<OFX><BANKMSGSRSV1><STMTTRNRS><STMTRS><CURDEF>USD
            <BANKTRANLIST>
            <STMTTRN><TRNAMT>-1.00<DTPOSTED>20260401<FITID>A</STMTTRN>
            </BANKTRANLIST>
            <LEDGERBAL><BALAMT>9.00<DTASOF>20260430</LEDGERBAL>
            </STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>";
        let parsed = ofx(whole).unwrap();
        assert_eq!(parsed.opening, Some(d("10.00")));
        let no_balance = whole.replace("<LEDGERBAL><BALAMT>9.00<DTASOF>20260430</LEDGERBAL>", "");
        assert!(ofx(&no_balance).unwrap_err().contains("closing balance"));
        let unclosed = whole.replace("<FITID>A</STMTTRN>", "<FITID>A");
        assert!(ofx(&unclosed).unwrap_err().contains("STMTTRN"));
        let cut = &whole[..whole.find("</STMTRS>").unwrap()];
        assert!(ofx(cut).unwrap_err().contains("cut off"));
    }

    #[test]
    fn malformed_xml_is_refused_with_its_line() {
        let error = camt053("<Document>\n<BkToCstmrStmt>\n<Stmt>\n</Bal>\n").unwrap_err();
        assert!(error.starts_with("line 4"), "{error}");
        let error = camt053(
            "<!DOCTYPE x [<!ENTITY e SYSTEM \"file:///etc/passwd\">]>\n<Document>&e;</Document>",
        )
        .unwrap_err();
        assert!(error.contains("document type"), "{error}");
    }
}
