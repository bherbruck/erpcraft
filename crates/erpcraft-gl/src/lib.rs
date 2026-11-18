//! The general ledger.
//!
//! Other modules write to it with [`post`], under a posting the kernel
//! opened for one of their documents. The ledger's rules (entries balance,
//! the period is open, the account can be posted to, amounts are correctly
//! converted and rounded, nothing is ever edited) are enforced by database
//! triggers, so they hold for every module.

use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DynCommand, EntityMeta, Error, FieldMeta, FieldType,
    JsonSchema, Ledger, Migration, Module, PgConnection, Posting, Result, async_trait, command,
    company, new_id, round_money, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod reports;
mod rules;

pub use rules::account_for;

pub struct GlModule;

impl Module for GlModule {
    fn name(&self) -> &'static str {
        "gl"
    }

    fn label(&self) -> String {
        "General ledger".into()
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "gl",
                sql: include_str!("../migrations/0001_gl.sql"),
            },
            Migration {
                version: 2,
                name: "exchange_rate",
                sql: include_str!("../migrations/0002_exchange_rate.sql"),
            },
            Migration {
                version: 3,
                name: "account_rule",
                sql: include_str!("../migrations/0003_account_rule.sql"),
            },
            Migration {
                version: 4,
                name: "period_guard",
                sql: include_str!("../migrations/0004_period_guard.sql"),
            },
            Migration {
                version: 5,
                name: "entry_check_per_statement",
                sql: include_str!("../migrations/0005_entry_check_per_statement.sql"),
            },
            Migration {
                version: 6,
                name: "account_bank",
                sql: include_str!("../migrations/0006_account_bank.sql"),
            },
            Migration {
                version: 7,
                name: "account_currency",
                sql: include_str!("../migrations/0007_account_currency.sql"),
            },
            Migration {
                version: 8,
                name: "entry_amount",
                sql: include_str!("../migrations/0008_entry_amount.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let company = || {
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable()
        };
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        vec![
            EntityMeta::new("account", "Account")
                .field(company())
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new(
                        "account_type",
                        "Type",
                        FieldType::options(&["asset", "liability", "equity", "income", "expense"]),
                    )
                    .required(),
                )
                .field(FieldMeta::new(
                    "parent_id",
                    "Parent account",
                    FieldType::link("account"),
                ))
                .field(FieldMeta::new("is_group", "Group account", FieldType::Bool))
                .field(
                    FieldMeta::new("is_bank", "Bank or cash account", FieldType::Bool)
                        .help("Money is paid from or into it: payments offer only these."),
                )
                .field(
                    FieldMeta::new(
                        "currency",
                        "Currency",
                        FieldType::link_by("currency", "code"),
                    )
                    .help(
                        "For a bank account that holds another currency than the company's: \
                         payments through it are in this currency.",
                    ),
                )
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .company_field("company_id"),
            EntityMeta::new("fiscal_period", "Fiscal period")
                .field(company())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new("start_date", "First day", FieldType::Date).required())
                .field(FieldMeta::new("end_date", "Last day", FieldType::Date).required())
                // Changed only by fiscal_period.close and fiscal_period.reopen.
                .field(
                    FieldMeta::new("status", "Status", FieldType::options(&["open", "closed"]))
                        .readonly(),
                )
                .field(created())
                .order_by(&["start_date"])
                .search(&["name"])
                .state_field("status")
                .company_field("company_id"),
            // Shared by all companies, so changing rates needs a grant for
            // all companies.
            EntityMeta::new("exchange_rate", "Exchange rate")
                .field(
                    FieldMeta::new(
                        "from_currency",
                        "From",
                        FieldType::link_by("currency", "code"),
                    )
                    .required()
                    .immutable(),
                )
                .field(
                    FieldMeta::new("to_currency", "To", FieldType::link_by("currency", "code"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("rate_date", "Applies from", FieldType::Date)
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("rate", "Rate", FieldType::Decimal)
                        .rate(10)
                        .required(),
                )
                .field(created())
                .order_by(&["from_currency", "to_currency", "rate_date"])
                .display(&["from_currency", "to_currency", "rate_date"]),
            rules::entity(),
            EntityMeta::new("gl_entry", "Ledger entry")
                .field(company())
                .field(FieldMeta::new("posting_id", "Posting", FieldType::Posting))
                .field(FieldMeta::new("line_no", "Line", FieldType::Int))
                .field(FieldMeta::new(
                    "source_line_id",
                    "Source line",
                    FieldType::Uuid,
                ))
                .field(FieldMeta::new(
                    "account_id",
                    "Account",
                    FieldType::link("account"),
                ))
                .field(FieldMeta::new(
                    "posting_date",
                    "Posting date",
                    FieldType::Date,
                ))
                .field(FieldMeta::new("debit", "Debit", FieldType::Decimal).money())
                .field(FieldMeta::new("credit", "Credit", FieldType::Decimal).money())
                .field(FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                ))
                .field(
                    FieldMeta::new(
                        "txn_debit",
                        "Debit (transaction currency)",
                        FieldType::Decimal,
                    )
                    .money()
                    .currency_in("currency"),
                )
                .field(
                    FieldMeta::new(
                        "txn_credit",
                        "Credit (transaction currency)",
                        FieldType::Decimal,
                    )
                    .money()
                    .currency_in("currency"),
                )
                .field(FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal).rate(10))
                .field(FieldMeta::new("memo", "Memo", FieldType::Text))
                .field(
                    FieldMeta::new("amount", "Amount", FieldType::Decimal)
                        .money()
                        .readonly()
                        .help("Debit less credit: a credit is below zero."),
                )
                .field(created())
                .order_by(&["posting_date", "id"])
                // A reference to an entry: its day, amount and memo.
                .display(&["posting_date", "amount", "memo"])
                .read_only()
                .company_field("company_id"),
        ]
    }

    fn policies(&self) -> Vec<erpcraft_kernel::PolicyPoint> {
        vec![
            erpcraft_kernel::PolicyPoint::new(
                RATE_DIFFERS,
                "Asked when a document in a foreign currency states its own exchange rate and a \
                 rate is on record for its date: may it post at the stated rate? Unless \
                 configured otherwise any stated rate is accepted. A company that wants rates \
                 typed by hand checked refuses them beyond a limit, for example when \
                 `facts.difference_percent > 2 || facts.difference_percent < -2`.",
            )
            .text("currency", "The document's currency")
            .text(
                "doc_type",
                "The type of the document: purchase_receipt, vendor_invoice, payment, ...",
            )
            .number("stated", "The rate stated on the document")
            .as_rate()
            .number("on_record", "The rate on record for the document's date")
            .as_rate()
            .number(
                "difference_percent",
                "stated minus on_record, as a percentage of on_record",
            )
            .as_percent()
            .refusal("the stated exchange rate differs too much from the rate on record"),
        ]
    }

    fn ledgers(&self) -> Vec<Arc<dyn Ledger>> {
        vec![Arc::new(GeneralLedger)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(TrialBalance),
            command(reports::BalanceSheet),
            command(reports::IncomeStatement),
            command(reports::AccountLedger),
            command(SetPeriodStatus {
                name: "fiscal_period.close",
                description: "Close a fiscal period. Nothing can be posted into a closed period.",
                permission: "fiscal_period:close",
                status: "closed",
                from: &["open"],
            }),
            command(SetPeriodStatus {
                name: "fiscal_period.reopen",
                description: "Reopen a closed fiscal period for posting.",
                permission: "fiscal_period:reopen",
                status: "open",
                from: &["closed"],
            }),
        ]
    }
}

/// One entry to write. Give the amount on one side, in `currency`.
#[derive(Debug, Clone)]
pub struct GlLine {
    pub account_id: Uuid,
    pub debit: Decimal,
    pub credit: Decimal,
    /// Defaults to the company's accounting currency.
    pub currency: Option<String>,
    /// Accounting-currency units per unit of `currency`. When absent for a
    /// foreign currency, the latest rate on record dated on or before the
    /// posting date is used. Either way it is captured on the entry and
    /// never recomputed.
    pub fx_rate: Option<Decimal>,
    pub memo: Option<String>,
    /// The document line this entry comes from.
    pub source_line_id: Option<Uuid>,
}

/// The rate that converts `currency` into the accounting currency of
/// `company_id` on `date`: 1 for the accounting currency itself, else the
/// latest rate on record dated on or before `date`.
pub async fn rate_on(
    conn: &mut PgConnection,
    company_id: Uuid,
    currency: &str,
    date: chrono::NaiveDate,
) -> Result<Decimal> {
    let base = company::base_currency(conn, company_id).await?;
    if currency == base {
        return Ok(Decimal::ONE);
    }
    let on_record: Option<Decimal> = sqlx::query_scalar(
        "SELECT rate FROM exchange_rate
         WHERE from_currency = $1 AND to_currency = $2 AND rate_date <= $3
         ORDER BY rate_date DESC LIMIT 1",
    )
    .bind(currency)
    .bind(&base)
    .bind(date)
    .fetch_optional(&mut *conn)
    .await?;
    on_record.ok_or_else(|| {
        Error::validation(format!(
            "an exchange rate from {currency} to {base} is required: state one, or record one \
             that applies on {date}"
        ))
    })
}

/// The line that makes `lines` balance in the accounting currency, when
/// lines in other currencies leave them apart: a realized exchange
/// difference, debited to the account for the purpose `fx_loss` or
/// credited to the one for `fx_gain` (each falling back to the account for
/// `fx_difference`). `None` when they balance. Every line in a foreign
/// currency must state its rate.
pub async fn exchange_difference(
    conn: &mut PgConnection,
    company_id: Uuid,
    lines: &[GlLine],
    memo: Option<&str>,
) -> Result<Option<GlLine>> {
    let decimals = company::currency_decimals(conn, company_id).await?;
    let mut difference = Decimal::ZERO;
    for line in lines {
        let rate = line.fx_rate.unwrap_or(Decimal::ONE);
        difference +=
            round_money(line.debit * rate, decimals) - round_money(line.credit * rate, decimals);
    }
    if difference.is_zero() {
        return Ok(None);
    }
    // More debited than credited: what is missing is a gain, else a loss.
    let purpose = if difference > Decimal::ZERO {
        "fx_gain"
    } else {
        "fx_loss"
    };
    let account = match account_for(conn, company_id, purpose, &[]).await {
        Ok(account) => account,
        Err(_) => account_for(conn, company_id, "fx_difference", &[])
            .await
            .map_err(|_| {
                Error::validation(format!(
                    "this company has no account for exchange differences: add an account rule \
                     for the purpose '{purpose}' or 'fx_difference'"
                ))
            })?,
    };
    Ok(Some(GlLine {
        account_id: account,
        debit: (-difference).max(Decimal::ZERO),
        credit: difference.max(Decimal::ZERO),
        currency: None,
        fx_rate: None,
        memo: memo.map(str::to_owned),
        source_line_id: None,
    }))
}

/// The policy point asked by [`document_rate`].
pub const RATE_DIFFERS: &str = "fx.rate_differs";

/// The rate a document of `doc_type` in `currency`, dated `date`, posts
/// at: the rate it states, or else the rate on record. A stated rate is
/// checked against the rate on record by the company's policy
/// [`RATE_DIFFERS`] (by default it is accepted). In the accounting
/// currency the rate is 1, and a stated rate other than 1 is refused.
pub async fn document_rate(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    currency: &str,
    date: NaiveDate,
    stated: Option<Decimal>,
    doc_type: &str,
) -> Result<Decimal> {
    let base = company::base_currency(&mut *ctx.conn, company_id).await?;
    if currency == base {
        if stated.is_some_and(|rate| rate != Decimal::ONE) {
            return Err(
                Error::validation(format!("the exchange rate for {base} must be 1"))
                    .with_field("fx_rate"),
            );
        }
        return Ok(Decimal::ONE);
    }
    let Some(stated) = stated else {
        return rate_on(&mut *ctx.conn, company_id, currency, date).await;
    };
    if stated <= Decimal::ZERO || stated.normalize().scale() > 10 {
        return Err(Error::validation(
            "an exchange rate is more than zero, with at most 10 decimal places",
        )
        .with_field("fx_rate"));
    }
    if let Ok(on_record) = rate_on(&mut *ctx.conn, company_id, currency, date).await {
        let difference = ((stated - on_record) * Decimal::ONE_HUNDRED / on_record).round_dp(6);
        let facts = json!({
            "currency": currency,
            "doc_type": doc_type,
            "stated": stated.normalize().to_string(),
            "on_record": on_record.normalize().to_string(),
            "difference_percent": difference.normalize().to_string(),
        });
        ctx.decide(RATE_DIFFERS, company_id, facts)
            .await
            .map_err(|e| e.with_field("fx_rate"))?;
    }
    Ok(stated)
}

/// Why `account_id` cannot be the account of a plain document line (a
/// service, rent, a charge), or `None` when it can: it must belong to the
/// company and take postings, and must not be an account another ledger
/// keeps in step with its own entries (a bank or cash account, payables,
/// receivables, goods received not invoiced, inventory, a tax account),
/// which a plain line would move without them.
pub async fn line_account_refusal(
    conn: &mut PgConnection,
    company_id: Uuid,
    account_id: Uuid,
) -> Result<Option<String>> {
    let found: Option<(String, bool, bool)> = sqlx::query_as(
        "SELECT code, is_group, is_bank FROM account WHERE id = $1 AND company_id = $2",
    )
    .bind(account_id)
    .bind(company_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((code, group, bank)) = found else {
        return Ok(Some(
            "the account must belong to the document's company".into(),
        ));
    };
    if group {
        return Ok(Some(format!(
            "account {code} is a group account and cannot be posted to"
        )));
    }
    if bank {
        return Ok(Some(format!(
            "account {code} is a bank or cash account: money moves through it with payments"
        )));
    }
    // The tables of other modules, where they are installed.
    let kept = [
        (
            "purchasing_settings",
            "payable_account_id",
            "the payables account",
        ),
        (
            "purchasing_settings",
            "grni_account_id",
            "the goods received not invoiced account",
        ),
        (
            "sales_settings",
            "receivable_account_id",
            "the receivables account",
        ),
        ("tax_settings", "collected_account_id", "a tax account"),
        ("tax_settings", "paid_account_id", "a tax account"),
    ];
    for (table, column, what) in kept {
        let installed: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&mut *conn)
            .await?;
        if !installed {
            continue;
        }
        let used: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {table} WHERE company_id = $1 AND {column} = $2)"
        ))
        .bind(company_id)
        .bind(account_id)
        .fetch_one(&mut *conn)
        .await?;
        if used {
            return Ok(Some(format!(
                "account {code} is {what}, which only its own ledger posts to"
            )));
        }
    }
    let inventory: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM account_rule
                        WHERE company_id = $1 AND purpose = 'inventory' AND account_id = $2)",
    )
    .bind(company_id)
    .bind(account_id)
    .fetch_one(&mut *conn)
    .await?;
    if inventory {
        return Ok(Some(format!(
            "account {code} is an inventory account, which only the stock ledger posts to"
        )));
    }
    Ok(None)
}

/// The minor units of a currency: 2 for EUR, 0 for JPY.
pub async fn currency_decimals(conn: &mut PgConnection, currency: &str) -> Result<u32> {
    let decimals: Option<i16> = sqlx::query_scalar("SELECT decimals FROM currency WHERE code = $1")
        .bind(currency)
        .fetch_optional(&mut *conn)
        .await?;
    decimals
        .map(|d| d as u32)
        .ok_or_else(|| Error::validation(format!("unknown currency {currency}")))
}

/// Write the general-ledger entries of a posting.
///
/// Amounts are converted to the company's accounting currency and must
/// balance there. A posting whose foreign amounts leave a rounding
/// difference needs an explicit line for it.
pub async fn post(conn: &mut PgConnection, posting: &Posting, lines: &[GlLine]) -> Result<()> {
    if lines.is_empty() {
        return Err(Error::validation(
            "a posting needs at least one ledger entry",
        ));
    }
    let base = company::base_currency(conn, posting.company_id).await?;
    let base_decimals = company::currency_decimals(conn, posting.company_id).await?;

    struct Row<'a> {
        line: &'a GlLine,
        currency: String,
        rate: Decimal,
        debit: Decimal,
        credit: Decimal,
    }
    let mut rows = Vec::with_capacity(lines.len());
    let (mut total_debit, mut total_credit) = (Decimal::ZERO, Decimal::ZERO);

    for (index, line) in lines.iter().enumerate() {
        let n = index + 1;
        let fail = |msg: String| Error::validation(format!("line {n}: {msg}"));
        if line.debit.is_sign_negative() || line.credit.is_sign_negative() {
            return Err(fail("amounts cannot be negative".into()));
        }
        if line.debit.is_zero() == line.credit.is_zero() {
            return Err(fail(
                "give an amount on exactly one side, debit or credit".into(),
            ));
        }
        let currency = line.currency.clone().unwrap_or_else(|| base.clone());
        let (rate, decimals) = if currency == base {
            if line.fx_rate.is_some_and(|r| r != Decimal::ONE) {
                return Err(fail(format!("the exchange rate for {base} must be 1")));
            }
            (Decimal::ONE, base_decimals)
        } else {
            let decimals: Option<i16> =
                sqlx::query_scalar("SELECT decimals FROM currency WHERE code = $1")
                    .bind(&currency)
                    .fetch_optional(&mut *conn)
                    .await?;
            let decimals = decimals.ok_or_else(|| fail(format!("unknown currency {currency}")))?;
            let rate = match line.fx_rate {
                Some(rate) => rate,
                None => {
                    let on_record: Option<Decimal> = sqlx::query_scalar(
                        "SELECT rate FROM exchange_rate
                         WHERE from_currency = $1 AND to_currency = $2 AND rate_date <= $3
                         ORDER BY rate_date DESC LIMIT 1",
                    )
                    .bind(&currency)
                    .bind(&base)
                    .bind(posting.posting_date)
                    .fetch_optional(&mut *conn)
                    .await?;
                    on_record.ok_or_else(|| {
                        fail(format!(
                            "an exchange rate from {currency} to {base} is required: state one, \
                             or record one that applies on {}",
                            posting.posting_date
                        ))
                    })?
                }
            };
            if rate <= Decimal::ZERO {
                return Err(fail("the exchange rate must be positive".into()));
            }
            // The rate is stored with 10 decimals. A finer one would be
            // rounded on the entry and no longer explain its own amount.
            if rate.normalize().scale() > 10 {
                return Err(fail(
                    "an exchange rate has at most 10 decimal places".into(),
                ));
            }
            (rate, decimals as u32)
        };
        if line.debit != round_money(line.debit, decimals)
            || line.credit != round_money(line.credit, decimals)
        {
            return Err(fail(format!(
                "{currency} amounts have at most {decimals} decimal places"
            )));
        }
        let debit = round_money(line.debit * rate, base_decimals);
        let credit = round_money(line.credit * rate, base_decimals);
        total_debit += debit;
        total_credit += credit;
        rows.push(Row {
            line,
            currency,
            rate,
            debit,
            credit,
        });
    }

    if total_debit != total_credit {
        return Err(Error::validation(format!(
            "the entries do not balance: debit {total_debit} {base} is not equal to credit {total_credit} {base}"
        )));
    }

    // One statement for all entries. The ledger's row triggers still check
    // each of them, in line order.
    let ids: Vec<Uuid> = rows.iter().map(|_| new_id()).collect();
    let line_nos: Vec<i32> = (1..=rows.len() as i32).collect();
    let sources: Vec<Option<Uuid>> = rows.iter().map(|r| r.line.source_line_id).collect();
    let accounts: Vec<Uuid> = rows.iter().map(|r| r.line.account_id).collect();
    let debits: Vec<Decimal> = rows.iter().map(|r| r.debit).collect();
    let credits: Vec<Decimal> = rows.iter().map(|r| r.credit).collect();
    let currencies: Vec<&str> = rows.iter().map(|r| r.currency.as_str()).collect();
    let txn_debits: Vec<Decimal> = rows.iter().map(|r| r.line.debit).collect();
    let txn_credits: Vec<Decimal> = rows.iter().map(|r| r.line.credit).collect();
    let rates: Vec<Decimal> = rows.iter().map(|r| r.rate).collect();
    let memos: Vec<Option<&str>> = rows.iter().map(|r| r.line.memo.as_deref()).collect();
    sqlx::query(
        "INSERT INTO gl_entry (id, company_id, posting_id, line_no, source_line_id, account_id,
            posting_date, debit, credit, currency, txn_debit, txn_credit, fx_rate, memo)
         SELECT r.id, $1, $2, r.line_no, r.source_line_id, r.account_id,
            $3, r.debit, r.credit, r.currency, r.txn_debit, r.txn_credit, r.fx_rate, r.memo
         FROM unnest($4::uuid[], $5::int4[], $6::uuid[], $7::uuid[], $8::numeric[], $9::numeric[],
                     $10::text[], $11::numeric[], $12::numeric[], $13::numeric[], $14::text[])
            AS r(id, line_no, source_line_id, account_id, debit, credit,
                 currency, txn_debit, txn_credit, fx_rate, memo)
         ORDER BY r.line_no",
    )
    .bind(posting.company_id)
    .bind(posting.id)
    .bind(posting.posting_date)
    .bind(&ids)
    .bind(&line_nos)
    .bind(&sources)
    .bind(&accounts)
    .bind(&debits)
    .bind(&credits)
    .bind(&currencies)
    .bind(&txn_debits)
    .bind(&txn_credits)
    .bind(&rates)
    .bind(&memos)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

struct GeneralLedger;

/// One general-ledger entry as a document shows it under its posting.
/// Amounts are decimals as text. The account's code and name are absent
/// for a reader who may not read accounts.
#[derive(JsonSchema)]
#[allow(dead_code)] // describes the rows `entries` builds in SQL
struct GlEntryRow {
    /// Entry id.
    id: Option<Uuid>,
    /// Position within the posting, from 1.
    line_no: Option<i32>,
    /// The document line the entry comes from.
    source_line_id: Option<Uuid>,
    account_id: Option<Uuid>,
    /// Account code.
    account_code: Option<String>,
    /// Account name.
    account_name: Option<String>,
    /// Debit in the accounting currency.
    debit: Option<String>,
    /// Credit in the accounting currency.
    credit: Option<String>,
    /// The transaction currency.
    currency: Option<String>,
    /// Debit in the transaction currency.
    txn_debit: Option<String>,
    /// Credit in the transaction currency.
    txn_credit: Option<String>,
    /// Accounting-currency units per unit of the transaction currency.
    fx_rate: Option<String>,
    memo: Option<String>,
}

#[async_trait]
impl Ledger for GeneralLedger {
    fn name(&self) -> &'static str {
        "gl"
    }

    fn label(&self) -> String {
        "General ledger".into()
    }

    fn entry_schema(&self) -> Value {
        erpcraft_kernel::document::row_schema::<GlEntryRow>()
    }

    fn entry_tables(&self) -> &'static [&'static str] {
        &["gl_entry"]
    }

    /// Copy the original entries with debit and credit swapped. Amounts and
    /// rates are taken as recorded, never recomputed.
    async fn reverse(
        &self,
        conn: &mut PgConnection,
        original: &Posting,
        reversal: &Posting,
    ) -> Result<()> {
        // Each reversing entry gets a time-ordered id, in the order of the
        // original lines, so that lists by date and id show it after
        // everything recorded before it.
        let originals: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM gl_entry WHERE posting_id = $1 ORDER BY line_no")
                .bind(original.id)
                .fetch_all(&mut *conn)
                .await?;
        let ids: Vec<Uuid> = originals.iter().map(|_| new_id()).collect();
        sqlx::query(
            "INSERT INTO gl_entry (id, company_id, posting_id, line_no, source_line_id, account_id,
                posting_date, debit, credit, currency, txn_debit, txn_credit, fx_rate, memo)
             SELECT m.new_id, e.company_id, $2, e.line_no, e.source_line_id, e.account_id,
                $3, e.credit, e.debit, e.currency, e.txn_credit, e.txn_debit, e.fx_rate, e.memo
             FROM unnest($1::uuid[], $4::uuid[]) AS m(old_id, new_id)
             JOIN gl_entry e ON e.id = m.old_id
             ORDER BY e.line_no",
        )
        .bind(&originals)
        .bind(reversal.id)
        .bind(reversal.posting_date)
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// The entries of the posting with their accounts. The code and name
    /// of an account come only with the right to read accounts.
    async fn entries(&self, ctx: &mut Ctx<'_>, posting: &Posting) -> Result<Option<Vec<Value>>> {
        const READ: &str = "gl_entry:read";
        let actor = ctx.actor;
        if !actor.can(READ, posting.company_id) {
            return Ok(None);
        }
        let accounts = actor.can("account:read", posting.company_id)
            && !["code", "name"]
                .iter()
                .any(|field| actor.may_withhold("account:read", field));
        let mut rows: Vec<Value> = sqlx::query_scalar(
            "SELECT jsonb_build_object(
                'id', g.id, 'line_no', g.line_no, 'source_line_id', g.source_line_id,
                'account_id', g.account_id,
                'account_code', CASE WHEN $2 THEN a.code END,
                'account_name', CASE WHEN $2 THEN a.name END,
                'debit', g.debit::text, 'credit', g.credit::text,
                'currency', g.currency,
                'txn_debit', g.txn_debit::text, 'txn_credit', g.txn_credit::text,
                'fx_rate', g.fx_rate::text, 'memo', g.memo)
             FROM gl_entry g JOIN account a ON a.id = g.account_id
             WHERE g.posting_id = $1 ORDER BY g.line_no",
        )
        .bind(posting.id)
        .bind(accounts)
        .fetch_all(&mut *ctx.conn)
        .await?;
        erpcraft_kernel::document::withhold(actor, READ, &mut rows);
        Ok(Some(rows))
    }
}

// ------------------------------------------------------------ trial balance

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrialBalanceInput {
    /// The company to report on.
    company_id: Uuid,
    /// Include entries up to and including this date. Defaults to all.
    as_of: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct TrialBalanceRow {
    account_id: Uuid,
    code: String,
    name: String,
    account_type: String,
    debit: Decimal,
    credit: Decimal,
    /// Debit minus credit.
    balance: Decimal,
}

#[derive(Serialize, JsonSchema)]
struct TrialBalanceOutput {
    company_id: Uuid,
    as_of: Option<NaiveDate>,
    currency: String,
    total_debit: Decimal,
    total_credit: Decimal,
    rows: Vec<TrialBalanceRow>,
}

struct TrialBalance;

#[async_trait]
impl Command for TrialBalance {
    type Input = TrialBalanceInput;
    type Output = TrialBalanceOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "gl.trial_balance",
            "Total debit, credit and balance of every account of a company, in its accounting currency.",
        )
        .report("Trial balance")
        .permission("gl_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: TrialBalanceInput) -> Result<TrialBalanceOutput> {
        ctx.require("gl_entry:read", input.company_id)?;
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let rows: Vec<TrialBalanceRow> = sqlx::query_as(
            "SELECT a.id AS account_id, a.code, a.name, a.account_type,
                    COALESCE(sum(g.debit), 0) AS debit,
                    COALESCE(sum(g.credit), 0) AS credit,
                    COALESCE(sum(g.debit), 0) - COALESCE(sum(g.credit), 0) AS balance
             FROM account a
             LEFT JOIN gl_entry g ON g.account_id = a.id AND ($2::date IS NULL OR g.posting_date <= $2)
             WHERE a.company_id = $1 AND NOT a.is_group
             GROUP BY a.id ORDER BY a.code",
        )
        .bind(input.company_id)
        .bind(input.as_of)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(TrialBalanceOutput {
            company_id: input.company_id,
            as_of: input.as_of,
            currency,
            total_debit: rows.iter().map(|r| r.debit).sum(),
            total_credit: rows.iter().map(|r| r.credit).sum(),
            rows,
        })
    }
}

// ------------------------------------------------------------------ periods

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PeriodInput {
    /// Fiscal period id.
    id: Uuid,
}

struct SetPeriodStatus {
    name: &'static str,
    description: &'static str,
    permission: &'static str,
    status: &'static str,
    /// The status the period must be in for the command to change it.
    from: &'static [&'static str],
}

#[async_trait]
impl Command for SetPeriodStatus {
    type Input = PeriodInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(self.name, self.description)
            .permission(self.permission)
            .from_states(self.from)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PeriodInput) -> Result<Value> {
        // The row lock waits for postings into this period that are still
        // in flight; see gl_entry_check.
        let company: Option<Uuid> =
            sqlx::query_scalar("SELECT company_id FROM fiscal_period WHERE id = $1 FOR UPDATE")
                .bind(input.id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        let company = company.ok_or_else(|| Error::not_found("fiscal_period"))?;
        // Closing and reopening belong in the period's history.
        ctx.touch("fiscal_period", input.id);
        ctx.require(self.permission, company)?;
        sqlx::query("UPDATE fiscal_period SET status = $2 WHERE id = $1")
            .bind(input.id)
            .bind(self.status)
            .execute(&mut *ctx.conn)
            .await?;
        Ok(json!({ "id": input.id, "status": self.status }))
    }
}
