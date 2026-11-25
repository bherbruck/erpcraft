//! Fixed assets: the register of what a company owns and uses for years,
//! its depreciation month by month, and its sale or scrapping.
//!
//! An `asset_class` (per company) says how a kind of asset is depreciated
//! and, through account rules with scope `asset_class`, which accounts hold
//! it: `asset_cost`, `accumulated_depreciation`, `depreciation_expense`,
//! `asset_disposal_gain` and `asset_disposal_loss`. An `asset` is
//! capitalised from a posted ledger line that debited its class's cost
//! account (`asset.capitalise`), or taken over from an older system with
//! its cost and depreciation to date (`asset.take_over`); neither posts,
//! because the ledger holds the money already. A `depreciation_run`
//! depreciates every asset in service up to a month end and posts it; an
//! `asset_disposal` takes an asset out of the books. `asset.register` sets
//! the register against the ledger; `asset.schedule` shows one asset's
//! depreciation, past and to come.
//!
//! The module writes the ledger only through `erpcraft_gl::post` and reads
//! it through SQL, as a report does.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_kernel::document::DocumentHandler;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DynCommand, EntityMeta, Error, FieldMeta, FieldType,
    JsonSchema, Migration, Module, PgConnection, PolicyPoint, Result, async_trait, command, entity,
    schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod disposal;
pub mod plan;
mod reports;
mod run;

use plan::{Method, Params, first_of, month_end, months_between, next_month};

/// The policy point asked when an asset is capitalised or taken over.
pub const BELOW_MINIMUM: &str = "asset.below_minimum";

pub const ASSET_COST: &str = "asset_cost";
pub const ACCUMULATED: &str = "accumulated_depreciation";
pub const EXPENSE: &str = "depreciation_expense";
pub const GAIN: &str = "asset_disposal_gain";
pub const LOSS: &str = "asset_disposal_loss";

pub struct AssetsModule;

impl Module for AssetsModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![("de", include_str!("../i18n/de.json"))]
    }

    fn name(&self) -> &'static str {
        "assets"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "assets",
                sql: include_str!("../migrations/0001_assets.sql"),
            },
            Migration {
                version: 2,
                name: "delete_taken_over",
                sql: include_str!("../migrations/0002_delete_taken_over.sql"),
            },
            Migration {
                version: 3,
                name: "opening_entry",
                sql: include_str!("../migrations/0003_opening_entry.sql"),
            },
            Migration {
                version: 4,
                name: "keep_capitalised_postings",
                sql: include_str!("../migrations/0004_keep_capitalised_postings.sql"),
            },
            Migration {
                version: 5,
                name: "keep_opening_postings",
                sql: include_str!("../migrations/0005_keep_opening_postings.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![settings_entity(), class_entity(), asset_entity()]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(run::DepreciationRun), Arc::new(disposal::Disposal)]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                BELOW_MINIMUM,
                "Asked when an asset is capitalised or taken over. By default an asset that \
                 costs less than the company's minimum (asset_settings.minimum_cost; 0 means \
                 none) is refused: below it, a purchase is an expense. A company that wants a \
                 class capitalised whatever it costs allows it, for example when \
                 `facts.class_code == 'IT'`.",
            )
            .number("cost", "What the asset cost, in the accounting currency")
            .as_company_money()
            .number("minimum", "The company's minimum cost, 0 for none")
            .as_company_money()
            .text("class_code", "The code of the asset's class")
            .refuse_when("facts.minimum > 0 && facts.cost < facts.minimum")
            .refusal("the asset costs less than the company capitalises"),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Capitalise),
            command(CostLines),
            command(TakeOver),
            command(LinkOpening),
            command(PutInService),
            command(run::Post),
            command(run::Reverse),
            command(disposal::Post),
            command(disposal::Reverse),
            command(reports::Register),
            command(reports::Schedule),
        ]
    }
}

// --------------------------------------------------------------- entities

const METHODS: &[&str] = &["straight_line", "declining_balance", "none"];

fn company() -> FieldMeta {
    FieldMeta::new("company_id", "Company", FieldType::link("company"))
        .required()
        .immutable()
}

fn created() -> FieldMeta {
    FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly()
}

fn settings_entity() -> EntityMeta {
    EntityMeta::new("asset_settings", "Fixed asset settings")
        .plural("Fixed asset settings")
        .field(company())
        .field(
            FieldMeta::new(
                "convention",
                "Month of service",
                FieldType::options(&["full_month", "next_month"]),
            )
            .help(
                "full_month: the month an asset goes into service is depreciated in full. \
                 next_month: depreciation starts the month after.",
            ),
        )
        .field(
            FieldMeta::new(
                "minimum_cost",
                "Minimum cost to capitalise",
                FieldType::Decimal,
            )
            .money()
            .help("Below it, a purchase is an expense (policy asset.below_minimum). 0: none."),
        )
        .field(created())
        .order_by(&["created_at"])
        .display(&["company_id"])
        .company_field("company_id")
}

fn class_entity() -> EntityMeta {
    EntityMeta::new("asset_class", "Asset class")
        .plural("Asset classes")
        .field(company())
        .field(FieldMeta::new("code", "Code", FieldType::Text).required())
        .field(FieldMeta::new("name", "Name", FieldType::Text).required())
        .field(
            FieldMeta::new("method", "Depreciation method", FieldType::options(METHODS)).help(
                "straight_line: the same each month. declining_balance: a yearly rate of \
                     the book value, switching to straight line once that is more. none: land.",
            ),
        )
        .field(
            FieldMeta::new(
                "declining_rate",
                "Declining rate (% a year)",
                FieldType::Decimal,
            )
            .percent(4),
        )
        .field(FieldMeta::new(
            "useful_life_months",
            "Useful life (months)",
            FieldType::Int,
        ))
        .field(FieldMeta::new("active", "Active", FieldType::Bool))
        .field(created())
        .order_by(&["code"])
        .search(&["code", "name"])
        .company_field("company_id")
}

fn asset_entity() -> EntityMeta {
    let money = |name: &str, label: &str| FieldMeta::new(name, label, FieldType::Decimal).money();
    let date = |name: &str, label: &str| FieldMeta::new(name, label, FieldType::Date).readonly();
    EntityMeta::new("asset", "Fixed asset")
        .field(company())
        .field(FieldMeta::new("class_id", "Class", FieldType::link("asset_class")).required())
        .field(FieldMeta::new("code", "Code", FieldType::Text).required())
        .field(FieldMeta::new("name", "Name", FieldType::Text).required())
        // Changed only by the module's commands.
        .field(
            FieldMeta::new(
                "status",
                "Status",
                FieldType::options(&["planned", "in_service", "fully_depreciated", "disposed"]),
            )
            .readonly(),
        )
        .field(
            FieldMeta::new("method", "Depreciation method", FieldType::options(METHODS))
                .help("The class's unless given."),
        )
        .field(
            FieldMeta::new(
                "declining_rate",
                "Declining rate (% a year)",
                FieldType::Decimal,
            )
            .percent(4),
        )
        .field(
            FieldMeta::new("useful_life_months", "Useful life (months)", FieldType::Int)
                .help("The class's unless given."),
        )
        .field(money("salvage_value", "Salvage value"))
        .field(money("cost", "Cost").readonly())
        .field(date("acquisition_date", "Acquired"))
        .field(date("recorded_on", "In the books from"))
        .field(date("in_service_date", "In service from"))
        .field(
            FieldMeta::new(
                "source_entry_id",
                "Capitalised from",
                FieldType::link("gl_entry"),
            )
            .readonly(),
        )
        .field(
            FieldMeta::new(
                "opening_entry_id",
                "Opened by",
                FieldType::link("gl_entry"),
            )
            .readonly()
            .help("For an asset taken over: the ledger line of the opening entry that brought its cost in."),
        )
        .field(money("opening_accumulated", "Depreciation taken over").readonly())
        .field(date("opening_through", "Taken over depreciated through"))
        .field(date("disposed_on", "Disposed of"))
        .field(
            FieldMeta::new(
                "disposal_id",
                "Disposal",
                FieldType::document("asset_disposal"),
            )
            .readonly(),
        )
        .field(FieldMeta::new("location", "Location", FieldType::Text))
        .field(FieldMeta::new(
            "cost_centre",
            "Cost centre",
            FieldType::Text,
        ))
        .field(created())
        .order_by(&["code"])
        .search(&["code", "name"])
        .state_field("status")
        .company_field("company_id")
}

// ------------------------------------------------------------ the asset

/// An asset as its depreciation sees it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct Asset {
    pub id: Uuid,
    pub company_id: Uuid,
    pub class_id: Uuid,
    pub code: String,
    pub name: String,
    pub status: String,
    pub method: String,
    pub declining_rate: Option<Decimal>,
    pub useful_life_months: Option<i32>,
    pub salvage_value: Decimal,
    pub cost: Option<Decimal>,
    pub acquisition_date: Option<NaiveDate>,
    pub recorded_on: Option<NaiveDate>,
    pub in_service_date: Option<NaiveDate>,
    pub opening_accumulated: Decimal,
    pub opening_through: Option<NaiveDate>,
    pub disposed_on: Option<NaiveDate>,
}

pub(crate) const ASSET_COLUMNS: &str = "id, company_id, class_id, code, name, status, method, \
     declining_rate, useful_life_months, salvage_value, cost, acquisition_date, recorded_on, \
     in_service_date, opening_accumulated, opening_through, disposed_on";

pub(crate) async fn load_asset(conn: &mut PgConnection, id: Uuid, lock: bool) -> Result<Asset> {
    let sql = format!(
        "SELECT {ASSET_COLUMNS} FROM asset WHERE id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| Error::not_found("asset"))
}

/// How a company depreciates.
pub(crate) struct Settings {
    pub next_month: bool,
    pub minimum_cost: Decimal,
}

pub(crate) async fn settings(conn: &mut PgConnection, company_id: Uuid) -> Result<Settings> {
    let row: Option<(String, Decimal)> =
        sqlx::query_as("SELECT convention, minimum_cost FROM asset_settings WHERE company_id = $1")
            .bind(company_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map_or(
        Settings {
            next_month: false,
            minimum_cost: Decimal::ZERO,
        },
        |(convention, minimum_cost)| Settings {
            next_month: convention == "next_month",
            minimum_cost,
        },
    ))
}

/// The first month an asset in service from `in_service` is depreciated.
pub(crate) fn start_month(in_service: NaiveDate, settings: &Settings) -> NaiveDate {
    if settings.next_month {
        next_month(first_of(in_service))
    } else {
        first_of(in_service)
    }
}

/// What the asset's plan is worked out from, once it has a cost and is in
/// service.
pub(crate) fn params(asset: &Asset, settings: &Settings, places: u32) -> Option<Params> {
    let cost = asset.cost?;
    let in_service = asset.in_service_date?;
    let start = start_month(in_service, settings);
    let (basis, elapsed, accumulated) = match asset.opening_through {
        Some(through) => (
            next_month(first_of(through)).max(start),
            (months_between(start, through) + 1).max(0),
            asset.opening_accumulated,
        ),
        None => (start, 0, Decimal::ZERO),
    };
    Some(Params {
        cost,
        salvage: asset.salvage_value,
        method: Method::parse(&asset.method),
        life: asset.useful_life_months.unwrap_or(0),
        rate: asset.declining_rate.unwrap_or_default() / Decimal::from(100),
        basis,
        elapsed,
        accumulated,
        places,
    })
}

/// The months of an asset depreciated so far, not reversed.
pub(crate) async fn depreciated(
    conn: &mut PgConnection,
    asset: Uuid,
) -> Result<BTreeSet<NaiveDate>> {
    let months: Vec<NaiveDate> = sqlx::query_scalar(
        "SELECT month FROM asset_depreciation WHERE asset_id = $1 AND NOT reversed",
    )
    .bind(asset)
    .fetch_all(&mut *conn)
    .await?;
    Ok(months.into_iter().collect())
}

/// The months of the plan not depreciated yet that ended by `through`.
pub(crate) async fn due(
    conn: &mut PgConnection,
    asset: &Asset,
    settings: &Settings,
    places: u32,
    through: NaiveDate,
) -> Result<Vec<(NaiveDate, Decimal)>> {
    let Some(params) = params(asset, settings, places) else {
        return Ok(vec![]);
    };
    let done = depreciated(&mut *conn, asset.id).await?;
    Ok(plan::plan(&params)
        .into_iter()
        .filter(|(month, _)| month_end(*month) <= through && !done.contains(month))
        .collect())
}

/// The status an asset that is not disposed of has: planned until it is in
/// service, fully depreciated once its plan has nothing left.
pub(crate) async fn status_of(
    conn: &mut PgConnection,
    asset: &Asset,
    settings: &Settings,
    places: u32,
) -> Result<&'static str> {
    let Some(params) = params(asset, settings, places) else {
        return Ok("planned");
    };
    let done = depreciated(&mut *conn, asset.id).await?;
    let left = plan::plan(&params)
        .iter()
        .any(|(month, _)| !done.contains(month));
    Ok(if left {
        "in_service"
    } else {
        "fully_depreciated"
    })
}

/// The account a class posts to for `purpose`.
pub(crate) async fn account(
    conn: &mut PgConnection,
    company_id: Uuid,
    purpose: &str,
    class_id: Uuid,
) -> Result<Uuid> {
    erpcraft_gl::account_for(conn, company_id, purpose, &[("asset_class", class_id)]).await
}

pub(crate) fn shown(amount: Decimal) -> String {
    amount.normalize().to_string()
}

async fn class_code(conn: &mut PgConnection, class_id: Uuid) -> Result<String> {
    Ok(
        sqlx::query_scalar("SELECT code FROM asset_class WHERE id = $1")
            .bind(class_id)
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// The company's policy on capitalising this cost.
async fn check_minimum(ctx: &mut Ctx<'_>, asset: &Asset, cost: Decimal) -> Result<()> {
    let settings = settings(&mut *ctx.conn, asset.company_id).await?;
    let code = class_code(&mut *ctx.conn, asset.class_id).await?;
    ctx.decide(
        BELOW_MINIMUM,
        asset.company_id,
        json!({
            "cost": shown(cost),
            "minimum": shown(settings.minimum_cost),
            "class_code": code,
        }),
    )
    .await
}

/// Lock an asset for a command on it, which belongs in its history.
async fn locked(ctx: &mut Ctx<'_>, id: Uuid, permission: &str) -> Result<Asset> {
    let asset = load_asset(&mut *ctx.conn, id, true).await?;
    ctx.touch("asset", id);
    ctx.require(permission, asset.company_id)?;
    Ok(asset)
}

/// A cost or an amount has at most the accounting currency's decimals.
async fn check_places(
    conn: &mut PgConnection,
    company_id: Uuid,
    field: &str,
    amount: Decimal,
) -> Result<()> {
    let places = erpcraft_kernel::company::currency_decimals(conn, company_id).await?;
    if amount.normalize().scale() > places {
        return Err(Error::field(
            field,
            format!("has at most {places} decimal places"),
        ));
    }
    Ok(())
}

// -------------------------------------------------------------- capitalise

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CapitaliseInput {
    /// The asset, planned and not capitalised yet.
    id: Uuid,
    /// The posted line that bought the asset, of a journal entry for
    /// example. What it posted to the asset class's `asset_cost` account is
    /// the asset's cost. `asset.cost_lines` lists the lines that can be
    /// chosen.
    #[schemars(extend("x-field-type" = {
        "kind": "lookup",
        "command": "asset.cost_lines",
        "rows": "items",
        "key": "line_id",
        "display": ["number", "posting_date", "amount"],
        "params": { "id": { "from": "field", "field": "id" } },
        "by_keys": "line_ids"
    }))]
    line_id: Uuid,
    /// The document of that line. Not needed: the line names it. When
    /// given, the line must be one of its lines.
    document_id: Option<Uuid>,
    /// When the asset went into service, if it has: depreciation starts
    /// then. Without it the asset stays planned until asset.put_in_service.
    in_service_date: Option<NaiveDate>,
}

struct Capitalise;

#[async_trait]
impl Command for Capitalise {
    type Input = CapitaliseInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "asset.capitalise",
            "Capitalise a planned asset from a posted document line that debited its class's \
             asset_cost account, such as a journal entry line: the line's amount becomes the \
             asset's cost and its posting date the acquisition date. Nothing is posted: the \
             ledger holds the cost already. A ledger line is capitalised once. The company's \
             policy asset.below_minimum is asked.",
        )
        .permission("asset:capitalise")
        .label("Capitalise")
        .from_states(&["planned"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CapitaliseInput) -> Result<Value> {
        let asset = locked(ctx, input.id, "asset:capitalise").await?;
        if asset.cost.is_some() {
            return Err(Error::state("the asset is capitalised already"));
        }
        let cost_account =
            account(&mut *ctx.conn, asset.company_id, ASSET_COST, asset.class_id).await?;
        let entries: Vec<(Uuid, Decimal, NaiveDate, Option<Uuid>)> = sqlx::query_as(
            "SELECT g.id, g.debit, g.posting_date,
                    (SELECT a.id FROM asset a
                     WHERE a.source_entry_id = g.id OR a.opening_entry_id = g.id LIMIT 1)
             FROM gl_entry g JOIN posting p ON p.id = g.posting_id
             WHERE ($1::uuid IS NULL OR p.document_id = $1) AND g.source_line_id = $2
               AND g.company_id = $3
               AND g.account_id = $4 AND g.debit > 0 AND p.reverses_id IS NULL
               AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)",
        )
        .bind(input.document_id)
        .bind(input.line_id)
        .bind(asset.company_id)
        .bind(cost_account)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let (entry, cost, posted_on, taken) = match entries.as_slice() {
            [one] => *one,
            [] => {
                return Err(Error::field(
                    "line_id",
                    "posted nothing to the asset class's cost account that stands: name a posted \
                     line of the company that debited it",
                ));
            }
            _ => {
                return Err(Error::field(
                    "line_id",
                    "posted to the cost account more than once; capitalise it by hand",
                ));
            }
        };
        if taken.is_some() {
            return Err(Error::field(
                "line_id",
                "is capitalised as another asset already, or is the opening entry of assets taken \
                 over",
            ));
        }
        if let Some(date) = input.in_service_date
            && date < posted_on
        {
            return Err(Error::field(
                "in_service_date",
                format!("must not be before the acquisition on {posted_on}"),
            ));
        }
        if asset.salvage_value >= cost {
            return Err(Error::validation(format!(
                "the salvage value {} is not below the cost {}",
                shown(asset.salvage_value),
                shown(cost)
            )));
        }
        check_minimum(ctx, &asset, cost).await?;
        sqlx::query(
            "UPDATE asset SET cost = $2, acquisition_date = $3, recorded_on = $3,
                 source_entry_id = $4, in_service_date = $5,
                 status = CASE WHEN $5::date IS NULL THEN 'planned' ELSE 'in_service' END
             WHERE id = $1",
        )
        .bind(asset.id)
        .bind(cost)
        .bind(posted_on)
        .bind(entry)
        .bind(input.in_service_date)
        .execute(&mut *ctx.conn)
        .await?;
        entity::get(ctx, "asset", asset.id).await
    }
}

// -------------------------------------------------------------- cost lines

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CostLinesInput {
    /// The asset to capitalise.
    id: Uuid,
    /// Only these lines, whether or not they can still be chosen: to show
    /// the line an asset was capitalised from.
    line_ids: Option<Vec<Uuid>>,
    /// As a field's lookup: at most this many lines (and never more than
    /// 200).
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct CostLine {
    /// The document line: what `asset.capitalise` takes as `line_id`.
    line_id: Uuid,
    /// The document it is a line of: its id, type and number.
    document_id: Uuid,
    doc_type: String,
    number: Option<String>,
    /// When the line was posted: the acquisition date it would give.
    posting_date: NaiveDate,
    /// What it debited to the cost account: the cost it would give.
    amount: Decimal,
}

#[derive(Serialize, JsonSchema)]
struct CostLinesOutput {
    items: Vec<CostLine>,
}

struct CostLines;

#[async_trait]
impl Command for CostLines {
    type Input = CostLinesInput;
    type Output = CostLinesOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "asset.cost_lines",
            "The posted lines an asset can be capitalised from with asset.capitalise: lines of \
             its company that debited its class's asset_cost account, are not reversed and are \
             not capitalised as an asset yet. Newest first, at most 200.",
        )
        .permission("asset:capitalise")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CostLinesInput) -> Result<CostLinesOutput> {
        let asset = load_asset(&mut *ctx.conn, input.id, false).await?;
        ctx.require("asset:capitalise", asset.company_id)?;
        let cost_account =
            account(&mut *ctx.conn, asset.company_id, ASSET_COST, asset.class_id).await?;
        let items = sqlx::query_as(
            "SELECT g.source_line_id AS line_id, p.document_id, d.doc_type, d.number,
                    g.posting_date, g.debit AS amount
             FROM gl_entry g
             JOIN posting p ON p.id = g.posting_id
             JOIN document d ON d.id = p.document_id
             WHERE g.company_id = $1 AND g.account_id = $2 AND g.debit > 0
               AND g.source_line_id IS NOT NULL
               AND CASE WHEN $3::uuid[] IS NULL
                   THEN p.reverses_id IS NULL
                        AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)
                        AND NOT EXISTS (SELECT 1 FROM asset a
                                        WHERE a.source_entry_id = g.id
                                           OR a.opening_entry_id = g.id)
                   ELSE g.source_line_id = ANY($3) END
             ORDER BY g.posting_date DESC, d.number DESC, g.source_line_id
             LIMIT LEAST(GREATEST(COALESCE($4, 200), 0), 200)",
        )
        .bind(asset.company_id)
        .bind(cost_account)
        .bind(&input.line_ids)
        .bind(input.limit)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(CostLinesOutput { items })
    }
}

// --------------------------------------------------------------- take over

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TakeOverInput {
    /// The asset, planned and not capitalised yet.
    id: Uuid,
    /// What it cost, in the accounting currency.
    cost: Decimal,
    /// Its depreciation up to the end of the month of
    /// `depreciated_through`.
    accumulated_depreciation: Decimal,
    /// When the asset was bought.
    acquisition_date: NaiveDate,
    /// When it went into service: its life is counted from then.
    in_service_date: NaiveDate,
    /// The last month the accumulated depreciation covers: depreciation
    /// here starts the month after.
    depreciated_through: NaiveDate,
    /// The day the books here hold the asset from: the date of the opening
    /// entry that brought its cost and depreciation into the ledger. The
    /// day after `depreciated_through` unless given.
    opening_date: Option<NaiveDate>,
    /// The opening entry's document, when it is posted: with `line_id`.
    document_id: Option<Uuid>,
    /// The line of the opening entry that debited the class's cost account
    /// with this asset's cost (alone or with other assets taken over): that
    /// line is then not offered for capitalising. Link it later with
    /// asset.link_opening when the entry is posted after the take-over.
    line_id: Option<Uuid>,
}

struct TakeOver;

#[async_trait]
impl Command for TakeOver {
    type Input = TakeOverInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "asset.take_over",
            "Record an asset taken over from another system with its cost and its depreciation \
             to date. Nothing is posted: the opening entry that brings the balances into the \
             ledger is a journal entry of its own, and asset.register shows whether the \
             register and the ledger agree. Depreciation continues the month after \
             depreciated_through, over the life that is left.",
        )
        .permission("asset:capitalise")
        .label("Take over")
        .from_states(&["planned"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: TakeOverInput) -> Result<Value> {
        let asset = locked(ctx, input.id, "asset:capitalise").await?;
        if asset.cost.is_some() {
            return Err(Error::state("the asset is capitalised already"));
        }
        if input.cost <= Decimal::ZERO {
            return Err(Error::field("cost", "must be more than zero"));
        }
        check_places(&mut *ctx.conn, asset.company_id, "cost", input.cost).await?;
        check_places(
            &mut *ctx.conn,
            asset.company_id,
            "accumulated_depreciation",
            input.accumulated_depreciation,
        )
        .await?;
        if input.accumulated_depreciation < Decimal::ZERO
            || input.accumulated_depreciation + asset.salvage_value > input.cost
        {
            return Err(Error::field(
                "accumulated_depreciation",
                "must be between zero and the cost less the salvage value",
            ));
        }
        if input.in_service_date < input.acquisition_date {
            return Err(Error::field(
                "in_service_date",
                "must not be before the acquisition date",
            ));
        }
        if month_end(input.depreciated_through) != input.depreciated_through {
            return Err(Error::field(
                "depreciated_through",
                "must be the last day of a month",
            ));
        }
        if input.depreciated_through < input.in_service_date {
            return Err(Error::field(
                "depreciated_through",
                "must not be before the asset went into service",
            ));
        }
        let opening = input
            .opening_date
            .unwrap_or(input.depreciated_through + chrono::Duration::days(1));
        check_minimum(ctx, &asset, input.cost).await?;
        let opening_entry = match input.line_id {
            Some(line_id) => {
                Some(opening_entry(ctx, &asset, input.document_id, line_id, input.cost).await?)
            }
            None => None,
        };
        sqlx::query(
            "UPDATE asset SET cost = $2, opening_accumulated = $3, acquisition_date = $4,
                 in_service_date = $5, opening_through = $6, recorded_on = $7,
                 status = 'in_service', opening_entry_id = $8
             WHERE id = $1",
        )
        .bind(asset.id)
        .bind(input.cost)
        .bind(input.accumulated_depreciation)
        .bind(input.acquisition_date)
        .bind(input.in_service_date)
        .bind(input.depreciated_through)
        .bind(opening)
        .bind(opening_entry)
        .execute(&mut *ctx.conn)
        .await?;
        // Fully depreciated already, perhaps.
        let asset = load_asset(&mut *ctx.conn, asset.id, true).await?;
        let settings = settings(&mut *ctx.conn, asset.company_id).await?;
        let places =
            erpcraft_kernel::company::currency_decimals(&mut *ctx.conn, asset.company_id).await?;
        let status = status_of(&mut *ctx.conn, &asset, &settings, places).await?;
        sqlx::query("UPDATE asset SET status = $2 WHERE id = $1")
            .bind(asset.id)
            .bind(status)
            .execute(&mut *ctx.conn)
            .await?;
        entity::get(ctx, "asset", asset.id).await
    }
}

/// The ledger entry of an opening entry's line that brought the cost of an
/// asset taken over into the books: a posted line of the asset's company,
/// not reversed, that debited its class's cost account, and no asset was
/// capitalised from. The assets taken over on one line together cost no
/// more than it debited.
async fn opening_entry(
    ctx: &mut Ctx<'_>,
    asset: &Asset,
    document_id: Option<Uuid>,
    line_id: Uuid,
    cost: Decimal,
) -> Result<Uuid> {
    let cost_account =
        account(&mut *ctx.conn, asset.company_id, ASSET_COST, asset.class_id).await?;
    let entries: Vec<(Uuid, Decimal, bool, Decimal)> = sqlx::query_as(
        "SELECT g.id, g.debit,
                EXISTS (SELECT 1 FROM asset a WHERE a.source_entry_id = g.id),
                COALESCE((SELECT sum(a.cost) FROM asset a
                          WHERE a.opening_entry_id = g.id AND a.id <> $5), 0)
         FROM gl_entry g JOIN posting p ON p.id = g.posting_id
         WHERE ($1::uuid IS NULL OR p.document_id = $1) AND g.source_line_id = $2
           AND g.company_id = $3
           AND g.account_id = $4 AND g.debit > 0 AND p.reverses_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)",
    )
    .bind(document_id)
    .bind(line_id)
    .bind(asset.company_id)
    .bind(cost_account)
    .bind(asset.id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let (entry, debit, capitalised, opened) = match entries.as_slice() {
        [one] => *one,
        [] => {
            return Err(Error::field(
                "line_id",
                "posted nothing to the asset class's cost account that stands: name the posted \
                 opening line that debited it",
            ));
        }
        _ => {
            return Err(Error::field(
                "line_id",
                "posted to the cost account more than once; name it by hand",
            ));
        }
    };
    if capitalised {
        return Err(Error::field(
            "line_id",
            "is the line an asset was capitalised from, not an opening entry",
        ));
    }
    if opened + cost > debit {
        return Err(Error::field(
            "line_id",
            format!(
                "debited {} to the cost account; the assets taken over on it would come to {}",
                shown(debit),
                shown(opened + cost)
            ),
        ));
    }
    Ok(entry)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LinkOpeningInput {
    /// The asset taken over.
    id: Uuid,
    /// The opening entry's document, with `line_id`.
    document_id: Option<Uuid>,
    /// The opening entry's line that debited the class's cost account.
    line_id: Uuid,
}

struct LinkOpening;

#[async_trait]
impl Command for LinkOpening {
    type Input = LinkOpeningInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "asset.link_opening",
            "Name the line of the opening entry that brought an asset taken over into the \
             ledger, when the entry was posted after asset.take_over. The line is then not \
             offered for capitalising another asset. Several assets taken over may share one \
             line, up to what it debited.",
        )
        .permission("asset:capitalise")
        .label("Link opening entry")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: LinkOpeningInput) -> Result<Value> {
        let asset = locked(ctx, input.id, "asset:capitalise").await?;
        let taken_over: (Option<NaiveDate>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT opening_through, source_entry_id, opening_entry_id FROM asset WHERE id = $1",
        )
        .bind(asset.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let cost = match (asset.cost, taken_over) {
            (Some(cost), (Some(_), None, None)) => cost,
            (_, (Some(_), None, Some(_))) => {
                return Err(Error::state("the asset's opening entry is linked already"));
            }
            _ => {
                return Err(Error::state(
                    "only an asset taken over has an opening entry",
                ));
            }
        };
        let entry = opening_entry(ctx, &asset, input.document_id, input.line_id, cost).await?;
        sqlx::query("UPDATE asset SET opening_entry_id = $2 WHERE id = $1")
            .bind(asset.id)
            .bind(entry)
            .execute(&mut *ctx.conn)
            .await?;
        entity::get(ctx, "asset", asset.id).await
    }
}

// ----------------------------------------------------------- put in service

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PutInServiceInput {
    /// The asset, capitalised and planned.
    id: Uuid,
    /// From when it is used: depreciation starts then.
    in_service_date: NaiveDate,
}

struct PutInService;

#[async_trait]
impl Command for PutInService {
    type Input = PutInServiceInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "asset.put_in_service",
            "Put a capitalised asset into service: it is depreciated from that month (or the \
             next, as the company's asset_settings say).",
        )
        .permission("asset:capitalise")
        .label("Put in service")
        .from_states(&["planned"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PutInServiceInput) -> Result<Value> {
        let asset = locked(ctx, input.id, "asset:capitalise").await?;
        if asset.status != "planned" {
            return Err(Error::state(format!(
                "the asset is {}, not planned",
                asset.status
            )));
        }
        let Some(acquired) = asset.acquisition_date else {
            return Err(Error::state(
                "capitalise the asset first: it has no cost yet",
            ));
        };
        if input.in_service_date < acquired {
            return Err(Error::field(
                "in_service_date",
                format!("must not be before the acquisition on {acquired}"),
            ));
        }
        sqlx::query("UPDATE asset SET in_service_date = $2, status = 'in_service' WHERE id = $1")
            .bind(asset.id)
            .bind(input.in_service_date)
            .execute(&mut *ctx.conn)
            .await?;
        entity::get(ctx, "asset", asset.id).await
    }
}
