//! Work centers and routings: where a product's operations are done, how
//! long they take, what that time costs, and how full each work center is.
//!
//! A bill's routing (`bom_operation`) names each operation's work center by
//! code, since bills are shared by companies and work centers are not. A
//! released order copies the routing (`production_order_operation`) with
//! the hours for its quantity and the work center's activity rate then in
//! force. Completing it absorbs those standard hours into the product, as
//! the bill's activities are, and posts the difference between the
//! standard and the time recorded (`production_operation_time`) to the
//! account for `production_variance`, against the absorbed cost account.

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, DocumentView};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, EntityMeta, Error, FieldMeta, FieldType, JsonSchema,
    PgConnection, Result, async_trait, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::PRODUCTION_ORDER;

pub(crate) fn entities() -> Vec<EntityMeta> {
    let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
    vec![
        EntityMeta::new("work_center", "Work center")
            .field(
                FieldMeta::new("company_id", "Company", FieldType::link("company"))
                    .required()
                    .immutable(),
            )
            .field(
                FieldMeta::new("site_id", "Site", FieldType::Uuid)
                    .help("The site it stands at, where the company has several."),
            )
            .field(FieldMeta::new("code", "Code", FieldType::Text).required())
            .field(FieldMeta::new("name", "Name", FieldType::Text).required())
            .field(
                FieldMeta::new("activity_id", "Activity", FieldType::link("cost_activity"))
                    .required()
                    .help("What an hour here costs: the activity's rate for the company."),
            )
            .field(
                FieldMeta::new("hours_per_day", "Hours per day", FieldType::Decimal)
                    .quantity()
                    .help("How many hours it can work in a day: its capacity."),
            )
            .field(FieldMeta::new("active", "Active", FieldType::Bool))
            .field(created())
            .order_by(&["code"])
            .search(&["code", "name"])
            .company_field("company_id"),
        EntityMeta::new("bom_operation", "Bill of materials operation")
            .field(
                FieldMeta::new("bom_id", "Bill of materials", FieldType::link("bom"))
                    .required()
                    .immutable(),
            )
            .field(
                FieldMeta::new("seq", "Step", FieldType::Int)
                    .required()
                    .help("Operations are done in this order."),
            )
            .field(
                FieldMeta::new("work_center_code", "Work center", FieldType::Text)
                    .required()
                    .help(
                        "The code of the work center it is done at: each company that makes \
                         the product has one of that code.",
                    ),
            )
            .field(FieldMeta::new(
                "description",
                "Description",
                FieldType::Text,
            ))
            .field(
                FieldMeta::new("setup_hours", "Setup hours", FieldType::Decimal)
                    .quantity()
                    .help("Hours once per production order."),
            )
            .field(
                FieldMeta::new("run_hours", "Run hours per unit", FieldType::Decimal)
                    .quantity()
                    .help("Hours per unit of the product made."),
            )
            .field(created())
            .order_by(&["bom_id", "seq"])
            .display(&["seq", "work_center_code"])
            .child_of("bom", "bom_id"),
    ]
}

/// One operation of a released order, with the time recorded on it.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct Operation {
    pub id: Uuid,
    pub line_no: i32,
    pub seq: i32,
    pub work_center_id: Uuid,
    pub activity_id: Uuid,
    pub description: Option<String>,
    /// Standard hours for the order.
    pub hours: Decimal,
    pub rate: Decimal,
    /// Hours recorded so far.
    pub actual_hours: Decimal,
}

pub(crate) async fn operations(
    conn: &mut PgConnection,
    document_id: Uuid,
) -> Result<Vec<Operation>> {
    Ok(sqlx::query_as(
        "SELECT o.id, o.line_no, o.seq, o.work_center_id, o.activity_id, o.description,
                trim_scale(o.hours) AS hours, o.rate,
                trim_scale(COALESCE((SELECT sum(t.hours) FROM production_operation_time t
                                     WHERE t.operation_id = o.id), 0)) AS actual_hours
         FROM production_order_operation o WHERE o.document_id = $1 ORDER BY o.line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Copy a bill's routing onto an order being released: each operation at
/// the company's work center of its code, setup plus run time for `qty`
/// units, at the work center's activity rate now.
pub(crate) async fn release(
    conn: &mut PgConnection,
    company_id: Uuid,
    document_id: Uuid,
    bom_id: Uuid,
    qty: Decimal,
) -> Result<()> {
    #[allow(clippy::type_complexity)]
    let steps: Vec<(
        i32,
        String,
        Option<String>,
        Decimal,
        Decimal,
        Option<Uuid>,
        Option<Uuid>,
        Option<Decimal>,
        Option<bool>,
    )> = sqlx::query_as(
        "SELECT b.seq, b.work_center_code, b.description, b.setup_hours, b.run_hours,
                w.id, w.activity_id, r.rate, w.active
         FROM bom_operation b
         LEFT JOIN work_center w ON w.company_id = $2 AND w.code = b.work_center_code
         LEFT JOIN cost_activity_rate r ON r.company_id = $2 AND r.activity_id = w.activity_id
         WHERE b.bom_id = $1 ORDER BY b.seq",
    )
    .bind(bom_id)
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    for (index, (seq, code, description, setup, run, center, activity, rate, active)) in
        steps.into_iter().enumerate()
    {
        let (Some(center), Some(activity)) = (center, activity) else {
            return Err(Error::validation(format!(
                "operation {seq} is done at work center {code}, which this company does not have"
            )));
        };
        if active == Some(false) {
            return Err(Error::validation(format!(
                "work center {code} of operation {seq} is inactive"
            )));
        }
        let Some(rate) = rate else {
            return Err(Error::validation(format!(
                "this company has no rate for the activity of work center {code}: add a \
                 cost_activity_rate"
            )));
        };
        let hours = (setup + run * qty).round_dp(6);
        if hours.is_zero() {
            continue;
        }
        sqlx::query(
            "INSERT INTO production_order_operation (id, document_id, line_no, seq,
                 work_center_id, activity_id, description, hours, rate)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(new_id())
        .bind(document_id)
        .bind(index as i32 + 1)
        .bind(seq)
        .bind(center)
        .bind(activity)
        .bind(description)
        .bind(hours)
        .bind(rate)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

// ------------------------------------------------------------ record time

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordInput {
    /// Production order id.
    id: Uuid,
    /// The operation: its line number on the order.
    line_no: i32,
    /// Hours spent; below zero to correct an entry made before.
    hours: Decimal,
    /// The day the work was done. Today when omitted.
    worked_on: Option<NaiveDate>,
    /// What was done, for the record.
    note: Option<String>,
}

pub(crate) struct RecordTime;

#[async_trait]
impl Command for RecordTime {
    type Input = RecordInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "production_order.record_time",
            "Record the hours actually spent on an operation of a released production order. \
             When the order is completed, the difference between the standard hours and those \
             recorded, at the operation's rate, goes to the account for production_variance. \
             An operation with no time recorded counts as done in its standard time. A \
             mistake is corrected with hours below zero.",
        )
        .from_states(&["released"])
        .permission("production_order:record_time")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RecordInput) -> Result<DocumentView> {
        let doc = document::lock(ctx, PRODUCTION_ORDER, input.id, "record_time").await?;
        if doc.status != "released" {
            return Err(Error::state(format!(
                "time is recorded on a released production order, not a {} one",
                doc.status
            )));
        }
        if input.hours.is_zero() || input.hours.normalize().scale() > 6 {
            return Err(Error::field(
                "hours",
                "give the hours, not zero, with at most 6 decimal places",
            ));
        }
        let operation: Option<(Uuid, Decimal)> = sqlx::query_as(
            "SELECT o.id, COALESCE((SELECT sum(t.hours) FROM production_operation_time t
                                    WHERE t.operation_id = o.id), 0)
             FROM production_order_operation o WHERE o.document_id = $1 AND o.line_no = $2",
        )
        .bind(doc.id)
        .bind(input.line_no)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let (operation, recorded) = operation.ok_or_else(|| {
            Error::field(
                "line_no",
                "the order has no operation with this line number",
            )
        })?;
        if recorded + input.hours < Decimal::ZERO {
            return Err(Error::field(
                "hours",
                format!(
                    "only {} hours are recorded on this operation",
                    recorded.normalize()
                ),
            ));
        }
        sqlx::query(
            "INSERT INTO production_operation_time (id, operation_id, hours, worked_on, note,
                 recorded_by)
             VALUES ($1, $2, $3, $4, NULLIF(trim($5), ''), $6)",
        )
        .bind(new_id())
        .bind(operation)
        .bind(input.hours)
        .bind(
            input
                .worked_on
                .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive()),
        )
        .bind(&input.note)
        .bind(ctx.actor.user_id)
        .execute(&mut *ctx.conn)
        .await?;
        ctx.touch("document", doc.id);
        document::view(ctx, doc).await
    }
}

// ----------------------------------------------------------- capacity load

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoadInput {
    /// The company whose work centers are looked at.
    company_id: Uuid,
    /// The first day.
    from: NaiveDate,
    /// The last day.
    to: NaiveDate,
    /// Only this work center.
    work_center_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct LoadRow {
    work_center_id: Uuid,
    code: String,
    day: NaiveDate,
    /// What it can work that day.
    capacity_hours: Decimal,
    /// The standard hours of released orders dated that day, less what is
    /// already recorded on them.
    load_hours: Decimal,
    /// Load as a percentage of capacity.
    load_percent: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LoadOutput {
    company_id: Uuid,
    rows: Vec<LoadRow>,
}

pub(crate) struct Load;

#[async_trait]
impl Command for Load {
    type Input = LoadInput;
    type Output = LoadOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "work_center.load",
            "How full each active work center of a company is, day by day: its capacity in \
             hours beside the standard hours still to work on released production orders \
             dated that day (an order is planned on its date), and the load as a percentage. \
             Days with nothing planned are listed with no load. At most 366 days.",
        )
        .report("Work center load")
        .permission("production_order:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: LoadInput) -> Result<LoadOutput> {
        ctx.require("production_order:read", input.company_id)?;
        if input.to < input.from || (input.to - input.from).num_days() > 365 {
            return Err(Error::field(
                "to",
                "the last day is on or after the first, at most a year later",
            ));
        }
        let rows = sqlx::query_as(
            "WITH days AS (SELECT d::date AS day FROM generate_series($2::date, $3::date,
                                                                      '1 day') AS d),
                  planned AS (
                    SELECT o.work_center_id, d.doc_date AS day,
                           sum(GREATEST(o.hours - COALESCE((
                               SELECT sum(t.hours) FROM production_operation_time t
                               WHERE t.operation_id = o.id), 0), 0)) AS hours
                    FROM production_order_operation o
                    JOIN document d ON d.id = o.document_id
                    WHERE d.company_id = $1 AND d.status = 'released'
                      AND d.doc_date BETWEEN $2 AND $3
                    GROUP BY o.work_center_id, d.doc_date)
             SELECT w.id AS work_center_id, w.code, days.day,
                    w.hours_per_day AS capacity_hours,
                    trim_scale(COALESCE(p.hours, 0)) AS load_hours,
                    round(COALESCE(p.hours, 0) * 100 / w.hours_per_day, 1) AS load_percent
             FROM work_center w CROSS JOIN days
             LEFT JOIN planned p ON p.work_center_id = w.id AND p.day = days.day
             WHERE w.company_id = $1 AND w.active
               AND ($4::uuid IS NULL OR w.id = $4)
             ORDER BY w.code, days.day",
        )
        .bind(input.company_id)
        .bind(input.from)
        .bind(input.to)
        .bind(input.work_center_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(LoadOutput {
            company_id: input.company_id,
            rows,
        })
    }
}
