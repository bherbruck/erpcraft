//! Banking: bank statements, read from what banks export, matched to what
//! the books hold, and reconciled.
//!
//! A `bank_statement` is a document per bank account (an `is_bank` account)
//! and period: an opening balance, lines, and a closing balance that must
//! come out of them. Statements of one account follow each other without a
//! gap. `bank_statement.import` reads a CSV (with a mapping saved per bank
//! account), camt.053 or OFX file into a draft. `bank_statement.proposals`
//! says what each line probably is; `bank_statement.match` records what it
//! is: a posting that moved the bank account (a payment, say), open items
//! to pay (it drafts the payment), or an account to post it to (fees,
//! interest). `bank_statement.post` checks that every line is accounted
//! for, posts what the statement itself posts, and locks it.
//! `bank.reconciliation` sets the ledger against the statements.
//!
//! The module reads the general ledger's entries, postings and documents
//! through SQL, as a report does; it writes the ledger only through
//! `erpcraft_gl::post`, and payments only through `payment.draft`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DynCommand, EntityMeta, Error,
    FieldMeta, FieldType, JsonSchema, Migration, Module, PgConnection, PolicyPoint, Result,
    StateMeta, async_trait, command, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod parse;
mod reconcile;

const BANK_STATEMENT: &str = "bank_statement";
/// The policy point asked when a line is matched to a posting of another
/// amount, the difference to go to an account.
pub const AMOUNT_DIFFERENCE: &str = "bank_statement.amount_difference";

pub struct BankingModule;

impl Module for BankingModule {
    fn name(&self) -> &'static str {
        "banking"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "payments"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "banking",
                sql: include_str!("../migrations/0001_banking.sql"),
            },
            Migration {
                version: 2,
                name: "one_line_per_posting",
                sql: include_str!("../migrations/0002_one_line_per_posting.sql"),
            },
            Migration {
                version: 3,
                name: "bank_account_number",
                sql: include_str!("../migrations/0003_bank_account_number.sql"),
            },
            Migration {
                version: 4,
                name: "bank_account_details",
                sql: include_str!("../migrations/0004_bank_account_details.sql"),
            },
            Migration {
                version: 5,
                name: "party_bank_account",
                sql: include_str!("../migrations/0005_party_bank_account.sql"),
            },
            Migration {
                version: 6,
                name: "iban_on_change",
                sql: include_str!("../migrations/0006_iban_on_change.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![
            csv_mapping(),
            account_number_entity(),
            party_account_entity(),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(BankStatement)]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                AMOUNT_DIFFERENCE,
                "Asked when a bank statement line is matched to a posting of another amount, \
                 the difference to be posted to an account, as when the bank keeps a fee from a \
                 transfer. Unless configured otherwise it is refused: a line matches a posting \
                 of exactly its amount. A company that accepts small differences allows them, \
                 for example when `facts.difference_abs <= 5`.",
            )
            .number("line_amount", "The amount on the statement line")
            .as_money(None)
            .number(
                "posting_amount",
                "What the posting moved through the bank account",
            )
            .as_money(None)
            .number("difference", "Line amount minus posting amount")
            .as_money(None)
            .number("difference_abs", "The difference without its sign")
            .as_money(None)
            .refuse_by_default()
            .refusal("the line and the posting differ, and the company does not accept that"),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Import),
            command(reconcile::Proposals),
            command(Match),
            command(Post),
            command(reconcile::Report),
        ]
    }
}

// ------------------------------------------------------------ CSV mapping

fn csv_mapping() -> EntityMeta {
    let column = |name: &str, label: &str| {
        FieldMeta::new(name, label, FieldType::Text)
            .help("The column's header, or its position from 1 when the file has no header.")
    };
    EntityMeta::new("bank_csv_mapping", "Bank CSV mapping")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new(
                "bank_account_id",
                "Bank account",
                FieldType::link("account").filter("is_bank", "true"),
            )
            .required()
            .immutable(),
        )
        .field(
            FieldMeta::new("delimiter", "Delimiter", FieldType::Text)
                .help("One character: , or ; or a tab."),
        )
        .field(FieldMeta::new(
            "has_header",
            "First row is a header",
            FieldType::Bool,
        ))
        .field(
            FieldMeta::new("date_format", "Date format", FieldType::Text)
                .help("As strftime: %d.%m.%Y for 31.03.2026, %m/%d/%Y for 03/31/2026."),
        )
        .field(
            FieldMeta::new("decimal_comma", "Decimal comma", FieldType::Bool)
                .help("Amounts are written 1.234,50."),
        )
        .field(column("booking_date_column", "Booking date column").required())
        .field(column("value_date_column", "Value date column"))
        .field(column("amount_column", "Amount column (signed)"))
        .field(column("debit_column", "Debit column (money out)"))
        .field(column("credit_column", "Credit column (money in)"))
        .field(column(
            "counterparty_name_column",
            "Counterparty name column",
        ))
        .field(column(
            "counterparty_account_column",
            "Counterparty account column",
        ))
        .field(column("reference_column", "Reference column"))
        .field(column(
            "transaction_id_column",
            "Bank transaction id column",
        ))
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["created_at"])
        .search(&["booking_date_column"])
        .company_field("company_id")
}

// ---------------------------------------------------- account numbers

fn account_number_entity() -> EntityMeta {
    EntityMeta::new("bank_account_number", "Bank account number")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new(
                "bank_account_id",
                "Bank account",
                FieldType::link("account").filter("is_bank", "true"),
            )
            .required()
            .immutable(),
        )
        .field(
            FieldMeta::new("iban", "IBAN", FieldType::Text)
                .help("Without spaces, in capitals. A camt.053 file names it."),
        )
        .field(
            FieldMeta::new("account_number", "Account number", FieldType::Text)
                .help("The bank's own number for the account, as an OFX file names it (ACCTID)."),
        )
        .field(
            FieldMeta::new("bic", "BIC", FieldType::Text)
                .help("The bank's code, 8 or 11 characters, in capitals."),
        )
        .field(
            FieldMeta::new("account_name", "Account holder", FieldType::Text)
                .help("The name the account is held in, when it is not the company's."),
        )
        .field(
            FieldMeta::new("routing_number", "Routing number", FieldType::Text)
                .help("For a US bank: its nine-digit ABA routing number."),
        )
        .field(
            FieldMeta::new("ach_company_id", "ACH company ID", FieldType::Text).help(
                "The company identification the bank gave for NACHA files, up to 10 characters.",
            ),
        )
        .field(
            FieldMeta::new("ach_origin", "ACH immediate origin", FieldType::Text).help(
                "What the bank wants as the file's immediate origin, when not the company ID.",
            ),
        )
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["created_at"])
        .search(&["iban", "account_number"])
        .company_field("company_id")
}

/// The bank accounts of vendors and customers: where a payment run pays
/// them.
fn party_account_entity() -> EntityMeta {
    EntityMeta::new("party_bank_account", "Party bank account")
        .field(
            FieldMeta::new(
                "party_type",
                "Party type",
                FieldType::options(&["vendor", "customer"]),
            )
            .required(),
        )
        .field(
            FieldMeta::new(
                "party_id",
                "Party",
                FieldType::entity_named_by("party_type"),
            )
            .required(),
        )
        .field(
            FieldMeta::new("iban", "IBAN", FieldType::Text)
                .help("Without spaces, in capitals; checked by its check digits."),
        )
        .field(FieldMeta::new("bic", "BIC", FieldType::Text))
        .field(
            FieldMeta::new("routing_number", "Routing number", FieldType::Text)
                .help("For a US bank: its nine-digit ABA routing number."),
        )
        .field(FieldMeta::new(
            "account_number",
            "Account number",
            FieldType::Text,
        ))
        .field(FieldMeta::new(
            "account_type",
            "Account type",
            FieldType::options(&["checking", "savings"]),
        ))
        .field(
            FieldMeta::new("account_name", "Account holder", FieldType::Text)
                .help("The name the account is held in, when it is not the party's."),
        )
        .field(FieldMeta::new(
            "currency",
            "Currency",
            FieldType::link_by("currency", "code"),
        ))
        .field(
            FieldMeta::new("is_default", "Default", FieldType::Bool)
                .help("The account a payment run pays into."),
        )
        .field(FieldMeta::new("active", "Active", FieldType::Bool))
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["party_type", "party_id"])
        .search(&["iban", "account_number"])
        .display(&["iban", "account_number"])
        .written_together(&["party_type", "party_id"])
}

/// A vendor's or customer's bank account, as a bank file writes it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PartyAccount {
    pub id: Uuid,
    pub iban: Option<String>,
    pub bic: Option<String>,
    pub routing_number: Option<String>,
    pub account_number: Option<String>,
    /// `checking` or `savings`.
    pub account_type: String,
    pub account_name: Option<String>,
    pub currency: Option<String>,
}

/// The default active bank account of a party, else its only active one.
pub async fn party_account(
    conn: &mut PgConnection,
    party_type: &str,
    party_id: Uuid,
) -> Result<Option<PartyAccount>> {
    let found: Vec<PartyAccount> = sqlx::query_as(
        "SELECT id, iban, bic, routing_number, account_number, account_type, account_name, currency
         FROM party_bank_account
         WHERE party_type = $1 AND party_id = $2 AND active
         ORDER BY is_default DESC, created_at",
    )
    .bind(party_type)
    .bind(party_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(found.into_iter().next())
}

/// The numbers the bank knows a bank account by, as recorded in
/// `bank_account_number`: the one place they are kept, for every module
/// (bank files here, the payee account of an e-invoice).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AccountNumber {
    /// Without spaces, in capitals.
    pub iban: Option<String>,
    /// The bank's own number for the account.
    pub account_number: Option<String>,
    /// The bank's BIC.
    pub bic: Option<String>,
    /// The name the account is held in, when it is not the company's.
    pub account_name: Option<String>,
    /// A US bank's ABA routing number.
    pub routing_number: Option<String>,
    /// The company identification of a NACHA file.
    pub ach_company_id: Option<String>,
    /// The immediate origin of a NACHA file, when not the company ID.
    pub ach_origin: Option<String>,
}

/// The numbers recorded for bank account `account_id`; `None` when none are.
pub async fn account_number(
    conn: &mut PgConnection,
    account_id: Uuid,
) -> Result<Option<AccountNumber>> {
    Ok(sqlx::query_as(
        "SELECT iban, account_number, bic, account_name, routing_number, ach_company_id,
                ach_origin
         FROM bank_account_number WHERE bank_account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Written as a bank file or a person may write it: without spaces, in
/// capitals.
fn plain_number(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_uppercase()
}

/// Whether the account a file names is `bank_account_id`. `Ok(false)` when
/// the file names none, to be said; refused when it names another, or the
/// bank account has no number recorded to compare it with.
async fn same_account(
    conn: &mut PgConnection,
    bank_account_id: Uuid,
    named: Option<&str>,
) -> Result<bool> {
    let Some(named) = named else {
        return Ok(false);
    };
    let shown = parse::quoted(named);
    let Some(AccountNumber {
        iban,
        account_number: number,
        ..
    }) = account_number(&mut *conn, bank_account_id).await?
    else {
        return Err(Error::field(
            "content",
            format!(
                "is a statement of account {shown}, and the bank account has no number to \
                 compare it with: record its IBAN or account number as a bank_account_number \
                 first"
            ),
        ));
    };
    let named = plain_number(named);
    if [iban, number]
        .into_iter()
        .flatten()
        .any(|own| plain_number(&own) == named)
    {
        return Ok(true);
    }
    Err(Error::field(
        "content",
        format!(
            "is a statement of account {shown}, which is not this bank account's IBAN or \
             account number"
        ),
    ))
}

// --------------------------------------------------------------- accounts

/// What a statement needs to know about its bank account.
struct BankAccount {
    company_id: Uuid,
    is_bank: bool,
    is_group: bool,
    /// The account's own currency, else the company's accounting currency.
    currency: String,
}

async fn bank_account(conn: &mut PgConnection, id: Uuid) -> Result<BankAccount> {
    let row: Option<(Uuid, bool, bool, String)> = sqlx::query_as(
        "SELECT a.company_id, a.is_bank, a.is_group, COALESCE(a.currency, c.base_currency)
         FROM account a JOIN company c ON c.id = a.company_id WHERE a.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let (company_id, is_bank, is_group, currency) =
        row.ok_or_else(|| Error::field("bank_account_id", "must be an account"))?;
    Ok(BankAccount {
        company_id,
        is_bank,
        is_group,
        currency,
    })
}

// --------------------------------------------------------- document type

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    bank_account_id: Uuid,
    period_start: NaiveDate,
    period_end: NaiveDate,
    opening_balance: Decimal,
    closing_balance: Decimal,
    source_format: Option<String>,
    #[serde(default)]
    lines: Vec<LineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    id: Option<Uuid>,
    booking_date: NaiveDate,
    value_date: Option<NaiveDate>,
    amount: Decimal,
    counterparty_name: Option<String>,
    counterparty_account: Option<String>,
    reference: Option<String>,
    bank_transaction_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub(crate) struct Line {
    pub id: Uuid,
    pub line_no: i32,
    pub booking_date: NaiveDate,
    pub value_date: Option<NaiveDate>,
    pub amount: Decimal,
    pub counterparty_name: Option<String>,
    pub counterparty_account: Option<String>,
    pub reference: Option<String>,
    pub bank_transaction_id: Option<String>,
    pub posting_id: Option<Uuid>,
    pub payment_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
    pub difference: Decimal,
    pub difference_posting_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub(crate) struct Header {
    pub bank_account_id: Uuid,
    pub currency: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub opening_balance: Decimal,
    pub closing_balance: Decimal,
    pub source_format: Option<String>,
}

pub(crate) async fn load_header(conn: &mut PgConnection, document_id: Uuid) -> Result<Header> {
    sqlx::query_as(
        "SELECT bank_account_id, currency, period_start, period_end, opening_balance,
                closing_balance, source_format
         FROM bank_statement WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found(BANK_STATEMENT))
}

pub(crate) async fn load_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, booking_date, value_date, amount, counterparty_name,
                counterparty_account, reference, bank_transaction_id, posting_id, payment_id,
                account_id, difference, difference_posting_id
         FROM bank_statement_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Whether a line is accounted for: it has a posting, a payment drafted
/// for it, or an account to be posted to.
fn is_matched(line: &Line) -> bool {
    line.posting_id.is_some() || line.payment_id.is_some() || line.account_id.is_some()
}

struct BankStatement;

#[async_trait]
impl DocumentHandler for BankStatement {
    fn meta(&self) -> DocTypeMeta {
        let money = |name: &str, label: &str| {
            FieldMeta::new(name, label, FieldType::Decimal)
                .money()
                .currency_in("currency")
        };
        DocTypeMeta {
            name: BANK_STATEMENT.into(),
            module: String::new(),
            label: "Bank statement".into(),
            number_prefix: "BS".into(),
            fields: vec![
                FieldMeta::new(
                    "bank_account_id",
                    "Bank account",
                    FieldType::link("account").filter("is_bank", "true"),
                )
                .required(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly()
                .help("The bank account's currency: the statement is in it."),
                FieldMeta::new("period_start", "From", FieldType::Date).required(),
                FieldMeta::new("period_end", "To", FieldType::Date).required(),
                money("opening_balance", "Opening balance")
                    .required()
                    .help("Must be the closing balance of the account's statement before."),
                money("closing_balance", "Closing balance")
                    .required()
                    .help("Must be the opening balance plus the lines."),
                FieldMeta::new(
                    "source_format",
                    "Imported from",
                    FieldType::options(&["csv", "camt053", "ofx"]),
                ),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("booking_date", "Booking date", FieldType::Date).required(),
                    FieldMeta::new("value_date", "Value date", FieldType::Date),
                    money("amount", "Amount")
                        .required()
                        .help("Positive for money in, negative for money out."),
                    FieldMeta::new("counterparty_name", "Counterparty", FieldType::Text),
                    FieldMeta::new(
                        "counterparty_account",
                        "Counterparty account",
                        FieldType::Text,
                    ),
                    FieldMeta::new("reference", "Reference", FieldType::Text),
                    FieldMeta::new(
                        "bank_transaction_id",
                        "Bank's transaction id",
                        FieldType::Text,
                    )
                    .help("Unique per bank account: a line is imported once."),
                    FieldMeta::new("posting_id", "Matched posting", FieldType::Posting).readonly(),
                    FieldMeta::new("payment_id", "Payment", FieldType::document("payment"))
                        .readonly(),
                    FieldMeta::new("account_id", "Post to account", FieldType::link("account"))
                        .readonly(),
                    money("difference", "Difference")
                        .readonly()
                        .help("The part of the line posted to the account, beside the posting."),
                    FieldMeta::new(
                        "difference_posting_id",
                        "Difference posting",
                        FieldType::Posting,
                    )
                    .readonly(),
                ],
            }],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("posted", "Reconciled"),
            ],
            initial_state: "draft".into(),
            transitions: vec![("draft".into(), "posted".into())],
            summary: vec![
                FieldMeta::new(
                    "bank_account_id",
                    "Bank account",
                    FieldType::link("account"),
                )
                .readonly(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly(),
                FieldMeta::new("closing_balance", "Closing balance", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly(),
                FieldMeta::new("lines", "Lines", FieldType::Int).readonly(),
                FieldMeta::new("unmatched", "Lines not matched", FieldType::Int).readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "bank statement body")?;
        let account = bank_account(&mut *ctx.conn, body.bank_account_id).await?;
        if account.company_id != doc.company_id {
            return Err(Error::field(
                "bank_account_id",
                "must be an account of the statement's company",
            ));
        }
        if !account.is_bank || account.is_group {
            return Err(Error::field(
                "bank_account_id",
                "must be a bank or cash account (is_bank)",
            ));
        }
        if body.period_end < body.period_start {
            return Err(Error::field(
                "period_end",
                "must not be before period_start",
            ));
        }
        let places = erpcraft_gl::currency_decimals(&mut *ctx.conn, &account.currency).await?;
        let max = parse::max_amount();
        for (field, balance) in [
            ("opening_balance", body.opening_balance),
            ("closing_balance", body.closing_balance),
        ] {
            if balance.abs() > max {
                return Err(Error::field(field, "is more than an amount can be"));
            }
        }
        if body.lines.len() > parse::MAX_LINES {
            return Err(Error::field(
                "lines",
                format!("a statement holds at most {} lines", parse::MAX_LINES),
            ));
        }
        let mut sum = Decimal::ZERO;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            if line.amount.abs() > max {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("amount"),
                    "is more than an amount can be",
                ));
            }
            if line.booking_date < body.period_start || line.booking_date > body.period_end {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("booking_date"),
                    format!(
                        "{} is outside the statement's period, {} to {}",
                        line.booking_date, body.period_start, body.period_end
                    ),
                ));
            }
            if line.amount.is_zero() {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("amount"),
                    "an amount cannot be zero",
                ));
            }
            if line.amount.normalize().scale() > places {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("amount"),
                    format!("{} has at most {places} decimal places", account.currency),
                ));
            }
            // At most MAX_LINES amounts below the cap: this cannot overflow.
            sum += line.amount;
        }
        // The statement's own arithmetic.
        if body.opening_balance + sum != body.closing_balance {
            return Err(Error::field(
                "closing_balance",
                format!(
                    "is {}, but the opening balance {} plus the lines ({}) comes to {}",
                    body.closing_balance.normalize(),
                    body.opening_balance.normalize(),
                    sum.normalize(),
                    (body.opening_balance + sum).normalize()
                ),
            ));
        }
        // Statements of an account follow each other: this one starts where
        // the last one before it ended, and none comes after it.
        let last: Option<(Option<String>, NaiveDate, Decimal)> = sqlx::query_as(
            "SELECT d.number, s.period_end, s.closing_balance
             FROM bank_statement s JOIN document d ON d.id = s.document_id
             WHERE s.bank_account_id = $1 AND s.document_id <> $2
             ORDER BY s.period_end DESC, d.created_at DESC LIMIT 1",
        )
        .bind(body.bank_account_id)
        .bind(doc.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some((number, end, closing)) = last {
            let name = number.unwrap_or_else(|| "the draft statement".into());
            if end > body.period_start {
                return Err(Error::field(
                    "period_start",
                    format!(
                        "must not be before {end}, where {name}, the account's last statement, \
                         ends"
                    ),
                ));
            }
            if closing != body.opening_balance {
                return Err(Error::field(
                    "opening_balance",
                    format!(
                        "is {}, but {name}, the statement before, closes at {}",
                        body.opening_balance.normalize(),
                        closing.normalize()
                    ),
                ));
            }
        }
        // A transaction of the bank is on one statement.
        let ids: Vec<String> = body
            .lines
            .iter()
            .filter_map(|l| l.bank_transaction_id.clone())
            .collect();
        let mut unique = HashSet::new();
        for (index, line) in body.lines.iter().enumerate() {
            if let Some(id) = &line.bank_transaction_id
                && !unique.insert(id.clone())
            {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("bank_transaction_id"),
                    format!("transaction {id} is on the statement twice"),
                ));
            }
        }
        let taken: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT l.bank_transaction_id, d.number FROM bank_statement_line l
             JOIN document d ON d.id = l.document_id
             WHERE l.bank_account_id = $1 AND l.bank_transaction_id = ANY($2)
               AND l.document_id <> $3 LIMIT 1",
        )
        .bind(body.bank_account_id)
        .bind(&ids)
        .bind(doc.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some((id, number)) = taken {
            return Err(Error::validation(format!(
                "transaction {id} is already on statement {}",
                number.unwrap_or_else(|| "(a draft)".into())
            ))
            .with_field("lines"));
        }

        sqlx::query(
            "INSERT INTO bank_statement (document_id, bank_account_id, currency, period_start,
                 period_end, opening_balance, closing_balance, source_format)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (document_id) DO UPDATE SET bank_account_id = EXCLUDED.bank_account_id,
                 currency = EXCLUDED.currency, period_start = EXCLUDED.period_start,
                 period_end = EXCLUDED.period_end, opening_balance = EXCLUDED.opening_balance,
                 closing_balance = EXCLUDED.closing_balance, source_format = EXCLUDED.source_format",
        )
        .bind(doc.id)
        .bind(body.bank_account_id)
        .bind(&account.currency)
        .bind(body.period_start)
        .bind(body.period_end)
        .bind(body.opening_balance)
        .bind(body.closing_balance)
        .bind(&body.source_format)
        .execute(&mut *ctx.conn)
        .await?;

        // What a kept line was matched to stays, as long as the line is the
        // same money in the same account.
        let before: HashMap<Uuid, Line> = load_lines(&mut *ctx.conn, doc.id)
            .await?
            .into_iter()
            .map(|l| (l.id, l))
            .collect();
        let ids = document::line_ids(
            &mut *ctx.conn,
            "bank_statement_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM bank_statement_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, (line, id)) in body.lines.iter().zip(&ids).enumerate() {
            let kept = before.get(id).filter(|b| b.amount == line.amount);
            let text = |t: &Option<String>| {
                t.as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| t.chars().take(500).collect::<String>())
            };
            sqlx::query(
                "INSERT INTO bank_statement_line (id, document_id, line_no, bank_account_id,
                     booking_date, value_date, amount, counterparty_name, counterparty_account,
                     reference, bank_transaction_id, posting_id, payment_id, account_id, difference)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
            )
            .bind(id)
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(body.bank_account_id)
            .bind(line.booking_date)
            .bind(line.value_date)
            .bind(line.amount)
            .bind(text(&line.counterparty_name))
            .bind(text(&line.counterparty_account))
            .bind(text(&line.reference))
            .bind(text(&line.bank_transaction_id))
            .bind(kept.and_then(|k| k.posting_id))
            .bind(kept.and_then(|k| k.payment_id))
            .bind(kept.and_then(|k| k.account_id))
            .bind(kept.map_or(Decimal::ZERO, |k| k.difference))
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let Ok(header) = load_header(&mut *conn, doc.id).await else {
            return Ok(json!({ "lines": [] }));
        };
        let lines = load_lines(&mut *conn, doc.id).await?;
        let mut body = serde_json::to_value(header).map_err(Error::internal)?;
        body["lines"] = serde_json::to_value(lines).map_err(Error::internal)?;
        Ok(body)
    }

    async fn summary(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let Ok(header) = load_header(&mut *conn, doc.id).await else {
            return Ok(json!({}));
        };
        let lines = load_lines(&mut *conn, doc.id).await?;
        Ok(json!({
            "bank_account_id": header.bank_account_id,
            "currency": header.currency,
            "closing_balance": header.closing_balance,
            "lines": lines.len(),
            "unmatched": lines.iter().filter(|l| !is_matched(l)).count(),
        }))
    }
}

// ----------------------------------------------------------------- import

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Format {
    /// Comma- or semicolon-separated, read with the bank account's saved
    /// `bank_csv_mapping`.
    Csv,
    /// ISO 20022 bank-to-customer statement, camt.053 (XML).
    Camt053,
    /// Open Financial Exchange, version 1 (SGML) or 2 (XML).
    Ofx,
}

impl Format {
    fn as_str(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Camt053 => "camt053",
            Format::Ofx => "ofx",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportInput {
    /// The bank or cash account the statement is for.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "account", "key": "id", "filters": { "is_bank": "true" } }))]
    bank_account_id: Uuid,
    /// The file's format: `csv`, `camt053` or `ofx`.
    format: Format,
    /// The file's content, as text. At most 5 MiB and 10,000 transactions.
    content: String,
    /// The balance before the first line. Only where the file states none
    /// (CSV) and there is no statement of the account before; otherwise
    /// taken from the file, or from the statement before.
    opening_balance: Option<Decimal>,
}

struct Import;

#[async_trait]
impl Command for Import {
    type Input = ImportInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "bank_statement.import",
            "Read a bank statement file (CSV with the account's saved bank_csv_mapping, \
             camt.053, or OFX) into a draft bank statement. A file whose transactions were all \
             imported before returns that statement and creates nothing; one that repeats only \
             some of them is refused. A malformed file is refused with the line it is about.",
        )
        .permission("bank_statement:create")
        .label("Import statement")
        .resource(BANK_STATEMENT)
        .target(false)
        .creates(BANK_STATEMENT)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ImportInput) -> Result<DocumentView> {
        let account = bank_account(&mut *ctx.conn, input.bank_account_id).await?;
        ctx.require("bank_statement:create", account.company_id)?;
        if input.content.len() > parse::MAX_BYTES {
            return Err(Error::field("content", "the file is larger than 5 MiB"));
        }
        let mapping = match input.format {
            Format::Csv => Some(load_mapping(&mut *ctx.conn, input.bank_account_id).await?),
            _ => None,
        };
        // Reading a file is work for the processor, not for the runtime
        // that answers other requests: it runs on a thread of its own.
        let (format, content) = (input.format, input.content);
        let parsed = tokio::task::spawn_blocking(move || match (format, mapping) {
            (Format::Csv, Some(mapping)) => parse::csv(&content, &mapping),
            (Format::Camt053, _) => parse::camt053(&content),
            (Format::Ofx, _) => parse::ofx(&content),
            (Format::Csv, None) => Err("a CSV file needs a mapping".into()),
        })
        .await
        .map_err(Error::internal)?
        .map_err(|problem| Error::field("content", format!("cannot be read: {problem}")))?;
        // A CSV file names no currency: it is the account's. The others
        // must say.
        let currency = match (&parsed.currency, input.format) {
            (Some(currency), _) => currency.clone(),
            (None, Format::Csv) => account.currency.clone(),
            (None, _) => {
                return Err(Error::field(
                    "content",
                    "names no currency: it cannot be checked",
                ));
            }
        };
        let named_account = same_account(
            &mut *ctx.conn,
            input.bank_account_id,
            parsed.account.as_deref(),
        )
        .await?;
        if !currency.eq_ignore_ascii_case(&account.currency) {
            return Err(Error::field(
                "content",
                format!(
                    "is a statement in {}; the bank account holds {}",
                    parse::quoted(&currency),
                    account.currency
                ),
            ));
        }
        if parsed.lines.is_empty() {
            return Err(Error::field("content", "holds no transactions"));
        }

        // Imported before, whole or in part?
        let ids: Vec<String> = parsed
            .lines
            .iter()
            .filter_map(|l| l.transaction_id.clone())
            .collect();
        let before: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT document_id, bank_transaction_id FROM bank_statement_line
             WHERE bank_account_id = $1 AND bank_transaction_id = ANY($2)",
        )
        .bind(input.bank_account_id)
        .bind(&ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
        if !before.is_empty() {
            let statements: HashSet<Uuid> = before.iter().map(|(d, _)| *d).collect();
            if before.len() == parsed.lines.len() && statements.len() == 1 {
                ctx.no_change();
                let id = *statements.iter().next().unwrap_or(&Uuid::nil());
                return document::get(ctx, BANK_STATEMENT, id).await;
            }
            return Err(Error::field(
                "content",
                format!(
                    "{} of its {} transactions were imported before; import a file of new \
                     transactions only",
                    before.len(),
                    parsed.lines.len()
                ),
            ));
        }

        let previous: Option<Decimal> = sqlx::query_scalar(
            "SELECT closing_balance FROM bank_statement WHERE bank_account_id = $1
             ORDER BY period_end DESC LIMIT 1",
        )
        .bind(input.bank_account_id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let too_much = || Error::field("content", "adds up to more than an amount can be");
        let moved = parsed
            .lines
            .iter()
            .try_fold(Decimal::ZERO, |sum, l| sum.checked_add(l.amount))
            .ok_or_else(too_much)?;
        let opening = parsed
            .opening
            .or(input.opening_balance)
            .or(previous)
            .or_else(|| parsed.closing.and_then(|c| c.checked_sub(moved)))
            .ok_or_else(|| {
                Error::field(
                    "opening_balance",
                    "is needed: the file states no balance and the account has no statement \
                     before",
                )
            })?;
        let closing = match parsed.closing {
            Some(closing) => closing,
            None => opening.checked_add(moved).ok_or_else(too_much)?,
        };
        let dates: Vec<NaiveDate> = parsed.lines.iter().filter_map(|l| l.booking_date).collect();
        let start = parsed
            .period_start
            .or_else(|| dates.iter().min().copied())
            .ok_or_else(|| Error::field("content", "holds no dates"))?;
        let end = parsed
            .period_end
            .or_else(|| dates.iter().max().copied())
            .unwrap_or(start);
        let lines: Vec<Value> = parsed
            .lines
            .iter()
            .map(|l| {
                json!({
                    "booking_date": l.booking_date,
                    "value_date": l.value_date,
                    "amount": l.amount,
                    "counterparty_name": l.counterparty_name,
                    "counterparty_account": l.counterparty_account,
                    "reference": l.reference,
                    "bank_transaction_id": l.transaction_id,
                })
            })
            .collect();
        document::create(
            ctx,
            document::NewDocument {
                doc_type: BANK_STATEMENT.into(),
                company_id: account.company_id,
                doc_date: end,
                memo: (!named_account).then(|| {
                    "The file names no account: check that it is a statement of this bank \
                     account."
                        .to_owned()
                }),
                body: json!({
                    "bank_account_id": input.bank_account_id,
                    "period_start": start,
                    "period_end": end,
                    "opening_balance": opening,
                    "closing_balance": closing,
                    "source_format": input.format.as_str(),
                    "lines": lines,
                }),
            },
        )
        .await
    }
}

async fn load_mapping(conn: &mut PgConnection, account: Uuid) -> Result<parse::CsvMapping> {
    #[derive(sqlx::FromRow)]
    struct Row {
        delimiter: String,
        has_header: bool,
        date_format: String,
        decimal_comma: bool,
        booking_date_column: String,
        value_date_column: Option<String>,
        amount_column: Option<String>,
        debit_column: Option<String>,
        credit_column: Option<String>,
        counterparty_name_column: Option<String>,
        counterparty_account_column: Option<String>,
        reference_column: Option<String>,
        transaction_id_column: Option<String>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT delimiter, has_header, date_format, decimal_comma, booking_date_column,
                value_date_column, amount_column, debit_column, credit_column,
                counterparty_name_column, counterparty_account_column, reference_column,
                transaction_id_column
         FROM bank_csv_mapping WHERE bank_account_id = $1",
    )
    .bind(account)
    .fetch_optional(&mut *conn)
    .await?;
    let row = row.ok_or_else(|| {
        Error::field(
            "format",
            "a CSV file needs a bank_csv_mapping for the bank account: save one first",
        )
    })?;
    Ok(parse::CsvMapping {
        delimiter: row.delimiter.bytes().next().unwrap_or(b','),
        has_header: row.has_header,
        date_format: row.date_format,
        decimal_comma: row.decimal_comma,
        booking_date: row.booking_date_column,
        value_date: row.value_date_column,
        amount: row.amount_column,
        debit: row.debit_column,
        credit: row.credit_column,
        counterparty_name: row.counterparty_name_column,
        counterparty_account: row.counterparty_account_column,
        reference: row.reference_column,
        transaction_id: row.transaction_id_column,
    })
}

// ------------------------------------------------------------------ match

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MatchAction {
    /// The line is this posting, which moved the bank account: a payment
    /// recorded before, a transfer, a journal entry.
    Posting,
    /// The line pays these open items: a payment is drafted for them,
    /// through the bank account, dated the booking date. Post it before the
    /// statement.
    OpenItems,
    /// The line is posted to this account when the statement is posted:
    /// a bank fee, interest.
    Account,
    /// Forget what the line was matched to.
    Clear,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MatchInput {
    /// Bank statement id.
    id: Uuid,
    /// The line, by its id.
    line_id: Uuid,
    /// What the line is: `posting`, `open_items`, `account` or `clear`.
    action: MatchAction,
    /// For `posting`: the posting, as bank_statement.proposals names it.
    posting_id: Option<Uuid>,
    /// For `open_items`: the items to pay, all of one party.
    open_item_ids: Option<Vec<Uuid>>,
    /// For `account`: the account to post the line to. For `posting`
    /// with an amount that differs: the account for the difference, which
    /// the company's policy must accept.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "account", "key": "id" }))]
    account_id: Option<Uuid>,
}

struct Match;

#[async_trait]
impl Command for Match {
    type Input = MatchInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "bank_statement.match",
            "Say what one line of a draft bank statement is: a posting that moved the bank \
             account (a recorded payment), open items it pays (a payment is drafted for them), \
             or an account to post it to when the statement is posted (fees, interest); or \
             clear what it was. See bank_statement.proposals for what each line probably is.",
        )
        .permission("bank_statement:match")
        .label("Match line")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: MatchInput) -> Result<DocumentView> {
        let doc = document::lock(ctx, BANK_STATEMENT, input.id, "match").await?;
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} bank statement is no longer matched",
                doc.status
            )));
        }
        let header = load_header(&mut *ctx.conn, doc.id).await?;
        let line = load_lines(&mut *ctx.conn, doc.id)
            .await?
            .into_iter()
            .find(|l| l.id == input.line_id)
            .ok_or_else(|| Error::field("line_id", "is not a line of this statement"))?;
        let (mut posting, mut payment, mut account, mut difference) =
            (None, None, None, Decimal::ZERO);
        match input.action {
            MatchAction::Clear => {}
            MatchAction::Posting => {
                let posting_id = input
                    .posting_id
                    .ok_or_else(|| Error::field("posting_id", "is needed to match a posting"))?;
                let moved =
                    reconcile::posting_amount(&mut *ctx.conn, doc.company_id, &header, posting_id)
                        .await?
                        .ok_or_else(|| {
                            Error::field(
                                "posting_id",
                                "must be a posting of the company that moved this bank account and \
                         was not reversed",
                            )
                        })?;
                if let Some(number) = matched_elsewhere(&mut *ctx.conn, posting_id, line.id).await?
                {
                    return Err(Error::field(
                        "posting_id",
                        format!(
                            "is matched to a line of statement {} already",
                            number.unwrap_or_else(|| "(a draft)".into())
                        ),
                    ));
                }
                if moved != line.amount {
                    let Some(account_id) = input.account_id else {
                        return Err(Error::field(
                            "posting_id",
                            format!(
                                "moved {} through the account, the line {}; give account_id \
                                 for the difference if the company accepts it",
                                moved.normalize(),
                                line.amount.normalize()
                            ),
                        ));
                    };
                    let diff = line.amount - moved;
                    let shown = |d: Decimal| d.normalize().to_string();
                    ctx.decide(
                        AMOUNT_DIFFERENCE,
                        doc.company_id,
                        json!({
                            "line_amount": shown(line.amount),
                            "posting_amount": shown(moved),
                            "difference": shown(diff),
                            "difference_abs": shown(diff.abs()),
                        }),
                    )
                    .await?;
                    check_post_account(&mut *ctx.conn, doc.company_id, &header, account_id).await?;
                    account = Some(account_id);
                    difference = diff;
                }
                posting = Some(posting_id);
            }
            MatchAction::OpenItems => {
                let items = input
                    .open_item_ids
                    .clone()
                    .filter(|i| !i.is_empty())
                    .ok_or_else(|| Error::field("open_item_ids", "name at least one open item"))?;
                // Drafting a payment is the payments module's: its own command,
                // with its own checks, under the caller's rights.
                ctx.require("payment:create", doc.company_id)?;
                let draft = ctx
                    .kernel
                    .registry()
                    .command("payment.draft")?
                    .handler
                    .clone();
                let created = draft
                    .run_json(
                        ctx,
                        json!({
                            "company_id": doc.company_id,
                            "open_item_ids": items,
                            "bank_account_id": header.bank_account_id,
                            "doc_date": line.booking_date,
                        }),
                    )
                    .await?;
                payment = Some(
                    created["id"]
                        .as_str()
                        .and_then(|id| Uuid::parse_str(id).ok())
                        .ok_or_else(|| Error::internal("payment.draft returned no id"))?,
                );
            }
            MatchAction::Account => {
                let account_id = input
                    .account_id
                    .ok_or_else(|| Error::field("account_id", "is needed to post to an account"))?;
                check_post_account(&mut *ctx.conn, doc.company_id, &header, account_id).await?;
                account = Some(account_id);
                difference = line.amount;
            }
        }
        sqlx::query(
            "UPDATE bank_statement_line SET posting_id = $2, payment_id = $3, account_id = $4,
                 difference = $5 WHERE id = $1",
        )
        .bind(line.id)
        .bind(posting)
        .bind(payment)
        .bind(account)
        .bind(difference)
        .execute(&mut *ctx.conn)
        .await?;
        let mut doc = doc;
        document::refresh_summary(ctx, &mut doc).await?;
        document::view(ctx, doc).await
    }
}

/// The statement of another line that already explains `posting_id`, if
/// one does: matched to it, posting it as its difference, or matched to the
/// payment it posts. One movement of money explains one bank line. `None`
/// when no line does; `Some(None)` for a draft statement, which has no
/// number.
async fn matched_elsewhere(
    conn: &mut PgConnection,
    posting_id: Uuid,
    line_id: Uuid,
) -> Result<Option<Option<String>>> {
    Ok(sqlx::query_scalar(
        "SELECT d.number FROM bank_statement_line l
         JOIN document d ON d.id = l.document_id
         WHERE l.id <> $2
           AND (l.posting_id = $1 OR l.difference_posting_id = $1
                OR EXISTS (SELECT 1 FROM posting p
                           WHERE p.id = $1 AND p.document_id = l.payment_id))
         LIMIT 1",
    )
    .bind(posting_id)
    .bind(line_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// An account a line, or part of it, may be posted to: of the company, not
/// a group, not the bank account itself.
async fn check_post_account(
    conn: &mut PgConnection,
    company_id: Uuid,
    header: &Header,
    account_id: Uuid,
) -> Result<()> {
    let ok: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM account WHERE id = $1 AND company_id = $2 AND NOT is_group
                        AND active)",
    )
    .bind(account_id)
    .bind(company_id)
    .fetch_one(&mut *conn)
    .await?;
    if !ok || account_id == header.bank_account_id {
        return Err(Error::field(
            "account_id",
            "must be an active account of the company that can be posted to, other than the \
             bank account",
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------- post

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PostInput {
    /// Bank statement id.
    id: Uuid,
}

struct Post;

#[async_trait]
impl Command for Post {
    type Input = PostInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "bank_statement.post",
            "Reconcile a bank statement and lock it: every line must be matched, and every \
             payment drafted for a line posted, through the bank account, for the line's \
             amount. Lines and differences matched to an account are posted, one posting per \
             line. Posting a statement that is posted returns it unchanged.",
        )
        .permission("bank_statement:post")
        .label("Reconcile")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, BANK_STATEMENT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        let header = load_header(&mut *ctx.conn, doc.id).await?;
        let lines = load_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation(
                "a statement without lines has nothing to reconcile",
            ));
        }
        // The statement still follows the one before and adds up: saving
        // the body checked it, and nothing has changed it since but this.
        for line in &lines {
            let n = line.line_no as usize;
            let mut posting = line.posting_id;
            if let Some(payment) = line.payment_id {
                let found: Option<(String, Option<String>)> = sqlx::query_as(
                    "SELECT status, number FROM document WHERE id = $1 AND doc_type = 'payment'",
                )
                .bind(payment)
                .fetch_optional(&mut *ctx.conn)
                .await?;
                let (status, number) = found.ok_or_else(|| {
                    Error::line("lines", n, None, "the payment drafted for the line is gone")
                })?;
                if status != "posted" {
                    let which = number.map_or("drafted for the line".to_owned(), |n| n);
                    return Err(Error::line(
                        "lines",
                        n,
                        None,
                        format!("the payment {which} is {status}, not posted: post it first"),
                    ));
                }
                let id: Option<Uuid> = sqlx::query_scalar(
                    "SELECT id FROM posting WHERE document_id = $1 AND reverses_id IS NULL
                     ORDER BY created_at LIMIT 1",
                )
                .bind(payment)
                .fetch_optional(&mut *ctx.conn)
                .await?;
                posting = id;
            }
            if let Some(posting_id) = posting {
                if let Some(number) = matched_elsewhere(&mut *ctx.conn, posting_id, line.id).await?
                {
                    return Err(Error::line(
                        "lines",
                        n,
                        None,
                        format!(
                            "what the line is matched to explains a line of statement {} \
                             already: one movement explains one bank line",
                            number.unwrap_or_else(|| "(a draft)".into())
                        ),
                    ));
                }
                let moved =
                    reconcile::posting_amount(&mut *ctx.conn, doc.company_id, &header, posting_id)
                        .await?;
                let expected = line.amount - line.difference;
                if moved != Some(expected) {
                    return Err(Error::line(
                        "lines",
                        n,
                        None,
                        format!(
                            "the line is {}, but what it is matched to moved {} through the \
                             bank account",
                            line.amount.normalize(),
                            moved.map_or("nothing".into(), |m| m.normalize().to_string())
                        ),
                    ));
                }
            } else if line.account_id.is_none() {
                return Err(Error::line(
                    "lines",
                    n,
                    None,
                    "the line is not matched: match it, or post it to an account",
                ));
            }
            // What the statement posts itself: a line, or the difference.
            let mut own = None;
            if let Some(account_id) = line.account_id
                && !line.difference.is_zero()
            {
                let memo = line
                    .reference
                    .clone()
                    .or_else(|| line.counterparty_name.clone());
                let made =
                    document::create_posting(ctx, &doc, line.booking_date, memo.clone()).await?;
                let (inflow, amount) = (line.difference > Decimal::ZERO, line.difference.abs());
                let entry = |account: Uuid, debit: bool| GlLine {
                    account_id: account,
                    debit: if debit { amount } else { Decimal::ZERO },
                    credit: if debit { Decimal::ZERO } else { amount },
                    currency: Some(header.currency.clone()),
                    fx_rate: None,
                    memo: memo.clone(),
                    source_line_id: Some(line.id),
                    ..GlLine::default()
                };
                erpcraft_gl::post(
                    ctx,
                    &made,
                    &[
                        entry(header.bank_account_id, inflow),
                        entry(account_id, !inflow),
                    ],
                )
                .await
                .map_err(|e| e.with_line("lines", n))?;
                own = Some(made.id);
            }
            sqlx::query(
                "UPDATE bank_statement_line SET posting_id = $2, difference_posting_id = $3
                 WHERE id = $1",
            )
            .bind(line.id)
            .bind(posting.or(own))
            .bind(if posting.is_some() { own } else { None })
            .execute(&mut *ctx.conn)
            .await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}
