import { Button } from "@astryxdesign/core/Button";
import { HStack } from "@astryxdesign/core/HStack";
import { Center } from "@astryxdesign/core/Center";
import { EmptyState } from "@astryxdesign/core/EmptyState";
import { VStack } from "@astryxdesign/core/VStack";
import { useMemo, useState } from "react";
import { type BulkResult, BulkBar, BulkResults } from "@/components/bulk-actions";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { DocTypeMeta, DocumentSummary, FieldMeta, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import type { GridColumn } from "@erpcraft/grid";
import { fieldColumn, ListGrid } from "@/components/list-grid";
import { useDecimalHeader } from "@/components/decimal-value";
import { Page } from "@/components/page";
import { SavedViews } from "@/components/saved-views";
import { FilterBar, filterSpec, parseFilters, toQuery, type FilterSpec } from "@/components/filter-bar";
import { StatusBadge } from "@/components/status-badge";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { canDoc, canEntity, canRun, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useListUrl } from "@/lib/list-url";
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
/** The column of checkboxes that chooses documents for a bulk action. */
const CHOSEN = "_chosen";
type Row = DocumentSummary & JsonObject;

function DocumentList({ docType }: { docType: DocTypeMeta }) {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const { url, sort, page, size, setQuery } = useListUrl();
  const filters = parseFilters(url.get("f"));
  const canCreate = canDoc(meta, docType.name, "create") && company !== null;
  const summaryHeaders = useHeaders(docType.summary);
  // Documents chosen for one action on all of them, kept across pages of the list.
  const [chosenDocs, setChosenDocs] = useState<Map<string, DocumentSummary>>(() => new Map());
  const [bulk, setBulk] = useState<{ action: string; results: BulkResult[] } | null>(null);
  const canBulk = meta.commands.some((command) => command.resource === docType.name && command.target && !command.read_only && command.creates === null);
  const choose = (docs: DocumentSummary[], on: boolean) =>
    setChosenDocs((previous) => {
      const next = new Map(previous);
      for (const doc of docs) on ? next.set(doc.id, doc) : next.delete(doc.id);
      return next;
    });

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
    company ? `${docType.name}|${company.id}|${filterKey}|${sort.join()}|${page}|${size}` : null,
    (signal) =>
      api.run(
        "document.list",
        {
          doc_type: docType.name,
          company_id: company?.id ?? null,
          q: query.q || null,
          where: query.where,
          summary: query.summary,
          sort: sort.length ? sort : ["-doc_date", "-created_at"],
          limit: size,
          offset: (page - 1) * size,
        },
        { signal },
      ),
  );

  // The grid's columns: the header, the state, the type's summary and the memo.
  const columns = useMemo<GridColumn<Row>[]>(() => {
    const header = (name: string, fallback: string): string => meta.document_header.find((field) => field.name === name)?.label ?? fallback;
    const named = docType.summary.some((field) => field.type.kind === "text");
    const text = (id: string, label: string, read: (doc: Row) => string | null, extra: Partial<GridColumn<Row>> = {}): GridColumn<Row> => ({
      id,
      header: label,
      type: "text",
      field: id,
      filterable: false,
      searchable: false,
      value: (doc) => read(doc),
      ...extra,
    });
    const chooser: GridColumn<Row>[] = canBulk
      ? [{ id: CHOSEN, header: t("list.select"), type: "boolean", sortable: false, filterable: false, searchable: false, hideable: false, width: 84, pin: "left", editable: true, value: (doc) => chosenDocs.has(doc.id) }]
      : [];
    return [
      ...chooser,
      text("number", header("number", t("document.number")), (doc) => doc.number ?? t("document.unnumbered"), { width: 150, hideable: false }),
      {
        id: "doc_date",
        header: header("doc_date", t("document.date")),
        type: "date",
        field: "doc_date",
        filterable: false,
        searchable: false,
        width: 120,
        value: (doc) => doc.doc_date,
        format: (v) => (typeof v === "string" ? formatCalendarDate(v) : ""),
      },
      {
        id: "status",
        header: header("status", t("document.status")),
        type: "state",
        field: "status",
        filterable: false,
        searchable: false,
        width: 110,
        options: docType.states.map((state) => ({ value: state.name, label: state.label })),
        value: (doc) => doc.status,
      },
      ...docType.summary.map((field) =>
        fieldColumn<Row>(field, (doc) => summaryOf(doc)[field.name], {
          id: `summary.${field.name}`,
          header: summaryHeaders[field.name] ?? field.label,
          sortField: field.type.kind === "link" || field.type.kind === "document_link" ? null : `summary.${field.name}`,
          decimals: currencyDecimals[company?.base_currency ?? ""] ?? 2,
          hidden:
            ((field.type.kind === "link" || field.type.kind === "document_link") && named) ||
            field.type.kind === "entity_link" ||
            field.name.endsWith("_accounting") ||
            (field.name === "total_value" && docType.summary.some((other) => other.name === "total_gross")),
        }),
      ),
      text("memo", header("memo", t("document.memo")), (doc) => doc.memo, { width: 280, sortable: false }),
      text("created_by_name", header("created_by_name", t("document.created_by_column")), (doc) => doc.created_by_name, { hidden: true, sortable: false }),
      {
        id: "created_at",
        header: header("created_at", t("document.created")),
        type: "datetime",
        field: "created_at",
        filterable: false,
        searchable: false,
        hidden: true,
        value: (doc) => doc.created_at,
        format: (v) => (typeof v === "string" ? formatTimestamp(v) : ""),
      },
      {
        id: "updated_at",
        header: header("updated_at", t("document.updated")),
        type: "datetime",
        field: "updated_at",
        filterable: false,
        searchable: false,
        hidden: true,
        value: (doc) => doc.updated_at,
        format: (v) => (typeof v === "string" ? formatTimestamp(v) : ""),
      },
    ];
    // biome-ignore lint/correctness/useExhaustiveDependencies: headers follow the type
  }, [docType, meta.document_header, chosenDocs, canBulk]);

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
      <ListGrid<Row>
        key={`${docType.name}|${filterKey}`}
        id={`document.${docType.name}`}
        label={docType.plural}
        columns={columns}
        rows={company ? (list.data?.items as Row[] | undefined) : []}
        total={list.data?.total ?? 0}
        loading={list.loading}
        sort={sort}
        page={page}
        size={size}
        onQuery={setQuery}
        rowHref={(doc) => paths.doc(code, docType.name, doc.id)}
        linkColumn="number"
        decorate={(columnId, doc) => (columnId === "status" ? <StatusBadge status={doc.status} docType={docType} /> : undefined)}
        check={canBulk ? { columnId: CHOSEN, label: (doc) => t("bulk.choose", { number: doc.number ?? doc.memo ?? doc.id }), onToggle: (doc, on) => choose([doc], on) } : undefined}
        // The choice column takes typing, paste and Delete like any cell: y/n, x, 1/0.
        onEdit={(changes) => {
          for (const change of changes) if (change.columnId === CHOSEN) choose([change.row], change.newValue === true);
        }}
        toolbar={<HStack gap={2} vAlign="center" width="100%">
          <FilterBar
            meta={meta}
            specs={specs}
            filters={filters}
            companyId={company?.id ?? null}
            resultCount={list.data?.total}
            label={t("filter.placeholder", { label: docType.plural })}
            onChange={(next) => url.set({ f: next.length ? JSON.stringify(next) : null, page: null })}
          />
          <SavedViews tableKey={`document.${docType.name}`} columns={null} onColumns={() => {}} />
        </HStack>}
        empty={
          <Center padding={6}>
            <EmptyState
              title={
                !company
                  ? t("list.needs_company", { label: docType.plural })
                  : filtered
                    ? t("list.no_match_filtered")
                    : t("list.empty_in_company", { label: docType.plural, company: company.name })
              }
              actions={filtered ? <Button label={t("list.clear_filters")} size="sm" onClick={clearFilters} /> : (newButton ?? undefined)}
            />
          </Center>
        }
      />
    </Page>
  );
}

export default DocumentListPage;
