//! Open items: what the company owes and is owed, item by item.
//!
//! A module that posts an invoice opens an item with [`open`], under the
//! same posting as its general ledger entries. A module that posts a
//! payment settles items with [`settle`]. This crate keeps the detail
//! behind the payables and receivables accounts and knows nothing about
//! purchasing, sales or payments, so those modules do not depend on each
//! other.
//!
//! The rules are database triggers, so they hold for every module:
//!
//! - an item is never settled for more than its amount;
//! - an item is a debt (more than zero) or a credit (less than zero: a
//!   credit note, money paid on account), and its settlements run its way,
//!   so a payment can settle debts and credits of one party together;
//! - when the posting that opened an item is reversed, the item is called
//!   off, which is refused once any of it has been paid;
//! - when a posting that settled items is reversed, exactly those
//!   settlements are taken back;
//! - items and settlements are never edited.
//!
//! This crate does not post to the general ledger. The caller posts the
//! matching entries to the item's account in the same posting.

use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_kernel::document;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DynCommand, EntityMeta, Error, JsonSchema, Ledger,
    Migration, Module, PgConnection, Posting, Result, async_trait, command, new_id, schemars,
};

mod aging;
mod terms;

use serde::{Deserialize, Serialize};
pub use terms::{Terms, discount_steps, due_dates, instalments};
use uuid::Uuid;

pub struct OpenItemsModule;

impl Module for OpenItemsModule {
    fn name(&self) -> &'static str {
        "open_items"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "open_items",
                sql: include_str!("../migrations/0001_open_items.sql"),
            },
            Migration {
                version: 2,
                name: "credits",
                sql: include_str!("../migrations/0002_credits.sql"),
            },
            Migration {
                version: 3,
                name: "currency",
                sql: include_str!("../migrations/0003_currency.sql"),
            },
            Migration {
                version: 4,
                name: "terms",
                sql: include_str!("../migrations/0004_terms.sql"),
            },
            Migration {
                version: 5,
                name: "revaluation",
                sql: include_str!("../migrations/0005_revaluation.sql"),
            },
            Migration {
                version: 6,
                name: "settlement_discount",
                sql: include_str!("../migrations/0006_settlement_discount.sql"),
            },
            Migration {
                version: 7,
                name: "discount_steps",
                sql: include_str!("../migrations/0007_discount_steps.sql"),
            },
            Migration {
                version: 8,
                name: "instalments",
                sql: include_str!("../migrations/0008_instalments.sql"),
            },
            Migration {
                version: 9,
                name: "instalment_locks",
                sql: include_str!("../migrations/0009_instalment_locks.sql"),
            },
            Migration {
                version: 10,
                name: "rate_precision",
                sql: include_str!("../migrations/0010_rate_precision.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![
            terms::entity(),
            terms::step_entity(),
            terms::instalment_entity(),
        ]
    }

    fn ledgers(&self) -> Vec<Arc<dyn Ledger>> {
        vec![Arc::new(OpenItemLedger)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(List), command(aging::Aging)]
    }
}

/// How a field that holds the id of an open item finds the items to
/// choose from: through `open_item.list`, which leaves out what is settled.
/// The caller adds the inputs that narrow it, such as the company and the
/// kind: `lookup().field("company_id", "company_id")`.
pub fn lookup() -> erpcraft_kernel::Lookup {
    erpcraft_kernel::Lookup::new("open_item.list", "items", "id")
        .display(&["number", "doc_date", "open"])
        .by_keys("ids")
}

/// Which way the debt runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The company owes the party.
    Payable,
    /// The party owes the company.
    Receivable,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Payable => "payable",
            Kind::Receivable => "receivable",
        }
    }
}

/// A debt to record.
#[derive(Debug, Clone)]
pub struct NewOpenItem {
    pub kind: Kind,
    /// The entity the party is a record of: `vendor`, `customer`.
    pub party_type: &'static str,
    pub party_id: Uuid,
    /// The general ledger account that carries the debt.
    pub account_id: Uuid,
    /// In the company's accounting currency. More than zero for a debt;
    /// less than zero for a credit the party is owed back against its
    /// debts: a credit note, or money paid on account.
    pub amount: Decimal,
    /// When it is due, if no `terms` say. `None`: no date.
    pub due_date: Option<NaiveDate>,
    /// The payment terms the item is owed on: they set its due date from
    /// the posting's date, and the early-payment discount it offers.
    pub terms: Option<Uuid>,
    /// What is owed in the currency of the document, when that is not the
    /// accounting currency: the currency and the amount, of the same sign
    /// as `amount`. `None`: the accounting currency, the same amount.
    pub foreign: Option<(String, Decimal)>,
}

/// Record a debt under `posting`. Returns the item's id.
pub async fn open(conn: &mut PgConnection, posting: &Posting, item: &NewOpenItem) -> Result<Uuid> {
    if item.amount.is_zero() && item.foreign.as_ref().is_none_or(|(_, txn)| txn.is_zero()) {
        return Err(Error::validation("an open item cannot be for nothing"));
    }
    let (currency, txn_amount) = match &item.foreign {
        Some((currency, txn)) => (Some(currency.as_str()), *txn),
        None => (None, item.amount),
    };
    let (due_date, steps, parts) = match item.terms {
        Some(terms) => {
            let terms = Terms::load(conn, terms).await?;
            let decimals = match currency {
                Some(currency) => {
                    let decimals: Option<i16> =
                        sqlx::query_scalar("SELECT decimals FROM currency WHERE code = $1")
                            .bind(currency)
                            .fetch_optional(&mut *conn)
                            .await?;
                    decimals.map(|d| d as u32)
                }
                None => None,
            };
            let decimals = match decimals {
                Some(decimals) => decimals,
                None => {
                    erpcraft_kernel::company::currency_decimals(conn, posting.company_id).await?
                }
            };
            let (due, _) = due_dates(&terms, posting.posting_date, txn_amount, decimals);
            let steps = discount_steps(&terms, posting.posting_date, txn_amount, decimals);
            let parts = instalments(&terms, posting.posting_date, txn_amount, decimals);
            (Some(due), steps, parts)
        }
        None => (item.due_date, vec![], vec![]),
    };
    // The first step is the item's discount; every step is kept apart.
    let discount = steps.first().copied();
    let id = new_id();
    sqlx::query(
        "INSERT INTO open_item (id, company_id, posting_id, kind, party_type, party_id, account_id,
            amount, doc_date, due_date, currency, txn_amount, discount_date, discount_amount)
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                COALESCE($11, c.base_currency), $12, $13, $14
         FROM company c WHERE c.id = $2",
    )
    .bind(id)
    .bind(posting.company_id)
    .bind(posting.id)
    .bind(item.kind.as_str())
    .bind(item.party_type)
    .bind(item.party_id)
    .bind(item.account_id)
    .bind(item.amount)
    .bind(posting.posting_date)
    .bind(due_date)
    .bind(currency)
    .bind(txn_amount)
    .bind(discount.map(|(date, _)| date))
    .bind(discount.map(|(_, amount)| amount))
    .execute(&mut *conn)
    .await?;
    if !steps.is_empty() {
        sqlx::query(
            "INSERT INTO open_item_discount (open_item_id, until, amount)
             SELECT $1, s.until, s.amount FROM unnest($2::date[], $3::numeric[]) AS s(until, amount)",
        )
        .bind(id)
        .bind(steps.iter().map(|(until, _)| *until).collect::<Vec<_>>())
        .bind(steps.iter().map(|(_, amount)| *amount).collect::<Vec<_>>())
        .execute(&mut *conn)
        .await?;
    }
    // The parts it is due in, when the terms make it due in parts.
    if !parts.is_empty() {
        let seqs: Vec<i32> = (1..=parts.len() as i32).collect();
        sqlx::query(
            "INSERT INTO open_item_instalment (open_item_id, seq, due_date, txn_amount)
             SELECT $1, p.seq, p.due_date, p.amount
             FROM unnest($2::int4[], $3::date[], $4::numeric[]) AS p(seq, due_date, amount)",
        )
        .bind(id)
        .bind(&seqs)
        .bind(parts.iter().map(|(due, _)| *due).collect::<Vec<_>>())
        .bind(parts.iter().map(|(_, amount)| *amount).collect::<Vec<_>>())
        .execute(&mut *conn)
        .await?;
    }
    Ok(id)
}

/// What the open item a document's posting opened is worth in the
/// accounting currency, as a positive amount: what it posted to its
/// control account. `None` when the document opened none (or its posting
/// was reversed and it was posted again without one).
pub async fn posted_value(conn: &mut PgConnection, document_id: Uuid) -> Result<Option<Decimal>> {
    let value: Option<Decimal> = sqlx::query_scalar(
        "SELECT i.amount FROM open_item i JOIN posting p ON p.id = i.posting_id
         WHERE p.document_id = $1 AND p.reverses_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)
         ORDER BY i.created_at DESC LIMIT 1",
    )
    .bind(document_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(value.map(|v| v.abs()))
}

/// An item in a foreign currency still open on a day, and what it is
/// carried at then.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Revaluable {
    pub id: Uuid,
    /// `payable` or `receivable`.
    pub kind: String,
    pub account_id: Uuid,
    pub currency: String,
    /// What is open in the item's currency on the day.
    pub txn_open: Decimal,
    /// What that is carried at in the accounting currency on the day: the
    /// item's amount, less what was settled, plus revaluations standing.
    pub carried: Decimal,
}

/// The items of `company_id` in a currency other than its accounting
/// currency that are open on `day`, as postings dated on or before it left
/// them. Locks them until the transaction ends.
pub async fn revaluable(
    conn: &mut PgConnection,
    company_id: Uuid,
    day: NaiveDate,
) -> Result<Vec<Revaluable>> {
    let items = sqlx::query_as(
        "WITH items AS (
            SELECT i.id, i.kind, i.account_id, i.currency,
                   i.txn_amount - COALESCE((
                       SELECT sum(s.txn_amount) FROM settlement s
                       JOIN posting p ON p.id = s.posting_id
                       WHERE s.open_item_id = i.id AND p.posting_date <= $2), 0) AS txn_open,
                   i.amount - COALESCE((
                       SELECT sum(s.amount) FROM settlement s
                       JOIN posting p ON p.id = s.posting_id
                       WHERE s.open_item_id = i.id AND p.posting_date <= $2), 0)
                   + COALESCE((
                       SELECT sum(v.amount) FROM open_item_revaluation v
                       JOIN posting p ON p.id = v.posting_id
                       WHERE v.open_item_id = i.id AND p.posting_date <= $2), 0) AS carried
            FROM open_item i JOIN company c ON c.id = i.company_id
            WHERE i.company_id = $1 AND i.doc_date <= $2 AND i.currency <> c.base_currency
            ORDER BY i.id
            FOR UPDATE OF i
         )
         SELECT * FROM items WHERE txn_open <> 0 ORDER BY id",
    )
    .bind(company_id)
    .bind(day)
    .fetch_all(&mut *conn)
    .await?;
    Ok(items)
}

/// A revaluation of one item: what its value changes by (in the item's
/// sign), at which rate, from what.
#[derive(Debug, Clone)]
pub struct Revaluation {
    pub open_item_id: Uuid,
    pub amount: Decimal,
    pub rate: Decimal,
    pub txn_open: Decimal,
    pub carried: Decimal,
}

/// Record revaluations under `posting`. The caller posts the same amounts
/// to the items' accounts in the same posting; reversing the posting takes
/// them back.
pub async fn revalue(
    conn: &mut PgConnection,
    posting: &Posting,
    revaluations: &[Revaluation],
) -> Result<()> {
    if revaluations.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = revaluations.iter().map(|_| new_id()).collect();
    sqlx::query(
        "INSERT INTO open_item_revaluation (id, company_id, posting_id, open_item_id, amount, rate,
            txn_open, carried)
         SELECT r.id, $1, $2, r.item, r.amount, r.rate, r.txn_open, r.carried
         FROM unnest($3::uuid[], $4::uuid[], $5::numeric[], $6::numeric[], $7::numeric[],
                     $8::numeric[]) AS r(id, item, amount, rate, txn_open, carried)
         ORDER BY r.item",
    )
    .bind(posting.company_id)
    .bind(posting.id)
    .bind(&ids)
    .bind(
        revaluations
            .iter()
            .map(|r| r.open_item_id)
            .collect::<Vec<_>>(),
    )
    .bind(revaluations.iter().map(|r| r.amount).collect::<Vec<_>>())
    .bind(revaluations.iter().map(|r| r.rate).collect::<Vec<_>>())
    .bind(revaluations.iter().map(|r| r.txn_open).collect::<Vec<_>>())
    .bind(revaluations.iter().map(|r| r.carried).collect::<Vec<_>>())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The number of a document that revalued one of these items and that a
/// settlement dated `day` would make wrong: a revaluation not reversed by a
/// posting dated on or before `day`. Such a revaluation either still
/// stands on `day`, and the payment would realize against the invoice's
/// rate while it does (counting the movement twice), or is dated after
/// `day` and was worked out on what was open then, which the payment,
/// back-dated, changes. `None` when there is none: the payment may go
/// ahead.
pub async fn standing_revaluation(
    conn: &mut PgConnection,
    ids: &[Uuid],
    day: NaiveDate,
) -> Result<Option<String>> {
    let number: Option<Option<String>> = sqlx::query_scalar(
        "SELECT d.number FROM open_item_revaluation v
         JOIN posting p ON p.id = v.posting_id
         JOIN document d ON d.id = p.document_id
         WHERE v.open_item_id = ANY($1) AND v.reverses_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM open_item_revaluation r
                           JOIN posting rp ON rp.id = r.posting_id
                           WHERE r.reverses_id = v.id AND rp.posting_date <= $2)
         ORDER BY p.posting_date LIMIT 1",
    )
    .bind(ids)
    .bind(day)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(number.map(|n| n.unwrap_or_default()))
}

/// What part of the open item that a document's standing posting opened
/// was kept back as early-payment discount by the settlements that stand:
/// the discount over the item's amount, both in its currency. Zero when no
/// discount was taken. A credit note takes that share of what it credits
/// back as discount.
pub async fn discount_share(conn: &mut PgConnection, document_id: Uuid) -> Result<Decimal> {
    let found: Option<(Decimal, Decimal)> = sqlx::query_as(
        "SELECT i.txn_amount,
                COALESCE((SELECT sum(s.discount) FROM settlement s WHERE s.open_item_id = i.id), 0)
         FROM open_item i JOIN posting p ON p.id = i.posting_id
         WHERE p.document_id = $1 AND p.reverses_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)
         ORDER BY i.created_at DESC LIMIT 1",
    )
    .bind(document_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match found {
        Some((amount, discount)) if !amount.is_zero() && !discount.is_zero() => discount / amount,
        _ => Decimal::ZERO,
    })
}

/// An open item as it stands.
#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub struct OpenItem {
    pub id: Uuid,
    pub company_id: Uuid,
    pub posting_id: Uuid,
    /// `payable` or `receivable`.
    pub kind: String,
    pub party_type: String,
    pub party_id: Uuid,
    pub account_id: Uuid,
    pub amount: Decimal,
    pub settled: Decimal,
    /// What is still owed: amount minus settled. In the accounting
    /// currency, at the rate of the document that opened the item.
    pub open: Decimal,
    pub doc_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    /// The currency of the document: what a payment settles it in.
    pub currency: String,
    /// The amount in that currency, what of it is settled, and what is
    /// still owed. The same as `amount`, `settled` and `open` for an item
    /// in the accounting currency.
    pub txn_amount: Decimal,
    pub txn_settled: Decimal,
    pub txn_open: Decimal,
    /// The last day the early-payment discount may be taken, when the
    /// item offers one.
    pub discount_date: Option<NaiveDate>,
    /// The discount, in the item's currency: paying the rest of the item
    /// less this by the discount date settles it in full.
    pub discount_amount: Option<Decimal>,
    /// Every early-payment discount the item offers, earliest first, when
    /// its terms give more than one step: `[{"until", "amount"}]`.
    pub discount_steps: Option<serde_json::Value>,
    /// The parts the item is due in, when its terms make it due in parts,
    /// earliest first, in its currency: `[{"seq", "due_date", "amount",
    /// "open"}]`. A payment that names no part pays the earliest part
    /// still open first.
    pub instalments: Option<serde_json::Value>,
}

impl OpenItem {
    /// The early-payment discount the item offers when paid on `day`: the
    /// step whose last day is the earliest not before `day`. `None` once
    /// every step has ended, or when it offers none.
    pub fn discount_on(&self, day: NaiveDate) -> Option<(NaiveDate, Decimal)> {
        self.discounts()
            .into_iter()
            .find(|(until, _)| *until >= day)
    }

    /// The last step of the item's early-payment discount, if any.
    pub fn last_discount(&self) -> Option<(NaiveDate, Decimal)> {
        self.discounts().into_iter().last()
    }

    fn discounts(&self) -> Vec<(NaiveDate, Decimal)> {
        let steps: Vec<(NaiveDate, Decimal)> = self
            .discount_steps
            .as_ref()
            .and_then(serde_json::Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|s| {
                        Some((
                            s["until"].as_str()?.parse().ok()?,
                            s["amount"].as_str()?.parse().ok()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if steps.is_empty() {
            return self
                .discount_date
                .zip(self.discount_amount)
                .into_iter()
                .collect();
        }
        steps
    }
}

const ITEM_COLUMNS: &str = "i.id, i.company_id, i.posting_id, i.kind, i.party_type, i.party_id, \
                            i.account_id, i.amount, i.settled, i.amount - i.settled AS open, \
                            i.doc_date, i.due_date, i.currency, i.txn_amount, i.txn_settled, \
                            i.txn_amount - i.txn_settled AS txn_open, i.discount_date, \
                            i.discount_amount, \
                            (SELECT jsonb_agg(jsonb_build_object('until', s.until, \
                                                                 'amount', s.amount::text) \
                                              ORDER BY s.until) \
                             FROM open_item_discount s WHERE s.open_item_id = i.id) \
                                AS discount_steps, \
                            (SELECT jsonb_agg(jsonb_build_object('seq', x.seq, \
                                                                 'due_date', x.due_date, \
                                                                 'amount', trim_scale(x.txn_amount)::text, \
                                                                 'open', trim_scale(x.txn_amount - x.txn_settled)::text) \
                                              ORDER BY x.seq) \
                             FROM open_item_instalments(i.id, NULL) x) AS instalments";

/// Lock these items until the transaction ends and return them as they
/// stand, in the order given. Items are locked in a fixed order, so two
/// payments naming the same items cannot deadlock. Fails if one does not
/// exist in `company_id`.
pub async fn lock(
    conn: &mut PgConnection,
    company_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<OpenItem>> {
    // Locked first, then read in a statement of its own: a statement that
    // waited for a lock reads the locked row as it is now, but what its
    // subqueries read (the parts, from the settlements) as it was before
    // it waited.
    sqlx::query(
        "SELECT id FROM open_item WHERE id = ANY($1) AND company_id = $2 ORDER BY id FOR UPDATE",
    )
    .bind(ids)
    .bind(company_id)
    .execute(&mut *conn)
    .await?;
    let found: Vec<OpenItem> = sqlx::query_as(&format!(
        "SELECT {ITEM_COLUMNS} FROM open_item i WHERE i.id = ANY($1) AND i.company_id = $2"
    ))
    .bind(ids)
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    ids.iter()
        .map(|id| {
            found
                .iter()
                .find(|item| item.id == *id)
                .cloned()
                .ok_or_else(|| Error::not_found("open item"))
        })
        .collect()
}

/// Part or all of one item to settle.
#[derive(Debug, Clone, Copy)]
pub struct Allocation {
    pub open_item_id: Uuid,
    /// In the item's currency. Runs the way of the item (more than zero
    /// for a debt, less than zero for a credit) and is no more than is
    /// still open.
    pub amount: Decimal,
    /// Of `amount`, what is an early-payment discount kept back rather
    /// than paid. Zero for none.
    pub discount: Decimal,
    /// The part of the item it pays, for an item due in parts. `None`: the
    /// earliest part still open first.
    pub instalment: Option<i32>,
}

/// Settle items under `posting`. The ledger refuses to settle more than is
/// open. Lock the items first with [`lock`] to read what is open.
///
/// Returns, for each allocation, what it took off the item's value in the
/// accounting currency: its share at the item's own rate, or all that was
/// left when it settles the last of the item. The caller posts that to the
/// item's control account; for an item in a foreign currency the payment
/// is worth something else at its own rate, and the difference is a
/// realized exchange difference.
pub async fn settle(
    conn: &mut PgConnection,
    posting: &Posting,
    allocations: &[Allocation],
) -> Result<Vec<Decimal>> {
    let mut taken = Vec::with_capacity(allocations.len());
    for allocation in allocations {
        // The trigger works out the accounting amount.
        let amount: Decimal = sqlx::query_scalar(
            "INSERT INTO settlement (id, company_id, posting_id, open_item_id, amount, txn_amount,
                discount, instalment)
             VALUES ($1, $2, $3, $4, 0, $5, $6, $7) RETURNING amount",
        )
        .bind(new_id())
        .bind(posting.company_id)
        .bind(posting.id)
        .bind(allocation.open_item_id)
        .bind(allocation.amount)
        .bind(allocation.discount)
        .bind(allocation.instalment)
        .fetch_one(&mut *conn)
        .await?;
        taken.push(amount);
    }
    Ok(taken)
}

struct OpenItemLedger;

/// What a posting did to open items, as a document shows it: an `item`
/// it created, or a `settlement` of one. Amounts are decimals as text.
#[derive(JsonSchema)]
#[allow(dead_code)] // describes the rows `entries` builds in SQL
struct OpenItemEntryRow {
    /// `item`, `settlement` or `revaluation`.
    entry: Option<String>,
    /// Id of the item or the settlement.
    id: Option<Uuid>,
    /// `payable` or `receivable`.
    kind: Option<String>,
    /// `vendor` or `customer`.
    party_type: Option<String>,
    party_id: Option<Uuid>,
    /// For an item: the control account.
    account_id: Option<Uuid>,
    /// The item's amount, or the amount settled, in the accounting
    /// currency.
    amount: Option<String>,
    /// The currency of the item.
    currency: Option<String>,
    /// The item's amount, or the amount settled, in that currency.
    txn_amount: Option<String>,
    /// For an item: how much of it is settled.
    settled: Option<String>,
    /// For an item: what is still open.
    open: Option<String>,
    /// For an item: its date.
    doc_date: Option<String>,
    /// For an item: when it is due.
    due_date: Option<String>,
    /// For a settlement or a revaluation: the item it settles or revalues.
    open_item_id: Option<Uuid>,
    /// For a revaluation: what was open of the item in its currency.
    txn_open: Option<String>,
    /// For a revaluation: what that was carried at in the accounting
    /// currency before it.
    carried: Option<String>,
    /// For a revaluation: the rate of the day it revalued at.
    rate: Option<String>,
    /// For a settlement: whether it takes back an earlier one.
    cancels: Option<bool>,
    /// For a settlement: the document of the settled item.
    document_id: Option<Uuid>,
    /// Type of that document.
    doc_type: Option<String>,
    /// Number of that document.
    number: Option<String>,
}

#[async_trait]
impl Ledger for OpenItemLedger {
    fn name(&self) -> &'static str {
        "open_items"
    }

    fn label(&self) -> String {
        "Open items".into()
    }

    fn entry_schema(&self) -> serde_json::Value {
        erpcraft_kernel::document::row_schema::<OpenItemEntryRow>()
    }

    fn entry_tables(&self) -> &'static [&'static str] {
        &["open_item", "settlement", "open_item_revaluation"]
    }

    /// Reversing a payment takes its settlements back. Reversing an invoice
    /// calls its items off, which the ledger refuses once any of an item
    /// has been paid.
    async fn reverse(
        &self,
        conn: &mut PgConnection,
        original: &Posting,
        reversal: &Posting,
    ) -> Result<()> {
        // In item order, as every other writer locks them.
        let settlements: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, open_item_id FROM settlement
             WHERE posting_id = $1 AND reverses_id IS NULL ORDER BY open_item_id, id",
        )
        .bind(original.id)
        .fetch_all(&mut *conn)
        .await?;
        for (settlement_id, item_id) in settlements {
            // The trigger sets the amount to the opposite of the original's.
            sqlx::query(
                "INSERT INTO settlement (id, company_id, posting_id, open_item_id, amount, reverses_id)
                 VALUES ($1, $2, $3, $4, 1, $5)",
            )
            .bind(new_id())
            .bind(reversal.company_id)
            .bind(reversal.id)
            .bind(item_id)
            .bind(settlement_id)
            .execute(&mut *conn)
            .await?;
        }

        let items: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM open_item WHERE posting_id = $1 ORDER BY id")
                .bind(original.id)
                .fetch_all(&mut *conn)
                .await?;
        for item_id in items {
            // The trigger sets the amount to the whole item.
            sqlx::query(
                "INSERT INTO settlement (id, company_id, posting_id, open_item_id, amount, cancels)
                 VALUES ($1, $2, $3, $4, 1, true)",
            )
            .bind(new_id())
            .bind(reversal.company_id)
            .bind(reversal.id)
            .bind(item_id)
            .execute(&mut *conn)
            .await?;
        }

        // The trigger takes the opposite of each revaluation.
        sqlx::query(
            "INSERT INTO open_item_revaluation (id, company_id, posting_id, open_item_id, amount,
                rate, txn_open, carried, reverses_id)
             SELECT gen_random_uuid(), v.company_id, $2, v.open_item_id, 1, 1, 0, 0, v.id
             FROM open_item_revaluation v WHERE v.posting_id = $1 AND v.reverses_id IS NULL
             ORDER BY v.open_item_id",
        )
        .bind(original.id)
        .bind(reversal.id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// What the posting did to open items: the items it opened (`entry`
    /// is `item`), then the settlements it made (`entry` is `settlement`),
    /// each with the document whose item it settles, then the revaluations
    /// it made (`entry` is `revaluation`).
    async fn entries(
        &self,
        ctx: &mut Ctx<'_>,
        posting: &Posting,
    ) -> Result<Option<Vec<serde_json::Value>>> {
        if !ctx.actor.can("open_item:read", posting.company_id) {
            return Ok(None);
        }
        let rows = sqlx::query_scalar(
            "SELECT row FROM (
                SELECT 0 AS part, i.id, jsonb_build_object(
                    'entry', 'item', 'id', i.id, 'kind', i.kind,
                    'party_type', i.party_type, 'party_id', i.party_id,
                    'account_id', i.account_id,
                    'amount', i.amount::text, 'settled', i.settled::text,
                    'open', (i.amount - i.settled)::text,
                    'currency', i.currency, 'txn_amount', i.txn_amount::text,
                    'doc_date', i.doc_date, 'due_date', i.due_date) AS row
                FROM open_item i WHERE i.posting_id = $1
                UNION ALL
                SELECT 1, s.id, jsonb_build_object(
                    'entry', 'settlement', 'id', s.id, 'open_item_id', s.open_item_id,
                    'kind', i.kind, 'party_type', i.party_type, 'party_id', i.party_id,
                    'amount', s.amount::text, 'cancels', s.cancels,
                    'currency', i.currency, 'txn_amount', s.txn_amount::text,
                    'document_id', d.id, 'doc_type', d.doc_type, 'number', d.number)
                FROM settlement s
                JOIN open_item i ON i.id = s.open_item_id
                JOIN posting p ON p.id = i.posting_id
                JOIN document d ON d.id = p.document_id
                WHERE s.posting_id = $1
                UNION ALL
                SELECT 2, v.id, jsonb_build_object(
                    'entry', 'revaluation', 'id', v.id, 'open_item_id', v.open_item_id,
                    'kind', i.kind, 'party_type', i.party_type, 'party_id', i.party_id,
                    'account_id', i.account_id, 'amount', v.amount::text,
                    'currency', i.currency, 'txn_open', v.txn_open::text,
                    'carried', v.carried::text, 'rate', v.rate::text,
                    'document_id', d.id, 'doc_type', d.doc_type, 'number', d.number)
                FROM open_item_revaluation v
                JOIN open_item i ON i.id = v.open_item_id
                JOIN posting p ON p.id = i.posting_id
                JOIN document d ON d.id = p.document_id
                WHERE v.posting_id = $1
             ) AS entries ORDER BY part, id",
        )
        .bind(posting.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        // A settlement names the document whose item it settles, which
        // is another document than the one being viewed.
        let mut rows: Vec<serde_json::Value> = rows;
        let documents: Vec<Uuid> = rows
            .iter()
            .filter_map(|row| row["document_id"].as_str()?.parse().ok())
            .collect();
        let readable = document::readable(ctx, &documents).await?;
        for row in &mut rows {
            let Some(id) = row["document_id"].as_str().and_then(|id| id.parse().ok()) else {
                continue;
            };
            if !readable.contains(&id) {
                for field in ["document_id", "doc_type", "number"] {
                    row[field] = serde_json::Value::Null;
                }
            }
        }
        Ok(Some(rows))
    }
}

// --------------------------------------------------------------------- list

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    /// The company to report on.
    company_id: Uuid,
    /// Only payables, or only receivables.
    kind: Option<Kind>,
    /// Only one party: give both the type (`vendor`, `customer`) and the id.
    party_type: Option<String>,
    /// Id of the vendor or customer; give `party_type` with it.
    party_id: Option<Uuid>,
    /// Also list items that are fully settled or were called off.
    #[serde(default)]
    include_settled: bool,
    /// Only these items, settled or not: to show the items a payment
    /// settled. At most 500.
    ids: Option<Vec<Uuid>>,
    /// Only items at least this many days past due as of `as_of`, counted
    /// as `open_item.aging` counts them: from the due date, or the document
    /// date when there is none. 1 leaves out what is not yet due.
    overdue_min: Option<i32>,
    /// Only items at most this many days past due as of `as_of`. 0 keeps
    /// only what is not yet past due (the aging's `current`).
    overdue_max: Option<i32>,
    /// The items as they were at the end of this day: those posted by then,
    /// with what was open of each then (settlements and parts as postings
    /// dated on or before it left them, so an item paid later is listed as
    /// open), and what it was carried at with the revaluations standing that
    /// day. `overdue_min` and `overdue_max` count from it too. Omitted: what
    /// is open now, overdue counted from today.
    as_of: Option<NaiveDate>,
    /// At most 500; 50 when omitted.
    limit: Option<i64>,
    /// Records to skip, for paging.
    offset: Option<i64>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct ListedItem {
    #[serde(flatten)]
    #[sqlx(flatten)]
    item: OpenItem,
    /// The document that created the debt: its id, type and number. All
    /// three are absent for a caller who may not read that document.
    document_id: Option<Uuid>,
    doc_type: Option<String>,
    number: Option<String>,
    /// What the item is carried at in the accounting currency: what is open
    /// of it at the rates booked, plus the revaluations standing (on
    /// `as_of`, when given). What the ledger holds for it.
    carried: Decimal,
}

#[derive(Serialize, JsonSchema)]
struct ListOutput {
    items: Vec<ListedItem>,
    /// How many items match.
    total: i64,
    /// What all matching items still owe, together, at the rates booked.
    total_open: Decimal,
    /// What they are carried at with revaluations standing: the balance of
    /// their accounts in the ledger (on `as_of`, when given).
    total_carried: Decimal,
}

struct List;

#[async_trait]
impl Command for List {
    type Input = ListInput;
    type Output = ListOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "open_item.list",
            "What a company owes its vendors (payables) and is owed by its customers \
             (receivables), invoice by invoice, oldest first, with the amount still open on each \
             and the document it came from (left out when the caller may not read that \
             document). Use the item ids to allocate a payment.",
        )
        .report("Open items")
        .permission("open_item:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ListInput) -> Result<ListOutput> {
        ctx.require("open_item:read", input.company_id)?;
        // `{PIT}` is the parameter that says whether the list is as of a
        // day: then what was settled and revalued counts only from postings
        // dated on or before it ($9), and items posted after it are left out.
        const FROM: &str = "FROM open_item i
            JOIN posting ip ON ip.id = i.posting_id
            CROSS JOIN LATERAL (SELECT
                CASE WHEN {PIT} THEN COALESCE((
                    SELECT sum(s.amount) FROM settlement s JOIN posting sp ON sp.id = s.posting_id
                    WHERE s.open_item_id = i.id AND sp.posting_date <= $9), 0)
                ELSE i.settled END AS settled,
                CASE WHEN {PIT} THEN COALESCE((
                    SELECT sum(s.txn_amount) FROM settlement s JOIN posting sp ON sp.id = s.posting_id
                    WHERE s.open_item_id = i.id AND sp.posting_date <= $9), 0)
                ELSE i.txn_settled END AS txn_settled,
                COALESCE((
                    SELECT sum(v.amount) FROM open_item_revaluation v JOIN posting vp ON vp.id = v.posting_id
                    WHERE v.open_item_id = i.id AND (NOT {PIT} OR vp.posting_date <= $9)), 0) AS revalued
            ) pit";
        const WHERE: &str = "i.company_id = $1
            AND ($2::text IS NULL OR i.kind = $2)
            AND ($3::text IS NULL OR i.party_type = $3)
            AND ($4::uuid IS NULL OR i.party_id = $4)
            AND ($5 OR pit.settled <> i.amount)
            AND ($6::uuid[] IS NULL OR i.id = ANY($6))
            AND ($7::int IS NULL OR $9::date - COALESCE(i.due_date, i.doc_date) >= $7)
            AND ($8::int IS NULL OR $9::date - COALESCE(i.due_date, i.doc_date) <= $8)
            AND (NOT {PIT} OR ip.posting_date <= $9)";
        let kind = input.kind.map(Kind::as_str);
        if input.ids.as_ref().is_some_and(|ids| ids.len() > 500) {
            return Err(Error::field("ids", "can name at most 500 items"));
        }
        if let (Some(min), Some(max)) = (input.overdue_min, input.overdue_max)
            && min > max
        {
            return Err(Error::field("overdue_max", "must not be below overdue_min"));
        }
        let point_in_time = input.as_of.is_some();
        let as_of = input
            .as_of
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        // Asked for by id, an item is listed whatever it still owes.
        let include_settled = input.include_settled || input.ids.is_some();

        let (from, filter) = (FROM.replace("{PIT}", "$10"), WHERE.replace("{PIT}", "$10"));
        let (total, total_open, total_carried): (i64, Decimal, Decimal) = sqlx::query_as(&format!(
            "SELECT count(*), COALESCE(sum(i.amount - pit.settled), 0),
                    COALESCE(sum(i.amount - pit.settled + pit.revalued), 0)
             {from} WHERE {filter}"
        ))
        .bind(input.company_id)
        .bind(kind)
        .bind(&input.party_type)
        .bind(input.party_id)
        .bind(include_settled)
        .bind(&input.ids)
        .bind(input.overdue_min)
        .bind(input.overdue_max)
        .bind(as_of)
        .bind(point_in_time)
        .fetch_one(&mut *ctx.conn)
        .await?;
        // The item's columns as of the day: what was settled then.
        let columns = ITEM_COLUMNS
            .replace(
                "i.amount - i.settled AS open",
                "i.amount - pit.settled AS open",
            )
            .replace("i.settled,", "pit.settled AS settled,")
            .replace(
                "i.txn_amount - i.txn_settled AS txn_open",
                "i.txn_amount - pit.txn_settled AS txn_open",
            )
            .replace("i.txn_settled,", "pit.txn_settled AS txn_settled,")
            .replace(
                "open_item_instalments(i.id, NULL)",
                "open_item_instalments(i.id, CASE WHEN $12 THEN $9::date END)",
            );
        let (from, filter) = (FROM.replace("{PIT}", "$12"), WHERE.replace("{PIT}", "$12"));
        let mut items: Vec<ListedItem> = sqlx::query_as(&format!(
            "SELECT {columns}, d.id AS document_id, d.doc_type, d.number,
                    i.amount - pit.settled + pit.revalued AS carried
             {from}
             JOIN document d ON d.id = ip.document_id
             WHERE {filter}
             ORDER BY i.doc_date, i.id LIMIT $10 OFFSET $11"
        ))
        .bind(input.company_id)
        .bind(kind)
        .bind(&input.party_type)
        .bind(input.party_id)
        .bind(include_settled)
        .bind(&input.ids)
        .bind(input.overdue_min)
        .bind(input.overdue_max)
        .bind(as_of)
        .bind(input.limit.unwrap_or(50).clamp(1, 500))
        .bind(input.offset.unwrap_or(0).max(0))
        .bind(point_in_time)
        .fetch_all(&mut *ctx.conn)
        .await?;
        // The right to see what is owed is not the right to read the
        // invoice it is owed for.
        let documents: Vec<Uuid> = items.iter().filter_map(|i| i.document_id).collect();
        let readable = document::readable(ctx, &documents).await?;
        for item in &mut items {
            if !item.document_id.is_some_and(|id| readable.contains(&id)) {
                item.document_id = None;
                item.doc_type = None;
                item.number = None;
            }
        }
        Ok(ListOutput {
            items,
            total,
            total_open,
            total_carried,
        })
    }
}
