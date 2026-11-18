//! What statement lines probably are, and how a bank account's ledger
//! balance and its statements come together.

use std::collections::{BTreeSet, HashMap};

use chrono::{Duration, NaiveDate};
use erpcraft_kernel::document;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, PgConnection, Result, async_trait,
    schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{BANK_STATEMENT, Header, bank_account, is_matched, load_header, load_lines};

/// How far a posting's date may be from a line's booking date and still
/// be proposed for it.
const DATE_WINDOW_DAYS: i64 = 10;
/// The most proposals given for one line.
const MAX_PROPOSALS: usize = 5;

async fn base_currency(conn: &mut PgConnection, company_id: Uuid) -> Result<String> {
    erpcraft_kernel::company::base_currency(conn, company_id).await
}

/// What `posting_id` moved through the statement's bank account, in the
/// statement's currency: `None` if it is not the company's, moved nothing
/// there, is a reversal or was reversed.
pub(crate) async fn posting_amount(
    conn: &mut PgConnection,
    company_id: Uuid,
    header: &Header,
    posting_id: Uuid,
) -> Result<Option<Decimal>> {
    let base = base_currency(&mut *conn, company_id).await?;
    let in_base = header.currency == base;
    let moved: Option<Decimal> = sqlx::query_scalar(
        "SELECT sum(CASE WHEN $4 THEN g.debit - g.credit ELSE g.txn_debit - g.txn_credit END)
         FROM gl_entry g JOIN posting p ON p.id = g.posting_id
         WHERE p.id = $1 AND p.company_id = $2 AND g.account_id = $3
           AND ($4 OR g.currency = $5)
           AND p.reverses_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)",
    )
    .bind(posting_id)
    .bind(company_id)
    .bind(header.bank_account_id)
    .bind(in_base)
    .bind(&header.currency)
    .fetch_one(&mut *conn)
    .await?;
    Ok(moved.filter(|m| !m.is_zero()))
}

/// A posting that moved the bank account, as a candidate or as outstanding.
#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct BankPosting {
    /// Posting id.
    posting_id: Uuid,
    /// Accounting date of the posting.
    posting_date: NaiveDate,
    /// The document that made it.
    document_id: Uuid,
    /// Its type: `payment`, `journal_entry`.
    doc_type: String,
    /// Its number.
    number: Option<String>,
    /// What it moved through the bank account, in the account's currency:
    /// positive in, negative out.
    amount: Decimal,
    /// The posting's memo.
    memo: Option<String>,
}

/// The postings that moved the account, dated in `from..=to`, that no
/// statement line is matched to: `linked` names those of lines counted
/// elsewhere. Reversed postings and reversals pair off and are left out
/// only for proposals (`unreversed`).
async fn bank_postings(
    conn: &mut PgConnection,
    company_id: Uuid,
    header: &Header,
    from: Option<NaiveDate>,
    to: NaiveDate,
    unreversed: bool,
) -> Result<Vec<BankPosting>> {
    let base = base_currency(&mut *conn, company_id).await?;
    let in_base = header.currency == base;
    Ok(sqlx::query_as(
        "SELECT p.id AS posting_id, p.posting_date, d.id AS document_id, d.doc_type, d.number,
                sum(CASE WHEN $5 THEN g.debit - g.credit ELSE g.txn_debit - g.txn_credit END)
                    AS amount,
                p.memo
         FROM gl_entry g
         JOIN posting p ON p.id = g.posting_id
         JOIN document d ON d.id = p.document_id
         WHERE g.account_id = $1 AND p.company_id = $2
           AND ($3::date IS NULL OR p.posting_date >= $3) AND p.posting_date <= $4
           AND ($5 OR g.currency = $6)
           AND (NOT $7 OR (p.reverses_id IS NULL
                AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)))
         GROUP BY p.id, d.id
         HAVING sum(CASE WHEN $5 THEN g.debit - g.credit ELSE g.txn_debit - g.txn_credit END) <> 0
         ORDER BY p.posting_date, p.created_at",
    )
    .bind(header.bank_account_id)
    .bind(company_id)
    .bind(from)
    .bind(to)
    .bind(in_base)
    .bind(&header.currency)
    .bind(unreversed)
    .fetch_all(&mut *conn)
    .await?)
}

/// Every posting a statement line of the account is matched to, including
/// the posting of a payment drafted for a line of a draft statement.
async fn linked_postings(conn: &mut PgConnection, account: Uuid) -> Result<BTreeSet<Uuid>> {
    let rows: Vec<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT posting_id, difference_posting_id FROM bank_statement_line
         WHERE bank_account_id = $1
         UNION ALL
         SELECT p.id, NULL FROM bank_statement_line l
         JOIN posting p ON p.document_id = l.payment_id AND p.reverses_id IS NULL
         WHERE l.bank_account_id = $1",
    )
    .bind(account)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .flat_map(|(a, b)| [a, b])
        .flatten()
        .collect())
}

// -------------------------------------------------------------- proposals

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProposalsInput {
    /// Bank statement id.
    id: Uuid,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "snake_case")]
enum ProposalKind {
    /// A posting that moved the bank account by the line's amount.
    Posting,
    /// Open items the line pays.
    OpenItems,
    /// An account to post the line to.
    Account,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
struct Proposal {
    /// What to give bank_statement.match as `action`.
    kind: ProposalKind,
    /// For `posting`: the posting, with its document.
    posting: Option<BankPosting>,
    /// For `open_items`: the items.
    open_item_ids: Vec<Uuid>,
    /// For `open_items`: the numbers of the documents they come from.
    numbers: Vec<String>,
    /// For `open_items`: the party.
    party_name: Option<String>,
    /// For `account`: the account to post to, when one can be proposed.
    account_id: Option<Uuid>,
    /// What the proposal comes to, in the statement's currency, signed as
    /// the line.
    amount: Decimal,
    /// How sure the proposal is, from 0 to 100.
    score: u32,
    /// Why it is proposed, for a person to judge.
    reason: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct LineProposals {
    /// The line's id, for bank_statement.match.
    line_id: Uuid,
    /// The line's position.
    line_no: i32,
    /// The line's amount.
    amount: Decimal,
    /// Whether the line is matched already; it then has no proposals.
    matched: bool,
    /// The best first.
    proposals: Vec<Proposal>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ProposalsOutput {
    /// One entry per line of the statement.
    lines: Vec<LineProposals>,
}

#[derive(sqlx::FromRow)]
struct Item {
    id: Uuid,
    party_type: String,
    party_id: Uuid,
    open: Decimal,
    number: Option<String>,
}

const FEE_WORDS: &[&str] = &[
    "fee",
    "charge",
    "interest",
    "commission",
    "gebühr",
    "gebuehr",
    "entgelt",
    "zins",
    "frais",
    "intérêt",
    "spesen",
];

pub(crate) struct Proposals;

#[async_trait]
impl Command for Proposals {
    type Input = ProposalsInput;
    type Output = ProposalsOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "bank_statement.proposals",
            "For each line of a bank statement not matched yet, what it probably is, best \
             first: a posting that moved the bank account by the same amount within ten days \
             (a payment recorded before); open items it pays, found by a document number in \
             the reference, or by amount and the counterparty's name; or an account, where an \
             earlier line from the same counterparty was posted to one. Nothing is changed: \
             accept a proposal with bank_statement.match.",
        )
        .label("Proposals")
        .resource(BANK_STATEMENT)
        .target(true)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ProposalsInput) -> Result<ProposalsOutput> {
        // Read through the document's own path, so its permissions hold.
        let view = document::get(ctx, BANK_STATEMENT, input.id).await?;
        let company_id = view.header.company_id;
        let header = load_header(&mut *ctx.conn, input.id).await?;
        let lines = load_lines(&mut *ctx.conn, input.id).await?;

        let window = Duration::days(DATE_WINDOW_DAYS);
        let linked = linked_postings(&mut *ctx.conn, header.bank_account_id).await?;
        let postings: Vec<BankPosting> = bank_postings(
            &mut *ctx.conn,
            company_id,
            &header,
            Some(header.period_start - window),
            header.period_end + window,
            true,
        )
        .await?
        .into_iter()
        .filter(|p| !linked.contains(&p.posting_id))
        .collect();

        // Open items only for whoever may read them.
        let read_items = ctx.require("open_item:read", company_id).is_ok();
        let mut items: Vec<Item> = vec![];
        let mut names: HashMap<Uuid, String> = HashMap::new();
        if read_items {
            items = sqlx::query_as(
                "SELECT i.id, i.party_type, i.party_id, i.txn_amount - i.txn_settled AS open,
                        d.number
                 FROM open_item i JOIN posting p ON p.id = i.posting_id
                 JOIN document d ON d.id = p.document_id
                 WHERE i.company_id = $1 AND i.currency = $2
                   AND i.txn_amount - i.txn_settled > 0",
            )
            .bind(company_id)
            .bind(&header.currency)
            .fetch_all(&mut *ctx.conn)
            .await?;
            for (entity, kind) in [("vendor", "vendor"), ("customer", "customer")] {
                if ctx.kernel.registry().entity(entity).is_err() {
                    continue;
                }
                let ids: Vec<Uuid> = items
                    .iter()
                    .filter(|i| i.party_type == kind)
                    .map(|i| i.party_id)
                    .collect();
                // Only names the caller may read.
                let readable = erpcraft_kernel::entity::readable_ids(ctx, entity, &ids).await?;
                let readable: Vec<Uuid> = readable.into_iter().collect();
                let rows: Vec<(Uuid, String)> =
                    sqlx::query_as(&format!("SELECT id, name FROM {entity} WHERE id = ANY($1)"))
                        .bind(&readable)
                        .fetch_all(&mut *ctx.conn)
                        .await?;
                names.extend(rows);
            }
        }
        let kind_of = |amount: Decimal| {
            if amount > Decimal::ZERO {
                "receivable"
            } else {
                "payable"
            }
        };
        let item_kind: HashMap<Uuid, String> = if read_items {
            sqlx::query_as::<_, (Uuid, String)>("SELECT id, kind FROM open_item WHERE id = ANY($1)")
                .bind(items.iter().map(|i| i.id).collect::<Vec<_>>())
                .fetch_all(&mut *ctx.conn)
                .await?
                .into_iter()
                .collect()
        } else {
            HashMap::new()
        };

        // Earlier lines of the account posted straight to an account.
        let earlier: Vec<(Option<String>, Option<String>, Uuid)> = sqlx::query_as(
            "SELECT counterparty_name, reference, account_id FROM bank_statement_line
             WHERE bank_account_id = $1 AND document_id <> $2 AND account_id IS NOT NULL
               AND difference = amount
             ORDER BY booking_date DESC",
        )
        .bind(header.bank_account_id)
        .bind(input.id)
        .fetch_all(&mut *ctx.conn)
        .await?;

        let mut out = vec![];
        for line in &lines {
            let matched = is_matched(line);
            let mut proposals: Vec<Proposal> = vec![];
            if !matched {
                let reference = line.reference.clone().unwrap_or_default().to_lowercase();
                let counterparty = line
                    .counterparty_name
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase();
                let blank =
                    |kind: ProposalKind, amount: Decimal, score: u32, reason: String| Proposal {
                        kind,
                        posting: None,
                        open_item_ids: vec![],
                        numbers: vec![],
                        party_name: None,
                        account_id: None,
                        amount,
                        score,
                        reason,
                    };
                for posting in postings.iter().filter(|p| p.amount == line.amount) {
                    let days = (posting.posting_date - line.booking_date).num_days().abs();
                    let named = posting
                        .number
                        .as_ref()
                        .is_some_and(|n| reference.contains(&n.to_lowercase()));
                    let mut p = blank(
                        ProposalKind::Posting,
                        posting.amount,
                        (95 - days.min(10) as u32 * 3) + if named { 5 } else { 0 },
                        format!(
                            "{} {} moved the same amount, {days} day(s) from the booking date{}",
                            posting.doc_type,
                            posting.number.as_deref().unwrap_or(""),
                            if named {
                                ", and the reference names it"
                            } else {
                                ""
                            }
                        ),
                    );
                    p.posting = Some(posting.clone());
                    proposals.push(p);
                }
                let wanted = kind_of(line.amount);
                let candidates: Vec<&Item> = items
                    .iter()
                    .filter(|i| item_kind.get(&i.id).map(String::as_str) == Some(wanted))
                    .collect();
                // Documents the reference names, grouped by party.
                let mut by_party: HashMap<(String, Uuid), Vec<&Item>> = HashMap::new();
                for item in &candidates {
                    if item
                        .number
                        .as_ref()
                        .is_some_and(|n| !n.is_empty() && reference.contains(&n.to_lowercase()))
                    {
                        by_party
                            .entry((item.party_type.clone(), item.party_id))
                            .or_default()
                            .push(item);
                    }
                }
                for ((_, party), found) in by_party {
                    let total: Decimal = found.iter().map(|i| i.open).sum();
                    let exact = total == line.amount.abs();
                    let mut p = blank(
                        ProposalKind::OpenItems,
                        if line.amount < Decimal::ZERO {
                            -total
                        } else {
                            total
                        },
                        if exact { 90 } else { 50 },
                        format!(
                            "the reference names {}{}",
                            found
                                .iter()
                                .filter_map(|i| i.number.clone())
                                .collect::<Vec<_>>()
                                .join(", "),
                            if exact {
                                ", and they come to the line's amount".to_owned()
                            } else {
                                format!(", which come to {}", total.normalize())
                            }
                        ),
                    );
                    p.open_item_ids = found.iter().map(|i| i.id).collect();
                    p.numbers = found.iter().filter_map(|i| i.number.clone()).collect();
                    p.party_name = names.get(&party).cloned();
                    proposals.push(p);
                }
                for item in candidates.iter().filter(|i| i.open == line.amount.abs()) {
                    if proposals.iter().any(|p| p.open_item_ids.contains(&item.id)) {
                        continue;
                    }
                    let name = names.get(&item.party_id).cloned();
                    let same_party = name.as_ref().is_some_and(|n| {
                        let n = n.to_lowercase();
                        !counterparty.is_empty()
                            && (counterparty.contains(&n) || n.contains(&counterparty))
                    });
                    let mut p = blank(
                        ProposalKind::OpenItems,
                        line.amount,
                        if same_party { 80 } else { 40 },
                        format!(
                            "{} is open for the same amount{}",
                            item.number.as_deref().unwrap_or("an item"),
                            if same_party {
                                ", and the counterparty is the party"
                            } else {
                                ""
                            }
                        ),
                    );
                    p.open_item_ids = vec![item.id];
                    p.numbers = item.number.clone().into_iter().collect();
                    p.party_name = name;
                    proposals.push(p);
                }
                if let Some((_, _, account)) = earlier.iter().find(|(name, reference_before, _)| {
                    let same = |a: &Option<String>, b: &str| {
                        a.as_ref()
                            .is_some_and(|a| !b.is_empty() && a.to_lowercase() == b)
                    };
                    same(name, &counterparty) || same(reference_before, &reference)
                }) {
                    let mut p = blank(
                        ProposalKind::Account,
                        line.amount,
                        60,
                        "an earlier line like this one was posted to this account".into(),
                    );
                    p.account_id = Some(*account);
                    proposals.push(p);
                } else if FEE_WORDS
                    .iter()
                    .any(|w| reference.contains(w) || counterparty.contains(w))
                {
                    proposals.push(blank(
                        ProposalKind::Account,
                        line.amount,
                        20,
                        "it reads like a bank fee or interest: choose the account to post it to"
                            .into(),
                    ));
                }
                proposals.sort_by_key(|p| std::cmp::Reverse(p.score));
                proposals.truncate(MAX_PROPOSALS);
            }
            out.push(LineProposals {
                line_id: line.id,
                line_no: line.line_no,
                amount: line.amount,
                matched,
                proposals,
            });
        }
        Ok(ProposalsOutput { lines: out })
    }
}

// ---------------------------------------------------------- reconciliation

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportInput {
    /// The bank or cash account.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "account", "key": "id", "filters": { "is_bank": "true" } }))]
    bank_account_id: Uuid,
    /// The date to reconcile at: postings up to it, and the statements
    /// that end on or before it. Today when omitted.
    as_of: Option<NaiveDate>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct UnmatchedLine {
    /// The statement.
    statement_id: Uuid,
    /// Its number, once it has one.
    number: Option<String>,
    /// The line's id.
    line_id: Uuid,
    /// The line's position.
    line_no: i32,
    /// Booking date of the line.
    booking_date: NaiveDate,
    /// The line's amount.
    amount: Decimal,
    /// What of it the books do not hold yet.
    unmatched: Decimal,
    /// The line's reference.
    reference: Option<String>,
    /// The counterparty on the line.
    counterparty_name: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ReportOutput {
    /// The account's currency, which every amount here is in.
    currency: String,
    /// The date reconciled at.
    as_of: NaiveDate,
    /// What the general ledger says is in the account at that date.
    ledger_balance: Decimal,
    /// The closing balance of the last statement that ends on or before
    /// that date; zero without one.
    statement_balance: Decimal,
    /// The opening balance of the account's first statement: money that
    /// was there before the statements start.
    first_opening_balance: Decimal,
    /// Ledger balance minus statement balance.
    difference: Decimal,
    /// Postings that moved the account up to that date and are on no
    /// statement yet, such as a payment the bank has not executed.
    outstanding: Vec<BankPosting>,
    /// Their total.
    outstanding_total: Decimal,
    /// Statement lines, or parts of them, the books do not hold yet.
    unmatched_lines: Vec<UnmatchedLine>,
    /// Their total.
    unmatched_total: Decimal,
    /// Outstanding, less unmatched, less the first opening balance: what
    /// explains the difference.
    explained: Decimal,
    /// Difference minus explained: zero when the books and the statements
    /// agree.
    unexplained: Decimal,
}

pub(crate) struct Report;

#[async_trait]
impl Command for Report {
    type Input = ReportInput;
    type Output = ReportOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "bank.reconciliation",
            "Set a bank account's ledger balance against its statements at a date: the ledger \
             balance, the last statement's closing balance, the postings not on a statement \
             yet (outstanding), the statement lines the books do not hold yet (unmatched), and \
             the difference those explain to the cent. Takes bank_statement:read.",
        )
        .permission("bank_statement:read")
        .label("Bank reconciliation")
        .resource("bank")
        .target(false)
        .report("Bank reconciliation")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReportInput) -> Result<ReportOutput> {
        let account = bank_account(&mut *ctx.conn, input.bank_account_id).await?;
        ctx.require("bank_statement:read", account.company_id)?;
        if !account.is_bank {
            return Err(Error::field(
                "bank_account_id",
                "must be a bank or cash account (is_bank)",
            ));
        }
        let as_of = input
            .as_of
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        let header = Header {
            bank_account_id: input.bank_account_id,
            currency: account.currency.clone(),
            period_start: as_of,
            period_end: as_of,
            opening_balance: Decimal::ZERO,
            closing_balance: Decimal::ZERO,
            source_format: None,
        };
        // Every posting up to the date, reversals included: they are money
        // the ledger counts.
        let postings = bank_postings(
            &mut *ctx.conn,
            account.company_id,
            &header,
            None,
            as_of,
            false,
        )
        .await?;
        let ledger_balance: Decimal = postings.iter().map(|p| p.amount).sum();
        let amount_of: HashMap<Uuid, Decimal> =
            postings.iter().map(|p| (p.posting_id, p.amount)).collect();

        #[derive(sqlx::FromRow)]
        struct Statement {
            document_id: Uuid,
            number: Option<String>,
            opening_balance: Decimal,
            closing_balance: Decimal,
        }
        let statements: Vec<Statement> = sqlx::query_as(
            "SELECT s.document_id, d.number, s.opening_balance, s.closing_balance
             FROM bank_statement s JOIN document d ON d.id = s.document_id
             WHERE s.bank_account_id = $1 AND s.period_end <= $2
             ORDER BY s.period_end, s.period_start, d.created_at",
        )
        .bind(input.bank_account_id)
        .bind(as_of)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let first_opening = statements
            .first()
            .map_or(Decimal::ZERO, |s| s.opening_balance);
        let statement_balance = statements
            .last()
            .map_or(Decimal::ZERO, |s| s.closing_balance);

        let mut linked = BTreeSet::new();
        let mut unmatched_lines = vec![];
        for statement in &statements {
            for line in load_lines(&mut *ctx.conn, statement.document_id).await? {
                let mut held = Decimal::ZERO;
                for posting in [line.posting_id, line.difference_posting_id]
                    .into_iter()
                    .flatten()
                {
                    linked.insert(posting);
                    held += amount_of.get(&posting).copied().unwrap_or(Decimal::ZERO);
                }
                let unmatched = line.amount - held;
                if !unmatched.is_zero() {
                    unmatched_lines.push(UnmatchedLine {
                        statement_id: statement.document_id,
                        number: statement.number.clone(),
                        line_id: line.id,
                        line_no: line.line_no,
                        booking_date: line.booking_date,
                        amount: line.amount,
                        unmatched,
                        reference: line.reference.clone(),
                        counterparty_name: line.counterparty_name.clone(),
                    });
                }
            }
        }
        let outstanding: Vec<BankPosting> = postings
            .into_iter()
            .filter(|p| !linked.contains(&p.posting_id))
            .collect();
        let outstanding_total: Decimal = outstanding.iter().map(|p| p.amount).sum();
        let unmatched_total: Decimal = unmatched_lines.iter().map(|l| l.unmatched).sum();
        let difference = ledger_balance - statement_balance;
        let explained = outstanding_total - unmatched_total - first_opening;
        Ok(ReportOutput {
            currency: account.currency,
            as_of,
            ledger_balance,
            statement_balance,
            first_opening_balance: first_opening,
            difference,
            outstanding,
            outstanding_total,
            unmatched_lines,
            unmatched_total,
            explained,
            unexplained: difference - explained,
        })
    }
}
