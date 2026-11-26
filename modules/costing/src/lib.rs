//! Standard costing: cost versions.
//!
//! A cost version (`cost_version`, a document per company) holds a
//! standard cost per item, with its material, labour and overhead parts.
//! `cost_version.rollup` works the parts of made items out from their bill
//! of materials and routing, level by level, refusing a bill that leads
//! back to itself. `cost_version.activate` writes the standards and, in one
//! posting dated the activation day, revalues the stock on hand of every
//! item kept at standard to its new standard: the stock ledger's revaluation
//! entries, and in the general ledger each level's inventory account against
//! the account rule `standard_cost_revaluation`. The stock ledger values
//! standard items at their standard from then on (see the stock crate).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DynCommand, Error, FieldMeta,
    FieldType, JsonSchema, Migration, Module, PgConnection, Result, StateMeta, async_trait,
    command, company, new_id, schemars,
};
use erpcraft_stock::costing::{self, Method, Standard};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const COST_VERSION: &str = "cost_version";
/// The account purpose of a revaluation to a new standard.
pub const REVALUATION: &str = "standard_cost_revaluation";

pub struct CostingModule;

impl Module for CostingModule {
    fn name(&self) -> &'static str {
        "costing"
    }

    fn label(&self) -> String {
        "Standard costing".into()
    }

    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("de", include_str!("../i18n/de.json")),
            ("fr", include_str!("../i18n/fr.json")),
        ]
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "stock", "manufacturing"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "costing",
            sql: include_str!("../migrations/0001_costing.sql"),
        }]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(CostVersion)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Rollup), command(Activate)]
    }

    fn reversal_hooks(&self) -> Vec<Arc<dyn document::ReversalHook>> {
        vec![Arc::new(StandardReversal)]
    }
}

/// The general ledger lines of a change in the value of stock at
/// standard, level by level: its inventory account (or the one for goods
/// in transit) by `amount`, against the account rule
/// standard_cost_revaluation.
async fn revaluation_lines(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    moved: &[(Uuid, Uuid, Decimal)],
) -> Result<Vec<GlLine>> {
    let places =
        erpcraft_stock::locations(&mut *ctx.conn, moved.iter().map(|(_, w, _)| *w)).await?;
    let mut gl = vec![];
    for (item_id, warehouse_id, amount) in moved {
        let transit: bool = sqlx::query_scalar(
            "SELECT COALESCE(bool_or(transit), false) FROM warehouse WHERE id = $1",
        )
        .bind(warehouse_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let about = [("item", *item_id), ("warehouse", *warehouse_id)];
        let inventory = erpcraft_gl::account_for(
            &mut *ctx.conn,
            company_id,
            if transit {
                "inventory_in_transit"
            } else {
                "inventory"
            },
            &about,
        )
        .await?;
        let against =
            erpcraft_gl::account_for(&mut *ctx.conn, company_id, REVALUATION, &about).await?;
        // Each line is about its level's place and item.
        let about = places.scopes(*warehouse_id, Some(*item_id));
        let (debit, credit) = ((*amount).max(Decimal::ZERO), (-*amount).max(Decimal::ZERO));
        gl.push(
            GlLine {
                account_id: inventory,
                debit,
                credit,
                ..GlLine::default()
            }
            .about(&about),
        );
        gl.push(
            GlLine {
                account_id: against,
                debit: credit,
                credit: debit,
                ..GlLine::default()
            }
            .about(&about),
        );
    }
    Ok(gl)
}

/// A movement of an item now at standard is reversed at its standard (the
/// stock ledger, stock 15), and a cost adjustment made while it was at
/// average takes nothing back off its stock (no entry is written for it).
/// The reversal's general ledger
/// lines mirror the original's, so they differ from the stock by as much:
/// that difference is posted beside the reversal, on its document and
/// date, between the inventory account and standard_cost_revaluation.
struct StandardReversal;

#[async_trait]
impl document::ReversalHook for StandardReversal {
    async fn after_reverse(
        &self,
        ctx: &mut Ctx<'_>,
        original: &erpcraft_kernel::Posting,
        reversal: &erpcraft_kernel::Posting,
    ) -> Result<()> {
        let moved: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
            "SELECT o.item_id, o.warehouse_id, sum(COALESCE(r.value, 0) + o.value)
             FROM stock_entry o
             LEFT JOIN stock_entry r ON r.reverses_id = o.id AND r.posting_id = $2
             WHERE o.posting_id = $1
             GROUP BY o.item_id, o.warehouse_id
             HAVING sum(COALESCE(r.value, 0) + o.value) <> 0
             ORDER BY o.item_id, o.warehouse_id",
        )
        .bind(original.id)
        .bind(reversal.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        if moved.is_empty() {
            return Ok(());
        }
        let doc: Document = sqlx::query_as("SELECT * FROM document WHERE id = $1")
            .bind(reversal.document_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
        let memo = Some(match &doc.number {
            Some(n) => format!("Standard cost difference of reversing {n}"),
            None => "Standard cost difference of a reversal".to_owned(),
        });
        let posting = document::create_posting(ctx, &doc, reversal.posting_date, memo).await?;
        let gl = revaluation_lines(ctx, doc.company_id, &moved).await?;
        erpcraft_gl::post(ctx, &posting, &gl).await
    }
}

// ------------------------------------------------------------ the document

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    #[serde(default)]
    lines: Vec<LineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    id: Option<Uuid>,
    item_id: Uuid,
    unit_cost: Option<Decimal>,
    #[serde(default)]
    material_cost: Decimal,
    #[serde(default)]
    labour_cost: Decimal,
    #[serde(default)]
    overhead_cost: Decimal,
    #[serde(default)]
    switch_to_standard: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct Line {
    id: Uuid,
    line_no: i32,
    item_id: Uuid,
    unit_cost: Decimal,
    material_cost: Decimal,
    labour_cost: Decimal,
    overhead_cost: Decimal,
    rolled_up: bool,
    switch_to_standard: bool,
}

impl Line {
    fn standard(&self) -> Standard {
        Standard {
            unit_cost: self.unit_cost,
            material_cost: self.material_cost,
            labour_cost: self.labour_cost,
            overhead_cost: self.overhead_cost,
        }
    }
}

async fn lines(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, item_id, unit_cost, material_cost, labour_cost, overhead_cost,
                rolled_up, switch_to_standard
         FROM cost_version_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Replace a version's lines.
async fn write_lines(conn: &mut PgConnection, id: Uuid, rows: &[Line]) -> Result<()> {
    sqlx::query("DELETE FROM cost_version_line WHERE document_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    for (n, line) in rows.iter().enumerate() {
        sqlx::query(
            "INSERT INTO cost_version_line (id, document_id, line_no, item_id, unit_cost,
                 material_cost, labour_cost, overhead_cost, rolled_up, switch_to_standard)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(line.id)
        .bind(id)
        .bind(n as i32 + 1)
        .bind(line.item_id)
        .bind(line.unit_cost)
        .bind(line.material_cost)
        .bind(line.labour_cost)
        .bind(line.overhead_cost)
        .bind(line.rolled_up)
        .bind(line.switch_to_standard)
        .execute(&mut *conn)
        .await
        .map_err(|e| match Error::from(e) {
            Error::Conflict(_) => Error::line(
                "lines",
                n + 1,
                Some("item_id"),
                "the item is in the version twice",
            ),
            other => other,
        })?;
    }
    Ok(())
}

struct CostVersion;

#[async_trait]
impl DocumentHandler for CostVersion {
    fn meta(&self) -> DocTypeMeta {
        let cost =
            |name: &str, label: &str| FieldMeta::new(name, label, FieldType::Decimal).unit_price();
        DocTypeMeta {
            name: COST_VERSION.into(),
            module: String::new(),
            label: "Cost version".into(),
            plural: "Cost versions".into(),
            number_prefix: "CV".into(),
            fields: vec![],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Standard costs".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("item_id", "Item", FieldType::link("item")).required(),
                    cost("unit_cost", "Standard cost").help(
                        "Per unit of the item's stock unit, in the company's accounting \
                         currency. Left out: the sum of the parts.",
                    ),
                    cost("material_cost", "Material"),
                    cost("labour_cost", "Labour"),
                    cost("overhead_cost", "Overhead"),
                    FieldMeta::new("rolled_up", "Rolled up", FieldType::Bool)
                        .readonly()
                        .help("Worked out from the item's bill of materials and routing."),
                    FieldMeta::new("switch_to_standard", "Switch to standard", FieldType::Bool)
                        .help(
                            "Value the item at standard from activation on (its stock on hand \
                             revalued), where it is at moving average now.",
                        ),
                ],
            }],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("active", "Active"),
            ],
            initial_state: "draft".into(),
            transitions: vec![("draft".into(), "active".into())],
            summary: vec![FieldMeta::new("items", "Items", FieldType::Int).readonly()],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "cost version body")?;
        let ids = document::line_ids(
            &mut *ctx.conn,
            "cost_version_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        let mut rows = vec![];
        for (index, (line, id)) in body.lines.iter().zip(ids).enumerate() {
            let parts = line.material_cost + line.labour_cost + line.overhead_cost;
            let unit_cost = line.unit_cost.unwrap_or(parts);
            for value in [
                unit_cost,
                line.material_cost,
                line.labour_cost,
                line.overhead_cost,
            ] {
                if value.is_sign_negative() {
                    return Err(Error::line(
                        "lines",
                        index + 1,
                        Some("unit_cost"),
                        "a cost is zero or more",
                    ));
                }
            }
            rows.push(Line {
                id,
                line_no: index as i32 + 1,
                item_id: line.item_id,
                unit_cost,
                material_cost: line.material_cost,
                labour_cost: line.labour_cost,
                overhead_cost: line.overhead_cost,
                rolled_up: false,
                switch_to_standard: line.switch_to_standard,
            });
        }
        write_lines(&mut *ctx.conn, doc.id, &rows).await
    }

    async fn summary_of(
        &self,
        _conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        Ok(json!({ "items": document::body::lines(body, "lines").len() }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        Ok(json!({ "lines": lines(conn, doc.id).await? }))
    }
}

// ------------------------------------------------------------ rollup

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RollupInput {
    /// Cost version id, a draft.
    id: Uuid,
    /// Items to roll up besides those on the version: made items get a
    /// line, worked out from their bill.
    #[serde(default)]
    item_ids: Vec<Uuid>,
}

/// What one unit of a made item takes, from its active bill.
struct Bill {
    output_qty: Decimal,
    components: Vec<(Uuid, Decimal)>,
    /// Labour: the routing's hours per order of output_qty, at the work
    /// center's rate; and the bill's activities by kind.
    labour: Decimal,
    overhead: Decimal,
}

async fn bill_of(conn: &mut PgConnection, company_id: Uuid, item_id: Uuid) -> Result<Option<Bill>> {
    // Which bill a made item is made by is not the newest one's to say:
    // with more than one active, the rollup cannot know and refuses.
    let boms: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "SELECT id, output_qty FROM bom WHERE item_id = $1 AND active ORDER BY created_at, id",
    )
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await?;
    if boms.len() > 1 {
        let code = item_code(conn, item_id).await?;
        return Err(Error::validation(format!(
            "item {code} has {} active bills of materials: deactivate all but the one it is \
             made by, or give its cost in the version",
            boms.len()
        )));
    }
    let Some((bom, output_qty)) = boms.into_iter().next() else {
        return Ok(None);
    };
    let components: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "SELECT item_id, qty FROM bom_line WHERE bom_id = $1 ORDER BY created_at, id",
    )
    .bind(bom)
    .fetch_all(&mut *conn)
    .await?;
    // Routing: setup once per order of output_qty, run per unit.
    let routing: Option<Decimal> = sqlx::query_scalar(
        "SELECT sum((o.setup_hours + o.run_hours * $3) * COALESCE(r.rate, 0))
         FROM bom_operation o
         LEFT JOIN work_center w ON w.company_id = $2 AND w.code = o.work_center_code
         LEFT JOIN cost_activity_rate r ON r.company_id = $2 AND r.activity_id = w.activity_id
         WHERE o.bom_id = $1",
    )
    .bind(bom)
    .bind(company_id)
    .bind(output_qty)
    .fetch_one(&mut *conn)
    .await?;
    let activities: Vec<(String, Decimal)> = sqlx::query_as(
        "SELECT c.kind, sum(a.qty * COALESCE(r.rate, 0))
         FROM bom_activity a JOIN cost_activity c ON c.id = a.activity_id
         LEFT JOIN cost_activity_rate r ON r.company_id = $2 AND r.activity_id = a.activity_id
         WHERE a.bom_id = $1 GROUP BY c.kind",
    )
    .bind(bom)
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut labour = routing.unwrap_or_default();
    let mut overhead = Decimal::ZERO;
    for (kind, amount) in activities {
        if kind == "overhead" {
            overhead += amount;
        } else {
            labour += amount;
        }
    }
    Ok(Some(Bill {
        output_qty,
        components,
        labour,
        overhead,
    }))
}

/// Works out standards, bottom up, remembering each item's.
struct Roller<'a> {
    company_id: Uuid,
    /// What the version says of items it holds and that are not made.
    given: &'a HashMap<Uuid, Standard>,
    done: HashMap<Uuid, (Standard, bool)>,
    path: Vec<Uuid>,
    unknown: Vec<Uuid>,
}

impl Roller<'_> {
    /// The standard of `item`: rolled up from its bill when it has one
    /// (true), else the version's, the current standard, or the average
    /// cost in the company.
    async fn cost(&mut self, conn: &mut PgConnection, item: Uuid) -> Result<(Standard, bool)> {
        if let Some(done) = self.done.get(&item) {
            return Ok(*done);
        }
        if self.path.contains(&item) {
            let mut codes = vec![];
            for id in self.path.iter().skip_while(|i| **i != item).chain([&item]) {
                codes.push(item_code(conn, *id).await?);
            }
            let path = codes.join(" -> ");
            return Err(Error::validation(format!(
                "the bills of materials lead back to themselves: {path}"
            )));
        }
        let result = match bill_of(conn, self.company_id, item).await? {
            Some(bill) => {
                self.path.push(item);
                let mut material = Decimal::ZERO;
                for (component, qty) in &bill.components {
                    let (standard, _) = Box::pin(self.cost(conn, *component)).await?;
                    material += standard.unit_cost * qty;
                }
                self.path.pop();
                let per = |amount: Decimal| (amount / bill.output_qty).round_dp(6);
                let (material_cost, labour_cost, overhead_cost) =
                    (per(material), per(bill.labour), per(bill.overhead));
                (
                    Standard {
                        unit_cost: material_cost + labour_cost + overhead_cost,
                        material_cost,
                        labour_cost,
                        overhead_cost,
                    },
                    true,
                )
            }
            None => {
                let standard = match self.given.get(&item) {
                    Some(given) => *given,
                    None => {
                        // An item at standard costs its standard. One at
                        // average costs its average, which a standard kept
                        // from when it was at standard does not replace;
                        // that is only the fallback when none is on hand.
                        let at_standard =
                            costing::method(conn, self.company_id, item).await? == Method::Standard;
                        let current = costing::standard(conn, self.company_id, item).await?;
                        let average: Option<Decimal> = if at_standard && current.is_some() {
                            None
                        } else {
                            sqlx::query_scalar(
                                "SELECT CASE WHEN sum(qty) > 0
                                        THEN round(sum(value) / sum(qty), 6) END
                                 FROM stock_level WHERE company_id = $1 AND item_id = $2",
                            )
                            .bind(self.company_id)
                            .bind(item)
                            .fetch_one(&mut *conn)
                            .await?
                        };
                        match (at_standard, current, average) {
                            (true, Some(current), _) => current,
                            (_, _, Some(unit_cost)) => Standard {
                                unit_cost,
                                material_cost: unit_cost,
                                ..Standard::default()
                            },
                            (_, Some(current), None) => current,
                            (_, None, None) => {
                                self.unknown.push(item);
                                Standard::default()
                            }
                        }
                    }
                };
                (standard, false)
            }
        };
        self.done.insert(item, result);
        Ok(result)
    }
}

async fn item_code(conn: &mut PgConnection, item: Uuid) -> Result<String> {
    Ok(sqlx::query_scalar("SELECT code FROM item WHERE id = $1")
        .bind(item)
        .fetch_one(&mut *conn)
        .await?)
}

#[derive(Serialize, JsonSchema)]
struct RollupOutput {
    version: DocumentView,
    /// Bought items with no cost anywhere (not on the version, no standard,
    /// no stock): rolled up at nothing. Give them a line.
    without_cost: Vec<String>,
}

struct Rollup;

#[async_trait]
impl Command for Rollup {
    type Input = RollupInput;
    type Output = RollupOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "cost_version.rollup",
            "Roll up a draft cost version: every made item on it (and in item_ids, and every \
             made component under them) gets its standard from its active bill of materials \
             and routing, level by level from the bottom: material is the components' \
             standards (this version's line for a bought item, else its current standard, else \
             its average cost in the company), labour the routing's hours at the work center's \
             rate with the bill's labour and machine activities, overhead its overhead \
             activities, each per unit of output. A bill that leads back to itself is refused, \
             naming the loop. Bought items keep the cost the version gives them.",
        )
        .permission("cost_version:update")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RollupInput) -> Result<RollupOutput> {
        let doc = document::lock(ctx, COST_VERSION, input.id, "update").await?;
        if doc.status != "draft" {
            return Err(Error::state("only a draft cost version is rolled up"));
        }
        let mut rows = lines(&mut *ctx.conn, doc.id).await?;
        let given: HashMap<Uuid, Standard> = rows
            .iter()
            .filter(|l| !l.rolled_up)
            .map(|l| (l.item_id, l.standard()))
            .collect();
        let mut roller = Roller {
            company_id: doc.company_id,
            given: &given,
            done: HashMap::new(),
            path: vec![],
            unknown: vec![],
        };
        let mut wanted: Vec<Uuid> = rows.iter().map(|l| l.item_id).collect();
        wanted.extend(&input.item_ids);
        for item in &wanted {
            roller.cost(&mut *ctx.conn, *item).await?;
        }
        let results: BTreeMap<Uuid, (Standard, bool)> = roller.done.clone().into_iter().collect();
        let unknown = roller.unknown.clone();
        // Made items get their rolled up line; bought ones keep theirs.
        for (item, (standard, made)) in &results {
            if !made {
                continue;
            }
            match rows.iter_mut().find(|l| l.item_id == *item) {
                Some(line) => {
                    line.unit_cost = standard.unit_cost;
                    line.material_cost = standard.material_cost;
                    line.labour_cost = standard.labour_cost;
                    line.overhead_cost = standard.overhead_cost;
                    line.rolled_up = true;
                }
                None => rows.push(Line {
                    id: new_id(),
                    line_no: 0,
                    item_id: *item,
                    unit_cost: standard.unit_cost,
                    material_cost: standard.material_cost,
                    labour_cost: standard.labour_cost,
                    overhead_cost: standard.overhead_cost,
                    rolled_up: true,
                    switch_to_standard: false,
                }),
            }
        }
        write_lines(&mut *ctx.conn, doc.id, &rows).await?;
        let mut without_cost = vec![];
        for item in unknown {
            without_cost.push(item_code(&mut *ctx.conn, item).await?);
        }
        Ok(RollupOutput {
            version: document::view(ctx, doc).await?,
            without_cost,
        })
    }
}

// ------------------------------------------------------------ activation

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ActivateInput {
    /// Cost version id, a draft.
    id: Uuid,
    /// The day the standards apply from, and the revaluation's date. The
    /// version's date when omitted.
    date: Option<NaiveDate>,
}

struct Activate;

#[async_trait]
impl Command for Activate {
    type Input = ActivateInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "cost_version.activate",
            "Activate a cost version: its lines become the items' standard costs in the \
             company, items marked switch_to_standard move to standard cost, and the stock on \
             hand of every item kept at standard is revalued to its new standard, level by \
             level, in one posting on the activation date: the inventory account (or the one \
             for goods in transit) against the account rule standard_cost_revaluation. Final: \
             a later version replaces it.",
        )
        .permission("cost_version:activate")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ActivateInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, COST_VERSION, input.id, "activate").await?;
        if doc.status == "active" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state("only a draft cost version is activated"));
        }
        let rows = lines(&mut *ctx.conn, doc.id).await?;
        if rows.is_empty() {
            return Err(Error::validation("a cost version needs at least one line"));
        }
        let date = input.date.unwrap_or(doc.doc_date);
        let decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        // Every item of the version, exclusively and in one order: movements
        // of them wait (they take the item shared, stock 15), so none
        // commits between the revaluation and the new standard, and two
        // activations never deadlock over items listed in other orders.
        let mut items: Vec<Uuid> = rows.iter().map(|l| l.item_id).collect();
        items.sort();
        items.dedup();
        sqlx::query("SELECT 1 FROM item WHERE id = ANY($1) ORDER BY id FOR NO KEY UPDATE")
            .bind(&items)
            .execute(&mut *ctx.conn)
            .await?;
        costing::begin_activation(&mut *ctx.conn).await?;
        let mut revaluations = vec![];
        for line in &rows {
            if line.switch_to_standard {
                costing::set_method(
                    &mut *ctx.conn,
                    doc.company_id,
                    line.item_id,
                    Method::Standard,
                )
                .await?;
            }
            costing::set_standard(
                &mut *ctx.conn,
                doc.company_id,
                line.item_id,
                &line.standard(),
                doc.id,
                date,
            )
            .await?;
            if costing::method(&mut *ctx.conn, doc.company_id, line.item_id).await?
                == Method::Standard
            {
                revaluations.extend(
                    costing::levels_to_revalue(
                        &mut *ctx.conn,
                        doc.company_id,
                        line.item_id,
                        line.unit_cost,
                        decimals,
                    )
                    .await?,
                );
            }
        }
        if revaluations.iter().any(|r| r.new_value != r.value) {
            let memo = doc
                .number
                .as_ref()
                .map(|n| format!("New standard costs {n}"));
            let posting = document::create_posting(ctx, &doc, date, memo).await?;
            let entries = costing::revalue(&mut *ctx.conn, &posting, &revaluations).await?;
            let moved: Vec<(Uuid, Uuid, Decimal)> = entries
                .iter()
                .map(|e| (e.item_id, e.warehouse_id, e.value))
                .collect();
            let gl = revaluation_lines(ctx, doc.company_id, &moved).await?;
            erpcraft_gl::post(ctx, &posting, &gl).await?;
        }
        document::transition(ctx, &mut doc, "active").await?;
        document::view(ctx, doc).await
    }
}
