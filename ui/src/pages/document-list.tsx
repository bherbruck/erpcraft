import { Plus } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { api } from "@/api/client";
import type { DocTypeMeta, DocumentSummary, FieldMeta, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { BulkBar, type BulkResult, BulkResults } from "@/components/bulk-actions";
import { useColumnChoice } from "@/components/column-chooser";
import { type Column, DataTable } from "@/components/data-table";
import { useDecimalHeader } from "@/components/decimal-value";
import { Empty, FieldValue, isNumeric } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { PageHeader } from "@/components/page";
import { SavedViews } from "@/components/saved-views";
import { SearchBox } from "@/components/search-box";
import { StatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { canDoc, canEntity, canRun, findDocType, withoutSource } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { pageFromUrl, useUrlState } from "@/lib/url-state";
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

/** The header of a summary column: money in the company's currency says which. */
function SummaryHeader({ field }: { field: FieldMeta }) {
  return <>{useDecimalHeader(field)}</>;
}

function DocumentList({ docType }: { docType: DocTypeMeta }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const q = url.get("q");
  const status = url.get("status");
  const sort = url.get("sort");
  const page = pageFromUrl(url.get("page"));
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
    company ? `${docType.name}|${company.id}|${q}|${status}|${sort}|${page}|${filterKey}` : null,
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
          sort: sort ? [sort] : ["-doc_date", "-created_at"],
          limit: PAGE_SIZE,
          offset: (page - 1) * PAGE_SIZE,
        },
        { signal },
      ),
  );

  const columns = useMemo<Column<DocumentSummary>[]>(() => {
    const header = (name: string, fallback: string): string =>
      meta.document_header.find((field) => field.name === name)?.label ?? fallback;
    // A summary shows who the document is with by name; a link to the party,
    // or to a document whose label names the party again, starts hidden.
    const named = docType.summary.some((field) => field.type.kind === "text");
    const summary = docType.summary.map(
      (field): Column<DocumentSummary> => ({
        id: `summary.${field.name}`,
        header: field.label,
        headerNode: field.type.kind === "decimal" ? <SummaryHeader field={field} /> : undefined,
        sortField:
          field.type.kind === "link" || field.type.kind === "document_link" ? undefined : `summary.${field.name}`,
        align: isNumeric(field) ? "end" : "start",
        // A figure that repeats another in the accounting currency (`total_value`, `*_accounting`)
        // starts hidden: in a single-currency company it says the same twice.
        defaultHidden:
          ((field.type.kind === "link" || field.type.kind === "document_link") && named) ||
          field.type.kind === "entity_link" ||
          field.name.endsWith("_accounting") ||
          (field.name === "total_value" && docType.summary.some((other) => other.name === "total_gross")),
        // Names stay on one line; the memo takes what width is left.
        className: "whitespace-nowrap",
        cell: (doc) => {
          const values = summaryOf(doc);
          return (
            <FieldValue
              field={field}
              value={values[field.name]}
              context={{ header: values, fields: docType.summary }}
            />
          );
        },
      }),
    );
    return [
      {
        id: "number",
        header: header("number", t("document.number")),
        sortField: "number",
        cell: (doc) =>
          doc.number ? (
            <span className="whitespace-nowrap font-medium tabular-nums">{doc.number}</span>
          ) : (
            <span className="whitespace-nowrap text-muted-foreground">{t("document.unnumbered")}</span>
          ),
      },
      {
        id: "doc_date",
        header: header("doc_date", t("document.date")),
        sortField: "doc_date",
        cell: (doc) => <span className="whitespace-nowrap">{formatCalendarDate(doc.doc_date)}</span>,
      },
      {
        id: "status",
        header: header("status", t("document.status")),
        sortField: "status",
        cell: (doc) => <StatusBadge status={doc.status} docType={docType} />,
      },
      ...summary,
      {
        id: "memo",
        header: header("memo", t("document.memo")),
        cell: (doc) => doc.memo || <Empty />,
        className: "w-full min-w-48",
      },
      {
        id: "created_by_name",
        header: header("created_by_name", t("document.created_by_column")),
        defaultHidden: true,
        cell: (doc) => <span className="whitespace-nowrap">{doc.created_by_name ?? <Empty />}</span>,
      },
      {
        id: "created_at",
        header: header("created_at", t("document.created")),
        sortField: "created_at",
        defaultHidden: true,
        cell: (doc) => (
          <span className="whitespace-nowrap text-muted-foreground">{formatTimestamp(doc.created_at)}</span>
        ),
      },
      {
        id: "updated_at",
        header: header("updated_at", t("document.updated")),
        sortField: "updated_at",
        defaultHidden: true,
        cell: (doc) => (
          <span className="whitespace-nowrap text-muted-foreground">{formatTimestamp(doc.updated_at)}</span>
        ),
      },
    ];
  }, [docType, meta.document_header]);

  const {
    visible,
    chooser,
    chosen: chosenColumns,
    choose: chooseColumns,
  } = useColumnChoice(`document.${docType.name}`, columns);

  // Documents chosen for one action on all of them, kept across pages of the list.
  const [chosen, setChosen] = useState<Map<string, DocumentSummary>>(() => new Map());
  const [bulk, setBulk] = useState<{ action: string; results: BulkResult[] } | null>(null);
  const canBulk = meta.commands.some(
    (command) => command.resource === docType.name && command.target && !command.read_only && command.creates === null,
  );

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
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <SearchBox value={q} onChange={onSearch} label={t("list.search", { label: docType.plural })} />
        <Select
          aria-label={t("document.filter_status")}
          className="w-40"
          value={status}
          onChange={(event) => url.set({ status: event.target.value, page: null })}
        >
          <option value="">{t("document.any_status")}</option>
          {docType.states.map((state) => (
            <option key={state.name} value={state.name}>
              {state.label}
            </option>
          ))}
        </Select>
        {partyFilters.map((field) =>
          field.type.kind === "link" ? (
            <Lookup
              key={field.name}
              aria-label={t("document.filter_by", { label: field.label })}
              placeholder={t("document.any_of", { label: field.label })}
              className="w-64 max-w-full"
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
        <div className="ms-auto flex gap-2">
          <SavedViews tableKey={`document.${docType.name}`} columns={chosenColumns} onColumns={chooseColumns} />
          {chooser}
        </div>
      </div>
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
      <DataTable
        caption={docType.plural}
        selection={
          canBulk
            ? {
                selected: new Set(chosen.keys()),
                label: (doc) => t("bulk.choose", { number: doc.number ?? doc.memo ?? doc.id }),
                onToggle: (doc, on) =>
                  setChosen((previous) => {
                    const next = new Map(previous);
                    if (on) next.set(doc.id, doc);
                    else next.delete(doc.id);
                    return next;
                  }),
              }
            : undefined
        }
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
        pageSize={PAGE_SIZE}
        total={list.data?.total ?? 0}
        onPage={(next) => url.set({ page: next === 1 ? null : String(next) })}
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
