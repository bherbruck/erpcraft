//! Sales and purchase analysis, and the top lists made of them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, company, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::common::{self, Compare, Period};

/// What the rows of an analysis are per.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Key {
    Customer,
    Vendor,
    Item,
    /// The item's price group of kind item.
    ItemGroup,
    /// The period of `period`.
    Period,
    /// The value of `group_by_dimension_id`.
    Dimension,
}

/// What a top list or an analysis is sorted by, largest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Sort {
    #[default]
    Net,
    Margin,
    MarginPercent,
    Qty,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Sales,
    Purchases,
}

/// One document line in one posting: the posting's date, and what the
/// line came to in it, with the sign of the posting (a reversal takes it
/// back on its own date).
#[derive(Debug, sqlx::FromRow)]
struct Fact {
    document_id: Uuid,
    posting_date: NaiveDate,
    party_id: Option<Uuid>,
    item_id: Option<Uuid>,
    qty: Decimal,
    net: Decimal,
    net_document: Decimal,
    currency: String,
    cost: Option<Decimal>,
    set_id: Option<Uuid>,
}

/// The posted lines of the documents of `types`, in postings dated within
/// `from..=to`. Amounts as the ledger has them; see the module's notes.
const SALES_FACTS: &str = "
WITH p AS (
    SELECT p.id, p.document_id, p.posting_date, COALESCE(p.reverses_id, p.id) AS original,
           CASE WHEN p.reverses_id IS NULL THEN 1 ELSE -1 END AS dir
    FROM posting p JOIN document d ON d.id = p.document_id
    WHERE p.company_id = $1 AND p.posting_date BETWEEN $2 AND $3 AND d.doc_type = ANY ($4)
),
l AS (
    SELECT il.document_id, il.id AS line_id, ol.item_id,
           COALESCE(o.customer_id, si.customer_id) AS party_id,
           CASE WHEN ol.item_id IS NULL THEN 0 ELSE il.qty * COALESCE(ol.factor, 1) END AS qty,
           COALESCE(il.net_amount, 0) AS net_document,
           COALESCE(o.currency, si.currency, co.base_currency) AS currency,
           il.order_line_id, 1 AS sign
    FROM sales_invoice_line il
    JOIN sales_invoice si ON si.document_id = il.document_id
    JOIN (SELECT DISTINCT document_id FROM p) pd ON pd.document_id = il.document_id
    JOIN document d ON d.id = il.document_id JOIN company co ON co.id = d.company_id
    LEFT JOIN sales_order_line ol ON ol.id = il.order_line_id
    LEFT JOIN sales_order o ON o.document_id = COALESCE(si.sales_order_id, ol.document_id)
    UNION ALL
    SELECT cl.document_id, cl.id, ol.item_id, COALESCE(o.customer_id, si.customer_id),
           CASE WHEN ol.item_id IS NULL THEN 0
                ELSE COALESCE(cl.returned_qty, 0) * COALESCE(ol.factor, 1) END,
           COALESCE(cl.net_amount, 0), COALESCE(o.currency, si.currency, co.base_currency),
           il.order_line_id, -1
    FROM sales_credit_note_line cl
    JOIN (SELECT DISTINCT document_id FROM p) pd ON pd.document_id = cl.document_id
    JOIN sales_invoice_line il ON il.id = cl.invoice_line_id
    JOIN sales_invoice si ON si.document_id = il.document_id
    JOIN document d ON d.id = cl.document_id JOIN company co ON co.id = d.company_id
    LEFT JOIN sales_order_line ol ON ol.id = il.order_line_id
    LEFT JOIN sales_order o ON o.document_id = COALESCE(si.sales_order_id, ol.document_id)
    UNION ALL
    SELECT cl.document_id, cl.id, NULL, sc.customer_id, 0, COALESCE(cl.net_amount, 0),
           COALESCE(sc.currency, co.base_currency), NULL, -1
    FROM sales_credit_line cl
    JOIN (SELECT DISTINCT document_id FROM p) pd ON pd.document_id = cl.document_id
    JOIN sales_credit sc ON sc.document_id = cl.document_id
    JOIN document d ON d.id = cl.document_id JOIN company co ON co.id = d.company_id
),
a AS (
    SELECT g.posting_id, g.source_line_id, sum(g.credit - g.debit) AS net,
           max(g.dimension_set_id::text)::uuid AS set_id
    FROM gl_entry g JOIN p ON p.id = g.posting_id
    WHERE g.source_line_id IS NOT NULL
      AND g.account_id NOT IN (SELECT o.account_id FROM open_item o WHERE o.posting_id = p.original)
      AND g.account_id NOT IN (SELECT t.account_id FROM tax_entry t
                               WHERE t.posting_id = p.original AND t.account_id IS NOT NULL)
    GROUP BY g.posting_id, g.source_line_id
),
u AS (
    SELECT x.order_line_id, sum(-se.value) / NULLIF(sum(-se.qty), 0) AS unit_cost
    FROM (SELECT id, order_line_id FROM sales_delivery_line
          UNION ALL SELECT id, order_line_id FROM sales_return_line) x
    JOIN stock_entry se ON se.source_line_id = x.id
    WHERE x.order_line_id IN (SELECT order_line_id FROM l WHERE order_line_id IS NOT NULL)
    GROUP BY x.order_line_id
)
SELECT p.document_id, p.posting_date, l.party_id, l.item_id,
       l.qty * l.sign * p.dir AS qty,
       COALESCE(a.net, 0) AS net,
       l.net_document * l.sign * p.dir AS net_document,
       l.currency,
       CASE WHEN l.item_id IS NULL THEN 0
            ELSE round(COALESCE(u.unit_cost, 0) * l.qty, 4) * l.sign * p.dir END AS cost,
       a.set_id
FROM p JOIN l ON l.document_id = p.document_id
LEFT JOIN a ON a.posting_id = p.id AND a.source_line_id = l.line_id
LEFT JOIN u ON u.order_line_id = l.order_line_id";

const PURCHASE_FACTS: &str = "
WITH p AS (
    SELECT p.id, p.document_id, p.posting_date, COALESCE(p.reverses_id, p.id) AS original,
           CASE WHEN p.reverses_id IS NULL THEN 1 ELSE -1 END AS dir
    FROM posting p JOIN document d ON d.id = p.document_id
    WHERE p.company_id = $1 AND p.posting_date BETWEEN $2 AND $3 AND d.doc_type = ANY ($4)
),
l AS (
    SELECT vl.document_id, vl.id AS line_id, pl.item_id,
           COALESCE(po.vendor_id, vi.vendor_id) AS party_id,
           CASE WHEN pl.item_id IS NULL THEN 0 ELSE vl.qty * COALESCE(pl.factor, 1) END AS qty,
           COALESCE(vl.net_amount, 0) AS net_document,
           COALESCE(po.currency, vi.currency, co.base_currency) AS currency, 1 AS sign
    FROM vendor_invoice_line vl
    JOIN (SELECT DISTINCT document_id FROM p) pd ON pd.document_id = vl.document_id
    JOIN vendor_invoice vi ON vi.document_id = vl.document_id
    JOIN document d ON d.id = vl.document_id JOIN company co ON co.id = d.company_id
    LEFT JOIN purchase_order_line pl ON pl.id = vl.order_line_id
    LEFT JOIN purchase_order po ON po.document_id = COALESCE(vi.purchase_order_id, pl.document_id)
    UNION ALL
    SELECT cl.document_id, cl.id, pl.item_id, COALESCE(po.vendor_id, vi.vendor_id),
           CASE WHEN pl.item_id IS NULL THEN 0
                ELSE COALESCE(cl.returned_qty, 0) * COALESCE(pl.factor, 1) END,
           COALESCE(cl.net_amount, 0), COALESCE(po.currency, vi.currency, co.base_currency), -1
    FROM vendor_credit_note_line cl
    JOIN (SELECT DISTINCT document_id FROM p) pd ON pd.document_id = cl.document_id
    JOIN vendor_invoice_line vl ON vl.id = cl.invoice_line_id
    JOIN vendor_invoice vi ON vi.document_id = vl.document_id
    JOIN document d ON d.id = cl.document_id JOIN company co ON co.id = d.company_id
    LEFT JOIN purchase_order_line pl ON pl.id = vl.order_line_id
    LEFT JOIN purchase_order po ON po.document_id = COALESCE(vi.purchase_order_id, pl.document_id)
    UNION ALL
    SELECT cl.document_id, cl.id, NULL, vc.vendor_id, 0, COALESCE(cl.net_amount, 0),
           COALESCE(vc.currency, co.base_currency), -1
    FROM vendor_credit_line cl
    JOIN (SELECT DISTINCT document_id FROM p) pd ON pd.document_id = cl.document_id
    JOIN vendor_credit vc ON vc.document_id = cl.document_id
    JOIN document d ON d.id = cl.document_id JOIN company co ON co.id = d.company_id
),
a AS (
    SELECT g.posting_id, g.source_line_id, sum(g.debit - g.credit) AS net,
           max(g.dimension_set_id::text)::uuid AS set_id
    FROM gl_entry g JOIN p ON p.id = g.posting_id
    WHERE g.source_line_id IS NOT NULL
      AND g.account_id NOT IN (SELECT o.account_id FROM open_item o WHERE o.posting_id = p.original)
      AND g.account_id NOT IN (SELECT t.account_id FROM tax_entry t
                               WHERE t.posting_id = p.original AND t.account_id IS NOT NULL)
    GROUP BY g.posting_id, g.source_line_id
)
SELECT p.document_id, p.posting_date, l.party_id, l.item_id,
       l.qty * l.sign * p.dir AS qty,
       COALESCE(a.net, 0) AS net,
       l.net_document * l.sign * p.dir AS net_document,
       l.currency,
       NULL::numeric AS cost,
       a.set_id
FROM p JOIN l ON l.document_id = p.document_id
LEFT JOIN a ON a.posting_id = p.id AND a.source_line_id = l.line_id";

/// What an analysis asks for, whichever side.
pub(crate) struct Ask {
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    group_by: Vec<Key>,
    period: Period,
    compare: Option<Compare>,
    dimension_value_ids: Vec<Uuid>,
    group_by_dimension_id: Option<Uuid>,
    party_id: Option<Uuid>,
    item_id: Option<Uuid>,
    currency: Option<String>,
    sort: Sort,
    limit: Option<usize>,
}

#[derive(Default, Clone, Copy)]
struct Sums {
    qty: Decimal,
    net: Decimal,
    net_document: Decimal,
    cost: Decimal,
}

impl Sums {
    fn add(&mut self, f: &Fact) {
        self.qty += f.qty;
        self.net += f.net;
        self.net_document += f.net_document;
        self.cost += f.cost.unwrap_or_default();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
struct RowKey {
    party: Option<Uuid>,
    item: Option<Uuid>,
    group: Option<Uuid>,
    period: Option<NaiveDate>,
    value: Option<Uuid>,
}

/// One row of a sales or purchase analysis: what it is about, and what it
/// came to. Keys a report is not grouped by are empty.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub(crate) struct Row {
    /// The customer (sales) or vendor (purchases).
    party_id: Option<Uuid>,
    party_code: Option<String>,
    party_name: Option<String>,
    item_id: Option<Uuid>,
    item_code: Option<String>,
    item_name: Option<String>,
    item_group_id: Option<Uuid>,
    item_group_code: Option<String>,
    item_group_name: Option<String>,
    /// The first day of the period.
    period: Option<NaiveDate>,
    dimension_value_id: Option<Uuid>,
    dimension_value_code: Option<String>,
    dimension_value_name: Option<String>,
    /// In the item's stock unit.
    qty: Decimal,
    /// In the accounting currency, without tax.
    net: Decimal,
    /// With `currency`: what the documents came to in it.
    net_document: Option<Decimal>,
    /// Sales: the cost of the goods, as the stock ledger posted it.
    cost: Option<Decimal>,
    /// Sales: net minus cost.
    margin: Option<Decimal>,
    /// Sales: the margin in percent of net.
    margin_percent: Option<Decimal>,
    /// Purchases: net per stock unit.
    average_price: Option<Decimal>,
    compare_qty: Option<Decimal>,
    compare_net: Option<Decimal>,
    compare_margin: Option<Decimal>,
    /// Net against the compared range, in percent of it.
    net_change_percent: Option<Decimal>,
}

/// A sales or purchase analysis.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AnalysisOutput {
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    /// The accounting currency.
    currency: String,
    /// With `currency`: the documents' currency.
    document_currency: Option<String>,
    /// With `compare`: the range compared with.
    compare_from: Option<NaiveDate>,
    compare_to: Option<NaiveDate>,
    rows: Vec<Row>,
    /// Of every row, also those `limit` leaves out.
    totals: Row,
}

async fn facts(
    ctx: &mut Ctx<'_>,
    side: Side,
    ask: &Ask,
    types: &[&str],
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Fact>> {
    let sql = match side {
        Side::Sales => SALES_FACTS,
        Side::Purchases => PURCHASE_FACTS,
    };
    let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    let mut facts: Vec<Fact> = sqlx::query_as(sql)
        .bind(ask.company_id)
        .bind(from)
        .bind(to)
        .bind(&types)
        .fetch_all(&mut *ctx.conn)
        .await?;
    let readable =
        common::readable_documents(ctx, facts.iter().map(|f| f.document_id).collect()).await?;
    facts.retain(|f| readable.contains(&f.document_id));
    facts.retain(|f| {
        ask.party_id.is_none_or(|p| f.party_id == Some(p))
            && ask.item_id.is_none_or(|i| f.item_id == Some(i))
            && ask.currency.as_ref().is_none_or(|c| &f.currency == c)
    });
    let sets: BTreeSet<Uuid> = facts.iter().filter_map(|f| f.set_id).collect();
    if let Some(keep) = common::sets_with(ctx, &ask.dimension_value_ids, &sets).await? {
        facts.retain(|f| f.set_id.is_some_and(|s| keep.contains(&s)));
    }
    Ok(facts)
}

/// The document types of a side the reader may read, the first required.
fn types(ctx: &Ctx<'_>, side: Side) -> Result<Vec<&'static str>> {
    let all: &[&str] = match side {
        Side::Sales => &["sales_invoice", "sales_credit_note", "sales_credit"],
        Side::Purchases => &["vendor_invoice", "vendor_credit_note", "vendor_credit"],
    };
    // A right with a condition reaches some documents of the company:
    // which, each document's rules decide (`readable_documents`).
    ctx.actor.require_any(&format!("{}:read", all[0]))?;
    Ok(all
        .iter()
        .copied()
        .filter(|t| ctx.actor.can_any(&format!("{t}:read")))
        .collect())
}

async fn analyse(ctx: &mut Ctx<'_>, side: Side, ask: Ask) -> Result<AnalysisOutput> {
    common::check_range(ask.from, ask.to)?;
    erpcraft_gl::check_dimensions(
        &mut *ctx.conn,
        ask.company_id,
        &ask.dimension_value_ids,
        ask.group_by_dimension_id,
    )
    .await?;
    if ask.group_by.contains(&Key::Dimension) && ask.group_by_dimension_id.is_none() {
        return Err(
            Error::validation("grouping by dimension needs group_by_dimension_id")
                .with_field("group_by_dimension_id"),
        );
    }
    let types = types(ctx, side)?;
    let costed = side == Side::Sales && ctx.actor.can("stock_entry:read", ask.company_id);
    let base = company::base_currency(&mut *ctx.conn, ask.company_id).await?;
    let current = facts(ctx, side, &ask, &types, ask.from, ask.to).await?;
    let (compare_range, earlier) = match ask.compare {
        Some(compare) => {
            let (f, t) = common::compared(ask.from, ask.to, compare)?;
            let mut facts = facts(ctx, side, &ask, &types, f, t).await?;
            for fact in &mut facts {
                fact.posting_date =
                    common::moved_forward(fact.posting_date, ask.from, ask.to, compare);
            }
            (Some((f, t)), facts)
        }
        None => (None, vec![]),
    };

    // What the rows are per.
    let all: Vec<&Fact> = current.iter().chain(earlier.iter()).collect();
    let items: BTreeSet<Uuid> = all.iter().filter_map(|f| f.item_id).collect();
    let groups = if ask.group_by.contains(&Key::ItemGroup) {
        common::groups_of(ctx, &items).await?
    } else {
        HashMap::new()
    };
    let values = match ask.group_by_dimension_id {
        Some(d) if ask.group_by.contains(&Key::Dimension) => {
            let sets = all.iter().filter_map(|f| f.set_id).collect();
            common::values_of(ctx, d, &sets).await?
        }
        _ => HashMap::new(),
    };
    let by = |k: Key| ask.group_by.contains(&k);
    let key = |f: &Fact| RowKey {
        party: f.party_id.filter(|_| by(Key::Customer) || by(Key::Vendor)),
        item: f.item_id.filter(|_| by(Key::Item)),
        group: f
            .item_id
            .and_then(|i| groups.get(&i).copied())
            .filter(|_| by(Key::ItemGroup)),
        period: by(Key::Period).then(|| ask.period.start(f.posting_date)),
        value: f
            .set_id
            .and_then(|s| values.get(&s).copied())
            .filter(|_| by(Key::Dimension)),
    };
    let mut now: BTreeMap<RowKey, Sums> = BTreeMap::new();
    let mut before: BTreeMap<RowKey, Sums> = BTreeMap::new();
    let (mut total, mut total_before) = (Sums::default(), Sums::default());
    for f in &current {
        now.entry(key(f)).or_default().add(f);
        total.add(f);
    }
    for f in &earlier {
        before.entry(key(f)).or_default().add(f);
        total_before.add(f);
    }
    let keys: BTreeSet<RowKey> = now.keys().chain(before.keys()).copied().collect();

    let (party_entity, party_table) = match side {
        Side::Sales => ("customer", "customer"),
        Side::Purchases => ("vendor", "vendor"),
    };
    let parties = common::labels(
        ctx,
        party_entity,
        party_table,
        &keys.iter().filter_map(|k| k.party).collect(),
    )
    .await?;
    let item_labels = common::labels(
        ctx,
        "item",
        "item",
        &keys.iter().filter_map(|k| k.item).collect(),
    )
    .await?;
    let group_labels = common::labels(
        ctx,
        "price_group",
        "price_group",
        &keys.iter().filter_map(|k| k.group).collect(),
    )
    .await?;
    let value_labels = common::labels(
        ctx,
        "dimension_value",
        "dimension_value",
        &keys.iter().filter_map(|k| k.value).collect(),
    )
    .await?;
    let compared = ask.compare.is_some();
    let in_document = ask.currency.is_some();
    let measures = |row: &mut Row, now: Sums, before: Option<Sums>| {
        row.qty = now.qty.round_dp(6).normalize();
        row.net = now.net;
        row.net_document = in_document.then_some(now.net_document);
        if costed {
            row.cost = Some(now.cost);
            row.margin = Some(now.net - now.cost);
            row.margin_percent = common::percent(now.net - now.cost, now.net);
        }
        if side == Side::Purchases {
            row.average_price = common::ratio(now.net, now.qty);
        }
        if let Some(before) = before {
            row.compare_qty = Some(before.qty.round_dp(6).normalize());
            row.compare_net = Some(before.net);
            row.compare_margin = costed.then_some(before.net - before.cost);
            row.net_change_percent = common::percent(now.net - before.net, before.net);
        }
    };
    let mut rows: Vec<Row> = keys
        .into_iter()
        .map(|k| {
            let mut row = Row::default();
            (
                row.party_id,
                row.item_id,
                row.item_group_id,
                row.period,
                row.dimension_value_id,
            ) = (k.party, k.item, k.group, k.period, k.value);
            (row.party_code, row.party_name) = common::label(&parties, k.party);
            (row.item_code, row.item_name) = common::label(&item_labels, k.item);
            (row.item_group_code, row.item_group_name) = common::label(&group_labels, k.group);
            (row.dimension_value_code, row.dimension_value_name) =
                common::label(&value_labels, k.value);
            let before = compared.then(|| before.get(&k).copied().unwrap_or_default());
            measures(&mut row, now.get(&k).copied().unwrap_or_default(), before);
            row
        })
        .collect();
    let value = |r: &Row| match ask.sort {
        Sort::Net => r.net,
        Sort::Margin => r.margin.unwrap_or(r.net),
        Sort::MarginPercent => r.margin_percent.unwrap_or_default(),
        Sort::Qty => r.qty,
    };
    if !by(Key::Period) {
        rows.sort_by_key(|r| std::cmp::Reverse(value(r)));
    }
    if let Some(limit) = ask.limit {
        rows.truncate(limit);
    }
    let mut totals = Row::default();
    measures(&mut totals, total, compared.then_some(total_before));
    Ok(AnalysisOutput {
        company_id: ask.company_id,
        from: ask.from,
        to: ask.to,
        currency: base,
        document_currency: ask.currency,
        compare_from: compare_range.map(|r| r.0),
        compare_to: compare_range.map(|r| r.1),
        rows,
        totals,
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SalesInput {
    /// The company to report on.
    company_id: Uuid,
    /// The first posting date counted.
    from: NaiveDate,
    /// The last posting date counted.
    to: NaiveDate,
    /// What the rows are per: customer, item, item_group, period,
    /// dimension; customer when empty.
    #[serde(default)]
    group_by: Vec<Key>,
    /// With group_by period: month (the default), quarter or year.
    #[serde(default)]
    period: Period,
    /// prior_period or prior_year.
    compare: Option<Compare>,
    /// Count only lines whose entries carry these values: for each
    /// dimension among them, one of its values.
    #[serde(default)]
    dimension_value_ids: Vec<Uuid>,
    /// With group_by dimension: the dimension.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "dimension", "key": "id" }))]
    group_by_dimension_id: Option<Uuid>,
    /// Only this customer.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "customer", "key": "id" }))]
    customer_id: Option<Uuid>,
    /// Only this item.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "item", "key": "id" }))]
    item_id: Option<Uuid>,
    /// Only documents in this currency, with what they came to in it.
    currency: Option<String>,
    /// net (the default), margin, margin_percent or qty, largest first.
    #[serde(default)]
    sort: Sort,
    /// At most this many rows.
    limit: Option<usize>,
}

pub(crate) struct SalesAnalysis;

#[async_trait]
impl Command for SalesAnalysis {
    type Input = SalesInput;
    type Output = AnalysisOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "analysis.sales",
            "Sales analysis from what was posted: the sales invoices, credit notes and credits \
             with a posting date in the range, per customer, item, item group, period or \
             dimension value (group_by, customer by default), with the quantity, net in the \
             accounting currency, the cost of the goods as the stock ledger posted it, the \
             margin and the margin in percent; with compare, the same for the period or year \
             before. A reversal counts on its own date. sort and limit make a top list; totals \
             are of every row. Cost and margin need stock_entry:read.",
        )
        .report("Sales analysis")
        .permission("sales_invoice:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SalesInput) -> Result<AnalysisOutput> {
        let mut group_by = input.group_by;
        if group_by.is_empty() {
            group_by.push(Key::Customer);
        }
        if group_by.contains(&Key::Vendor) {
            return Err(
                Error::validation("a sales analysis is not per vendor").with_field("group_by")
            );
        }
        analyse(
            ctx,
            Side::Sales,
            Ask {
                company_id: input.company_id,
                from: input.from,
                to: input.to,
                group_by,
                period: input.period,
                compare: input.compare,
                dimension_value_ids: input.dimension_value_ids,
                group_by_dimension_id: input.group_by_dimension_id,
                party_id: input.customer_id,
                item_id: input.item_id,
                currency: input.currency,
                sort: input.sort,
                limit: input.limit,
            },
        )
        .await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PurchaseInput {
    /// The company to report on.
    company_id: Uuid,
    /// The first posting date counted.
    from: NaiveDate,
    /// The last posting date counted.
    to: NaiveDate,
    /// What the rows are per: vendor, item, item_group, period, dimension;
    /// vendor when empty.
    #[serde(default)]
    group_by: Vec<Key>,
    /// With group_by period: month (the default), quarter or year.
    #[serde(default)]
    period: Period,
    /// prior_period or prior_year.
    compare: Option<Compare>,
    /// Count only lines whose entries carry these values.
    #[serde(default)]
    dimension_value_ids: Vec<Uuid>,
    /// With group_by dimension: the dimension.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "dimension", "key": "id" }))]
    group_by_dimension_id: Option<Uuid>,
    /// Only this vendor.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "vendor", "key": "id" }))]
    vendor_id: Option<Uuid>,
    /// Only this item.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "item", "key": "id" }))]
    item_id: Option<Uuid>,
    /// Only documents in this currency, with what they came to in it.
    currency: Option<String>,
    /// net (the default) or qty, largest first.
    #[serde(default)]
    sort: Sort,
    /// At most this many rows.
    limit: Option<usize>,
}

pub(crate) struct PurchaseAnalysis;

#[async_trait]
impl Command for PurchaseAnalysis {
    type Input = PurchaseInput;
    type Output = AnalysisOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "analysis.purchases",
            "Purchase analysis from what was posted: the vendor invoices, credit notes and \
             credits with a posting date in the range, per vendor, item, item group, period or \
             dimension value (group_by, vendor by default), with the quantity, net in the \
             accounting currency (what the lines cleared from goods received or charged, price \
             variances included) and the average price; with compare, the same for the period \
             or year before.",
        )
        .report("Purchase analysis")
        .permission("vendor_invoice:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PurchaseInput) -> Result<AnalysisOutput> {
        let mut group_by = input.group_by;
        if group_by.is_empty() {
            group_by.push(Key::Vendor);
        }
        if group_by.contains(&Key::Customer) {
            return Err(
                Error::validation("a purchase analysis is not per customer").with_field("group_by")
            );
        }
        let sort = match input.sort {
            Sort::Margin | Sort::MarginPercent => Sort::Net,
            sort => sort,
        };
        analyse(
            ctx,
            Side::Purchases,
            Ask {
                company_id: input.company_id,
                from: input.from,
                to: input.to,
                group_by,
                period: input.period,
                compare: input.compare,
                dimension_value_ids: input.dimension_value_ids,
                group_by_dimension_id: input.group_by_dimension_id,
                party_id: input.vendor_id,
                item_id: input.item_id,
                currency: input.currency,
                sort,
                limit: input.limit,
            },
        )
        .await
    }
}

/// What a top list ranks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Of {
    Customers,
    Items,
    ItemGroups,
    Vendors,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TopInput {
    /// The company to report on.
    company_id: Uuid,
    /// The first posting date counted.
    from: NaiveDate,
    /// The last posting date counted.
    to: NaiveDate,
    /// customers, items, item_groups (by sales) or vendors (by purchases).
    of: Of,
    /// net (the default), margin or qty.
    #[serde(default)]
    by: Sort,
    /// How many; 10 when omitted, 100 at most.
    n: Option<usize>,
    /// prior_period or prior_year.
    compare: Option<Compare>,
}

pub(crate) struct Top;

#[async_trait]
impl Command for Top {
    type Input = TopInput;
    type Output = AnalysisOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "analysis.top",
            "The top customers, items or item groups by sales, or vendors by purchases, in a \
             range: the first n rows (10 by default) of analysis.sales or analysis.purchases, \
             largest first by net, margin or quantity; totals are of all of them.",
        )
        .report("Top lists")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: TopInput) -> Result<AnalysisOutput> {
        let (side, key) = match input.of {
            Of::Customers => (Side::Sales, Key::Customer),
            Of::Items => (Side::Sales, Key::Item),
            Of::ItemGroups => (Side::Sales, Key::ItemGroup),
            Of::Vendors => (Side::Purchases, Key::Vendor),
        };
        let sort = match (side, input.by) {
            (Side::Purchases, Sort::Margin | Sort::MarginPercent) => Sort::Net,
            (_, by) => by,
        };
        analyse(
            ctx,
            side,
            Ask {
                company_id: input.company_id,
                from: input.from,
                to: input.to,
                group_by: vec![key],
                period: Period::Month,
                compare: input.compare,
                dimension_value_ids: vec![],
                group_by_dimension_id: None,
                party_id: None,
                item_id: None,
                currency: None,
                sort,
                limit: Some(input.n.unwrap_or(10).clamp(1, 100)),
            },
        )
        .await
    }
}

/// The posted net and cost of sales over a range, for the key figures.
pub(crate) async fn sales_totals(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<(Decimal, Decimal)> {
    let ask = Ask {
        company_id,
        from,
        to,
        group_by: vec![],
        period: Period::Month,
        compare: None,
        dimension_value_ids: vec![],
        group_by_dimension_id: None,
        party_id: None,
        item_id: None,
        currency: None,
        sort: Sort::Net,
        limit: None,
    };
    let types = types(ctx, Side::Sales)?;
    let facts = facts(ctx, Side::Sales, &ask, &types, from, to).await?;
    Ok(facts
        .iter()
        .fold((Decimal::ZERO, Decimal::ZERO), |(n, c), f| {
            (n + f.net, c + f.cost.unwrap_or_default())
        }))
}
