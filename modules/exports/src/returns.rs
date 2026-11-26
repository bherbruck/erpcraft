//! Information returns: what a US company reports on Forms 1099-NEC and
//! 1099-MISC, and a Canadian construction business on T5018 slips.

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::file::{ExportFile, csv_field};

const EXPORT: &str = "information_return:export";

/// Which return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Form {
    /// Forms 1099-NEC and 1099-MISC (US).
    #[serde(rename = "us_1099")]
    Us1099,
    /// T5018, contract payments (Canada).
    CaT5018,
}

impl Form {
    fn as_str(self) -> &'static str {
        match self {
            Form::Us1099 => "us_1099",
            Form::CaT5018 => "ca_t5018",
        }
    }
}

/// The least a vendor is paid in a box for it to be reported, in a tax
/// year: the IRS's for a 1099 (from 2026 as P.L. 119-21 set them), $500
/// for a T5018.
fn threshold(form: Form, box_: Option<&str>, year: i32) -> Decimal {
    let d = |n: i64| Decimal::from(n);
    match (form, box_) {
        (Form::CaT5018, _) => d(500),
        (Form::Us1099, Some("misc_2")) => d(10),
        (Form::Us1099, Some("misc_10")) => d(600),
        (Form::Us1099, _) if year >= 2026 => d(2000),
        (Form::Us1099, _) => d(600),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReturnInput {
    /// The paying company.
    company_id: Uuid,
    /// us_1099 or ca_t5018.
    form: Form,
    /// The calendar year paid in.
    year: i32,
    /// For a T5018 by fiscal year: its first day, instead of January 1.
    from: Option<NaiveDate>,
    /// For a T5018 by fiscal year: its last day.
    to: Option<NaiveDate>,
    /// The least amount reported, instead of the year's threshold.
    threshold: Option<Decimal>,
}

/// One vendor's payments in a box of a return.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct ReturnRow {
    vendor_id: Uuid,
    vendor_code: Option<String>,
    vendor_name: Option<String>,
    form: Form,
    /// For a 1099, the box: nec_1, misc_1, misc_2, misc_3, misc_6 or misc_10.
    #[serde(rename = "box")]
    #[schemars(rename = "box")]
    box_: Option<String>,
    /// Paid in the period, in the accounting currency: money out less
    /// money back.
    amount: Decimal,
    threshold: Decimal,
    /// The amount reaches the threshold.
    reportable: bool,
    /// EIN or SSN (US), BN or SIN (Canada).
    tin_type: Option<String>,
    /// Whole for who may export the return, else its last four digits.
    tin: Option<String>,
    street: Option<String>,
    city: Option<String>,
    region: Option<String>,
    postcode: Option<String>,
    country_code: Option<String>,
    /// What filing needs and the vendor's data lacks.
    missing: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ReturnOutput {
    company_id: Uuid,
    form: Form,
    from: NaiveDate,
    to: NaiveDate,
    currency: String,
    rows: Vec<ReturnRow>,
    /// Vendors to report.
    reportable: usize,
    /// What they were paid.
    reportable_amount: Decimal,
}

/// The vendors set up for `form` and what each was paid in the range.
async fn rows(
    ctx: &mut Ctx<'_>,
    input: &ReturnInput,
) -> Result<(NaiveDate, NaiveDate, Vec<ReturnRow>)> {
    let company_id = input.company_id;
    ctx.require("payment:read", company_id)?;
    let from = input
        .from
        .or_else(|| NaiveDate::from_ymd_opt(input.year, 1, 1))
        .ok_or_else(|| Error::validation("not a year").with_field("year"))?;
    let to = input
        .to
        .or_else(|| NaiveDate::from_ymd_opt(input.year, 12, 31))
        .ok_or_else(|| Error::validation("not a year").with_field("year"))?;
    if from > to {
        return Err(Error::validation("`from` cannot be after `to`").with_field("from"));
    }
    // Money that left (or came back through) the bank in each posted
    // payment to a vendor: credit minus debit on its bank account.
    let paid: Vec<(Uuid, Option<String>, Decimal)> = sqlx::query_as(
        "WITH pay AS (
            SELECT p.id AS posting_id, py.bank_account_id,
                   COALESCE(py.party_id, (SELECT o.party_id FROM payment_allocation a
                                          JOIN open_item o ON o.id = a.open_item_id
                                          WHERE a.document_id = py.document_id
                                          ORDER BY a.line_no LIMIT 1)) AS party_id,
                   COALESCE(py.party_type, (SELECT o.party_type FROM payment_allocation a
                                            JOIN open_item o ON o.id = a.open_item_id
                                            WHERE a.document_id = py.document_id
                                            ORDER BY a.line_no LIMIT 1)) AS party_type
            FROM posting p JOIN payment py ON py.document_id = p.document_id
            WHERE p.company_id = $1 AND p.posting_date BETWEEN $2 AND $3
              AND py.bank_account_id IS NOT NULL)
         SELECT pay.party_id, r.box, sum(g.credit - g.debit)
         FROM pay
         JOIN gl_entry g ON g.posting_id = pay.posting_id AND g.account_id = pay.bank_account_id
         JOIN vendor_information_return r ON r.vendor_id = pay.party_id AND r.form = $4
         WHERE pay.party_type = 'vendor'
         GROUP BY pay.party_id, r.box",
    )
    .bind(company_id)
    .bind(from)
    .bind(to)
    .bind(input.form.as_str())
    .fetch_all(&mut *ctx.conn)
    .await?;
    let full = ctx.actor.can(EXPORT, company_id);
    let mut out = vec![];
    for (vendor, box_, amount) in paid {
        let limit = input
            .threshold
            .unwrap_or_else(|| threshold(input.form, box_.as_deref(), input.year));
        let party = erpcraft_party::load_readable(ctx, "vendor", vendor).await?;
        let code: Option<String> = if party.is_some() {
            sqlx::query_scalar("SELECT code FROM vendor WHERE id = $1")
                .bind(vendor)
                .fetch_optional(&mut *ctx.conn)
                .await?
        } else {
            None
        };
        let (tin_type, tin) = match (&party, input.form) {
            (Some(p), Form::Us1099) => match (p.tax_id("us_ein"), p.tax_id("us_ssn")) {
                (Some(ein), _) => (Some("EIN"), Some(ein)),
                (None, Some(ssn)) => (Some("SSN"), Some(ssn)),
                _ => (None, None),
            },
            (Some(p), Form::CaT5018) => match (p.tax_id("ca_bn"), p.tax_id("ca_sin")) {
                (Some(bn), _) => (Some("BN"), Some(bn)),
                (None, Some(sin)) => (Some("SIN"), Some(sin)),
                _ => (None, None),
            },
            _ => (None, None),
        };
        let tin = tin.map(|t| {
            if full {
                t
            } else {
                let digits: String = t.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                let tail: String = digits
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("***{tail}")
            }
        });
        let address = party.as_ref().and_then(|p| p.address.clone());
        let mut missing = vec![];
        if party.is_some() {
            if tin.is_none() {
                missing.push("tax number");
            }
            if address
                .as_ref()
                .is_none_or(|a| a.street.is_none() || a.city.is_none())
            {
                missing.push("address");
            }
        }
        out.push(ReturnRow {
            vendor_id: vendor,
            vendor_code: code,
            vendor_name: party
                .as_ref()
                .and_then(|p| p.legal_name.clone().or(p.name.clone())),
            form: input.form,
            box_,
            reportable: amount >= limit,
            amount,
            threshold: limit,
            tin_type: tin_type.map(str::to_owned),
            tin,
            street: address.as_ref().and_then(|a| a.street.clone()),
            city: address.as_ref().and_then(|a| a.city.clone()),
            region: address.as_ref().and_then(|a| a.region.clone()),
            postcode: address.as_ref().and_then(|a| a.postcode.clone()),
            country_code: address.map(|a| a.country_code),
            missing: (!missing.is_empty()).then(|| missing.join(", ")),
        });
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.amount));
    Ok((from, to, out))
}

pub(crate) struct Report;

#[async_trait]
impl Command for Report {
    type Input = ReturnInput;
    type Output = ReturnOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "information_return.report",
            "What each vendor set up for a 1099 (us_1099) or a T5018 (ca_t5018) was paid in a \
             calendar year (or from..to): the money that left the bank in posted payments to it, \
             less money it paid back, per box, against the year's threshold, with its tax number \
             (masked unless the reader may export the return), address, and what filing still \
             needs.",
        )
        .report("Information returns")
        .permission("payment:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReturnInput) -> Result<ReturnOutput> {
        let (from, to, rows) = rows(ctx, &input).await?;
        let currency =
            erpcraft_kernel::company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let reportable: Vec<&ReturnRow> = rows.iter().filter(|r| r.reportable).collect();
        Ok(ReturnOutput {
            company_id: input.company_id,
            form: input.form,
            from,
            to,
            currency,
            reportable: reportable.len(),
            reportable_amount: reportable.iter().map(|r| r.amount).sum(),
            rows,
        })
    }
}

pub(crate) struct Export;

#[async_trait]
impl Command for Export {
    type Input = ReturnInput;
    type Output = ExportFile;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "information_return.export",
            "The vendors of information_return.report that reach the threshold, as a CSV to \
             file from: the payer's and each recipient's tax number, name and address, and for a \
             1099 an amount per box (one row per vendor and form), for a T5018 the amount paid. \
             Laid out to be mapped onto the IRS's IRIS or FIRE and the CRA's T5018 formats; it is \
             not those files. Records the export in export_log.",
        )
        .permission(EXPORT)
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReturnInput) -> Result<ExportFile> {
        ctx.require(EXPORT, input.company_id)?;
        let (from, to, rows) = rows(ctx, &input).await?;
        let payer = erpcraft_party::load_readable(ctx, "company", input.company_id).await?;
        let payer_tin = payer.as_ref().and_then(|p| match input.form {
            Form::Us1099 => p.tax_id("us_ein"),
            Form::CaT5018 => p.tax_id("ca_bn"),
        });
        let payer_name = payer
            .as_ref()
            .and_then(|p| p.legal_name.clone().or(p.name.clone()))
            .unwrap_or_default();
        let pa = payer.as_ref().and_then(|p| p.address.clone());
        let mut warnings = vec![];
        if payer_tin.is_none() {
            warnings.push("the company has no tax number of its own on record".to_owned());
        }
        let header: Vec<&str> = match input.form {
            Form::Us1099 => vec![
                "tax_year",
                "form",
                "payer_tin",
                "payer_name",
                "payer_street",
                "payer_city",
                "payer_state",
                "payer_zip",
                "recipient_tin_type",
                "recipient_tin",
                "recipient_name",
                "recipient_street",
                "recipient_city",
                "recipient_state",
                "recipient_zip",
                "recipient_country",
                "nec_box1",
                "misc_box1",
                "misc_box2",
                "misc_box3",
                "misc_box6",
                "misc_box10",
            ],
            Form::CaT5018 => vec![
                "period_start",
                "period_end",
                "payer_bn",
                "payer_name",
                "payer_street",
                "payer_city",
                "payer_province",
                "payer_postal_code",
                "recipient_type",
                "recipient_bn",
                "recipient_sin",
                "recipient_name",
                "recipient_street",
                "recipient_city",
                "recipient_province",
                "recipient_postal_code",
                "recipient_country",
                "contract_payments",
            ],
        };
        let f = |v: &Option<String>| csv_field(v.as_deref().unwrap_or(""));
        let mut lines = vec![header.join(",")];
        let mut count = 0;
        // A 1099 per vendor and form: NEC apart from MISC.
        let mut by_vendor: std::collections::BTreeMap<(Uuid, bool), Vec<&ReturnRow>> =
            Default::default();
        for row in rows.iter().filter(|r| r.reportable) {
            let nec = row.box_.as_deref() == Some("nec_1");
            by_vendor.entry((row.vendor_id, nec)).or_default().push(row);
            if row.missing.is_some() {
                warnings.push(format!(
                    "{}: {} missing",
                    row.vendor_code.as_deref().unwrap_or("a vendor"),
                    row.missing.as_deref().unwrap_or("")
                ));
            }
        }
        for ((_, nec), group) in &by_vendor {
            let r = group[0];
            let amount = |b: &str| {
                group
                    .iter()
                    .filter(|g| g.box_.as_deref() == Some(b))
                    .map(|g| g.amount)
                    .sum::<Decimal>()
            };
            let money = |d: Decimal| {
                if d.is_zero() {
                    String::new()
                } else {
                    format!("{:.2}", d.round_dp(2))
                }
            };
            let line: Vec<String> = match input.form {
                Form::Us1099 => vec![
                    input.year.to_string(),
                    if *nec { "1099-NEC" } else { "1099-MISC" }.into(),
                    f(&payer_tin),
                    csv_field(&payer_name),
                    f(&pa.as_ref().and_then(|a| a.street.clone())),
                    f(&pa.as_ref().and_then(|a| a.city.clone())),
                    f(&pa.as_ref().and_then(|a| a.region.clone())),
                    f(&pa.as_ref().and_then(|a| a.postcode.clone())),
                    f(&r.tin_type),
                    f(&r.tin),
                    f(&r.vendor_name),
                    f(&r.street),
                    f(&r.city),
                    f(&r.region),
                    f(&r.postcode),
                    f(&r.country_code),
                    money(amount("nec_1")),
                    money(amount("misc_1")),
                    money(amount("misc_2")),
                    money(amount("misc_3")),
                    money(amount("misc_6")),
                    money(amount("misc_10")),
                ],
                Form::CaT5018 => vec![
                    from.to_string(),
                    to.to_string(),
                    f(&payer_tin),
                    csv_field(&payer_name),
                    f(&pa.as_ref().and_then(|a| a.street.clone())),
                    f(&pa.as_ref().and_then(|a| a.city.clone())),
                    f(&pa.as_ref().and_then(|a| a.region.clone())),
                    f(&pa.as_ref().and_then(|a| a.postcode.clone())),
                    if r.tin_type.as_deref() == Some("SIN") {
                        "individual"
                    } else {
                        "business"
                    }
                    .into(),
                    if r.tin_type.as_deref() == Some("BN") {
                        f(&r.tin)
                    } else {
                        String::new()
                    },
                    if r.tin_type.as_deref() == Some("SIN") {
                        f(&r.tin)
                    } else {
                        String::new()
                    },
                    f(&r.vendor_name),
                    f(&r.street),
                    f(&r.city),
                    f(&r.region),
                    f(&r.postcode),
                    f(&r.country_code),
                    money(group.iter().map(|g| g.amount).sum()),
                ],
            };
            lines.push(line.join(","));
            count += 1;
        }
        let mut text = lines.join("\r\n");
        text.push_str("\r\n");
        let bytes = text.as_bytes().to_vec();
        let file = ExportFile::new(
            format!("{}_{}.csv", input.form.as_str(), input.year),
            "text/csv; charset=utf-8",
            text,
            bytes,
            count,
            warnings,
        );
        file.log(
            ctx,
            input.company_id,
            input.form.as_str(),
            Some((from, to)),
            false,
        )
        .await?;
        Ok(file)
    }
}
