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
    dimension_value_id: Option<Uuid>,
    dimension_value_code: Option<String>,
    dimension_value_name: Option<String>,
}

/// Which entries a report counts by dimension, and by which dimension it
/// groups them.
#[derive(Clone, Copy)]
pub(crate) struct ByDimension<'a> {
    pub values: &'a [Uuid],
    pub group_by: Option<Uuid>,
}

/// Debit minus credit of every account of the company that has a balance
/// other than zero over the entries dated within `from..=to`, by code.
/// Either bound may be absent. With `by.group_by`, an account has a row
/// per value of that dimension its entries carry (and one for entries
/// without one); only entries within `by.values` count.
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
    by: ByDimension<'_>,
) -> Result<Vec<AccountBalance>> {
    Ok(sqlx::query_as(&format!(
        "SELECT a.id AS account_id, a.code, a.name, a.account_type,
                sum(g.debit) - sum(g.credit) AS balance,
                gv.value_id AS dimension_value_id,
                v.code AS dimension_value_code, v.name AS dimension_value_name
         FROM account a
         JOIN gl_entry g ON g.account_id = a.id
         JOIN posting p ON p.id = g.posting_id
         LEFT JOIN dimension_set_value gv
             ON gv.set_id = g.dimension_set_id AND gv.dimension_id = $6
         LEFT JOIN dimension_value v ON v.id = gv.value_id
         WHERE a.company_id = $1
           AND ($2::date IS NULL OR g.posting_date >= $2)
           AND ($3::date IS NULL OR g.posting_date <= $3)
           AND ($4 OR NOT starts_with(p.command, 'year_end_close.'))
           AND {}
         GROUP BY a.id, gv.value_id, v.code, v.name
         HAVING sum(g.debit) <> sum(g.credit)
         ORDER BY a.code, v.code NULLS FIRST",
        filter_sql(5)
    ))
    .bind(company_id)
    .bind(from)
    .bind(to)
    .bind(with_closing)
    .bind(by.values)
    .bind(by.group_by)
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
    /// With `group_by_dimension_id`: the value of that dimension this row
    /// is for; null for entries without one.
    dimension_value_id: Option<Uuid>,
    dimension_value_code: Option<String>,
    dimension_value_name: Option<String>,
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
            dimension_value_id: b.dimension_value_id,
            dimension_value_code: b.dimension_value_code.clone(),
            dimension_value_name: b.dimension_value_name.clone(),
        })
        .collect();
    Section {
        total: rows.iter().map(|r| r.balance).sum(),
        rows,
    }
}

/// A value of the dimension a report groups by: null for entries without
/// one.
#[derive(Serialize, JsonSchema, Clone, PartialEq)]
struct GroupValue {
    dimension_value_id: Option<Uuid>,
    dimension_value_code: Option<String>,
    dimension_value_name: Option<String>,
}

/// The values the balances of accounts of these types fall in, in the
/// order of their codes.
fn groups(balances: &[AccountBalance], types: &[&str]) -> Vec<GroupValue> {
    let mut groups: Vec<GroupValue> = vec![];
    for b in balances
        .iter()
        .filter(|b| types.contains(&b.account_type.as_str()))
    {
        let group = GroupValue {
            dimension_value_id: b.dimension_value_id,
            dimension_value_code: b.dimension_value_code.clone(),
            dimension_value_name: b.dimension_value_name.clone(),
        };
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    groups.sort_by(|a, b| a.dimension_value_code.cmp(&b.dimension_value_code));
    groups
}

/// The balances of one value of the grouping dimension.
fn of_group(balances: &[AccountBalance], group: &GroupValue) -> Vec<AccountBalance> {
    balances
        .iter()
        .filter(|b| b.dimension_value_id == group.dimension_value_id)
        .map(|b| AccountBalance {
            account_id: b.account_id,
            code: b.code.clone(),
            name: b.name.clone(),
            account_type: b.account_type.clone(),
            balance: b.balance,
            dimension_value_id: b.dimension_value_id,
            dimension_value_code: b.dimension_value_code.clone(),
            dimension_value_name: b.dimension_value_name.clone(),
        })
        .collect()
}

/// The totals of a balance sheet for one value of the grouping dimension.
#[derive(Serialize, JsonSchema)]
struct BalanceSheetGroup {
    #[serde(flatten)]
    value: GroupValue,
    total_assets: Decimal,
    total_liabilities: Decimal,
    total_equity: Decimal,
    net_income: Decimal,
}

/// The totals of an income statement for one value of the grouping
/// dimension.
#[derive(Serialize, JsonSchema)]
struct IncomeStatementGroup {
    #[serde(flatten)]
    value: GroupValue,
    total_income: Decimal,
    total_expenses: Decimal,
    net_income: Decimal,
}

// ------------------------------------------------------------ balance sheet

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BalanceSheetInput {
    /// The company to report on.
    company_id: Uuid,
    /// Include entries up to and including this date. Defaults to all.
    as_of: Option<NaiveDate>,
    /// Count only entries with these dimension values: for each dimension
    /// among them, one of its values given. Empty or absent: all entries.
    #[serde(default)]
    dimension_value_ids: Vec<Uuid>,
    /// Show each account per value of this dimension, with totals per value.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "dimension", "key": "id" }))]
    group_by_dimension_id: Option<Uuid>,
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
    /// Liabilities plus equity plus `net_income`. Equal to `total_assets`
    /// for the whole company; for some dimension values it need not be,
    /// since the ledger does not balance per dimension.
    total_liabilities_and_equity: Decimal,
    /// With `group_by_dimension_id`: the totals of each value.
    groups: Vec<BalanceSheetGroup>,
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
             Accounts with a zero balance are left out. dimension_value_ids counts only \
             entries with those values (for each dimension among them, one of its values); \
             the ledger balances per posting, not per dimension, so a part of the company \
             need not balance. group_by_dimension_id shows each account per value of that \
             dimension, with the totals of each value in `groups`.",
        )
        .report("Balance sheet")
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: BalanceSheetInput) -> Result<BalanceSheetOutput> {
        ctx.require(READ, input.company_id)?;
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        check_dimensions(
            &mut *ctx.conn,
            input.company_id,
            &input.dimension_value_ids,
            input.group_by_dimension_id,
        )
        .await?;
        let by = ByDimension {
            values: &input.dimension_value_ids,
            group_by: input.group_by_dimension_id,
        };
        let balances = balances(
            &mut *ctx.conn,
            input.company_id,
            None,
            input.as_of,
            true,
            by,
        )
        .await?;
        let groups = if input.group_by_dimension_id.is_some() {
            groups(
                &balances,
                &["asset", "liability", "equity", "income", "expense"],
            )
            .into_iter()
            .map(|value| {
                let of = of_group(&balances, &value);
                BalanceSheetGroup {
                    total_assets: section(&of, "asset", true).total,
                    total_liabilities: section(&of, "liability", false).total,
                    total_equity: section(&of, "equity", false).total,
                    net_income: section(&of, "income", false).total
                        - section(&of, "expense", true).total,
                    value,
                }
            })
            .collect()
        } else {
            vec![]
        };
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
            groups,
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
    /// Count only entries with these dimension values: for each dimension
    /// among them, one of its values given. Empty or absent: all entries.
    #[serde(default)]
    dimension_value_ids: Vec<Uuid>,
    /// Show each account per value of this dimension, with totals per value.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "dimension", "key": "id" }))]
    group_by_dimension_id: Option<Uuid>,
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
    /// With `group_by_dimension_id`: the totals of each value.
    groups: Vec<IncomeStatementGroup>,
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
             counted, so a closed year still shows its income and expenses. \
             dimension_value_ids counts only entries with those values (for each dimension \
             among them, one of its values), such as one site's; group_by_dimension_id shows \
             each account per value of that dimension, with the totals of each value in \
             `groups`.",
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
        check_dimensions(
            &mut *ctx.conn,
            input.company_id,
            &input.dimension_value_ids,
            input.group_by_dimension_id,
        )
        .await?;
        let balances = balances(
            &mut *ctx.conn,
            input.company_id,
            Some(input.from),
            Some(input.to),
            false,
            ByDimension {
                values: &input.dimension_value_ids,
                group_by: input.group_by_dimension_id,
            },
        )
        .await?;
        let groups = if input.group_by_dimension_id.is_some() {
            groups(&balances, &["income", "expense"])
                .into_iter()
                .map(|value| {
                    let of = of_group(&balances, &value);
                    let total_income = section(&of, "income", false).total;
                    let total_expenses = section(&of, "expense", true).total;
                    IncomeStatementGroup {
                        total_income,
                        total_expenses,
                        net_income: total_income - total_expenses,
                        value,
                    }
                })
                .collect()
        } else {
            vec![]
        };
        let income = section(&balances, "income", false);
        let expenses = section(&balances, "expense", true);
        Ok(IncomeStatementOutput {
            company_id: input.company_id,
            from: input.from,
            to: input.to,
            currency,
            net_income: income.total - expenses.total,
            groups,
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
    /// List only entries with these dimension values: for each dimension
    /// among them, one of its values given. The opening balance counts the
    /// same entries. Empty or absent: all entries.
    #[serde(default)]
    dimension_value_ids: Vec<Uuid>,
    /// Total the range per value of this dimension, in `groups`.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "dimension", "key": "id" }))]
    group_by_dimension_id: Option<Uuid>,
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
    /// The entry's dimension values as DIMENSION:VALUE codes.
    dimensions: Option<String>,
}

/// What the entries of an account ledger's range come to for one value of
/// the dimension it groups by.
#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct AccountLedgerGroup {
    dimension_value_id: Option<Uuid>,
    dimension_value_code: Option<String>,
    dimension_value_name: Option<String>,
    debit: Decimal,
    credit: Decimal,
    /// Debit minus credit over the range.
    balance: Decimal,
    entries: i64,
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
    /// With `group_by_dimension_id`: the range per value of that dimension.
    groups: Vec<AccountLedgerGroup>,
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
             correct on every page. Each entry shows its dimension values; \
             dimension_value_ids lists only entries with those values (the opening balance \
             counts the same ones), and group_by_dimension_id totals the range per value of \
             that dimension in `groups`.",
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
        check_dimensions(
            &mut *ctx.conn,
            input.company_id,
            &input.dimension_value_ids,
            input.group_by_dimension_id,
        )
        .await?;
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
        let filter = filter_sql(6);
        let fetched = sqlx::query(&format!(
            "WITH opening AS (
                 SELECT COALESCE(sum(g.debit - g.credit), 0) AS opening_balance
                 FROM gl_entry g
                 WHERE g.account_id = $1 AND $2::date IS NOT NULL AND g.posting_date < $2
                   AND {filter}
             ),
             in_range AS (
                 SELECT g.id, g.posting_date, g.debit, g.credit, g.memo, g.posting_id,
                        g.dimension_set_id,
                        sum(g.debit - g.credit) OVER (ORDER BY g.posting_date, g.id) AS running
                 FROM gl_entry g
                 WHERE g.account_id = $1
                   AND ($2::date IS NULL OR g.posting_date >= $2)
                   AND ($3::date IS NULL OR g.posting_date <= $3)
                   AND {filter}
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
                    p.document_id, d.doc_type AS document_type, d.number AS document_number,
                    ds.label AS dimensions
             FROM opening o
             CROSS JOIN totals t
             LEFT JOIN page e ON true
             LEFT JOIN posting p ON p.id = e.posting_id
             LEFT JOIN document d ON d.id = p.document_id
             LEFT JOIN dimension_set ds ON ds.id = e.dimension_set_id
             ORDER BY e.posting_date, e.id"
        ))
        .bind(input.account_id)
        .bind(input.from)
        .bind(input.to)
        .bind(limit)
        .bind(offset)
        .bind(&input.dimension_value_ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let groups: Vec<AccountLedgerGroup> = match input.group_by_dimension_id {
            None => vec![],
            Some(dimension) => {
                sqlx::query_as(&format!(
                    "SELECT gv.value_id AS dimension_value_id,
                            v.code AS dimension_value_code, v.name AS dimension_value_name,
                            sum(g.debit) AS debit, sum(g.credit) AS credit,
                            sum(g.debit - g.credit) AS balance, count(*) AS entries
                     FROM gl_entry g
                     LEFT JOIN dimension_set_value gv
                         ON gv.set_id = g.dimension_set_id AND gv.dimension_id = $5
                     LEFT JOIN dimension_value v ON v.id = gv.value_id
                     WHERE g.account_id = $1
                       AND ($2::date IS NULL OR g.posting_date >= $2)
                       AND ($3::date IS NULL OR g.posting_date <= $3)
                       AND {}
                     GROUP BY gv.value_id, v.code, v.name
                     ORDER BY v.code NULLS FIRST",
                    filter_sql(4)
                ))
                .bind(input.account_id)
                .bind(input.from)
                .bind(input.to)
                .bind(&input.dimension_value_ids)
                .bind(dimension)
                .fetch_all(&mut *ctx.conn)
                .await?
            }
        };

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
            groups,
        })
    }
}

// --------------------------------------------------------------- dimensions

/// SQL that holds for an entry `g` within the dimension filter bound as
/// parameter `$n` (an array of dimension value ids): for every dimension
/// among the values, the entry's set has one of them. An empty array lets
/// every entry through. The subquery does not depend on the entry, so it
/// runs once.
pub(crate) fn filter_sql(n: usize) -> String {
    format!(
        "(cardinality(${n}::uuid[]) = 0 OR g.dimension_set_id IN (
             SELECT sv.set_id FROM dimension_set_value sv
             WHERE sv.value_id = ANY (${n}::uuid[])
             GROUP BY sv.set_id
             HAVING count(DISTINCT sv.dimension_id) =
                 (SELECT count(DISTINCT dimension_id) FROM dimension_value
                  WHERE id = ANY (${n}::uuid[]))))"
    )
}

/// Check a report's dimension filter and grouping: the values and the
/// dimension must be the company's. An unknown value would otherwise
/// narrow nothing.
pub async fn check_dimensions(
    conn: &mut PgConnection,
    company_id: Uuid,
    values: &[Uuid],
    group_by: Option<Uuid>,
) -> Result<()> {
    if !values.is_empty() {
        let known: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT id) FROM dimension_value WHERE id = ANY ($1) AND company_id = $2",
        )
        .bind(values)
        .bind(company_id)
        .fetch_one(&mut *conn)
        .await?;
        let distinct: std::collections::HashSet<&Uuid> = values.iter().collect();
        if known != distinct.len() as i64 {
            return Err(Error::validation(
                "a dimension value to filter by is not one of the company's",
            )
            .with_field("dimension_value_ids"));
        }
    }
    if let Some(dimension) = group_by {
        let known: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM dimension WHERE id = $1 AND company_id = $2)",
        )
        .bind(dimension)
        .bind(company_id)
        .fetch_one(&mut *conn)
        .await?;
        if !known {
            return Err(
                Error::validation("the dimension to group by is not one of the company's")
                    .with_field("group_by_dimension_id"),
            );
        }
    }
    Ok(())
}
