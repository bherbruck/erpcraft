//! Month-end revaluation of open items in foreign currencies.
//!
//! A revaluation, as of its date, values every item still open in another
//! currency than the company's at the rate on record for that day, and
//! posts the difference from what the item is carried at, item by item, to
//! its payables or receivables account against unrealized exchange gain or
//! loss. It is reversed the day after: when that day's period is open, at
//! once, as a posting of its own; otherwise with `fx_revaluation.reverse`
//! once it is.
//!
//! This is the common approach, revalue and reverse: once reversed, the
//! item is carried at its invoice's rate again, and a payment realizes its
//! whole difference against that. Unrealized and realized differences
//! never count the same movement twice: what was unrealized at the month
//! end is taken back the next day, and the payment realizes all of it. A
//! payment that would settle an item while a revaluation of it from before
//! the payment still stands is refused until the revaluation is reversed.
//!
//! The difference goes to the account for `fx_unrealized_gain` or
//! `fx_unrealized_loss`; a company that keeps no separate accounts for
//! unrealized differences has them go to `fx_gain` and `fx_loss` (and those
//! to `fx_difference`), as realized differences do.

use chrono::{Days, NaiveDate};
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, Error, FieldMeta, FieldType,
    JsonSchema, PgConnection, Result, StateMeta, async_trait, schemars,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) const FX_REVALUATION: &str = "fx_revaluation";

pub(crate) struct FxRevaluation;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// What a posted revaluation shows; ignored when saving a draft.
    #[serde(default)]
    #[allow(dead_code)]
    lines: Vec<Value>,
}

#[async_trait]
impl DocumentHandler for FxRevaluation {
    fn meta(&self) -> DocTypeMeta {
        let company_money = |name: &str, label: &str| {
            FieldMeta::new(name, label, FieldType::Decimal)
                .money()
                .readonly()
        };
        DocTypeMeta {
            name: FX_REVALUATION.into(),
            module: String::new(),
            label: "Exchange revaluation".into(),
            number_prefix: "FXR".into(),
            fields: vec![],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Items revalued".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new(
                        "open_item_id",
                        "Open item",
                        FieldType::Lookup(
                            erpcraft_openitems::lookup().field("company_id", "company_id"),
                        ),
                    )
                    .readonly(),
                    FieldMeta::new(
                        "kind",
                        "Kind",
                        FieldType::options(&["payable", "receivable"]),
                    )
                    .readonly(),
                    FieldMeta::new(
                        "currency",
                        "Currency",
                        FieldType::link_by("currency", "code"),
                    )
                    .readonly(),
                    FieldMeta::new("txn_open", "Open", FieldType::Decimal)
                        .money()
                        .currency_in("currency")
                        .readonly()
                        .help("What was open of the item on the day, in its currency."),
                    FieldMeta::new("rate", "Rate", FieldType::Decimal)
                        .rate(10)
                        .readonly()
                        .help("The rate on record for the day."),
                    company_money("carried", "Carried at")
                        .help("What the item was carried at before, in the accounting currency."),
                    company_money("amount", "Difference").help(
                        "What the item's value changed by, in its sign: more than zero when \
                         what is owed (either way) is worth more.",
                    ),
                ],
            }],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("posted", "Posted"),
                StateMeta::locked("reversed", "Reversed"),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "posted".into()),
                ("posted".into(), "reversed".into()),
            ],
            summary: vec![
                company_money("total_gain", "Unrealized gain"),
                company_money("total_loss", "Unrealized loss"),
                FieldMeta::new("items", "Items", FieldType::Int).readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    /// A draft has nothing to say: what is revalued is worked out when it
    /// is posted, as of its date.
    async fn save_body(&self, _ctx: &mut Ctx<'_>, _doc: &Document, body: &Value) -> Result<()> {
        let _: Body = erpcraft_kernel::input::parse(body.clone(), "revaluation body")?;
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let lines: Vec<Value> = sqlx::query_scalar(
            "SELECT jsonb_build_object(
                'id', v.id, 'line_no', row_number() OVER (ORDER BY v.id),
                'open_item_id', v.open_item_id, 'kind', i.kind, 'currency', i.currency,
                'txn_open', v.txn_open::text, 'rate', v.rate::text,
                'carried', v.carried::text, 'amount', v.amount::text)
             FROM open_item_revaluation v
             JOIN open_item i ON i.id = v.open_item_id
             JOIN posting p ON p.id = v.posting_id
             WHERE p.document_id = $1 AND p.reverses_id IS NULL
             ORDER BY v.id",
        )
        .bind(doc.id)
        .fetch_all(&mut *conn)
        .await?;
        Ok(json!({ "lines": lines }))
    }

    async fn summary_of(
        &self,
        _conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        use document::body::amount;
        let lines = document::body::lines(body, "lines");
        let (mut gain, mut loss) = (Decimal::ZERO, Decimal::ZERO);
        for line in lines {
            let change = gain_of(
                line["kind"].as_str() == Some("payable"),
                amount(&line["amount"]),
            );
            if change > Decimal::ZERO {
                gain += change;
            } else {
                loss -= change;
            }
        }
        Ok(json!({ "total_gain": gain, "total_loss": loss, "items": lines.len() }))
    }
}

/// What a change in an item's value is to the company: a gain when a
/// receivable is worth more or a payable less.
fn gain_of(payable: bool, change: Decimal) -> Decimal {
    if payable { -change } else { change }
}

/// The account for an unrealized exchange difference of `purpose`
/// (`fx_unrealized_gain` or `fx_unrealized_loss`), falling back to the
/// realized ones and then to `fx_difference`.
async fn difference_account(conn: &mut PgConnection, company_id: Uuid, gain: bool) -> Result<Uuid> {
    let (unrealized, realized) = if gain {
        ("fx_unrealized_gain", "fx_gain")
    } else {
        ("fx_unrealized_loss", "fx_loss")
    };
    for purpose in [unrealized, realized, "fx_difference"] {
        if let Ok(account) = erpcraft_gl::account_for(conn, company_id, purpose, &[]).await {
            return Ok(account);
        }
    }
    Err(Error::validation(format!(
        "this company has no account for unrealized exchange differences: add an account rule \
         for the purpose '{unrealized}', '{realized}' or 'fx_difference'"
    )))
}

fn line(account_id: Uuid, debit: Decimal, credit: Decimal, memo: &str) -> GlLine {
    GlLine {
        account_id,
        debit,
        credit,
        currency: None,
        fx_rate: None,
        memo: Some(memo.into()),
        source_line_id: None,
    }
}

/// Whether postings dated `day` can go into an open period of the company.
async fn period_open(conn: &mut PgConnection, company_id: Uuid, day: NaiveDate) -> Result<bool> {
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM fiscal_period
                        WHERE company_id = $1 AND $2 BETWEEN start_date AND end_date
                          AND status = 'open')",
    )
    .bind(company_id)
    .bind(day)
    .fetch_one(&mut *conn)
    .await?;
    Ok(open)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostInput {
    /// Revaluation id.
    id: Uuid,
}

pub(crate) struct PostRevaluation;

#[async_trait]
impl Command for PostRevaluation {
    type Input = PostInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "fx_revaluation.post",
            "Revalue, as of the revaluation's date, every payable and receivable still open in a \
             currency other than the accounting currency at the rate on record for that day. \
             The difference from what each item is carried at is posted to its payables or \
             receivables account against unrealized exchange gain or loss (the accounts for \
             fx_unrealized_gain and fx_unrealized_loss, else fx_gain and fx_loss). The \
             revaluation is reversed the next day at once when that day's period is open; \
             otherwise reverse it with fx_revaluation.reverse once it is.",
        )
        .from_states(&["draft"])
        .permission("fx_revaluation:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, FX_REVALUATION, input.id, "post").await?;
        if doc.status != "draft" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        let day = doc.doc_date;
        let decimals =
            erpcraft_kernel::company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        let items = erpcraft_openitems::revaluable(&mut *ctx.conn, doc.company_id, day).await?;
        let mut rates: Vec<(String, Decimal)> = vec![];
        let mut revaluations = vec![];
        // Per account, the change of the items it carries, and the gains
        // and losses that go with them.
        let mut accounts: Vec<(Uuid, bool, Decimal)> = vec![];
        let (mut gain, mut loss) = (Decimal::ZERO, Decimal::ZERO);
        for item in items {
            let rate = match rates.iter().find(|(c, _)| *c == item.currency) {
                Some((_, rate)) => *rate,
                None => {
                    let rate =
                        erpcraft_gl::rate_on(&mut *ctx.conn, doc.company_id, &item.currency, day)
                            .await?;
                    rates.push((item.currency.clone(), rate));
                    rate
                }
            };
            let value = erpcraft_kernel::round_money(item.txn_open * rate, decimals);
            let change = value - item.carried;
            if change.is_zero() {
                continue;
            }
            let payable = item.kind == "payable";
            match accounts.iter_mut().find(|(a, ..)| *a == item.account_id) {
                Some((_, _, total)) => *total += change,
                None => accounts.push((item.account_id, payable, change)),
            }
            let to_company = gain_of(payable, change);
            if to_company > Decimal::ZERO {
                gain += to_company;
            } else {
                loss -= to_company;
            }
            revaluations.push(erpcraft_openitems::Revaluation {
                open_item_id: item.id,
                amount: change,
                rate,
                txn_open: item.txn_open,
                carried: item.carried,
            });
        }
        if revaluations.is_empty() {
            return Err(Error::validation(format!(
                "nothing to revalue: no item open on {day} in another currency is carried at \
                 other than its value at the rate of that day"
            )));
        }

        let memo = "Unrealized exchange difference";
        let mut gl = vec![];
        for (account, payable, change) in accounts {
            if change.is_zero() {
                continue;
            }
            // A payable is a credit balance, a receivable a debit balance:
            // more owed either way moves the balance its own way.
            let more = change > Decimal::ZERO;
            let debit = more != payable;
            let amount = change.abs();
            gl.push(if debit {
                line(account, amount, Decimal::ZERO, memo)
            } else {
                line(account, Decimal::ZERO, amount, memo)
            });
        }
        if !gain.is_zero() {
            let account = difference_account(&mut *ctx.conn, doc.company_id, true).await?;
            gl.push(line(account, Decimal::ZERO, gain, memo));
        }
        if !loss.is_zero() {
            let account = difference_account(&mut *ctx.conn, doc.company_id, false).await?;
            gl.push(line(account, loss, Decimal::ZERO, memo));
        }
        let posting = document::create_posting(ctx, &doc, day, doc.memo.clone()).await?;
        erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        erpcraft_openitems::revalue(&mut *ctx.conn, &posting, &revaluations).await?;
        document::transition(ctx, &mut doc, "posted").await?;

        // Reversed the next day, when that can be posted now.
        if let Some(next) = day.checked_add_days(Days::new(1))
            && period_open(&mut *ctx.conn, doc.company_id, next).await?
        {
            reverse(ctx, &mut doc, next, None).await?;
        }
        document::view(ctx, doc).await
    }
}

async fn reverse(
    ctx: &mut Ctx<'_>,
    doc: &mut Document,
    day: NaiveDate,
    memo: Option<String>,
) -> Result<()> {
    let original = document::original_posting(&mut *ctx.conn, doc).await?;
    let memo = memo.or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
    document::reverse_posting(ctx, original.id, Some(day), memo).await?;
    document::transition(ctx, doc, "reversed").await
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReverseInput {
    /// Revaluation id.
    id: Uuid,
    /// Accounting date of the reversal: the day after the revaluation's
    /// when omitted.
    posting_date: Option<NaiveDate>,
    /// Memo of the reversing posting. Defaults to "Reversal of" and the
    /// document number.
    memo: Option<String>,
}

pub(crate) struct ReverseRevaluation;

#[async_trait]
impl Command for ReverseRevaluation {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "fx_revaluation.reverse",
            "Reverse a posted revaluation, by default the day after its date: the items it \
             revalued are carried at their own rates again. Posting a revaluation reverses it \
             by itself when the next day's period is open; this is for when it was not.",
        )
        .from_states(&["posted"])
        .permission("fx_revaluation:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, FX_REVALUATION, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} revaluation cannot be reversed",
                doc.status
            )));
        }
        let day = match input.posting_date {
            Some(day) => day,
            None => doc
                .doc_date
                .checked_add_days(Days::new(1))
                .ok_or_else(|| Error::field("posting_date", "give a date"))?,
        };
        if day < doc.doc_date {
            return Err(Error::field(
                "posting_date",
                "a revaluation is reversed on or after its date",
            ));
        }
        reverse(ctx, &mut doc, day, input.memo).await?;
        document::view(ctx, doc).await
    }
}
