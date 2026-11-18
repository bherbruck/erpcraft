//! Payments module: paying vendors and being paid by customers.
//!
//! A payment names the open items it settles and how much of each. It
//! knows open items and the general ledger, and nothing about purchasing
//! or sales: whatever module opened an item, a payment can settle it.

use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, Decision, DocTypeMeta, DynCommand, Error,
    FieldMeta, FieldType, JsonSchema, Migration, Module, PgConnection, PolicyPoint, Result,
    StateMeta, async_trait, command, schemars,
};
use erpcraft_openitems::{Allocation, Kind, NewOpenItem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod revaluation;

const PAYMENT: &str = "payment";

/// The policy point asked when a discount is taken after its date.
pub const LATE_DISCOUNT: &str = "payment.late_discount";
/// Asked when a payment is posted: may it be, for this amount?
pub const POST_PAYMENT: &str = "payment.post";
/// The policy point asked whether a discount takes the tax on it back.
pub const DISCOUNT_TAX: &str = "payment.discount_tax";
/// The policy point asked when a small difference is written off.
pub const WRITE_OFF: &str = "payment.write_off";

pub struct PaymentsModule;

impl Module for PaymentsModule {
    fn name(&self) -> &'static str {
        "payments"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        // Tax: an early-payment discount takes back the tax on it.
        vec!["gl", "open_items", "tax"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "payments",
                sql: include_str!("../migrations/0001_payments.sql"),
            },
            Migration {
                version: 2,
                name: "credits_and_on_account",
                sql: include_str!("../migrations/0002_credits_and_on_account.sql"),
            },
            Migration {
                version: 3,
                name: "bank_accounts",
                sql: include_str!("../migrations/0003_bank_accounts.sql"),
            },
            Migration {
                version: 4,
                name: "currency",
                sql: include_str!("../migrations/0004_currency.sql"),
            },
            Migration {
                version: 5,
                name: "discount",
                sql: include_str!("../migrations/0005_discount.sql"),
            },
            Migration {
                version: 6,
                name: "write_off",
                sql: include_str!("../migrations/0006_write_off.sql"),
            },
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                POST_PAYMENT,
                "Asked when a payment is posted: may it be, as it stands? Unless configured \
                 otherwise any payment is posted. A company that wants two signatures on large \
                 payments adds a rule with effect approve when, for example, \
                 `facts.direction == 'outgoing' && facts.amount > 5000`, naming who approves.",
            )
            .number("amount", "What the payment comes to, in its currency")
            .as_money(Some("currency"))
            .text("currency", "The payment's currency")
            .text(
                "direction",
                "outgoing (the company pays) or incoming (a customer pays)",
            ),
            PolicyPoint::new(
                LATE_DISCOUNT,
                "Asked for every allocation of a payment that takes an early-payment discount, \
                 when the payment is posted: may the discount be taken on this date? Unless \
                 configured otherwise it is refused after the item's discount date. A company \
                 that grants a few days' grace allows it, for example with an allow rule when \
                 `facts.days_late <= 3`.",
            )
            .text(
                "kind",
                "payable (the company pays) or receivable (a customer pays)",
            )
            .text(
                "discount_date",
                "The last day of the discount, as the item's terms set it",
            )
            .text("payment_date", "The date of the payment")
            .number(
                "days_late",
                "Days the payment is after the discount date; zero or less when in time",
            )
            .refuse_when("facts.days_late > 0")
            .refusal("the early-payment discount has ended"),
            PolicyPoint::new(
                WRITE_OFF,
                "Asked for every allocation of a payment that writes off what is left of an item \
                 (a customer short-paid by a few cents, a bank charge kept from a transfer), \
                 when the payment is posted: may this much be written off? Unless configured \
                 otherwise nothing is: a company sets its tolerance with an allow rule, for \
                 example when `facts.amount <= 1`, or `facts.percent <= 0.5`. What is written \
                 off goes to the account for `write_off`, tax included as it was charged.",
            )
            .text(
                "kind",
                "payable (the company pays) or receivable (a customer pays)",
            )
            .text("currency", "The currency of the item")
            .number("amount", "What is written off, in the item's currency")
            .as_money(Some("currency"))
            .number("percent", "What is written off as a percentage of the item")
            .as_percent()
            .refuse_when("facts.amount > 0")
            .refusal("this difference is more than the company writes off"),
            PolicyPoint::new(
                DISCOUNT_TAX,
                "Asked for every allocation of a payment that takes an early-payment discount, \
                 when the payment is posted: is the tax on the invoice corrected for the \
                 discount? Unless configured otherwise it is, as VAT and GST require: the \
                 discount reduces the taxable amount, and the tax on it is taken back in \
                 proportion, recorded in the tax ledger in the payment's period. A company \
                 whose tax does not change with a cash discount (US sales tax, for one) \
                 refuses it with a refuse rule; then the whole discount goes to the discount \
                 account.",
            )
            .text(
                "kind",
                "payable (the company pays) or receivable (a customer pays)",
            )
            .text("currency", "The currency of the item"),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(Payment), Arc::new(revaluation::FxRevaluation)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(DraftPayment),
            command(ApplyCredit),
            command(PostPayment),
            command(ReversePayment),
            command(revaluation::PostRevaluation),
            command(revaluation::ReverseRevaluation),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    /// The company pays a vendor.
    Outgoing,
    /// A customer pays the company.
    Incoming,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::Outgoing => "outgoing",
            Direction::Incoming => "incoming",
        }
    }

    fn parse(text: &str) -> Direction {
        if text == "incoming" {
            Direction::Incoming
        } else {
            Direction::Outgoing
        }
    }

    /// The kind of open item this direction settles.
    fn settles(self) -> Kind {
        match self {
            Direction::Outgoing => Kind::Payable,
            Direction::Incoming => Kind::Receivable,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    direction: Direction,
    /// Needed unless the payment comes to nothing.
    bank_account_id: Option<Uuid>,
    #[serde(default)]
    allocations: Vec<AllocationInput>,
    /// Money beyond the allocations, kept as a credit of the party.
    #[serde(default)]
    on_account: Decimal,
    /// Whose credit, when no allocation says so.
    party_type: Option<String>,
    party_id: Option<Uuid>,
    /// The payables or receivables account that carries the credit, when
    /// no allocation says so.
    account_id: Option<Uuid>,
    /// The currency of the payment: that of the items it settles, and of
    /// its bank account when that has one. The accounting currency when
    /// nothing says otherwise.
    currency: Option<String>,
    /// The exchange rate to state, for a payment in a foreign currency.
    /// The rate on record for the payment's date when omitted.
    fx_rate: Option<Decimal>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllocationInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    open_item_id: Uuid,
    amount: Decimal,
    /// Early-payment discount kept back, in the item's currency.
    #[serde(default)]
    discount: Decimal,
    /// What is left of the item after the amount and the discount,
    /// written off, in the item's currency.
    #[serde(default)]
    write_off: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct AllocationLine {
    id: Uuid,
    line_no: i32,
    open_item_id: Uuid,
    amount: Decimal,
    discount: Decimal,
    write_off: Decimal,
}

async fn load_allocations(
    conn: &mut PgConnection,
    document_id: Uuid,
) -> Result<Vec<AllocationLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, open_item_id, amount, discount, write_off FROM payment_allocation
         WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

/// The header of a payment, as stored.
#[derive(Debug, sqlx::FromRow)]
struct Header {
    direction: String,
    bank_account_id: Option<Uuid>,
    on_account: Decimal,
    party_type: Option<String>,
    party_id: Option<Uuid>,
    account_id: Option<Uuid>,
    /// The payment's currency; never null once read.
    currency: String,
    fx_rate: Option<Decimal>,
}

impl Header {
    fn direction(&self) -> Direction {
        Direction::parse(&self.direction)
    }
}

async fn load_header(conn: &mut PgConnection, document_id: Uuid) -> Result<Header> {
    let header = sqlx::query_as(
        "SELECT p.direction, p.bank_account_id, p.on_account, p.party_type, p.party_id,
                p.account_id, COALESCE(p.currency, c.base_currency) AS currency, p.fx_rate
         FROM payment p JOIN document d ON d.id = p.document_id
         JOIN company c ON c.id = d.company_id
         WHERE p.document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(header)
}

/// A debit or a credit of `amount` to an account: for an outgoing payment
/// what settles a debt is a debit, for an incoming one a credit. An amount
/// below zero goes the other way.
fn settling_line(
    account_id: Uuid,
    amount: Decimal,
    direction: Direction,
    source: Option<Uuid>,
) -> GlLine {
    let debit_side = (direction == Direction::Outgoing) == amount.is_sign_positive();
    let amount = amount.abs();
    GlLine {
        account_id,
        debit: if debit_side { amount } else { Decimal::ZERO },
        credit: if debit_side { Decimal::ZERO } else { amount },
        currency: None,
        fx_rate: None,
        memo: None,
        source_line_id: source,
    }
}

/// With the account of the items themselves as its bank account, a
/// payment would debit and credit one account: the items would be settled
/// while the account still carried them, and open items would no longer
/// add up to it.
fn not_a_bank_account(direction: Direction) -> Error {
    Error::validation(match direction {
        Direction::Outgoing => {
            "the bank or cash account cannot be the payables account of the items being paid"
        }
        Direction::Incoming => {
            "the bank or cash account cannot be the receivables account of the items being paid"
        }
    })
    .with_field("bank_account_id")
}

struct Payment;

#[async_trait]
impl DocumentHandler for Payment {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: PAYMENT.into(),
            module: String::new(),
            label: "Payment".into(),
            number_prefix: "PAY".into(),
            fields: vec![
                FieldMeta::new(
                    "direction",
                    "Direction",
                    FieldType::options(&["outgoing", "incoming"]),
                )
                .required(),
                FieldMeta::new(
                    "bank_account_id",
                    "Bank or cash account",
                    FieldType::link("account").filter("is_bank", "true"),
                )
                .help("Needed unless the payment comes to nothing, as when a credit is applied to an invoice."),
                FieldMeta::new("amount", "Amount", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly(),
                FieldMeta::new("currency", "Currency", FieldType::link_by("currency", "code")).help(
                    "The currency of the items paid and of the bank account. The company's \
                     accounting currency when nothing says otherwise.",
                ),
                FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal)
                    .rate(10)
                    .help(
                        "For a payment in another currency: accounting currency per unit. The \
                         rate on record for the payment's date when left out; once posted, the \
                         rate it was posted at.",
                    ),
                FieldMeta::new("on_account", "On account", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .help("Money paid beyond what is allocated. It stays with the vendor or customer as a credit, an open item to apply later."),
                FieldMeta::new("party_type", "Party type", FieldType::options(&["vendor", "customer"]))
                    .help("Whose credit money on account is, when nothing is allocated."),
                FieldMeta::new("party_id", "Party", FieldType::entity_named_by("party_type")),
                FieldMeta::new("account_id", "Payables or receivables account", FieldType::link("account"))
                    .help("The account that carries money on account, when nothing is allocated."),
            ],
            collections: vec![CollectionMeta {
                name: "allocations".into(),
                label: "Settles".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    // What the payment's direction settles, in its company.
                    FieldMeta::new(
                        "open_item_id",
                        "Open item",
                        FieldType::Lookup(
                            erpcraft_openitems::lookup()
                                .field("company_id", "company_id")
                                .mapped(
                                    "kind",
                                    "direction",
                                    &[("outgoing", "payable"), ("incoming", "receivable")],
                                ),
                        ),
                    )
                    .required(),
                    FieldMeta::new("amount", "Amount", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .required(),
                    FieldMeta::new("discount", "Discount", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .help(
                            "Early-payment discount kept back. With it the amount pays the rest \
                             of the item, which is then settled in full.",
                        ),
                    FieldMeta::new("write_off", "Written off", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .help(
                            "A small difference left of the item, settled without being paid, \
                             as far as the company's policy payment.write_off allows.",
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
                FieldMeta::new(
                    "direction",
                    "Direction",
                    FieldType::options(&["outgoing", "incoming"]),
                )
                .readonly(),
                FieldMeta::new("party_type", "Party type", FieldType::Text).readonly(),
                // The vendor or customer whose items are settled.
                FieldMeta::new(
                    "party_id",
                    "Party",
                    FieldType::entity_named_by("party_type"),
                )
                .readonly(),
                FieldMeta::new("currency", "Currency", FieldType::link_by("currency", "code"))
                    .readonly(),
                FieldMeta::new("total", "Total", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly(),
            ],
            // 1: currency, accounting totals and their declarations.
            summary_version: 1,
            ..DocTypeMeta::default()
        }
    }

    /// A draft is checked for what cannot change: the items exist, belong
    /// to one party, run the right way, and each allocation runs the way of
    /// its item (below zero for a credit). How much of each is still open
    /// is checked when the payment is posted.
    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "payment body")?;
        for (index, allocation) in body.allocations.iter().enumerate() {
            if allocation.amount.is_zero() {
                return Err(Error::line(
                    "allocations",
                    index + 1,
                    Some("amount"),
                    "the amount must be more than zero, or less than zero for a credit",
                ));
            }
            if allocation.write_off.is_sign_negative() {
                return Err(Error::line(
                    "allocations",
                    index + 1,
                    Some("write_off"),
                    "a write-off cannot be less than zero",
                ));
            }
            if allocation.discount.is_sign_negative() {
                return Err(Error::line(
                    "allocations",
                    index + 1,
                    Some("discount"),
                    "a discount cannot be less than zero",
                ));
            }
        }
        if body.on_account.is_sign_negative() {
            return Err(
                Error::validation("money on account cannot be less than zero")
                    .with_field("on_account"),
            );
        }
        if let Some(bank) = body.bank_account_id {
            check_account(
                ctx,
                doc,
                bank,
                "bank_account_id",
                "the bank or cash account",
            )
            .await?;
        }
        if let Some(account) = body.account_id {
            check_account(
                ctx,
                doc,
                account,
                "account_id",
                "the account of money on account",
            )
            .await?;
            check_control(ctx, doc.company_id, account, body.direction).await?;
        }
        if body.party_type.is_some() != body.party_id.is_some() {
            return Err(
                Error::validation("give both party_type and party_id, or neither")
                    .with_field("party_id"),
            );
        }
        if let Some(party_type) = &body.party_type {
            check_party(party_type, body.direction)?;
        }

        let mut ids: Vec<Uuid> = body.allocations.iter().map(|a| a.open_item_id).collect();
        ids.sort();
        ids.dedup();
        if ids.len() != body.allocations.len() {
            return Err(Error::validation(
                "an open item can appear only once in a payment",
            ));
        }
        let items: Vec<(Uuid, Decimal, String, Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, amount, party_type, party_id, account_id FROM open_item
             WHERE id = ANY($1) AND company_id = $2 AND kind = $3",
        )
        .bind(&ids)
        .bind(doc.company_id)
        .bind(body.direction.settles().as_str())
        .fetch_all(&mut *ctx.conn)
        .await?;
        if items.len() != ids.len() {
            return Err(Error::validation(match body.direction {
                Direction::Outgoing => {
                    "an outgoing payment settles payables of the document's company"
                }
                Direction::Incoming => {
                    "an incoming payment settles receivables of the document's company"
                }
            }));
        }
        let mut parties: Vec<(&str, Uuid)> = items
            .iter()
            .map(|(_, _, party_type, party_id, _)| (party_type.as_str(), *party_id))
            .collect();
        if let (Some(party_type), Some(party_id)) = (&body.party_type, body.party_id) {
            parties.push((party_type.as_str(), party_id));
        }
        parties.sort();
        parties.dedup();
        if parties.len() > 1 {
            return Err(Error::validation(
                "a payment settles the items of one vendor or one customer",
            ));
        }
        for (index, allocation) in body.allocations.iter().enumerate() {
            let Some((_, amount, ..)) = items.iter().find(|i| i.0 == allocation.open_item_id)
            else {
                continue;
            };
            if amount.is_sign_negative() != allocation.amount.is_sign_negative() {
                return Err(Error::line(
                    "allocations",
                    index + 1,
                    Some("amount"),
                    if amount.is_sign_negative() {
                        "this item is a credit: what is taken of it is below zero"
                    } else {
                        "the amount must be more than zero"
                    },
                ));
            }
            if amount.is_sign_negative() && !allocation.write_off.is_zero() {
                return Err(Error::line(
                    "allocations",
                    index + 1,
                    Some("write_off"),
                    "what is left of a debt is written off, not of a credit",
                ));
            }
            if amount.is_sign_negative() && !allocation.discount.is_zero() {
                return Err(Error::line(
                    "allocations",
                    index + 1,
                    Some("discount"),
                    "a discount is taken on a debt, not on a credit",
                ));
            }
        }
        if let Some(bank) = body.bank_account_id
            && (items.iter().any(|i| i.4 == bank) || body.account_id == Some(bank))
        {
            return Err(not_a_bank_account(body.direction));
        }
        if let Some(bank) = body.bank_account_id {
            check_bank(ctx, doc, bank).await?;
        }

        // One currency: the payment's, its items' and its bank account's.
        // Items of one party in different currencies are paid apart.
        let currencies: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT currency FROM open_item WHERE id = ANY($1) ORDER BY 1",
        )
        .bind(&ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
        if currencies.len() > 1 {
            return Err(Error::validation(format!(
                "a payment settles items of one currency, not {}: pay each currency apart",
                currencies.join(" and ")
            )));
        }
        let bank_currency: Option<String> = match body.bank_account_id {
            Some(bank) => {
                sqlx::query_scalar("SELECT currency FROM account WHERE id = $1")
                    .bind(bank)
                    .fetch_one(&mut *ctx.conn)
                    .await?
            }
            None => None,
        };
        let base = erpcraft_kernel::company::base_currency(&mut *ctx.conn, doc.company_id).await?;
        let currency = body
            .currency
            .clone()
            .or_else(|| currencies.first().cloned())
            .or_else(|| bank_currency.clone())
            .unwrap_or_else(|| base.clone());
        if let Some(items_currency) = currencies.first()
            && *items_currency != currency
        {
            return Err(Error::validation(format!(
                "the items are in {items_currency}, so the payment is too, not {currency}"
            ))
            .with_field("currency"));
        }
        if let Some(bank_currency) = &bank_currency
            && *bank_currency != currency
        {
            return Err(Error::validation(format!(
                "the bank account holds {bank_currency}; a payment through it is in {bank_currency}, not {currency}"
            ))
            .with_field("bank_account_id"));
        }
        erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency)
            .await
            .map_err(|e| e.with_field("currency"))?;
        if currency == base && body.fx_rate.is_some_and(|r| r != Decimal::ONE) {
            return Err(
                Error::validation(format!("the exchange rate for {base} must be 1"))
                    .with_field("fx_rate"),
            );
        }
        let stated_rate = body.fx_rate.filter(|_| currency != base);

        sqlx::query(
            "INSERT INTO payment (document_id, direction, bank_account_id, on_account, party_type,
                party_id, account_id, currency, fx_rate)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (document_id) DO UPDATE SET
                direction = EXCLUDED.direction, bank_account_id = EXCLUDED.bank_account_id,
                on_account = EXCLUDED.on_account, party_type = EXCLUDED.party_type,
                party_id = EXCLUDED.party_id, account_id = EXCLUDED.account_id,
                currency = EXCLUDED.currency, fx_rate = EXCLUDED.fx_rate",
        )
        .bind(doc.id)
        .bind(body.direction.as_str())
        .bind(body.bank_account_id)
        .bind(body.on_account)
        .bind(&body.party_type)
        .bind(body.party_id)
        .bind(body.account_id)
        .bind(&currency)
        .bind(stated_rate)
        .execute(&mut *ctx.conn)
        .await?;
        let ids = document::line_ids(
            &mut *ctx.conn,
            "payment_allocation",
            doc.id,
            body.allocations.iter().map(|a| a.id),
        )
        .await?;
        sqlx::query("DELETE FROM payment_allocation WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        if body.allocations.is_empty() {
            return Ok(());
        }
        // One statement for all allocations.
        let line_nos: Vec<i32> = (1..=body.allocations.len() as i32).collect();
        let items: Vec<Uuid> = body.allocations.iter().map(|a| a.open_item_id).collect();
        let amounts: Vec<Decimal> = body.allocations.iter().map(|a| a.amount).collect();
        let discounts: Vec<Decimal> = body.allocations.iter().map(|a| a.discount).collect();
        let write_offs: Vec<Decimal> = body.allocations.iter().map(|a| a.write_off).collect();
        sqlx::query(
            "INSERT INTO payment_allocation (id, document_id, line_no, open_item_id, amount,
                discount, write_off)
             SELECT r.id, $1, r.line_no, r.open_item_id, r.amount, r.discount, r.write_off
             FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[], $6::numeric[],
                         $7::numeric[])
                AS r(id, line_no, open_item_id, amount, discount, write_off)
             ORDER BY r.line_no",
        )
        .bind(doc.id)
        .bind(&ids)
        .bind(&line_nos)
        .bind(&items)
        .bind(&amounts)
        .bind(&discounts)
        .bind(&write_offs)
        .execute(&mut *ctx.conn)
        .await?;
        Ok(())
    }

    /// Which way the money goes, whose items it settles and how much it
    /// is: what is allocated, debts less credits, and what is paid on
    /// account, which is what posting moves through the bank.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        use document::body::{amount, lines, uuid};
        let allocations = lines(body, "allocations");
        let party: Option<(String, Uuid)> = match allocations.first() {
            Some(first) => {
                sqlx::query_as("SELECT party_type, party_id FROM open_item WHERE id = $1")
                    .bind(uuid(&first["open_item_id"]))
                    .fetch_optional(&mut *conn)
                    .await?
            }
            None => body["party_type"]
                .as_str()
                .map(str::to_owned)
                .zip(uuid(&body["party_id"])),
        };
        let total: Decimal = allocations
            .iter()
            .map(|a| amount(&a["amount"]))
            .sum::<Decimal>()
            + amount(&body["on_account"]);
        Ok(json!({
            "direction": body["direction"],
            "party_type": party.as_ref().map(|(party_type, _)| party_type),
            "party_id": party.as_ref().map(|(_, id)| id),
            "currency": body["currency"],
            "total": total,
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let header = load_header(conn, doc.id).await?;
        let allocations = load_allocations(conn, doc.id).await?;
        let amount: Decimal =
            allocations.iter().map(|a| a.amount).sum::<Decimal>() + header.on_account;
        Ok(json!({
            "direction": header.direction,
            "bank_account_id": header.bank_account_id,
            "amount": amount,
            "on_account": header.on_account,
            "party_type": header.party_type,
            "party_id": header.party_id,
            "account_id": header.account_id,
            "currency": header.currency,
            "fx_rate": header.fx_rate,
            "allocations": allocations,
        }))
    }
}

/// An account a payment names must be the company's own and take postings.
async fn check_account(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    account: Uuid,
    field: &str,
    what: &str,
) -> Result<()> {
    let is_group: Option<bool> =
        sqlx::query_scalar("SELECT is_group FROM account WHERE id = $1 AND company_id = $2")
            .bind(account)
            .bind(doc.company_id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    match is_group {
        None => Err(
            Error::validation(format!("{what} must belong to the document's company"))
                .with_field(field),
        ),
        Some(true) => Err(Error::validation(format!(
            "{what} is a group account and cannot be posted to"
        ))
        .with_field(field)),
        Some(false) => Ok(()),
    }
}

/// The account money moves through must be a bank or cash account of the
/// company: one marked `is_bank`, which no control account can be. Anything
/// else would let a payment debit one control account and credit another,
/// and the accounts would no longer add up to their open items.
async fn check_bank(ctx: &mut Ctx<'_>, doc: &Document, bank: Uuid) -> Result<()> {
    check_account(
        ctx,
        doc,
        bank,
        "bank_account_id",
        "the bank or cash account",
    )
    .await?;
    let is_bank: bool = sqlx::query_scalar("SELECT is_bank FROM account WHERE id = $1")
        .bind(bank)
        .fetch_one(&mut *ctx.conn)
        .await?;
    if !is_bank {
        return Err(Error::validation(
            "the bank or cash account must be one marked as a bank or cash account",
        )
        .with_field("bank_account_id"));
    }
    Ok(())
}

/// The account that carries money on account must be the company's
/// payables account for an outgoing payment, its receivables account for an
/// incoming one: where the open items of that kind are kept.
async fn check_control(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    account: Uuid,
    direction: Direction,
) -> Result<()> {
    // The settings belong to purchasing and sales, which may not be
    // installed.
    let (table, column, what) = match direction {
        Direction::Outgoing => (
            "purchasing_settings",
            "payable_account_id",
            "the payables account in the purchasing settings",
        ),
        Direction::Incoming => (
            "sales_settings",
            "receivable_account_id",
            "the receivables account in the sales settings",
        ),
    };
    let installed: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(&mut *ctx.conn)
        .await?;
    let matches = installed && {
        sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS (SELECT 1 FROM {table} WHERE company_id = $1 AND {column} = $2)"
        ))
        .bind(company_id)
        .bind(account)
        .fetch_one(&mut *ctx.conn)
        .await?
    };
    if !matches {
        return Err(
            Error::validation(format!("money on account is kept on {what}"))
                .with_field("account_id"),
        );
    }
    Ok(())
}

/// A vendor is paid, a customer pays.
fn check_party(party_type: &str, direction: Direction) -> Result<()> {
    let expected = match direction {
        Direction::Outgoing => "vendor",
        Direction::Incoming => "customer",
    };
    if party_type != expected {
        return Err(Error::validation(format!(
            "an {} payment is with a {expected}, not a {party_type}",
            direction.as_str()
        ))
        .with_field("party_type"));
    }
    Ok(())
}

/// An early-payment discount is the item's to give: no more than its terms
/// offer, with the money paying the rest of the item, and by its discount
/// date unless the company's policy allows it later.
async fn check_discount(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    line: &AllocationLine,
    item: &erpcraft_openitems::OpenItem,
) -> Result<()> {
    let refuse =
        |why: String| Error::line("allocations", line.line_no as usize, Some("discount"), why);
    // The step in force on the payment's date, or, after the last, the
    // last (a late discount, as the policy allows).
    let Some((until, offered)) = item
        .discount_on(doc.doc_date)
        .or_else(|| item.last_discount())
    else {
        return Err(refuse(
            "this item offers no early-payment discount".to_owned(),
        ));
    };
    if line.discount > offered {
        return Err(refuse(format!(
            "the discount on this item paid on {} is at most {}",
            doc.doc_date,
            offered.normalize()
        )));
    }
    if line.amount + line.discount + line.write_off != item.txn_open {
        return Err(refuse(format!(
            "a discount is taken when the rest of the item is paid: {} with a discount of {}",
            (item.txn_open - line.discount).normalize(),
            line.discount.normalize()
        )));
    }
    let facts = json!({
        "kind": item.kind,
        "discount_date": until.to_string(),
        "payment_date": doc.doc_date.to_string(),
        "days_late": (doc.doc_date - until).num_days().to_string(),
    });
    let policy = ctx.policy(LATE_DISCOUNT, doc.company_id).await?;
    if let Decision::Refused(refusal) = policy.evaluate(facts)? {
        return Err(refuse(refusal.message.unwrap_or_else(|| {
            format!("the early-payment discount on this item ended on {until}")
        })));
    }
    Ok(())
}

/// A write-off settles what is left of a debt, as far as the company's
/// policy accepts: the amount, the discount and the write-off together are
/// the rest of the item.
async fn check_write_off(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    line: &AllocationLine,
    item: &erpcraft_openitems::OpenItem,
) -> Result<()> {
    let refuse =
        |why: String| Error::line("allocations", line.line_no as usize, Some("write_off"), why);
    if line.amount + line.discount + line.write_off != item.txn_open {
        return Err(refuse(format!(
            "a write-off settles what is left of the item: {} is left after the amount and \
             the discount, not {}",
            (item.txn_open - line.amount - line.discount).normalize(),
            line.write_off.normalize()
        )));
    }
    let percent = if item.txn_amount.is_zero() {
        Decimal::ZERO
    } else {
        (line.write_off * Decimal::ONE_HUNDRED / item.txn_amount).round_dp(6)
    };
    let facts = json!({
        "kind": item.kind,
        "currency": item.currency,
        "amount": line.write_off.normalize().to_string(),
        "percent": percent.normalize().to_string(),
    });
    let policy = ctx.policy(WRITE_OFF, doc.company_id).await?;
    if let Decision::Refused(refusal) = policy.evaluate(facts)? {
        return Err(refuse(refusal.message.unwrap_or_else(|| {
            format!(
                "{} is more than the company writes off",
                line.write_off.normalize()
            )
        })));
    }
    Ok(())
}

/// The ledger lines of the discount an allocation takes, and the tax it
/// takes back: the discount at the payment's rate, its tax share back to
/// the tax accounts the invoice posted to (unless the company's policy says
/// tax is not corrected), and the rest to the account for
/// `discount_received` (payables) or `discount_given` (receivables). They
/// are on the side of the money paid: a discount is paid by not paying it.
async fn discount_lines(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    line: &AllocationLine,
    item: &erpcraft_openitems::OpenItem,
    direction: Direction,
    rate: Decimal,
    decimals: u32,
) -> Result<(Vec<GlLine>, Vec<erpcraft_tax::Tax>)> {
    let value = erpcraft_kernel::round_money(line.discount * rate, decimals);
    let facts = json!({ "kind": item.kind, "currency": item.currency });
    let policy = ctx.policy(DISCOUNT_TAX, doc.company_id).await?;
    let corrects = matches!(policy.evaluate(facts)?, Decision::Allowed);
    let taxes = if corrects {
        erpcraft_tax::discount(
            &mut *ctx.conn,
            doc.company_id,
            item.posting_id,
            line.discount,
            item.txn_amount,
            rate,
            Some(line.id),
        )
        .await?
    } else {
        vec![]
    };
    let purpose = match direction {
        Direction::Outgoing => "discount_received",
        Direction::Incoming => "discount_given",
    };
    let account = erpcraft_gl::account_for(&mut *ctx.conn, doc.company_id, purpose, &[])
        .await
        .map_err(|e| e.with_field("discount"))?;
    let mut gl = vec![];
    let mut net = value;
    for tax in &taxes {
        let tax_value = -tax.value();
        net -= tax_value;
        if !tax_value.is_zero() {
            gl.push(settling_line(
                tax.account_id,
                -tax_value,
                direction,
                Some(line.id),
            ));
        }
    }
    if !net.is_zero() {
        gl.push(settling_line(account, -net, direction, Some(line.id)));
    }
    Ok((gl, taxes))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DraftInput {
    /// The company the open items belong to.
    company_id: Uuid,
    /// The open items to settle, all payables of one vendor or all
    /// receivables of one customer. See `open_item.list`.
    open_item_ids: Vec<Uuid>,
    /// The bank or cash account the money leaves or arrives in.
    bank_account_id: Uuid,
    /// Date of the payment. Today when omitted.
    doc_date: Option<NaiveDate>,
}

struct DraftPayment;

#[async_trait]
impl Command for DraftPayment {
    type Input = DraftInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "payment.draft",
            "Draft a payment that settles open items of one party in full: outgoing for \
             payables, incoming for receivables, each item with what is still open on it. A \
             credit (a credit note, money on account) is taken at below zero, so the payment \
             comes to the debts less the credits. When the payment is dated (`doc_date`, \
             today by default) by an item's early-payment discount date, the discount its terms \
             offer is filled in and the item is paid that much less. Nothing is posted: change the amounts to what was paid, then post the payment with \
             payment.post.",
        )
        .label("Pay")
        .permission("payment:create")
        .creates(PAYMENT)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftInput) -> Result<DocumentView> {
        ctx.require("open_item:read", input.company_id)?;
        if input.open_item_ids.is_empty() {
            return Err(
                Error::validation("name at least one open item to pay").with_field("open_item_ids")
            );
        }
        let items =
            erpcraft_openitems::lock(&mut *ctx.conn, input.company_id, &input.open_item_ids)
                .await?;
        // Whether the items are of one kind and one party is the payment's
        // own rule, checked when the draft is saved.
        let direction = if items[0].kind == Kind::Receivable.as_str() {
            Direction::Incoming
        } else {
            Direction::Outgoing
        };
        let date = input
            .doc_date
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        // Paid by its discount date, an item is paid less its discount.
        let allocations: Vec<Value> = items
            .iter()
            .filter(|item| !item.txn_open.is_zero())
            .map(|item| match item.discount_on(date) {
                Some((_, discount)) if item.txn_open > discount => {
                    json!({ "open_item_id": item.id, "amount": item.txn_open - discount,
                            "discount": discount })
                }
                _ => json!({ "open_item_id": item.id, "amount": item.txn_open }),
            })
            .collect();
        if allocations.is_empty() {
            return Err(Error::validation(
                "nothing is open on these items: they are settled or were called off",
            ));
        }
        document::create(
            ctx,
            document::NewDocument {
                doc_type: PAYMENT.into(),
                company_id: input.company_id,
                doc_date: date,
                memo: None,
                body: json!({
                    "direction": direction.as_str(),
                    "bank_account_id": input.bank_account_id,
                    "allocations": allocations,
                }),
            },
        )
        .await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PostInput {
    /// Payment id.
    id: Uuid,
}

struct PostPayment;

#[async_trait]
impl Command for PostPayment {
    type Input = PostInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "payment.post",
            "Post a draft payment. Each open item it names is settled by the amount given, which \
             may be part of what is open but never more. An outgoing payment debits payables and \
             credits the bank account; an incoming one debits the bank account and credits \
             receivables. Posting a payment that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("payment:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PAYMENT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} payment cannot be posted",
                doc.status
            )));
        }
        let header = load_header(&mut *ctx.conn, doc.id).await?;
        let direction = header.direction();
        let lines = load_allocations(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() && header.on_account.is_zero() {
            return Err(Error::validation(
                "a payment needs at least one open item to settle, or money on account",
            ));
        }
        let ids: Vec<Uuid> = lines.iter().map(|l| l.open_item_id).collect();
        let paying: Decimal = lines.iter().map(|l| l.amount).sum::<Decimal>() + header.on_account;
        ctx.decide(
            POST_PAYMENT,
            doc.company_id,
            json!({
                "amount": paying.normalize().to_string(),
                "currency": header.currency,
                "direction": direction.as_str(),
            }),
        )
        .await?;
        let items = erpcraft_openitems::lock(&mut *ctx.conn, doc.company_id, &ids).await?;
        // A revaluation from before the payment that still stands would be
        // counted again by what the payment realizes.
        if let Some(number) =
            erpcraft_openitems::standing_revaluation(&mut *ctx.conn, &ids, doc.doc_date).await?
        {
            return Err(Error::state(format!(
                "an item this payment settles was revalued by {number}, which is not reversed \
                 on or before the payment's date: the revaluation would no longer be right. \
                 Reverse it (fx_revaluation.reverse, dated no later than the payment), then \
                 pay, and revalue again if need be"
            )));
        }

        // The payment's rate: what its money is worth in the accounting
        // currency. Its items are settled at their own rates; the
        // difference is a realized exchange difference.
        let rate = erpcraft_gl::document_rate(
            ctx,
            doc.company_id,
            &header.currency,
            doc.doc_date,
            header.fx_rate,
            PAYMENT,
        )
        .await?;
        let mut total = Decimal::ZERO;
        // How much is open on an item is the item's to tell, not the
        // payment's: someone who may post a payment but not read open
        // items learns only that it is too much.
        let reads_items = ctx.actor.can("open_item:read", doc.company_id);
        for (line, item) in lines.iter().zip(&items) {
            // Checked again here: the draft may be older than the rule, and
            // only now are the items locked.
            if Some(item.account_id) == header.bank_account_id {
                return Err(not_a_bank_account(direction));
            }
            if item.currency != header.currency {
                return Err(Error::line(
                    "allocations",
                    line.line_no as usize,
                    Some("open_item_id"),
                    format_args!(
                        "this item is in {}, the payment in {}",
                        item.currency, header.currency
                    ),
                ));
            }
            let wrong_way = line.amount.is_sign_negative() != item.amount.is_sign_negative();
            if !line.discount.is_zero() {
                check_discount(ctx, &doc, line, item).await?;
            }
            if !line.write_off.is_zero() {
                check_write_off(ctx, &doc, line, item).await?;
            }
            if wrong_way
                || (line.amount + line.discount + line.write_off).abs() > item.txn_open.abs()
            {
                let why = if wrong_way {
                    "this item is a credit: what is taken of it is below zero".to_owned()
                } else if reads_items {
                    format!(
                        "only {} is still open on this item, not {}",
                        item.txn_open.normalize(),
                        line.amount.normalize()
                    )
                } else {
                    format!(
                        "{} is more than is still open on this item",
                        line.amount.normalize()
                    )
                };
                return Err(Error::line(
                    "allocations",
                    line.line_no as usize,
                    Some("amount"),
                    why,
                ));
            }
            total += line.amount;
        }

        // Money beyond the allocations stays with the party as a credit.
        let on_account = if header.on_account.is_zero() {
            None
        } else {
            let party = items
                .first()
                .map(|i| (i.party_type.clone(), i.party_id))
                .or(header.party_type.clone().zip(header.party_id))
                .ok_or_else(|| {
                    Error::validation("say whose money on account this is: party_type and party_id")
                        .with_field("party_id")
                })?;
            let account = header
                .account_id
                .or(items.first().map(|i| i.account_id))
                .ok_or_else(|| {
                    Error::validation(
                        "say which payables or receivables account carries the money on account",
                    )
                    .with_field("account_id")
                })?;
            if Some(account) == header.bank_account_id {
                return Err(not_a_bank_account(direction));
            }
            // Checked again: the settings may have changed since the draft.
            check_control(ctx, doc.company_id, account, direction).await?;
            check_party(&party.0, direction)?;
            total += header.on_account;
            Some((party, account))
        };
        let bank = if total.is_zero() {
            None
        } else {
            let bank = header.bank_account_id.ok_or_else(|| {
                Error::validation("a payment that moves money needs a bank or cash account")
                    .with_field("bank_account_id")
            })?;
            check_bank(ctx, &doc, bank).await?;
            Some(bank)
        };

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        // The ledger says what each settlement took off its item's value
        // in the accounting currency; the control account is relieved by
        // exactly that, so open items still add up to it.
        // A discount settles the item with the money paid.
        let allocations: Vec<Allocation> = lines
            .iter()
            .map(|l| Allocation {
                open_item_id: l.open_item_id,
                amount: l.amount + l.discount + l.write_off,
                discount: l.discount,
            })
            .collect();
        let taken = erpcraft_openitems::settle(&mut *ctx.conn, &posting, &allocations).await?;
        let base = erpcraft_kernel::company::base_currency(&mut *ctx.conn, doc.company_id).await?;
        let decimals =
            erpcraft_kernel::company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        let foreign = header.currency != base;
        let mut gl = vec![];
        let mut taxes = vec![];
        for ((line, item), value) in lines.iter().zip(&items).zip(&taken) {
            if !value.is_zero() {
                gl.push(settling_line(
                    item.account_id,
                    *value,
                    direction,
                    Some(line.id),
                ));
            }
            if !line.discount.is_zero() {
                let (lines, line_taxes) =
                    discount_lines(ctx, &doc, line, item, direction, rate, decimals).await?;
                gl.extend(lines);
                taxes.extend(line_taxes);
            }
            // Written off: on the side of the money, as if it were paid.
            if !line.write_off.is_zero() {
                let account =
                    erpcraft_gl::account_for(&mut *ctx.conn, doc.company_id, "write_off", &[])
                        .await
                        .map_err(|e| e.with_field("write_off"))?;
                let value = erpcraft_kernel::round_money(line.write_off * rate, decimals);
                if !value.is_zero() {
                    gl.push(settling_line(account, -value, direction, Some(line.id)));
                }
            }
        }
        let on_account_value = erpcraft_kernel::round_money(header.on_account * rate, decimals);
        if let Some((_, account)) = &on_account {
            gl.push(settling_line(*account, on_account_value, direction, None));
        }
        if let Some(bank) = bank {
            let mut line = settling_line(bank, -total, direction, None);
            if foreign {
                line.currency = Some(header.currency.clone());
                line.fx_rate = Some(rate);
            }
            gl.push(line);
        }
        // Paid at another rate than the items were booked at: realized.
        if let Some(difference) = erpcraft_gl::exchange_difference(
            &mut *ctx.conn,
            doc.company_id,
            &gl,
            Some("Exchange difference"),
        )
        .await?
        {
            gl.push(difference);
        }
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }
        erpcraft_tax::record(&mut *ctx.conn, &posting, &taxes).await?;
        if let Some(((party_type, party_id), account_id)) = on_account {
            erpcraft_openitems::open(
                &mut *ctx.conn,
                &posting,
                &NewOpenItem {
                    kind: direction.settles(),
                    party_type: party_kind(&party_type)?,
                    party_id,
                    account_id,
                    amount: -on_account_value,
                    due_date: None,
                    terms: None,
                    foreign: foreign.then(|| (header.currency.clone(), -header.on_account)),
                },
            )
            .await?;
        }
        if foreign {
            sqlx::query("UPDATE payment SET fx_rate = $2 WHERE document_id = $1")
                .bind(doc.id)
                .bind(rate)
                .execute(&mut *ctx.conn)
                .await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

/// The party types an open item may name, as the static names it keeps.
fn party_kind(party_type: &str) -> Result<&'static str> {
    match party_type {
        "vendor" => Ok("vendor"),
        "customer" => Ok("customer"),
        _ => Err(Error::validation("party_type is vendor or customer").with_field("party_type")),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApplyInput {
    /// The company the items belong to.
    company_id: Uuid,
    /// The credit to apply: an open item below zero, such as a credit note
    /// or money paid on account.
    credit_item_id: Uuid,
    /// The debt it reduces: an open item of the same vendor or customer,
    /// such as an invoice.
    debt_item_id: Uuid,
    /// How much to apply. As much as both still have open when omitted.
    amount: Option<Decimal>,
    /// Date of the application. Today when omitted.
    doc_date: Option<NaiveDate>,
}

struct ApplyCredit;

#[async_trait]
impl Command for ApplyCredit {
    type Input = ApplyInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "payment.apply_credit",
            "Apply a credit of a vendor or customer (a credit note, money paid on account) to one \
             of its debts (an invoice), without money moving: a payment of nothing is posted \
             that settles the debt and the credit by the same amount. Reverse it with \
             payment.reverse.",
        )
        .label("Apply credit")
        .resource("open_item")
        .target(false)
        .permission("payment:post")
        .creates(PAYMENT)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ApplyInput) -> Result<DocumentView> {
        ctx.require("open_item:read", input.company_id)?;
        ctx.require("payment:create", input.company_id)?;
        let items = erpcraft_openitems::lock(
            &mut *ctx.conn,
            input.company_id,
            &[input.credit_item_id, input.debt_item_id],
        )
        .await?;
        let (credit, debt) = (&items[0], &items[1]);
        if !credit.amount.is_sign_negative() {
            return Err(Error::validation("this item is not a credit").with_field("credit_item_id"));
        }
        if debt.amount.is_sign_negative() {
            return Err(Error::validation("this item is not a debt").with_field("debt_item_id"));
        }
        if (
            credit.kind.as_str(),
            credit.party_type.as_str(),
            credit.party_id,
        ) != (debt.kind.as_str(), debt.party_type.as_str(), debt.party_id)
        {
            return Err(Error::validation(
                "a credit applies only to a debt of the same vendor or customer, payable or receivable alike",
            ));
        }
        // Applying moves value between the two items' accounts: they must
        // be one control account, of the kind the party belongs to.
        if credit.account_id != debt.account_id {
            return Err(Error::validation(
                "a credit applies only to a debt kept on the same account",
            ));
        }
        let direction = if credit.kind == Kind::Receivable.as_str() {
            Direction::Incoming
        } else {
            Direction::Outgoing
        };
        check_party(&credit.party_type, direction)?;
        if credit.currency != debt.currency {
            return Err(Error::validation(format!(
                "the credit is in {} and the debt in {}: a credit applies to a debt of its own currency",
                credit.currency, debt.currency
            )));
        }
        // In the items' currency. At their two rates the same amount is
        // worth two values; the difference is realized when it is applied.
        let most = (-credit.txn_open).min(debt.txn_open);
        let amount = input.amount.unwrap_or(most);
        if amount <= Decimal::ZERO || amount > most {
            return Err(Error::validation(format!(
                "between 0 and {} can be applied",
                most.normalize()
            ))
            .with_field("amount"));
        }
        let draft = document::create(
            ctx,
            document::NewDocument {
                doc_type: PAYMENT.into(),
                company_id: input.company_id,
                doc_date: input
                    .doc_date
                    .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive()),
                memo: None,
                body: json!({
                    "direction": direction.as_str(),
                    "allocations": [
                        { "open_item_id": debt.id, "amount": amount },
                        { "open_item_id": credit.id, "amount": -amount },
                    ],
                }),
            },
        )
        .await?;
        ctx.require("payment:post", input.company_id)?;
        PostPayment
            .run(
                ctx,
                PostInput {
                    id: draft.header.id,
                },
            )
            .await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReverseInput {
    /// Payment id.
    id: Uuid,
    /// Accounting date of the reversal. Defaults to the date of the
    /// original posting, whose period must then still be open.
    posting_date: Option<NaiveDate>,
    /// Memo of the reversing posting. Defaults to "Reversal of" and the document number.
    memo: Option<String>,
}

struct ReversePayment;

#[async_trait]
impl Command for ReversePayment {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "payment.reverse",
            "Cancel a posted payment, for example a bounced check. The items it settled are \
             open again by what it had paid.",
        )
        .from_states(&["posted"])
        .destructive()
        .permission("payment:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PAYMENT, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} payment cannot be reversed",
                doc.status
            )));
        }
        let original = document::original_posting(&mut *ctx.conn, &doc).await?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}
