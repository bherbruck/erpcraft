//! `open_item.aging`: what is open, by party and by how long past due.

use chrono::{Days, NaiveDate};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, company, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Kind;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgingInput {
    /// The company to report on.
    company_id: Uuid,
    /// Payables (what the company owes) or receivables (what it is owed).
    kind: Kind,
    /// The day to age as of: items and settlements dated after it are left
    /// out. Today when omitted.
    as_of: Option<NaiveDate>,
    /// Only one party: give both the type (`vendor`, `customer`) and the id.
    party_type: Option<String>,
    /// Id of the vendor or customer; give `party_type` with it.
    party_id: Option<Uuid>,
    /// Where the buckets past due end, in days, rising: `[30, 60, 90]` when
    /// omitted, for current, 1-30, 31-60, 61-90 and over 90 days past due.
    bounds: Option<Vec<i32>>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct Bucket {
    /// How the bucket is shown: `current`, `1-30`, `over 90`.
    label: String,
    /// The fewest days past due in the bucket. Absent for current.
    from_days: Option<i32>,
    /// The most days past due in the bucket. Absent for the last.
    to_days: Option<i32>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct PartyAging {
    /// `vendor` or `customer`.
    party_type: String,
    party_id: Uuid,
    /// The party's name, when it can be read.
    party_name: Option<String>,
    /// What is open, one amount per bucket, in the order of `buckets`.
    amounts: Vec<Decimal>,
    /// All that is open for the party.
    total: Decimal,
    /// Of what is not yet past due, what falls due within the next 7 days
    /// (the as-of day included).
    due_next_7: Decimal,
    /// Of what is not yet past due, what falls due within the next 30 days.
    due_next_30: Decimal,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct ControlAccount {
    account_id: Uuid,
    /// Account code.
    code: String,
    /// Account name.
    name: String,
    /// Its balance as of the day: credit less debit for payables, debit
    /// less credit for receivables.
    balance: Decimal,
    /// What the open items it carries add up to as of the day.
    open: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct AgingOutput {
    company_id: Uuid,
    /// `payable` or `receivable`.
    kind: Kind,
    /// The day aged as of.
    as_of: NaiveDate,
    /// The accounting currency, which every amount is in: an item in
    /// another currency at the value it is carried at.
    currency: String,
    buckets: Vec<Bucket>,
    /// One row per party with anything open, by name.
    parties: Vec<PartyAging>,
    /// Each bucket for all parties together.
    totals: Vec<Decimal>,
    /// All that is open.
    total: Decimal,
    /// What falls due within the next 7 days, for all parties.
    due_next_7: Decimal,
    /// What falls due within the next 30 days, for all parties.
    due_next_30: Decimal,
    /// The accounts that carry the items, with their balances as of the
    /// day beside what their items add up to. Left out for one party.
    accounts: Vec<ControlAccount>,
}

#[derive(sqlx::FromRow)]
struct Open {
    party_type: String,
    party_id: Uuid,
    due: NaiveDate,
    open: Decimal,
}

/// What each item of the kind still owes as of a day, in the accounting
/// currency, at the value it is carried at: its amount less what was
/// settled, plus revaluations standing, by postings dated on or before the
/// day. Binds: company, kind, day.
pub(crate) const OPEN_AS_OF: &str = "
    SELECT i.id, i.party_type, i.party_id, i.account_id,
           COALESCE(i.due_date, i.doc_date) AS due,
           i.amount - COALESCE((
               SELECT sum(s.amount) FROM settlement s JOIN posting p ON p.id = s.posting_id
               WHERE s.open_item_id = i.id AND p.posting_date <= $3), 0)
           + COALESCE((
               SELECT sum(v.amount) FROM open_item_revaluation v
               JOIN posting p ON p.id = v.posting_id
               WHERE v.open_item_id = i.id AND p.posting_date <= $3), 0) AS open
    FROM open_item i
    WHERE i.company_id = $1 AND i.kind = $2 AND i.doc_date <= $3";

pub(crate) struct Aging;

#[async_trait]
impl Command for Aging {
    type Input = AgingInput;
    type Output = AgingOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "open_item.aging",
            "What a company owes (payables) or is owed (receivables) as of a day, by vendor or \
             customer, in buckets by how many days past its due date each item is: current, \
             1-30, 31-60, 61-90 and over 90 unless other bounds are given. Amounts are in the \
             accounting currency; an item in another currency counts at the value it is carried \
             at. Also what falls due in the next 7 and 30 days, and the balance of each \
             payables or receivables account beside what its items add up to. As of a past day, \
             only what was posted by then counts.",
        )
        .report("Aging")
        .permission("open_item:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: AgingInput) -> Result<AgingOutput> {
        ctx.require("open_item:read", input.company_id)?;
        let as_of = input
            .as_of
            .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive());
        let bounds = input.bounds.unwrap_or_else(|| vec![30, 60, 90]);
        if bounds.is_empty()
            || bounds.len() > 12
            || bounds[0] < 1
            || bounds.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(Error::field(
                "bounds",
                "give 1 to 12 numbers of days, rising, from 1",
            ));
        }
        if input.party_type.is_some() != input.party_id.is_some() {
            return Err(Error::field(
                "party_id",
                "give both party_type and party_id, or neither",
            ));
        }
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let kind = input.kind.as_str();

        let rows: Vec<Open> = sqlx::query_as(&format!(
            "SELECT party_type, party_id, due, open FROM ({OPEN_AS_OF}) AS items
             WHERE open <> 0
               AND ($4::text IS NULL OR party_type = $4) AND ($5::uuid IS NULL OR party_id = $5)"
        ))
        .bind(input.company_id)
        .bind(kind)
        .bind(as_of)
        .bind(&input.party_type)
        .bind(input.party_id)
        .fetch_all(&mut *ctx.conn)
        .await?;

        let buckets = buckets(&bounds);
        let in_7 = as_of.checked_add_days(Days::new(7)).unwrap_or(as_of);
        let in_30 = as_of.checked_add_days(Days::new(30)).unwrap_or(as_of);
        let mut parties: Vec<PartyAging> = Vec::new();
        for row in rows {
            let index = match parties
                .iter()
                .position(|p| p.party_type == row.party_type && p.party_id == row.party_id)
            {
                Some(index) => index,
                None => {
                    parties.push(PartyAging {
                        party_type: row.party_type.clone(),
                        party_id: row.party_id,
                        party_name: None,
                        amounts: vec![Decimal::ZERO; buckets.len()],
                        total: Decimal::ZERO,
                        due_next_7: Decimal::ZERO,
                        due_next_30: Decimal::ZERO,
                    });
                    parties.len() - 1
                }
            };
            let party = &mut parties[index];
            let late = (as_of - row.due).num_days();
            let bucket = if late <= 0 {
                0
            } else {
                1 + bounds.iter().take_while(|b| late > i64::from(**b)).count()
            };
            party.amounts[bucket] += row.open;
            party.total += row.open;
            if row.due >= as_of && row.due <= in_7 {
                party.due_next_7 += row.open;
            }
            if row.due >= as_of && row.due <= in_30 {
                party.due_next_30 += row.open;
            }
        }
        names(ctx, &mut parties).await?;
        parties.sort_by(|a, b| {
            (a.party_name.as_deref(), a.party_id).cmp(&(b.party_name.as_deref(), b.party_id))
        });

        let mut totals = vec![Decimal::ZERO; buckets.len()];
        for party in &parties {
            for (total, amount) in totals.iter_mut().zip(&party.amounts) {
                *total += *amount;
            }
        }
        let accounts = if input.party_id.is_some() {
            vec![]
        } else {
            // Payables are credit balances, receivables debit balances.
            let sign = match input.kind {
                Kind::Payable => "-",
                Kind::Receivable => "",
            };
            sqlx::query_as(&format!(
                "WITH items AS ({OPEN_AS_OF}),
                 carried AS (SELECT DISTINCT account_id FROM open_item
                             WHERE company_id = $1 AND kind = $2)
                 SELECT a.id AS account_id, a.code, a.name,
                        {sign}COALESCE((SELECT sum(g.debit - g.credit) FROM gl_entry g
                                  WHERE g.account_id = a.id AND g.posting_date <= $3), 0)
                            AS balance,
                        COALESCE((SELECT sum(i.open) FROM items i WHERE i.account_id = a.id), 0)
                            AS open
                 FROM carried c JOIN account a ON a.id = c.account_id
                 ORDER BY a.code"
            ))
            .bind(input.company_id)
            .bind(kind)
            .bind(as_of)
            .fetch_all(&mut *ctx.conn)
            .await?
        };
        Ok(AgingOutput {
            company_id: input.company_id,
            kind: input.kind,
            as_of,
            currency,
            buckets,
            total: totals.iter().copied().sum(),
            due_next_7: parties.iter().map(|p| p.due_next_7).sum(),
            due_next_30: parties.iter().map(|p| p.due_next_30).sum(),
            totals,
            parties,
            accounts,
        })
    }
}

fn buckets(bounds: &[i32]) -> Vec<Bucket> {
    let mut buckets = vec![Bucket {
        label: "current".into(),
        from_days: None,
        to_days: None,
    }];
    let mut from = 1;
    for bound in bounds {
        buckets.push(Bucket {
            label: format!("{from}-{bound}"),
            from_days: Some(from),
            to_days: Some(*bound),
        });
        from = bound + 1;
    }
    buckets.push(Bucket {
        label: format!("over {}", from - 1),
        from_days: Some(from),
        to_days: None,
    });
    buckets
}

/// The names of vendors and customers, for whoever may read them; the
/// modules that keep them may not be installed.
async fn names(ctx: &mut Ctx<'_>, parties: &mut [PartyAging]) -> Result<()> {
    for table in ["vendor", "customer"] {
        let ids: Vec<Uuid> = parties
            .iter()
            .filter(|p| p.party_type == table)
            .map(|p| p.party_id)
            .collect();
        if ids.is_empty() || !ctx.actor.can_any(&format!("{table}:read")) {
            continue;
        }
        let installed: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&mut *ctx.conn)
            .await?;
        if !installed {
            continue;
        }
        let found: Vec<(Uuid, String)> =
            sqlx::query_as(&format!("SELECT id, name FROM {table} WHERE id = ANY($1)"))
                .bind(&ids)
                .fetch_all(&mut *ctx.conn)
                .await?;
        for party in parties.iter_mut().filter(|p| p.party_type == table) {
            party.party_name = found
                .iter()
                .find(|(id, _)| *id == party.party_id)
                .map(|(_, name)| name.clone());
        }
    }
    Ok(())
}
