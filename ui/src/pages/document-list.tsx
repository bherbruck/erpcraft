import type { GridColumn } from "@erpcraft/grid";
import { Plus } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { api } from "@/api/client";
import type { DocTypeMeta, DocumentSummary, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { BulkBar, type BulkResult, BulkResults } from "@/components/bulk-actions";
import { Button } from "@/components/button";
import { currencyInHeader } from "@/components/decimal-value";
import { fieldColumn, ListGrid } from "@/components/list-grid";
import { Lookup } from "@/components/lookup";
import { PageHeader } from "@/components/page";
import { SavedViews } from "@/components/saved-views";
import { SearchBox } from "@/components/search-box";
import { StatusBadge } from "@/components/status-badge";
import { NativeSelect } from "@/components/ui/native-select";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { useListUrl } from "@/lib/list-url";
import { canDoc, canEntity, canRun, findDocType, withoutSource } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { hasCommandPage } from "./command-page";
import { NotFoundPage } from "./not-found";

const PAGE_SIZE = 25;
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

/** The column of checkboxes that chooses documents for a bulk action. */
const CHOSEN = "_chosen";
type Row = DocumentSummary & JsonObject;

function DocumentList({ docType }: { docType: DocTypeMeta }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const q = url.get("q");
  const status = url.get("status");
  const { sort, page, size, setQuery } = useListUrl(PAGE_SIZE);
  const { currencyDecimals } = useApp();
  const canCreate = canDoc(meta, docType.name, "create") && company !== null;

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
    company ? `${docType.name}|${company.id}|${q}|${status}|${sort.join()}|${page}|${size}|${filterKey}` : null,
    (signal) =>
      api.run(
        "document.list",
        {
          doc_type: docType.name,
          company_id: company?.id ?? null,
          q: q || null,
          status: status || null,
          summary: summaryFilter,
          // Newest first unless the user chose otherwise.
          sort: sort.length > 0 ? sort : ["-doc_date", "-created_at"],
          limit: size,
          offset: (page - 1) * size,
        },
        { signal },
      ),
  );

  // Documents chosen for one action on all of them, kept across pages of the list.
  const [chosen, setChosen] = useState<Map<string, DocumentSummary>>(() => new Map());
  const [bulk, setBulk] = useState<{ action: string; results: BulkResult[] } | null>(null);
  const canBulk = meta.commands.some(
    (command) => command.resource === docType.name && command.target && !command.read_only && command.creates === null,
  );

  const choose = (doc: DocumentSummary, on: boolean): void =>
    setChosen((previous) => {
      const next = new Map(previous);
      if (on) next.set(doc.id, doc);
      else next.delete(doc.id);
      return next;
    });

  // The grid's columns: the header, the state, the type's summary and the memo.
  const columns = useMemo<GridColumn<Row>[]>(() => {
    const header = (name: string, fallback: string): string =>
      meta.document_header.find((field) => field.name === name)?.label ?? fallback;
    // A summary shows who the document is with by name; a link to the party,
    // or to a document whose label names the party again, starts hidden.
    const named = docType.summary.some((field) => field.type.kind === "text");
    const text = (
      id: string,
      label: string,
      read: (doc: Row) => string | null,
      extra: Partial<GridColumn<Row>> = {},
    ): GridColumn<Row> => ({
      id,
      header: label,
      type: "text",
      field: id,
      filterable: false,
      searchable: false,
      value: (doc) => read(doc),
      ...extra,
    });
    const timestamp = (id: "created_at" | "updated_at", label: string): GridColumn<Row> => ({
      id,
      header: label,
      type: "datetime",
      field: id,
      filterable: false,
      searchable: false,
      hidden: true,
      value: (doc) => doc[id],
      format: (v) => (typeof v === "string" ? formatTimestamp(v) : ""),
    });
    const chooser: GridColumn<Row>[] = canBulk
      ? [
          {
            id: CHOSEN,
            header: t("list.select"),
            type: "boolean",
            sortable: false,
            filterable: false,
            searchable: false,
            hideable: false,
            width: 72,
            pin: "left",
            editable: true,
            value: (doc) => chosen.has(doc.id),
          },
        ]
      : [];
    return [
      ...chooser,
      text("number", header("number", t("document.number")), (doc) => doc.number ?? t("document.unnumbered"), {
        hideable: false,
        // A number is read whole: it does not give up width to the memo.
        minWidth: 150,
      }),
      {
        id: "doc_date",
        header: header("doc_date", t("document.date")),
        type: "date",
        field: "doc_date",
        filterable: false,
        searchable: false,
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
        options: docType.states.map((state) => ({ value: state.name, label: state.label })),
        value: (doc) => doc.status,
      },
      ...docType.summary.map((field) =>
        fieldColumn<Row>(field, (doc) => summaryOf(doc)[field.name], {
          id: `summary.${field.name}`,
          // Money in the company's currency says which in its header.
          header:
            currencyInHeader(field) && company
              ? t("value.in_currency", { label: field.label, currency: company.base_currency })
              : field.label,
          sortField: field.type.kind === "link" || field.type.kind === "document_link" ? null : `summary.${field.name}`,
          decimals: currencyDecimals[company?.base_currency ?? ""] ?? 2,
          // A figure that repeats another in the accounting currency (`total_value`, `*_accounting`)
          // starts hidden: in a single-currency company it says the same twice.
          hidden:
            ((field.type.kind === "link" || field.type.kind === "document_link") && named) ||
            field.type.kind === "entity_link" ||
            field.name.endsWith("_accounting") ||
            (field.name === "total_value" && docType.summary.some((other) => other.name === "total_gross")),
        }),
      ),
      text("memo", header("memo", t("document.memo")), (doc) => doc.memo, { sortable: false }),
      text(
        "created_by_name",
        header("created_by_name", t("document.created_by_column")),
        (doc) => doc.created_by_name ?? null,
        { hidden: true, sortable: false },
      ),
      timestamp("created_at", header("created_at", t("document.created"))),
      timestamp("updated_at", header("updated_at", t("document.updated"))),
    ];
  }, [docType, meta.document_header, chosen, canBulk, currencyDecimals, company]);

  const setUrl = url.set;
  const onSearch = useCallback((next: string) => setUrl({ q: next, page: null }, { replace: true }), [setUrl]);
  const filtered = q !== "" || status !== "" || Object.keys(summaryFilter).length > 0;
  const clearFilters = (): void => {
    const cleared: Record<string, null> = { q: null, status: null, page: null };
    for (const field of partyFilters) cleared[`${SUMMARY_FILTER}${field.name}`] = null;
    url.set(cleared);
  };

  // Commands that make a document of this type from something else (importing a file) and have a screen.
  const makers = meta.commands.filter(
    (command) =>
      command.creates === docType.name && !command.target && canRun(meta, command.name) && hasCommandPage(command.name),
  );
  // A type that can be made without its usual source (an invoice without an order) offers that too.
  const source = withoutSource(docType);
  const newButton = canCreate ? (
    <>
      {source ? (
        <Button asChild variant="outline">
          <Link to={`${paths.docNew(code, docType.name)}?${new URLSearchParams({ without: source.name })}`}>
            <Plus aria-hidden="true" />
            {t("document.new_without", { label: docType.label, source: source.label })}
          </Link>
        </Button>
      ) : null}
      <Button asChild>
        <Link to={paths.docNew(code, docType.name)}>
          <Plus aria-hidden="true" />
          {t("list.new", { label: docType.label })}
        </Link>
      </Button>
    </>
  ) : null;

  return (
    <>
      <PageHeader
        title={docType.plural}
        actions={
          <>
            {makers.map((command) => (
              <Button key={command.name} asChild variant="outline">
                <Link to={paths.run(code, command.name)}>{command.label}</Link>
              </Button>
            ))}
            {newButton}
          </>
        }
      />
      {bulk ? (
        <BulkResults docType={docType} action={bulk.action} results={bulk.results} onClose={() => setBulk(null)} />
      ) : null}
      {chosen.size > 0 ? (
        <BulkBar
          docType={docType}
          chosen={[...chosen.values()]}
          onClear={() => setChosen(new Map())}
          onDone={(results, action) => {
            setBulk({ action, results });
            setChosen(new Map());
            list.reload();
          }}
        />
      ) : null}
      <ListGrid<Row>
        key={`${docType.name}|${q}|${status}|${filterKey}`}
        id={`document.${docType.name}`}
        label={docType.plural}
        columns={columns}
        rows={company ? (list.data?.items as Row[] | undefined) : []}
        total={list.data?.total ?? 0}
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        sort={sort}
        page={page}
        size={size}
        onQuery={setQuery}
        rowHref={(doc) => paths.doc(code, docType.name, doc.id)}
        linkColumn="number"
        decorate={(columnId, doc) =>
          columnId === "status" ? <StatusBadge status={doc.status} docType={docType} /> : undefined
        }
        check={
          canBulk
            ? {
                columnId: CHOSEN,
                label: (doc) => t("bulk.choose", { number: doc.number ?? doc.memo ?? doc.id }),
                onToggle: choose,
              }
            : undefined
        }
        // The choice column takes typing, paste and Delete like any cell: y/n, x, 1/0.
        onEdit={(changes) => {
          for (const change of changes) if (change.columnId === CHOSEN) choose(change.row, change.newValue === true);
        }}
        toolbar={
          <div className="flex flex-wrap items-center gap-2">
            <SearchBox value={q} onChange={onSearch} label={t("list.search", { label: docType.plural })} />
            <NativeSelect
              aria-label={t("document.filter_status")}
              className="w-36"
              value={status}
              onChange={(event) => url.set({ status: event.target.value, page: null })}
            >
              <option value="">{t("document.any_status")}</option>
              {docType.states.map((state) => (
                <option key={state.name} value={state.name}>
                  {state.label}
                </option>
              ))}
            </NativeSelect>
            {partyFilters.map((field) =>
              field.type.kind === "link" ? (
                <Lookup
                  key={field.name}
                  aria-label={t("document.filter_by", { label: field.label })}
                  placeholder={t("document.any_of", { label: field.label })}
                  className="w-52 max-w-full"
                  entity={field.type.entity}
                  keyField={field.type.key}
                  value={url.get(`${SUMMARY_FILTER}${field.name}`) || null}
                  companyId={company?.id}
                  allowCreate={false}
                  recordLink={false}
                  onChange={(next) => url.set({ [`${SUMMARY_FILTER}${field.name}`]: next, page: null })}
                />
              ) : null,
            )}
            <div className="ms-auto">
              <SavedViews tableKey={`document.${docType.name}`} columns={null} onColumns={() => undefined} />
            </div>
          </div>
        }
        empty={
          !company ? (
            <p>{t("list.needs_company", { label: docType.plural })}</p>
          ) : filtered ? (
            <>
              <p>{t("list.no_match_filtered")}</p>
              <Button variant="outline" size="sm" onClick={clearFilters}>
                {t("list.clear_filters")}
              </Button>
            </>
          ) : page > 1 ? (
            <>
              <p>{t("list.past_end")}</p>
              <Button variant="outline" size="sm" onClick={() => url.set({ page: null })}>
                {t("list.first_page")}
              </Button>
            </>
          ) : (
            <>
              <p>{t("list.empty_in_company", { label: docType.plural, company: company.name })}</p>
              {newButton}
            </>
          )
        }
      />
    </>
  );
}
