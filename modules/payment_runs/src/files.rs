//! Bank files of a posted payment run: SEPA credit transfers (ISO 20022
//! pain.001.001.09) and NACHA ACH credits. Generated, never sent.

use chrono::{NaiveDate, Utc};
use erpcraft_kernel::document;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, PgConnection, Result, async_trait,
    schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PAYMENT_RUN, by_vendor, load_header, load_lines};

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Format {
    /// ISO 20022 pain.001.001.09, SEPA credit transfer, EUR.
    Sepa,
    /// NACHA ACH credits, USD.
    Nacha,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) enum Sec {
    /// Corporate credit or debit: to businesses.
    #[default]
    #[serde(rename = "CCD")]
    Ccd,
    /// Prearranged payment and deposit: to people.
    #[serde(rename = "PPD")]
    Ppd,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BankFileInput {
    /// Payment run id.
    id: Uuid,
    /// `sepa` or `nacha`.
    format: Format,
    /// For NACHA: the standard entry class, `CCD` (the default) or `PPD`.
    #[serde(default)]
    sec_code: Sec,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BankFileOutput {
    file_name: String,
    content_type: String,
    /// The file, as text.
    content: String,
    /// Credits in the file.
    count: usize,
    total: Decimal,
    currency: String,
}

/// One credit: a payment to a vendor.
struct Credit {
    end_to_end: String,
    vendor_code: String,
    vendor_name: String,
    amount: Decimal,
    references: String,
    account: erpcraft_banking::PartyAccount,
}

pub(crate) struct BankFile;

/// Text within a field of at most `max` characters.
fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

async fn credits(conn: &mut PgConnection, run: Uuid, lines: &[crate::Line]) -> Result<Vec<Credit>> {
    let mut out = vec![];
    for (vendor, lines) in by_vendor(lines) {
        let (code, name): (String, String) =
            sqlx::query_as("SELECT code, name FROM vendor WHERE id = $1")
                .bind(vendor)
                .fetch_one(&mut *conn)
                .await?;
        let account = erpcraft_banking::party_account(conn, "vendor", vendor)
            .await?
            .ok_or_else(|| {
                Error::validation(format!(
                    "vendor {code} has no bank account on record: add a party_bank_account"
                ))
            })?;
        let items: Vec<Uuid> = lines.iter().map(|l| l.open_item_id).collect();
        let refs: Vec<String> = sqlx::query_scalar(
            "SELECT COALESCE(NULLIF(vi.vendor_reference, ''), d.number, '')
             FROM open_item i JOIN posting p ON p.id = i.posting_id
             JOIN document d ON d.id = p.document_id
             LEFT JOIN vendor_invoice vi ON vi.document_id = d.id
             WHERE i.id = ANY ($1) ORDER BY i.doc_date, d.number",
        )
        .bind(&items)
        .fetch_all(&mut *conn)
        .await?;
        let payment = lines
            .first()
            .and_then(|l| l.payment_id)
            .ok_or_else(|| Error::internal("a posted run's line without its payment"))?;
        let number: Option<String> =
            sqlx::query_scalar("SELECT number FROM document WHERE id = $1")
                .bind(payment)
                .fetch_one(&mut *conn)
                .await?;
        out.push(Credit {
            end_to_end: number.unwrap_or_else(|| run.simple().to_string()),
            vendor_code: code,
            vendor_name: name,
            amount: lines.iter().map(|l| l.amount).sum(),
            references: refs
                .into_iter()
                .filter(|r| !r.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
            account,
        });
    }
    Ok(out)
}

#[async_trait]
impl Command for BankFile {
    type Input = BankFileInput;
    type Output = BankFileOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "payment_run.bank_file",
            "The bank file of a posted payment run, one credit per payment, to hand to the bank \
             (nothing is sent): `sepa`, an ISO 20022 pain.001.001.09 SEPA credit transfer in EUR \
             from the bank account's IBAN and BIC to each vendor's default bank account; \
             `nacha`, a NACHA ACH file of CCD (or PPD) credits in USD from the bank account's \
             routing number and ACH company ID to each vendor's routing and account number. The \
             invoices' references go with each credit. Takes the right to read the run and \
             vendors' bank accounts.",
        )
        .permission("payment_run:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: BankFileInput) -> Result<BankFileOutput> {
        let view = document::get(ctx, PAYMENT_RUN, input.id).await?;
        let doc = view.header;
        if !ctx.actor.can_any("party_bank_account:read") {
            return Err(Error::forbidden(
                "a bank file shows vendors' bank accounts: it takes party_bank_account:read",
            ));
        }
        if doc.status != "posted" {
            return Err(Error::state(
                "a bank file is written for a posted payment run",
            ));
        }
        let header = load_header(&mut *ctx.conn, doc.id)
            .await?
            .ok_or_else(|| Error::internal("a payment run without its header"))?;
        let lines = load_lines(&mut *ctx.conn, doc.id).await?;
        let credits = credits(&mut *ctx.conn, doc.id, &lines).await?;
        let own = erpcraft_banking::account_number(&mut *ctx.conn, header.bank_account_id)
            .await?
            .ok_or_else(|| {
                Error::validation(
                    "the run's bank account has no numbers on record: add its \
                     bank_account_number",
                )
            })?;
        let company: String = sqlx::query_scalar("SELECT name FROM company WHERE id = $1")
            .bind(doc.company_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
        let number = doc
            .number
            .clone()
            .unwrap_or_else(|| doc.id.simple().to_string());
        let total: Decimal = credits.iter().map(|c| c.amount).sum();
        let content = match input.format {
            Format::Sepa => sepa(
                &header.currency,
                doc.doc_date,
                &number,
                &company,
                &own,
                &credits,
            )?,
            Format::Nacha => nacha(
                &header.currency,
                doc.doc_date,
                &company,
                &own,
                &credits,
                input.sec_code,
            )?,
        };
        Ok(BankFileOutput {
            file_name: match input.format {
                Format::Sepa => format!("{number}.xml"),
                Format::Nacha => format!("{number}.ach"),
            },
            content_type: match input.format {
                Format::Sepa => "application/xml".into(),
                Format::Nacha => "text/plain".into(),
            },
            content,
            count: credits.len(),
            total,
            currency: header.currency,
        })
    }
}

fn sepa(
    currency: &str,
    date: NaiveDate,
    number: &str,
    company: &str,
    own: &erpcraft_banking::AccountNumber,
    credits: &[Credit],
) -> Result<String> {
    if currency != "EUR" {
        return Err(Error::validation(format!(
            "a SEPA credit transfer is in EUR; this run is in {currency}"
        )));
    }
    let iban = own
        .iban
        .as_deref()
        .ok_or_else(|| Error::validation("the run's bank account has no IBAN on record"))?;
    let total: Decimal = credits.iter().map(|c| c.amount).sum();
    let agent = |bic: Option<&str>| match bic {
        Some(bic) => format!("<FinInstnId><BICFI>{}</BICFI></FinInstnId>", xml(bic)),
        None => "<FinInstnId><Othr><Id>NOTPROVIDED</Id></Othr></FinInstnId>".into(),
    };
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.09\">\n");
    out.push_str("  <CstmrCdtTrfInitn>\n");
    out.push_str(&format!(
        "    <GrpHdr><MsgId>{}</MsgId><CreDtTm>{}</CreDtTm><NbOfTxs>{}</NbOfTxs><CtrlSum>{:.2}</CtrlSum><InitgPty><Nm>{}</Nm></InitgPty></GrpHdr>\n",
        xml(&cut(number, 35)),
        Utc::now().format("%Y-%m-%dT%H:%M:%S"),
        credits.len(),
        total,
        xml(&cut(company, 70)),
    ));
    out.push_str(&format!(
        "    <PmtInf><PmtInfId>{}</PmtInfId><PmtMtd>TRF</PmtMtd><NbOfTxs>{}</NbOfTxs><CtrlSum>{:.2}</CtrlSum>\
         <PmtTpInf><SvcLvl><Cd>SEPA</Cd></SvcLvl></PmtTpInf>\
         <ReqdExctnDt><Dt>{}</Dt></ReqdExctnDt>\
         <Dbtr><Nm>{}</Nm></Dbtr><DbtrAcct><Id><IBAN>{}</IBAN></Id></DbtrAcct>\
         <DbtrAgt>{}</DbtrAgt><ChrgBr>SLEV</ChrgBr>\n",
        xml(&cut(number, 35)),
        credits.len(),
        total,
        date.format("%Y-%m-%d"),
        xml(&cut(own.account_name.as_deref().unwrap_or(company), 70)),
        xml(iban),
        agent(own.bic.as_deref()),
    ));
    for credit in credits {
        let iban = credit.account.iban.as_deref().ok_or_else(|| {
            Error::validation(format!(
                "vendor {} has no IBAN on record: a SEPA credit transfer needs one",
                credit.vendor_code
            ))
        })?;
        let creditor_agent = credit
            .account
            .bic
            .as_deref()
            .map(|bic| {
                format!(
                    "<CdtrAgt><FinInstnId><BICFI>{}</BICFI></FinInstnId></CdtrAgt>",
                    xml(bic)
                )
            })
            .unwrap_or_default();
        let remittance = if credit.references.is_empty() {
            String::new()
        } else {
            format!(
                "<RmtInf><Ustrd>{}</Ustrd></RmtInf>",
                xml(&cut(&credit.references, 140))
            )
        };
        out.push_str(&format!(
            "      <CdtTrfTxInf><PmtId><EndToEndId>{}</EndToEndId></PmtId>\
             <Amt><InstdAmt Ccy=\"EUR\">{:.2}</InstdAmt></Amt>{}\
             <Cdtr><Nm>{}</Nm></Cdtr><CdtrAcct><Id><IBAN>{}</IBAN></Id></CdtrAcct>{}</CdtTrfTxInf>\n",
            xml(&cut(&credit.end_to_end, 35)),
            credit.amount,
            creditor_agent,
            xml(&cut(
                credit.account.account_name.as_deref().unwrap_or(&credit.vendor_name),
                70
            )),
            xml(iban),
            remittance,
        ));
    }
    out.push_str("    </PmtInf>\n  </CstmrCdtTrfInitn>\n</Document>\n");
    Ok(out)
}

/// A NACHA field: text left-justified (numbers right-justified with
/// zeros), cut or padded to its width, in capitals.
fn alpha(text: &str, width: usize) -> String {
    let text: String = text
        .to_uppercase()
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .take(width)
        .collect();
    format!("{text:<width$}")
}

fn numeric(value: u64, width: usize) -> String {
    let text = value.to_string();
    let text = &text[text.len().saturating_sub(width)..];
    format!("{text:0>width$}")
}

fn cents(amount: Decimal) -> Result<u64> {
    let cents = (amount * Decimal::ONE_HUNDRED).round();
    u64::try_from(cents.mantissa() / 10i128.pow(cents.scale()))
        .map_err(|_| Error::validation("an amount does not fit a NACHA entry"))
}

fn nacha(
    currency: &str,
    date: NaiveDate,
    company: &str,
    own: &erpcraft_banking::AccountNumber,
    credits: &[Credit],
    sec: Sec,
) -> Result<String> {
    if currency != "USD" {
        return Err(Error::validation(format!(
            "a NACHA file is in USD; this run is in {currency}"
        )));
    }
    let routing = own.routing_number.as_deref().ok_or_else(|| {
        Error::validation("the run's bank account has no routing number on record")
    })?;
    let company_id = own.ach_company_id.as_deref().ok_or_else(|| {
        Error::validation("the run's bank account has no ACH company ID on record")
    })?;
    let origin = own.ach_origin.as_deref().unwrap_or(company_id);
    let odfi = &routing[..8];
    let now = Utc::now();
    let mut records: Vec<String> = vec![];
    records.push(format!(
        "101 {routing}{origin:>10}{}{}A094101{}{}{}",
        now.format("%y%m%d"),
        now.format("%H%M"),
        alpha("", 23),
        alpha(company, 23),
        alpha("", 8),
    ));
    let sec_code = match sec {
        Sec::Ccd => "CCD",
        Sec::Ppd => "PPD",
    };
    records.push(format!(
        "5220{}{}{}{sec_code}{}{}{}   1{odfi}{}",
        alpha(company, 16),
        alpha("", 20),
        alpha(company_id, 10),
        alpha("VENDOR PAY", 10),
        alpha("", 6),
        date.format("%y%m%d"),
        numeric(1, 7),
    ));
    let mut hash: u64 = 0;
    let mut total: u64 = 0;
    let mut entries: u64 = 0;
    for (n, credit) in credits.iter().enumerate() {
        let to = credit.account.routing_number.as_deref().ok_or_else(|| {
            Error::validation(format!(
                "vendor {} has no routing number on record: an ACH credit needs one",
                credit.vendor_code
            ))
        })?;
        let account = credit.account.account_number.as_deref().ok_or_else(|| {
            Error::validation(format!(
                "vendor {} has no account number on record",
                credit.vendor_code
            ))
        })?;
        let amount = cents(credit.amount)?;
        let code = if credit.account.account_type == "savings" {
            "32"
        } else {
            "22"
        };
        let addenda = sec == Sec::Ccd && !credit.references.is_empty();
        let seq = n as u64 + 1;
        records.push(format!(
            "6{code}{to}{}{}{}{}  {}{odfi}{}",
            alpha(account, 17),
            numeric(amount, 10),
            alpha(&credit.vendor_code, 15),
            alpha(
                credit
                    .account
                    .account_name
                    .as_deref()
                    .unwrap_or(&credit.vendor_name),
                22
            ),
            if addenda { "1" } else { "0" },
            numeric(seq, 7),
        ));
        hash += to[..8].parse::<u64>().unwrap_or_default();
        total += amount;
        entries += 1;
        if addenda {
            records.push(format!(
                "705{}0001{}",
                alpha(&credit.references, 80),
                numeric(seq, 7)
            ));
            entries += 1;
        }
    }
    records.push(format!(
        "8220{}{}{}{}{}{}{}{odfi}{}",
        numeric(entries, 6),
        numeric(hash, 10),
        numeric(0, 12),
        numeric(total, 12),
        alpha(company_id, 10),
        alpha("", 19),
        alpha("", 6),
        numeric(1, 7),
    ));
    let count = records.len() + 1;
    let blocks = count.div_ceil(10);
    records.push(format!(
        "9{}{}{}{}{}{}{}",
        numeric(1, 6),
        numeric(blocks as u64, 6),
        numeric(entries, 8),
        numeric(hash, 10),
        numeric(0, 12),
        numeric(total, 12),
        alpha("", 39),
    ));
    while !records.len().is_multiple_of(10) {
        records.push("9".repeat(94));
    }
    if let Some(bad) = records.iter().find(|r| r.chars().count() != 94) {
        return Err(Error::internal(format!(
            "a NACHA record is {} characters, not 94",
            bad.chars().count()
        )));
    }
    Ok(records.join("\n") + "\n")
}
