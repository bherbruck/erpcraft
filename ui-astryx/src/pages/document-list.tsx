import { Button } from "@astryxdesign/core/Button";
import { HStack } from "@astryxdesign/core/HStack";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { useMemo, useState } from "react";
import { type BulkResult, BulkBar, BulkResults } from "@/components/bulk-actions";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { DocTypeMeta, DocumentSummary, FieldMeta, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable, useColumnChoice } from "@/components/data-table";
import { useDecimalHeader } from "@/components/decimal-value";
import { Empty, FieldValue, isNumeric } from "@/components/field-value";
import { Page } from "@/components/page";
import { SavedViews } from "@/components/saved-views";
import { FilterBar, filterSpec, parseFilters, toQuery, type FilterSpec } from "@/components/filter-bar";
import { StatusBadge } from "@/components/status-badge";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { canDoc, canEntity, canRun, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { pageFromUrl, useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";


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
  const filters = parseFilters(url.get("f"));
  const sort = url.get("sort");
  const page = pageFromUrl(url.get("page"));
  const size = Number(url.get("size")) || 25;
  const canCreate = canDoc(meta, docType.name, "create") && company !== null;
  const summaryHeaders = useHeaders(docType.summary);

  // The header the server filters by expression, and who the document is with by exact value.
  const specs = useMemo(() => {
    const header = (name: string) => meta.document_header.find((field) => field.name === name);
    const out: FilterSpec[] = [];
    const number = header("number");
    if (number) out.push({ key: "number", label: number.label, kind: "text" });
    out.push({
      key: "status",
      label: header("status")?.label ?? t("document.status"),
      kind: "enum",
      values: docType.states.map((state) => ({ value: state.name, label: state.label })),
    });
    const date = header("doc_date");
    if (date) out.push({ key: "doc_date", label: date.label, kind: "date" });
    for (const field of docType.summary) {
      if (field.type.kind === "link" && canEntity(meta, field.type.entity, "read")) {
        const spec = filterSpec(field, `summary.${field.name}`, true);
        if (spec) out.push(spec);
      }
    }
    return out;
  }, [meta, docType]);
  const query = toQuery(filters, specs);
  const filterKey = `${query.q}|${query.where}|${JSON.stringify(query.summary)}`;

  const list = useAsync(
    company ? `${docType.name}|${company.id}|${filterKey}|${sort}|${page}|${size}` : null,
    (signal) =>
      api.run(
        "document.list",
        {
          doc_type: docType.name,
          company_id: company?.id ?? null,
          q: query.q || null,
          where: query.where,
          summary: query.summary,
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
  const { visible, chooser, chosen, choose } = useColumnChoice(`document.${docType.name}`, columns);

  // Documents chosen for one action on all of them, kept across pages of the list.
  const [chosenDocs, setChosenDocs] = useState<Map<string, DocumentSummary>>(() => new Map());
  const [bulk, setBulk] = useState<{ action: string; results: BulkResult[] } | null>(null);
  const canBulk = meta.commands.some((command) => command.resource === docType.name && command.target && !command.read_only && command.creates === null);
  const filtered = filters.length > 0;
  const clearFilters = (): void => url.set({ f: null, page: null });
  // Commands that make a document of this type from something else (importing a file).
  const makers = meta.commands.filter((command) => command.creates === docType.name && !command.target && canRun(meta, command.name));
  const newButton = canCreate ? (
    <Button label={t("list.new", { label: docType.label })} variant="primary" href={paths.docNew(code, docType.name)} />
  ) : null;

  return (
    <Page
      title={docType.plural}
      actions={
        <>
          {makers.map((command) => (
            <Button key={command.name} label={command.label} href={paths.run(code, command.name)} />
          ))}
          {newButton}
        </>
      }
      padded={false}
      toolbar={
        <HStack gap={2} vAlign="center">
          <FilterBar
            meta={meta}
            specs={specs}
            filters={filters}
            companyId={company?.id ?? null}
            resultCount={list.data?.total}
            label={t("filter.placeholder", { label: docType.plural })}
            onChange={(next) => url.set({ f: next.length ? JSON.stringify(next) : null, page: null })}
          />
          <SavedViews tableKey={`document.${docType.name}`} columns={chosen} onColumns={choose} />
          {chooser}
        </HStack>
      }
    >
      {bulk || chosenDocs.size > 0 ? (
        <VStack gap={2} padding={3}>
          {bulk ? <BulkResults docType={docType} action={bulk.action} results={bulk.results} onClose={() => setBulk(null)} /> : null}
          {chosenDocs.size > 0 ? (
            <BulkBar
              docType={docType}
              chosen={[...chosenDocs.values()]}
              onClear={() => setChosenDocs(new Map())}
              onDone={(results, action) => {
                setBulk({ action, results });
                setChosenDocs(new Map());
                list.reload();
              }}
            />
          ) : null}
        </VStack>
      ) : null}
      <DataTable
        selection={
          canBulk
            ? {
                selected: new Set(chosenDocs.keys()),
                label: (doc) => t("bulk.choose", { number: doc.number ?? doc.memo ?? doc.id }),
                onChange: (docs, on) =>
                  setChosenDocs((previous) => {
                    const next = new Map(previous);
                    for (const doc of docs) on ? next.set(doc.id, doc) : next.delete(doc.id);
                    return next;
                  }),
              }
            : undefined
        }
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

export default DocumentListPage;
