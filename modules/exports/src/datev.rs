//! DATEV: the Buchungsstapel and the Debitoren/Kreditoren file.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Datelike, Months, NaiveDate, Utc};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, company, schemars,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::file::{ExportFile, datev_amount, datev_text, windows_1252};

const BOOKING_COLUMNS: &str = include_str!("buchungsstapel.columns");
const PARTY_COLUMNS: &str = include_str!("debitoren_kreditoren.columns");

/// The tax keys of the codes the German setup templates make, when no
/// `datev_tax_key` says otherwise: (code, on sales, on purchases).
const TEMPLATE_KEYS: &[(&str, &str, &str)] = &[("DE-USt19", "3", "9"), ("DE-USt7", "2", "8")];

/// The best known automatic accounts of SKR 03 and SKR 04, when no
/// `datev_account_setting` says otherwise: revenue at 7 % and 19 % and
/// purchases with input tax at 7 % and 19 %.
fn automatic_by_default(chart: &str, code: &str) -> bool {
    match chart {
        "03" => matches!(code, "8300" | "8400" | "3300" | "3400"),
        "04" => matches!(code, "4300" | "4400" | "5300" | "5400"),
        _ => false,
    }
}

#[derive(Debug, sqlx::FromRow)]
struct Settings {
    consultant_number: i32,
    client_number: i32,
    account_length: i32,
    chart: String,
    fiscal_year_start_month: i32,
    dictation_code: Option<String>,
    kost1_dimension_id: Option<Uuid>,
    kost2_dimension_id: Option<Uuid>,
}

async fn settings(ctx: &mut Ctx<'_>, company_id: Uuid) -> Result<Settings> {
    sqlx::query_as(
        "SELECT consultant_number, client_number, account_length, chart, fiscal_year_start_month,
                dictation_code, kost1_dimension_id, kost2_dimension_id
         FROM datev_settings WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *ctx.conn)
    .await?
    .ok_or_else(|| {
        Error::validation(
            "the company has no DATEV settings: give its Beraternummer and Mandantennummer first",
        )
    })
}

/// The first day of the fiscal year `day` is in.
fn fiscal_year_start(day: NaiveDate, start_month: u32) -> NaiveDate {
    let year = if day.month() >= start_month {
        day.year()
    } else {
        day.year() - 1
    };
    NaiveDate::from_ymd_opt(year, start_month, 1).unwrap_or(day)
}

/// The header line of a DATEV file.
#[allow(clippy::too_many_arguments)]
fn header(
    category: u32,
    name: &str,
    version: u32,
    user: &str,
    s: &Settings,
    fiscal_year: Option<NaiveDate>,
    range: Option<(NaiveDate, NaiveDate)>,
    description: &str,
    fixed: Option<bool>,
    currency: Option<&str>,
) -> String {
    let date = |d: Option<NaiveDate>| {
        d.map(|d| d.format("%Y%m%d").to_string())
            .unwrap_or_default()
    };
    let fields: Vec<String> = vec![
        "\"EXTF\"".into(),
        "700".into(),
        category.to_string(),
        datev_text(name, 64),
        version.to_string(),
        Utc::now().format("%Y%m%d%H%M%S%3f").to_string(),
        String::new(),
        "\"ER\"".into(),
        datev_text(user, 25),
        String::new(),
        s.consultant_number.to_string(),
        s.client_number.to_string(),
        date(fiscal_year),
        s.account_length.to_string(),
        date(range.map(|r| r.0)),
        date(range.map(|r| r.1)),
        datev_text(description, 30),
        datev_text(s.dictation_code.as_deref().unwrap_or(""), 2),
        if range.is_some() {
            "1".into()
        } else {
            String::new()
        },
        if range.is_some() {
            "0".into()
        } else {
            String::new()
        },
        fixed
            .map(|f| if f { "1" } else { "0" }.to_owned())
            .unwrap_or_default(),
        currency.map(|c| datev_text(c, 3)).unwrap_or_default(),
        String::new(),
        "\"\"".into(),
        String::new(),
        String::new(),
        datev_text(&s.chart, 2),
        String::new(),
        String::new(),
        "\"\"".into(),
        "\"\"".into(),
    ];
    fields.join(";")
}

/// The names of a file's columns, and their positions by name.
fn columns(list: &'static str) -> (Vec<&'static str>, HashMap<&'static str, usize>) {
    let names: Vec<&str> = list.lines().filter(|l| !l.is_empty()).collect();
    let at = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    (names, at)
}

/// Personal account numbers for these parties, giving the next free
/// number to one that has none: Debitoren from 1 followed by zeros,
/// Kreditoren from 7, one digit longer than the Sachkonten.
async fn personal_accounts(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    length: i32,
    parties: &HashSet<(String, Uuid)>,
) -> Result<HashMap<(String, Uuid), i64>> {
    let rows: Vec<(String, Uuid, i32)> = sqlx::query_as(
        "SELECT party_type, party_id, account_number FROM datev_party_account WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let mut known: HashMap<(String, Uuid), i64> = rows
        .into_iter()
        .map(|(t, id, n)| ((t, id), i64::from(n)))
        .collect();
    let base = 10i64.pow(length as u32);
    let mut missing: Vec<&(String, Uuid)> =
        parties.iter().filter(|p| !known.contains_key(*p)).collect();
    missing.sort();
    for party in missing {
        let (first, end) = if party.0 == "customer" {
            (base, 7 * base)
        } else {
            (7 * base, 10 * base)
        };
        let next = known
            .values()
            .filter(|n| (first..end).contains(*n))
            .max()
            .map_or(first, |n| n + 1);
        if next >= end {
            return Err(Error::validation(
                "there is no free DATEV personal account number left",
            ));
        }
        sqlx::query(
            "INSERT INTO datev_party_account (id, company_id, party_type, party_id, account_number)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(erpcraft_kernel::new_id())
        .bind(company_id)
        .bind(&party.0)
        .bind(party.1)
        .bind(next as i32)
        .execute(&mut *ctx.conn)
        .await?;
        known.insert(party.clone(), next);
    }
    Ok(known)
}

/// What Belegfeld 1 may hold: letters, digits and $ & % * + - /.
fn document_field(number: &str) -> String {
    number
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "$&%*+-/".contains(*c))
        .take(36)
        .collect()
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostingsInput {
    /// The company to export.
    company_id: Uuid,
    /// The first posting date exported.
    from: NaiveDate,
    /// The last posting date exported; in the same fiscal year as `from`.
    to: NaiveDate,
    /// Mark the bookings to be fixed (Festschreibung) when imported.
    #[serde(default)]
    fixed: bool,
    /// The name of the batch in DATEV, 30 characters.
    description: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PostingRow {
    id: Uuid,
    posting_date: NaiveDate,
    reverses_id: Option<Uuid>,
    memo: Option<String>,
    number: Option<String>,
    doc_memo: Option<String>,
}

#[derive(sqlx::FromRow)]
struct EntryRow {
    posting_id: Uuid,
    account_id: Uuid,
    code: String,
    amount: Decimal,
    source_line_id: Option<Uuid>,
    dimension_set_id: Option<Uuid>,
}

pub(crate) struct ExportPostings;

#[async_trait]
impl Command for ExportPostings {
    type Input = PostingsInput;
    type Output = ExportFile;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "datev.export_postings",
            "The DATEV Buchungsstapel (EXTF, format 700, category 21, version 13) of a company's \
             postings dated from..to within one fiscal year: one row per account, line and \
             dimension set against the account the posting moves most on, customers and vendors \
             on their personal accounts (numbered on first export), lines with a tax code that \
             has a BU-Schlüssel gross with the key, others net with their tax rows; closing \
             entries left out. In Windows-1252. Records the export in export_log.",
        )
        .permission("datev:export")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostingsInput) -> Result<ExportFile> {
        let company_id = input.company_id;
        ctx.require("datev:export", company_id)?;
        ctx.require("gl_entry:read", company_id)?;
        let s = settings(ctx, company_id).await?;
        if input.from > input.to {
            return Err(Error::validation("`from` cannot be after `to`").with_field("from"));
        }
        let year = fiscal_year_start(input.from, s.fiscal_year_start_month as u32);
        let next_year = year.checked_add_months(Months::new(12)).unwrap_or(year);
        if input.to >= next_year {
            return Err(Error::validation(format!(
                "a DATEV batch covers one fiscal year: {year} to {}",
                next_year.pred_opt().unwrap_or(next_year)
            ))
            .with_field("to"));
        }
        let currency = company::base_currency(&mut *ctx.conn, company_id).await?;
        let postings: Vec<PostingRow> = sqlx::query_as(
            "SELECT p.id, p.posting_date, p.reverses_id, p.memo, d.number, d.memo AS doc_memo
             FROM posting p JOIN document d ON d.id = p.document_id
             WHERE p.company_id = $1 AND p.posting_date BETWEEN $2 AND $3
               AND NOT starts_with(p.command, 'year_end_close.')
             ORDER BY p.posting_date, p.created_at, p.id",
        )
        .bind(company_id)
        .bind(input.from)
        .bind(input.to)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let ids: Vec<Uuid> = postings.iter().map(|p| p.id).collect();
        let originals: Vec<Uuid> = postings
            .iter()
            .map(|p| p.reverses_id.unwrap_or(p.id))
            .collect();
        let entries: Vec<EntryRow> = sqlx::query_as(
            "SELECT g.posting_id, g.account_id, a.code, g.debit - g.credit AS amount,
                    g.source_line_id, g.dimension_set_id
             FROM gl_entry g JOIN account a ON a.id = g.account_id
             WHERE g.posting_id = ANY ($1)
             ORDER BY g.posting_id, g.line_no",
        )
        .bind(&ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
        // Tax by posting and line, from the posting a reversal reverses.
        let taxes: Vec<(Uuid, Option<Uuid>, Option<Uuid>, Uuid, String, bool, String)> =
            sqlx::query_as(
                "SELECT t.posting_id, t.source_line_id, t.account_id, t.tax_code_id, t.direction,
                        t.self_assessed, c.code
                 FROM tax_entry t JOIN tax_code c ON c.id = t.tax_code_id
                 WHERE t.posting_id = ANY ($1)",
            )
            .bind(&originals)
            .fetch_all(&mut *ctx.conn)
            .await?;
        let keys: HashMap<Uuid, (Option<String>, Option<String>)> =
            sqlx::query_as::<_, (Uuid, Option<String>, Option<String>)>(
                "SELECT tax_code_id, sales_key, purchase_key FROM datev_tax_key",
            )
            .fetch_all(&mut *ctx.conn)
            .await?
            .into_iter()
            .map(|(c, s, p)| (c, (s, p)))
            .collect();
        let automatic: HashMap<Uuid, bool> = sqlx::query_as::<_, (Uuid, bool)>(
            "SELECT account_id, automatic FROM datev_account_setting WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_all(&mut *ctx.conn)
        .await?
        .into_iter()
        .collect();
        // Whose each control account is in a posting: the open items it
        // made, and those it settled.
        let parties: Vec<(Uuid, Uuid, String, Uuid)> = sqlx::query_as(
            "SELECT p.id, o.account_id, o.party_type, o.party_id
             FROM posting p JOIN open_item o ON o.posting_id = COALESCE(p.reverses_id, p.id)
             WHERE p.id = ANY ($1) AND o.party_type IN ('customer', 'vendor')
             UNION
             SELECT st.posting_id, o.account_id, o.party_type, o.party_id
             FROM settlement st JOIN open_item o ON o.id = st.open_item_id
             WHERE st.posting_id = ANY ($1) AND o.party_type IN ('customer', 'vendor')",
        )
        .bind(&ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut party_of: HashMap<(Uuid, Uuid), HashSet<(String, Uuid)>> = HashMap::new();
        for (posting, account, kind, party) in &parties {
            party_of
                .entry((*posting, *account))
                .or_default()
                .insert((kind.clone(), *party));
        }
        let wanted: HashSet<(String, Uuid)> = party_of
            .values()
            .filter(|p| p.len() == 1)
            .flat_map(|p| p.iter().cloned())
            .collect();
        let numbers = personal_accounts(ctx, company_id, s.account_length, &wanted).await?;
        // The cost centers of each dimension set.
        let sets: Vec<Uuid> = entries.iter().filter_map(|e| e.dimension_set_id).collect();
        let kost: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "SELECT sv.set_id, sv.dimension_id, v.code FROM dimension_set_value sv
             JOIN dimension_value v ON v.id = sv.value_id
             WHERE sv.set_id = ANY ($1) AND sv.dimension_id = ANY ($2)",
        )
        .bind(&sets)
        .bind(
            [s.kost1_dimension_id, s.kost2_dimension_id]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
        )
        .fetch_all(&mut *ctx.conn)
        .await?;
        let kost_of = |set: Option<Uuid>, dimension: Option<Uuid>| -> String {
            match (set, dimension) {
                (Some(set), Some(d)) => kost
                    .iter()
                    .find(|k| k.0 == set && k.1 == d)
                    .map(|k| k.2.clone())
                    .unwrap_or_default(),
                _ => String::new(),
            }
        };

        let (names, at) = columns(BOOKING_COLUMNS);
        let mut warnings: Vec<String> = vec![];
        let mut too_long: BTreeMap<String, ()> = BTreeMap::new();
        let mut lines = vec![];
        let by_posting: HashMap<Uuid, Vec<&EntryRow>> =
            entries.iter().fold(HashMap::new(), |mut m, e| {
                m.entry(e.posting_id).or_insert_with(Vec::new).push(e);
                m
            });
        for p in &postings {
            let Some(rows) = by_posting.get(&p.id) else {
                continue;
            };
            let original = p.reverses_id.unwrap_or(p.id);
            let tax_here: Vec<_> = taxes.iter().filter(|t| t.0 == original).collect();
            let tax_accounts: HashSet<Uuid> = tax_here.iter().filter_map(|t| t.2).collect();
            // The account each entry is exported on.
            let konto = |e: &EntryRow| -> Result<i64, String> {
                if let Some(party) = party_of.get(&(p.id, e.account_id))
                    && party.len() == 1
                    && let Some(n) = party.iter().next().and_then(|p| numbers.get(p))
                {
                    return Ok(*n);
                }
                let digits = e.code.chars().all(|c| c.is_ascii_digit());
                match e.code.parse::<i64>() {
                    Ok(n) if digits && e.code.len() <= s.account_length as usize => Ok(n),
                    _ => Err(e.code.clone()),
                }
            };
            // Per exported account, what the posting moves on it.
            let mut per_account: BTreeMap<i64, Decimal> = BTreeMap::new();
            let mut grouped: BTreeMap<(i64, Option<Uuid>, Option<Uuid>), (Decimal, Uuid)> =
                BTreeMap::new();
            for e in rows {
                let k = match konto(e) {
                    Ok(k) => k,
                    Err(code) => {
                        too_long.insert(code, ());
                        continue;
                    }
                };
                *per_account.entry(k).or_default() += e.amount;
                let g = grouped
                    .entry((k, e.source_line_id, e.dimension_set_id))
                    .or_insert((Decimal::ZERO, e.account_id));
                g.0 += e.amount;
            }
            let Some(pivot) = per_account
                .iter()
                .max_by_key(|(_, a)| a.abs())
                .map(|(k, _)| *k)
            else {
                continue;
            };
            // Tax folded into the line it is on, where the code has a key.
            let mut keyed: HashMap<(i64, Option<Uuid>, Option<Uuid>), String> = HashMap::new();
            let group_keys: Vec<_> = grouped.keys().copied().collect();
            for key in &group_keys {
                let (konto_no, line, set) = *key;
                let Some(&(amount, account)) = grouped.get(key) else {
                    continue;
                };
                if konto_no == pivot || !tax_accounts.contains(&account) || line.is_none() {
                    continue;
                }
                let Some(tax) = tax_here
                    .iter()
                    .find(|t| t.1 == line && t.2 == Some(account))
                else {
                    continue;
                };
                let code_key = keys
                    .get(&tax.3)
                    .map(|k| {
                        if tax.4 == "collected" && !tax.5 {
                            k.0.clone()
                        } else {
                            k.1.clone()
                        }
                    })
                    .unwrap_or_else(|| {
                        TEMPLATE_KEYS.iter().find(|k| k.0 == tax.6).map(|k| {
                            if tax.4 == "collected" && !tax.5 {
                                k.1
                            } else {
                                k.2
                            }
                            .to_owned()
                        })
                    });
                let Some(code_key) = code_key else {
                    continue;
                };
                // The base: the line's entry on another account, not a
                // tax account and not the Gegenkonto.
                let base = group_keys.iter().copied().find(|b| {
                    b.1 == line
                        && b.0 != pivot
                        && b.0 != konto_no
                        && grouped.get(b).is_some_and(|g| !tax_accounts.contains(&g.1))
                        && (b.2 == set || b.2.is_none() || set.is_none())
                });
                let Some(base) = base else {
                    continue;
                };
                grouped.remove(key);
                if let Some(g) = grouped.get_mut(&base) {
                    g.0 += amount;
                    let auto = automatic.get(&g.1).copied().unwrap_or_else(|| {
                        rows.iter()
                            .find(|e| e.account_id == g.1)
                            .is_some_and(|e| automatic_by_default(&s.chart, &e.code))
                    });
                    keyed.insert(base, if auto { String::new() } else { code_key });
                }
            }
            let text = p
                .memo
                .clone()
                .or(p.doc_memo.clone())
                .or(p.number.clone())
                .unwrap_or_default();
            for (key, (amount, _)) in &grouped {
                if key.0 == pivot || amount.round_dp(2).is_zero() {
                    continue;
                }
                let mut row = vec![String::new(); names.len()];
                row[at["Umsatz (ohne Soll/Haben-Kz)"]] = datev_amount(amount.abs());
                row[at["Soll/Haben-Kennzeichen"]] =
                    datev_text(if *amount > Decimal::ZERO { "S" } else { "H" }, 1);
                row[at["Konto"]] = key.0.to_string();
                row[at["Gegenkonto (ohne BU-Schlüssel)"]] = pivot.to_string();
                if let Some(k) = keyed.get(key).filter(|k| !k.is_empty()) {
                    row[at["BU-Schlüssel"]] = datev_text(k, 4);
                }
                row[at["Belegdatum"]] = p.posting_date.format("%d%m").to_string();
                row[at["Belegfeld 1"]] =
                    datev_text(&document_field(p.number.as_deref().unwrap_or("")), 36);
                row[at["Buchungstext"]] = datev_text(&text, 60);
                row[at["KOST1 – Kostenstelle"]] =
                    match kost_of(key.2, s.kost1_dimension_id).as_str() {
                        "" => String::new(),
                        v => datev_text(v, 36),
                    };
                row[at["KOST2 – Kostenstelle"]] =
                    match kost_of(key.2, s.kost2_dimension_id).as_str() {
                        "" => String::new(),
                        v => datev_text(v, 36),
                    };
                lines.push(row.join(";"));
            }
        }
        if !too_long.is_empty() {
            return Err(Error::validation(format!(
                "these accounts are not numbers of at most {} digits, as the DATEV settings \
                 ask: {}",
                s.account_length,
                too_long.keys().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
        let ambiguous = party_of.values().filter(|p| p.len() > 1).count();
        if ambiguous > 0 {
            warnings.push(format!(
                "{ambiguous} postings name more than one party on a control account; they are \
                 exported on the control account"
            ));
        }
        let description = input
            .description
            .clone()
            .unwrap_or_else(|| format!("erpcraft {} bis {}", input.from, input.to));
        let head = header(
            21,
            "Buchungsstapel",
            13,
            &ctx.actor.name,
            &s,
            Some(year),
            Some((input.from, input.to)),
            &description,
            Some(input.fixed),
            Some(&currency),
        );
        let rows = lines.len();
        let mut text = String::new();
        text.push_str(&head);
        text.push_str("\r\n");
        text.push_str(&names.join(";"));
        text.push_str("\r\n");
        for line in lines {
            text.push_str(&line);
            text.push_str("\r\n");
        }
        let bytes = windows_1252(&text);
        let file = ExportFile::new(
            format!(
                "EXTF_Buchungsstapel_{}_{}.csv",
                input.from.format("%Y%m%d"),
                input.to.format("%Y%m%d")
            ),
            "text/csv; charset=windows-1252",
            text,
            bytes,
            rows,
            warnings,
        );
        file.log(
            ctx,
            company_id,
            "datev_postings",
            Some((input.from, input.to)),
            input.fixed,
        )
        .await?;
        Ok(file)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PartiesInput {
    /// The company to export.
    company_id: Uuid,
}

pub(crate) struct ExportParties;

#[async_trait]
impl Command for ExportParties {
    type Input = PartiesInput;
    type Output = ExportFile;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "datev.export_parties",
            "The DATEV Debitoren/Kreditoren file (EXTF, format 700, category 16, version 5) of the \
             customers and vendors with open items in a company, each on its personal account \
             (numbered on first export): name, short name, EU country and VAT number, billing \
             address, telephone and e-mail, as far as the caller may read them. In \
             Windows-1252. Records the export in export_log.",
        )
        .permission("datev:export")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PartiesInput) -> Result<ExportFile> {
        let company_id = input.company_id;
        ctx.require("datev:export", company_id)?;
        let s = settings(ctx, company_id).await?;
        let found: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT DISTINCT party_type, party_id FROM open_item
             WHERE company_id = $1 AND party_type IN ('customer', 'vendor')
             UNION
             SELECT party_type, party_id FROM datev_party_account WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let wanted: HashSet<(String, Uuid)> = found.into_iter().collect();
        let numbers = personal_accounts(ctx, company_id, s.account_length, &wanted).await?;
        let (names, at) = columns(PARTY_COLUMNS);
        let mut ordered: Vec<(&(String, Uuid), &i64)> = numbers
            .iter()
            .filter(|(p, _)| wanted.contains(*p))
            .collect();
        ordered.sort_by_key(|(_, n)| **n);
        let mut lines = vec![];
        let mut withheld = 0;
        for ((kind, id), number) in ordered {
            let Some(party) = erpcraft_party::load_readable(ctx, kind, *id).await? else {
                withheld += 1;
                continue;
            };
            let code: Option<String> =
                sqlx::query_scalar(&format!("SELECT code FROM {kind} WHERE id = $1"))
                    .bind(id)
                    .fetch_optional(&mut *ctx.conn)
                    .await?;
            let mut row = vec![String::new(); names.len()];
            let text = |v: Option<&str>, max: usize| match v {
                Some(v) if !v.is_empty() => datev_text(v, max),
                _ => String::new(),
            };
            row[at["Konto"]] = number.to_string();
            row[at["Name (Adressatentyp Unternehmen)"]] =
                text(party.legal_name.as_deref().or(party.name.as_deref()), 50);
            row[at["Adressatentyp"]] = datev_text("2", 1);
            row[at["Kurzbezeichnung"]] = text(code.as_deref(), 15);
            if let Some(vat) = party.tax_id("eu_vat") {
                let vat: String = vat.chars().filter(|c| !c.is_whitespace()).collect();
                if vat.len() > 2 {
                    row[at["EU-Land"]] = datev_text(&vat[..2], 2);
                    row[at["EU-USt-IdNr."]] = datev_text(&vat[2..], 13);
                }
            }
            if let Some(a) = &party.address {
                row[at["Adressart"]] = datev_text("STR", 3);
                row[at["Straße"]] = text(a.street.as_deref(), 36);
                row[at["Postleitzahl"]] = text(a.postcode.as_deref(), 10);
                row[at["Ort"]] = text(a.city.as_deref(), 30);
                row[at["Land"]] = datev_text(&a.country_code, 2);
            }
            row[at["Telefon"]] = text(party.contact_phone.as_deref(), 60);
            row[at["E-Mail"]] = text(party.contact_email.as_deref(), 60);
            lines.push(row.join(";"));
        }
        let mut warnings = vec![];
        if withheld > 0 {
            warnings.push(format!(
                "{withheld} customers or vendors you may not read are left out"
            ));
        }
        let head = header(
            16,
            "Debitoren/Kreditoren",
            5,
            &ctx.actor.name,
            &s,
            Some(fiscal_year_start(
                Utc::now().date_naive(),
                s.fiscal_year_start_month as u32,
            )),
            None,
            "Debitoren und Kreditoren",
            None,
            None,
        );
        let rows = lines.len();
        let mut text = head;
        text.push_str("\r\n");
        text.push_str(&names.join(";"));
        text.push_str("\r\n");
        for line in lines {
            text.push_str(&line);
            text.push_str("\r\n");
        }
        let bytes = windows_1252(&text);
        let file = ExportFile::new(
            "EXTF_Debitoren_Kreditoren.csv".into(),
            "text/csv; charset=windows-1252",
            text,
            bytes,
            rows,
            warnings,
        );
        file.log(ctx, company_id, "datev_parties", None, false)
            .await?;
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_files_have_the_columns_of_the_format() {
        assert_eq!(columns(BOOKING_COLUMNS).0.len(), 125);
        assert_eq!(columns(PARTY_COLUMNS).0.len(), 254);
        let s = Settings {
            consultant_number: 1001,
            client_number: 456,
            account_length: 4,
            chart: "03".into(),
            fiscal_year_start_month: 1,
            dictation_code: None,
            kost1_dimension_id: None,
            kost2_dimension_id: None,
        };
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let line = header(
            21,
            "Buchungsstapel",
            13,
            "Anna",
            &s,
            Some(d(2026, 1, 1)),
            Some((d(2026, 2, 1), d(2026, 2, 28))),
            "Februar",
            Some(false),
            Some("EUR"),
        );
        let fields: Vec<&str> = line.split(';').collect();
        assert_eq!(fields.len(), 31);
        assert_eq!(
            fields[..5],
            ["\"EXTF\"", "700", "21", "\"Buchungsstapel\"", "13"]
        );
        assert_eq!(
            fields[10..16],
            ["1001", "456", "20260101", "4", "20260201", "20260228"]
        );
        assert_eq!(fields[21], "\"EUR\"");
        assert_eq!(fiscal_year_start(d(2026, 3, 5), 7), d(2025, 7, 1));
        assert_eq!(document_field("RE 2026/0001_x"), "RE2026/0001x");
    }
}
