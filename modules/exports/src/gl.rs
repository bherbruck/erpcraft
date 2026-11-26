//! The general ledger as rows or CSV, for any accountant's tool.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::file::{ExportFile, csv_field};

/// The most rows one CSV file holds.
const MAX_ROWS: i64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Format {
    /// Rows, as a report.
    #[default]
    Rows,
    /// A CSV file.
    Csv,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportInput {
    /// The company to export.
    company_id: Uuid,
    /// The first posting date exported.
    from: NaiveDate,
    /// The last posting date exported.
    to: NaiveDate,
    /// Only entries on these accounts.
    #[serde(default)]
    account_ids: Vec<Uuid>,
    /// Count the entries of a year-end close too.
    #[serde(default)]
    include_closing: bool,
    /// rows (the default) or csv.
    #[serde(default)]
    format: Format,
    /// With rows: at most this many, 1000 when omitted.
    limit: Option<i64>,
    /// With rows: skip this many.
    offset: Option<i64>,
}

/// One entry of the general ledger.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct EntryRow {
    posting_date: NaiveDate,
    doc_type: String,
    document_number: Option<String>,
    document_id: Uuid,
    posting_id: Uuid,
    line_no: i32,
    /// The posting takes back another.
    reversal: bool,
    /// The posting is a year-end close.
    closing: bool,
    account_code: String,
    account_name: String,
    account_type: String,
    debit: Decimal,
    credit: Decimal,
    /// The entry's own currency, and its amounts and rate in it.
    currency: String,
    txn_debit: Decimal,
    txn_credit: Decimal,
    fx_rate: Decimal,
    memo: Option<String>,
    /// customer or vendor, when the entry is for one.
    party_type: Option<String>,
    party_code: Option<String>,
    party_name: Option<String>,
    /// The values of the company's dimensions, as DIMENSION:VALUE.
    dimensions: Option<String>,
    #[serde(skip)]
    #[sqlx(default)]
    dimension_set_id: Option<Uuid>,
    #[serde(skip)]
    #[sqlx(default)]
    party_id: Option<Uuid>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ExportOutput {
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    /// The entries of the range.
    total: i64,
    rows: Vec<EntryRow>,
    /// With csv: the file.
    file: Option<ExportFile>,
}

pub(crate) struct Export;

#[async_trait]
impl Command for Export {
    type Input = ExportInput;
    type Output = ExportOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "gl.export",
            "Every general ledger entry of a company dated from..to, in posting order: date, \
             document, posting and line, reversal and closing flags, account, debit and credit \
             in the accounting currency, the entry's currency with its amounts and rate, memo, \
             the customer or vendor it is for, and its dimension values. As rows (paged) or as a \
             CSV (UTF-8 with a byte order mark, comma, ISO dates; at most 100,000 entries, \
             recorded in export_log). Closing entries only with include_closing.",
        )
        .permission("gl_entry:read")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ExportInput) -> Result<ExportOutput> {
        let company_id = input.company_id;
        ctx.require("gl_entry:read", company_id)?;
        if input.from > input.to {
            return Err(Error::validation("`from` cannot be after `to`").with_field("from"));
        }
        const WHERE: &str = "g.company_id = $1 AND g.posting_date BETWEEN $2 AND $3
              AND (cardinality($4::uuid[]) = 0 OR g.account_id = ANY ($4))
              AND ($5 OR NOT starts_with(p.command, 'year_end_close.'))";
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM gl_entry g JOIN posting p ON p.id = g.posting_id WHERE {WHERE}"
        ))
        .bind(company_id)
        .bind(input.from)
        .bind(input.to)
        .bind(&input.account_ids)
        .bind(input.include_closing)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let csv = input.format == Format::Csv;
        if csv && total > MAX_ROWS {
            return Err(Error::validation(format!(
                "the range has {total} entries; a file takes at most {MAX_ROWS}: export it in \
                 parts"
            )));
        }
        let (limit, offset) = if csv {
            (MAX_ROWS, 0)
        } else {
            (
                input.limit.unwrap_or(1000).clamp(1, 10_000),
                input.offset.unwrap_or(0).max(0),
            )
        };
        let mut rows: Vec<EntryRow> = sqlx::query_as(&format!(
            "SELECT g.posting_date, d.doc_type, d.number AS document_number, d.id AS document_id,
                    g.posting_id, g.line_no, p.reverses_id IS NOT NULL AS reversal,
                    starts_with(p.command, 'year_end_close.') AS closing,
                    a.code AS account_code, a.name AS account_name, a.account_type,
                    g.debit, g.credit, g.currency, g.txn_debit, g.txn_credit, g.fx_rate,
                    COALESCE(g.memo, p.memo) AS memo,
                    pt.party_type, pt.party_id, NULL::text AS party_code, NULL::text AS party_name,
                    NULL::text AS dimensions, g.dimension_set_id
             FROM gl_entry g JOIN posting p ON p.id = g.posting_id
             JOIN document d ON d.id = p.document_id
             JOIN account a ON a.id = g.account_id
             LEFT JOIN LATERAL (
                 SELECT o.party_type, o.party_id FROM open_item o
                 WHERE o.account_id = g.account_id
                   AND (o.posting_id = COALESCE(p.reverses_id, p.id)
                        OR o.id IN (SELECT st.open_item_id FROM settlement st
                                    WHERE st.posting_id = p.id))
                 ORDER BY o.id LIMIT 1) pt ON true
             WHERE {WHERE}
             ORDER BY g.posting_date, p.created_at, g.posting_id, g.line_no
             LIMIT $6 OFFSET $7"
        ))
        .bind(company_id)
        .bind(input.from)
        .bind(input.to)
        .bind(&input.account_ids)
        .bind(input.include_closing)
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *ctx.conn)
        .await?;
        // Who each entry is for, as far as the reader may read them.
        let mut labels: HashMap<(String, Uuid), (String, String)> = HashMap::new();
        for kind in ["customer", "vendor"] {
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT DISTINCT o.party_id FROM open_item o
                 WHERE o.company_id = $1 AND o.party_type = $2",
            )
            .bind(company_id)
            .bind(kind)
            .fetch_all(&mut *ctx.conn)
            .await?;
            let readable = erpcraft_kernel::entity::readable_ids(ctx, kind, &ids).await?;
            let found: Vec<(Uuid, String, String)> = sqlx::query_as(&format!(
                "SELECT id, code, name FROM {kind} WHERE id = ANY ($1)"
            ))
            .bind(readable.into_iter().collect::<Vec<_>>())
            .fetch_all(&mut *ctx.conn)
            .await?;
            for (id, code, name) in found {
                labels.insert((kind.to_owned(), id), (code, name));
            }
        }
        let sets: Vec<Uuid> = rows.iter().filter_map(|r| r.dimension_set_id).collect();
        let values: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT sv.set_id, d.code, v.code FROM dimension_set_value sv
             JOIN dimension d ON d.id = sv.dimension_id
             JOIN dimension_value v ON v.id = sv.value_id
             WHERE sv.set_id = ANY ($1) ORDER BY d.code",
        )
        .bind(&sets)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut by_set: HashMap<Uuid, BTreeMap<String, String>> = HashMap::new();
        for (set, dimension, value) in values {
            by_set.entry(set).or_default().insert(dimension, value);
        }
        let dimensions: Vec<String> = {
            let mut all: Vec<String> = by_set.values().flat_map(|m| m.keys().cloned()).collect();
            all.sort();
            all.dedup();
            all
        };
        for row in &mut rows {
            if let (Some(kind), Some(id)) = (row.party_type.clone(), row.party_id)
                && let Some((code, name)) = labels.get(&(kind, id))
            {
                row.party_code = Some(code.clone());
                row.party_name = Some(name.clone());
            }
            if let Some(m) = row.dimension_set_id.and_then(|s| by_set.get(&s)) {
                row.dimensions = Some(
                    m.iter()
                        .map(|(d, v)| format!("{d}:{v}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
        }
        let mut file = None;
        if csv {
            let mut header: Vec<String> = [
                "posting_date",
                "doc_type",
                "document_number",
                "document_id",
                "posting_id",
                "line_no",
                "reversal",
                "closing",
                "account_code",
                "account_name",
                "account_type",
                "debit",
                "credit",
                "currency",
                "txn_debit",
                "txn_credit",
                "fx_rate",
                "memo",
                "party_type",
                "party_code",
                "party_name",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            header.extend(dimensions.iter().map(|d| format!("dimension_{d}")));
            let mut text = String::from("\u{feff}");
            text.push_str(
                &header
                    .iter()
                    .map(|h| csv_field(h))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            text.push_str("\r\n");
            let amount = |d: Decimal| format!("{:.2}", d.round_dp(2));
            for r in &rows {
                let mut fields = vec![
                    r.posting_date.to_string(),
                    r.doc_type.clone(),
                    csv_field(r.document_number.as_deref().unwrap_or("")),
                    r.document_id.to_string(),
                    r.posting_id.to_string(),
                    r.line_no.to_string(),
                    r.reversal.to_string(),
                    r.closing.to_string(),
                    csv_field(&r.account_code),
                    csv_field(&r.account_name),
                    r.account_type.clone(),
                    amount(r.debit),
                    amount(r.credit),
                    r.currency.clone(),
                    amount(r.txn_debit),
                    amount(r.txn_credit),
                    r.fx_rate.normalize().to_string(),
                    csv_field(r.memo.as_deref().unwrap_or("")),
                    r.party_type.clone().unwrap_or_default(),
                    csv_field(r.party_code.as_deref().unwrap_or("")),
                    csv_field(r.party_name.as_deref().unwrap_or("")),
                ];
                let set = r.dimension_set_id.and_then(|s| by_set.get(&s));
                for d in &dimensions {
                    fields.push(csv_field(
                        set.and_then(|m| m.get(d)).map(String::as_str).unwrap_or(""),
                    ));
                }
                text.push_str(&fields.join(","));
                text.push_str("\r\n");
            }
            let bytes = text.as_bytes().to_vec();
            let made = ExportFile::new(
                format!(
                    "general_ledger_{}_{}.csv",
                    input.from.format("%Y%m%d"),
                    input.to.format("%Y%m%d")
                ),
                "text/csv; charset=utf-8",
                text,
                bytes,
                rows.len(),
                vec![],
            );
            made.log(ctx, company_id, "gl", Some((input.from, input.to)), false)
                .await?;
            file = Some(made);
            rows = vec![];
        } else {
            ctx.no_change();
        }
        Ok(ExportOutput {
            company_id,
            from: input.from,
            to: input.to,
            total,
            rows,
            file,
        })
    }
}
