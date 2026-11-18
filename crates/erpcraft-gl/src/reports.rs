//! Financial reports over the general ledger: balance sheet, income
//! statement and the ledger of one account. All are read-only commands and
//! report in the company's accounting currency.

use chrono::NaiveDate;
use erpcraft_kernel::document;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, PgConnection, Result, async_trait,
    company, schemars,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use uuid::Uuid;

const READ: &str = "gl_entry:read";
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 500;

#[derive(sqlx::FromRow)]
struct AccountBalance {
    account_id: Uuid,
    code: String,
    name: String,
    account_type: String,
    /// Debit minus credit.
    balance: Decimal,
}

/// Debit minus credit of every account of the company that has a balance
/// other than zero over the entries dated within `from..=to`, by code.
/// Either bound may be absent.
///
/// `with_closing` says whether closing entries count. A closing posting
/// moves the balances of the income and expense accounts to equity at the
/// end of a fiscal year. The ledger does not know the document that does
/// it; the convention is that the command which creates such a posting, or
/// reverses it, has a name starting with `year_end_close.`, which the
/// posting records. A balance sheet counts closing entries: they are what
/// puts a closed year's result into equity. An income statement leaves
/// them out, or a closed year would show nothing.
async fn balances(
    conn: &mut PgConnection,
    company_id: Uuid,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    with_closing: bool,
) -> Result<Vec<AccountBalance>> {
    Ok(sqlx::query_as(
        "SELECT a.id AS account_id, a.code, a.name, a.account_type,
                sum(g.debit) - sum(g.credit) AS balance
         FROM account a
         JOIN gl_entry g ON g.account_id = a.id
         JOIN posting p ON p.id = g.posting_id
         WHERE a.company_id = $1
           AND ($2::date IS NULL OR g.posting_date >= $2)
           AND ($3::date IS NULL OR g.posting_date <= $3)
           AND ($4 OR NOT starts_with(p.command, 'year_end_close.'))
         GROUP BY a.id
         HAVING sum(g.debit) <> sum(g.credit)
         ORDER BY a.code",
    )
    .bind(company_id)
    .bind(from)
    .bind(to)
    .bind(with_closing)
    .fetch_all(&mut *conn)
    .await?)
}

#[derive(Serialize, JsonSchema)]
struct ReportRow {
    account_id: Uuid,
    code: String,
    name: String,
    /// Positive in the natural direction of the section.
    balance: Decimal,
}

#[derive(Serialize, JsonSchema)]
struct Section {
    rows: Vec<ReportRow>,
    total: Decimal,
}

/// The accounts of one type. `debit_side` says whether the type grows with
/// debits (assets, expenses) or with credits (liabilities, equity, income).
fn section(balances: &[AccountBalance], account_type: &str, debit_side: bool) -> Section {
    let rows: Vec<ReportRow> = balances
        .iter()
        .filter(|b| b.account_type == account_type)
        .map(|b| ReportRow {
            account_id: b.account_id,
            code: b.code.clone(),
            name: b.name.clone(),
            balance: if debit_side { b.balance } else { -b.balance },
        })
        .collect();
    Section {
        total: rows.iter().map(|r| r.balance).sum(),
        rows,
    }
}

// ------------------------------------------------------------ balance sheet

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BalanceSheetInput {
    /// The company to report on.
    company_id: Uuid,
    /// Include entries up to and including this date. Defaults to all.
    as_of: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BalanceSheetOutput {
    company_id: Uuid,
    as_of: Option<NaiveDate>,
    currency: String,
    /// Balances are debit minus credit.
    assets: Section,
    /// Balances are credit minus debit.
    liabilities: Section,
    /// Balances are credit minus debit.
    equity: Section,
    /// Income minus expenses over all entries up to `as_of`, closing
    /// entries included: the result that has not been closed to an equity
    /// account. A closed year adds nothing to it.
    net_income: Decimal,
    total_assets: Decimal,
    /// Liabilities plus equity plus `net_income`. Equal to `total_assets`.
    total_liabilities_and_equity: Decimal,
}

pub(crate) struct BalanceSheet;

#[async_trait]
impl Command for BalanceSheet {
    type Input = BalanceSheetInput;
    type Output = BalanceSheetOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "gl.balance_sheet",
            "Balance sheet of a company as of a date (default: all entries), in its accounting \
             currency: asset, liability and equity accounts with their balances and section \
             totals, each shown as a positive number in the section's natural direction. \
             net_income is income minus expenses up to that date, not yet closed to equity: \
             the result of a year closed with a year-end close is inside the equity section; \
             total_assets equals total_liabilities_and_equity, which includes net_income. \
             Accounts with a zero balance are left out.",
        )
        .report("Balance sheet")
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: BalanceSheetInput) -> Result<BalanceSheetOutput> {
        ctx.require(READ, input.company_id)?;
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let balances = balances(&mut *ctx.conn, input.company_id, None, input.as_of, true).await?;
        let assets = section(&balances, "asset", true);
        let liabilities = section(&balances, "liability", false);
        let equity = section(&balances, "equity", false);
        let net_income =
            section(&balances, "income", false).total - section(&balances, "expense", true).total;
        Ok(BalanceSheetOutput {
            company_id: input.company_id,
            as_of: input.as_of,
            currency,
            net_income,
            total_assets: assets.total,
            total_liabilities_and_equity: liabilities.total + equity.total + net_income,
            assets,
            liabilities,
            equity,
        })
    }
}

// --------------------------------------------------------- income statement

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IncomeStatementInput {
    /// The company to report on.
    company_id: Uuid,
    /// First day of the range.
    from: NaiveDate,
    /// Last day of the range, included.
    to: NaiveDate,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct IncomeStatementOutput {
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    currency: String,
    /// Balances are credit minus debit.
    income: Section,
    /// Balances are debit minus credit.
    expenses: Section,
    /// Total income minus total expenses.
    net_income: Decimal,
}

pub(crate) struct IncomeStatement;

#[async_trait]
impl Command for IncomeStatement {
    type Input = IncomeStatementInput;
    type Output = IncomeStatementOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "gl.income_statement",
            "Income statement (profit and loss) of a company for the entries dated from `from` \
             to `to`, both days included, in its accounting currency: income accounts and \
             expense accounts with their amounts for the range as positive numbers, the total \
             of each, and net_income, which is total income minus total expenses. Accounts \
             with a zero amount are left out. The entries of a year-end close are not \
             counted, so a closed year still shows its income and expenses.",
        )
        .report("Income statement")
        .permission(READ)
    }

    async fn run(
        &self,
        ctx: &mut Ctx<'_>,
        input: IncomeStatementInput,
    ) -> Result<IncomeStatementOutput> {
        ctx.require(READ, input.company_id)?;
        if input.from > input.to {
            return Err(Error::validation("`from` cannot be after `to`"));
        }
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let balances = balances(
            &mut *ctx.conn,
            input.company_id,
            Some(input.from),
            Some(input.to),
            false,
        )
        .await?;
        let income = section(&balances, "income", false);
        let expenses = section(&balances, "expense", true);
        Ok(IncomeStatementOutput {
            company_id: input.company_id,
            from: input.from,
            to: input.to,
            currency,
            net_income: income.total - expenses.total,
            income,
            expenses,
        })
    }
}

// ----------------------------------------------------------- account ledger

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountLedgerInput {
    /// The company the account belongs to.
    company_id: Uuid,
    /// The account whose entries to list.
    account_id: Uuid,
    /// First posting date to list. Entries before it make up the opening
    /// balance. Defaults to the beginning.
    from: Option<NaiveDate>,
    /// Last posting date to list, included. Defaults to the end.
    to: Option<NaiveDate>,
    /// Entries per page: 50 by default, 500 at most.
    limit: Option<i64>,
    /// Entries to skip, for paging.
    offset: Option<i64>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct AccountLedgerRow {
    id: Uuid,
    posting_date: NaiveDate,
    debit: Decimal,
    credit: Decimal,
    /// The account's balance (debit minus credit) after this entry.
    balance: Decimal,
    memo: Option<String>,
    posting_id: Uuid,
    /// The memo of the posting and the document that caused the entry.
    /// All four are absent for a caller who may not read that document.
    posting_memo: Option<String>,
    document_id: Option<Uuid>,
    document_type: Option<String>,
    document_number: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct AccountLedgerOutput {
    company_id: Uuid,
    account_id: Uuid,
    code: String,
    name: String,
    currency: String,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    /// Debit minus credit of all entries before `from`.
    opening_balance: Decimal,
    /// The opening balance plus all entries in the range, not only those
    /// on this page.
    closing_balance: Decimal,
    /// Number of entries in the range.
    total: i64,
    limit: i64,
    offset: i64,
    entries: Vec<AccountLedgerRow>,
}

pub(crate) struct AccountLedger;

#[async_trait]
impl Command for AccountLedger {
    type Input = AccountLedgerInput;
    type Output = AccountLedgerOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "gl.account_ledger",
            "The entries of one account of a company, optionally from one posting date to \
             another (both included), ordered by posting date, in the accounting currency. \
             Each entry has its debit, credit, memo, its posting and the document that caused \
             it (id, type and number; left out, with the posting's memo, when the caller may \
             not read that document), and the account's running balance after it (debit minus \
             credit). opening_balance is the balance before `from`, closing_balance the balance \
             after the last entry of the whole range, total the number of entries in the range. \
             Page with limit (default 50, at most 500) and offset; the running balance is \
             correct on every page.",
        )
        .report("Account ledger")
        .permission(READ)
    }

    async fn run(
        &self,
        ctx: &mut Ctx<'_>,
        input: AccountLedgerInput,
    ) -> Result<AccountLedgerOutput> {
        ctx.require(READ, input.company_id)?;
        if let (Some(from), Some(to)) = (input.from, input.to)
            && from > to
        {
            return Err(Error::validation("`from` cannot be after `to`"));
        }
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let offset = input.offset.unwrap_or(0).max(0);
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let (code, name): (String, String) =
            sqlx::query_as("SELECT code, name FROM account WHERE id = $1 AND company_id = $2")
                .bind(input.account_id)
                .bind(input.company_id)
                .fetch_optional(&mut *ctx.conn)
                .await?
                .ok_or_else(|| Error::not_found("account"))?;

        // One statement, so that the page and the figures for the whole
        // range come from the same snapshot. The running balance is
        // computed over the whole range before the page is cut out of it.
        // The totals row comes first; it is there even for an empty page.
        let fetched = sqlx::query(
            "WITH opening AS (
                 SELECT COALESCE(sum(debit - credit), 0) AS opening_balance
                 FROM gl_entry
                 WHERE account_id = $1 AND $2::date IS NOT NULL AND posting_date < $2
             ),
             in_range AS (
                 SELECT g.id, g.posting_date, g.debit, g.credit, g.memo, g.posting_id,
                        sum(g.debit - g.credit) OVER (ORDER BY g.posting_date, g.id) AS running
                 FROM gl_entry g
                 WHERE g.account_id = $1
                   AND ($2::date IS NULL OR g.posting_date >= $2)
                   AND ($3::date IS NULL OR g.posting_date <= $3)
             ),
             totals AS (
                 SELECT COALESCE(sum(debit - credit), 0) AS range_balance, count(*) AS total
                 FROM in_range
             ),
             page AS (
                 SELECT * FROM in_range ORDER BY posting_date, id LIMIT $4 OFFSET $5
             )
             SELECT o.opening_balance, t.range_balance, t.total,
                    e.id, e.posting_date, e.debit, e.credit,
                    o.opening_balance + e.running AS balance,
                    e.memo, e.posting_id, p.memo AS posting_memo,
                    p.document_id, d.doc_type AS document_type, d.number AS document_number
             FROM opening o
             CROSS JOIN totals t
             LEFT JOIN page e ON true
             LEFT JOIN posting p ON p.id = e.posting_id
             LEFT JOIN document d ON d.id = p.document_id
             ORDER BY e.posting_date, e.id",
        )
        .bind(input.account_id)
        .bind(input.from)
        .bind(input.to)
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *ctx.conn)
        .await?;

        let first = fetched
            .first()
            .ok_or_else(|| Error::internal("the account ledger query returned no row"))?;
        let opening_balance: Decimal = first.try_get("opening_balance")?;
        let range_balance: Decimal = first.try_get("range_balance")?;
        let total: i64 = first.try_get("total")?;
        let mut entries = Vec::with_capacity(fetched.len());
        for row in &fetched {
            if row.try_get::<Option<Uuid>, _>("id")?.is_some() {
                entries.push(AccountLedgerRow::from_row(row)?);
            }
        }
        // The right to read an account's entries is not the right to read
        // every document that posted to it.
        let documents: Vec<Uuid> = entries.iter().filter_map(|e| e.document_id).collect();
        let readable = document::readable(ctx, &documents).await?;
        for entry in &mut entries {
            if !entry.document_id.is_some_and(|id| readable.contains(&id)) {
                entry.posting_memo = None;
                entry.document_id = None;
                entry.document_type = None;
                entry.document_number = None;
            }
        }
        Ok(AccountLedgerOutput {
            company_id: input.company_id,
            account_id: input.account_id,
            code,
            name,
            currency,
            from: input.from,
            to: input.to,
            opening_balance,
            closing_balance: opening_balance + range_balance,
            total,
            limit,
            offset,
            entries,
        })
    }
}
