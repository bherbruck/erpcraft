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
    new_id, round_money, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub struct GlModule;

impl Module for GlModule {
    fn name(&self) -> &'static str {
        "gl"
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
                .field(FieldMeta::new("rate", "Rate", FieldType::Decimal).required())
                .field(created())
                .order_by(&["from_currency", "to_currency", "rate_date"]),
            EntityMeta::new("gl_entry", "Ledger entry")
                .field(company())
                .field(FieldMeta::new("posting_id", "Posting", FieldType::Uuid))
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
                .field(FieldMeta::new("debit", "Debit", FieldType::Decimal))
                .field(FieldMeta::new("credit", "Credit", FieldType::Decimal))
                .field(FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                ))
                .field(FieldMeta::new(
                    "txn_debit",
                    "Debit (transaction currency)",
                    FieldType::Decimal,
                ))
                .field(FieldMeta::new(
                    "txn_credit",
                    "Credit (transaction currency)",
                    FieldType::Decimal,
                ))
                .field(FieldMeta::new(
                    "fx_rate",
                    "Exchange rate",
                    FieldType::Decimal,
                ))
                .field(FieldMeta::new("memo", "Memo", FieldType::Text))
                .field(created())
                .order_by(&["posting_date", "id"])
                .read_only()
                .company_field("company_id"),
        ]
    }

    fn ledgers(&self) -> Vec<Arc<dyn Ledger>> {
        vec![Arc::new(GeneralLedger)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(TrialBalance),
            command(SetPeriodStatus {
                name: "fiscal_period.close",
                description: "Close a fiscal period. Nothing can be posted into a closed period.",
                permission: "fiscal_period:close",
                status: "closed",
            }),
            command(SetPeriodStatus {
                name: "fiscal_period.reopen",
                description: "Reopen a closed fiscal period for posting.",
                permission: "fiscal_period:reopen",
                status: "open",
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
    let (base, base_decimals): (String, i16) = sqlx::query_as(
        "SELECT c.base_currency, cur.decimals FROM company c
         JOIN currency cur ON cur.code = c.base_currency WHERE c.id = $1",
    )
    .bind(posting.company_id)
    .fetch_one(&mut *conn)
    .await?;
    let base_decimals = base_decimals as u32;

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

    for (index, row) in rows.iter().enumerate() {
        sqlx::query(
            "INSERT INTO gl_entry (id, company_id, posting_id, line_no, source_line_id, account_id,
                posting_date, debit, credit, currency, txn_debit, txn_credit, fx_rate, memo)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(new_id())
        .bind(posting.company_id)
        .bind(posting.id)
        .bind(index as i32 + 1)
        .bind(row.line.source_line_id)
        .bind(row.line.account_id)
        .bind(posting.posting_date)
        .bind(row.debit)
        .bind(row.credit)
        .bind(&row.currency)
        .bind(row.line.debit)
        .bind(row.line.credit)
        .bind(row.rate)
        .bind(&row.line.memo)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

struct GeneralLedger;

#[async_trait]
impl Ledger for GeneralLedger {
    fn name(&self) -> &'static str {
        "gl"
    }

    /// Copy the original entries with debit and credit swapped. Amounts and
    /// rates are taken as recorded, never recomputed.
    async fn reverse(
        &self,
        conn: &mut PgConnection,
        original: &Posting,
        reversal: &Posting,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO gl_entry (id, company_id, posting_id, line_no, source_line_id, account_id,
                posting_date, debit, credit, currency, txn_debit, txn_credit, fx_rate, memo)
             SELECT gen_random_uuid(), company_id, $2, line_no, source_line_id, account_id,
                $3, credit, debit, currency, txn_credit, txn_debit, fx_rate, memo
             FROM gl_entry WHERE posting_id = $1",
        )
        .bind(original.id)
        .bind(reversal.id)
        .bind(reversal.posting_date)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }
}

// ------------------------------------------------------------ trial balance

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrialBalanceInput {
    company_id: Uuid,
    /// Include entries up to and including this date. Defaults to all.
    as_of: Option<NaiveDate>,
}

#[derive(Serialize, sqlx::FromRow)]
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

#[derive(Serialize)]
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
        .permission("gl_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: TrialBalanceInput) -> Result<TrialBalanceOutput> {
        ctx.require("gl_entry:read", input.company_id)?;
        let currency: String =
            sqlx::query_scalar("SELECT base_currency FROM company WHERE id = $1")
                .bind(input.company_id)
                .fetch_optional(&mut *ctx.conn)
                .await?
                .ok_or_else(|| Error::not_found("company"))?;
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
}

#[async_trait]
impl Command for SetPeriodStatus {
    type Input = PeriodInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(self.name, self.description).permission(self.permission)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PeriodInput) -> Result<Value> {
        // The row lock waits for postings into this period that are still
        // in flight; see gl_entry_before_insert.
        let company: Option<Uuid> =
            sqlx::query_scalar("SELECT company_id FROM fiscal_period WHERE id = $1 FOR UPDATE")
                .bind(input.id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        let company = company.ok_or_else(|| Error::not_found("fiscal_period"))?;
        ctx.require(self.permission, company)?;
        sqlx::query("UPDATE fiscal_period SET status = $2 WHERE id = $1")
            .bind(input.id)
            .bind(self.status)
            .execute(&mut *ctx.conn)
            .await?;
        Ok(json!({ "id": input.id, "status": self.status }))
    }
}
