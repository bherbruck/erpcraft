import { Button } from "@astryxdesign/core/Button";
import { HStack } from "@astryxdesign/core/HStack";
import { Selector } from "@astryxdesign/core/Selector";
import { Text } from "@astryxdesign/core/Text";
import { useCallback, useMemo } from "react";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { DocTypeMeta, DocumentSummary, FieldMeta, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable, useColumnChoice } from "@/components/data-table";
import { useDecimalHeader } from "@/components/decimal-value";
import { Empty, FieldValue, isNumeric } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { Page } from "@/components/page";
import { SearchBox } from "@/components/search-box";
import { StatusBadge } from "@/components/status-badge";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { canDoc, canEntity, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { pageFromUrl, useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

/** URL parameters that filter by a summary field: `s.vendor_id`. */
const SUMMARY_FILTER = "s.";

export function DocumentListPage() {
  const { meta } = useApp();
  const params = useParams();
  const docType = findDocType(meta, params.docType);
  if (!docType || !canDoc(meta, docType.name, "read")) return <NotFoundPage />;
  return <DocumentList key={docType.name} docType={docType} />;
}

function summaryOf(doc: DocumentSummary): JsonObject {
  return typeof doc.summary === "object" && doc.summary !== null && !Array.isArray(doc.summary) ? doc.summary : {};
}

/** The header of a decimal summary column: money in the company's currency says which. */
function useHeaders(fields: FieldMeta[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const field of fields) {
    // biome-ignore lint/correctness/useHookAtTopLevel: the summary fields of a type never change
    out[field.name] = useDecimalHeader(field);
  }
  return out;
}

/** The list of any document type, generated from the registry: header, status and the type's summary. */
function DocumentList({ docType }: { docType: DocTypeMeta }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const q = url.get("q");
  const status = url.get("status");
  const sort = url.get("sort");
  const page = pageFromUrl(url.get("page"));
  const size = Number(url.get("size")) || 25;
  const canCreate = canDoc(meta, docType.name, "create") && company !== null;
  const summaryHeaders = useHeaders(docType.summary);

  // Who a document is with, as a filter: the summary's links to records.
  const partyFilters = docType.summary.filter(
    (field) => field.type.kind === "link" && canEntity(meta, field.type.entity, "read"),
  );
  const summaryFilter: Record<string, string> = {};
  for (const field of partyFilters) {
    const value = url.get(`${SUMMARY_FILTER}${field.name}`);
    if (value) summaryFilter[field.name] = value;
  }
  const filterKey = JSON.stringify(summaryFilter);

  const list = useAsync(
    company ? `${docType.name}|${company.id}|${q}|${status}|${sort}|${page}|${size}|${filterKey}` : null,
    (signal) =>
      api.run(
        "document.list",
        {
          doc_type: docType.name,
          company_id: company?.id ?? null,
          q: q || null,
          status: status || null,
          summary: summaryFilter,
          sort: sort ? [sort] : ["-doc_date", "-created_at"],
          limit: size,
          offset: (page - 1) * size,
        },
        { signal },
      ),
  );

  const columns = useMemo<Column<DocumentSummary>[]>(() => {
    const header = (name: string, fallback: string): string =>
      meta.document_header.find((field) => field.name === name)?.label ?? fallback;
    const named = docType.summary.some((field) => field.type.kind === "text");
    const summary = docType.summary.map(
      (field): Column<DocumentSummary> => ({
        id: `summary.${field.name}`,
        header: summaryHeaders[field.name] ?? field.label,
        sortField:
          field.type.kind === "link" || field.type.kind === "document_link" ? undefined : `summary.${field.name}`,
        align: isNumeric(field) ? "end" : "start",
        defaultHidden:
          ((field.type.kind === "link" || field.type.kind === "document_link") && named) ||
          field.type.kind === "entity_link" ||
          field.name.endsWith("_accounting") ||
          (field.name === "total_value" && docType.summary.some((other) => other.name === "total_gross")),
        cell: (doc) => {
          const values = summaryOf(doc);
          return <FieldValue field={field} value={values[field.name]} context={{ header: values, fields: docType.summary }} />;
        },
      }),
    );
    return [
      {
        id: "number",
        header: header("number", t("document.number")),
        sortField: "number",
        width: 150,
        cell: (doc) =>
          doc.number ? (
            <Text hasTabularNumbers weight="medium">
              {doc.number}
            </Text>
          ) : (
            <Text color="secondary">{t("document.unnumbered")}</Text>
          ),
      },
      {
        id: "doc_date",
        header: header("doc_date", t("document.date")),
        sortField: "doc_date",
        width: 120,
        cell: (doc) => <span className="nowrap">{formatCalendarDate(doc.doc_date)}</span>,
      },
      {
        id: "status",
        header: header("status", t("document.status")),
        sortField: "status",
        width: 110,
        cell: (doc) => <StatusBadge status={doc.status} docType={docType} />,
      },
      ...summary,
      {
        id: "memo",
        header: header("memo", t("document.memo")),
        cell: (doc) => doc.memo || <Empty />,
      },
      {
        id: "created_by_name",
        header: header("created_by_name", t("document.created_by_column")),
        defaultHidden: true,
        cell: (doc) => doc.created_by_name ?? <Empty />,
      },
      {
        id: "created_at",
        header: header("created_at", t("document.created")),
        sortField: "created_at",
        defaultHidden: true,
        cell: (doc) => formatTimestamp(doc.created_at),
      },
      {
        id: "updated_at",
        header: header("updated_at", t("document.updated")),
        sortField: "updated_at",
        defaultHidden: true,
        cell: (doc) => formatTimestamp(doc.updated_at),
      },
    ];
    // biome-ignore lint/correctness/useExhaustiveDependencies: headers follow the type
  }, [docType, meta.document_header]);
  const { visible, chooser } = useColumnChoice(`document.${docType.name}`, columns);

  const setUrl = url.set;
  const onSearch = useCallback((next: string) => setUrl({ q: next, page: null }, { replace: true }), [setUrl]);
  const filtered = q !== "" || status !== "" || Object.keys(summaryFilter).length > 0;
  const clearFilters = (): void => {
    const cleared: Record<string, null> = { q: null, status: null, page: null };
    for (const field of partyFilters) cleared[`${SUMMARY_FILTER}${field.name}`] = null;
    url.set(cleared);
  };
  const newButton = canCreate ? (
    <Button label={t("list.new", { label: docType.label })} variant="primary" href={paths.docNew(code, docType.name)} />
  ) : null;

  return (
    <Page
      title={docType.plural}
      actions={newButton}
      padded={false}
      toolbar={
        <HStack gap={2} wrap="wrap" vAlign="center">
          <SearchBox value={q} onChange={onSearch} label={t("list.search", { label: docType.plural })} busy={list.loading && list.data !== undefined} />
          <Selector
            label={t("document.filter_status")}
            isLabelHidden
            size="sm"
            variant="secondary"
            value={status}
            hasClear={status !== ""}
            placeholder={t("document.any_status")}
            onChange={(next: string | null) => url.set({ status: next ?? null, page: null })}
            options={docType.states.map((state) => ({ value: state.name, label: state.label }))}
          />
          {partyFilters.map((field) =>
            field.type.kind === "link" ? (
              <div key={field.name} className="filter-lookup">
                <Lookup
                  label={t("document.filter_by", { label: field.label })}
                  isLabelHidden
                  size="sm"
                  entity={field.type.entity}
                  keyField={field.type.key}
                  value={url.get(`${SUMMARY_FILTER}${field.name}`) || null}
                  companyId={company?.id}
                  allowCreate={false}
                  onChange={(next) => url.set({ [`${SUMMARY_FILTER}${field.name}`]: next, page: null })}
                />
              </div>
            ) : null,
          )}
          <div className="push-end">{chooser}</div>
        </HStack>
      }
    >
      <DataTable
        caption={docType.plural}
        columns={visible}
        rows={company ? list.data?.items : []}
        rowKey={(doc) => doc.id}
        rowHref={(doc) => paths.doc(code, docType.name, doc.id)}
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        sort={sort}
        onSort={(next) => url.set({ sort: next, page: null })}
        page={page}
        pageSize={size}
        total={list.data?.total ?? 0}
        onPage={(next) => url.set({ page: next === 1 ? null : String(next) })}
        onPageSize={(next) => url.set({ size: next === 25 ? null : String(next), page: null })}
        empty={
          !company
            ? { title: t("list.needs_company", { label: docType.plural }) }
            : filtered
              ? { title: t("list.no_match_filtered"), action: <Button label={t("list.clear_filters")} size="sm" onClick={clearFilters} /> }
              : page > 1
                ? { title: t("list.past_end"), action: <Button label={t("list.first_page")} size="sm" onClick={() => url.set({ page: null })} /> }
                : { title: t("list.empty_in_company", { label: docType.plural, company: company.name }), action: newButton ?? undefined }
        }
      />
    </Page>
  );
}
