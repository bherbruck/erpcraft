import { Plus } from "lucide-react";
import { useCallback, useMemo } from "react";
import { Link, useParams } from "react-router";
import { api } from "@/api/client";
import type { EntityMeta, EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Button } from "@/components/button";
import { FieldValue } from "@/components/field-value";
import { fieldColumn, ListGrid } from "@/components/list-grid";
import { PageHeader } from "@/components/page";
import { SavedViews } from "@/components/saved-views";
import { SearchBox } from "@/components/search-box";
import { t } from "@/i18n/t";
import { useListUrl } from "@/lib/list-url";
import {
  COMPANY,
  canEntity,
  canRun,
  createCommand,
  fieldReach,
  findEntity,
  isCompanyScoped,
  listFields,
} from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { hasCommandPage } from "./command-page";
import { SETUP_COMMAND } from "./company-setup";
import { EDI_MESSAGE, EDI_RECEIVE } from "./edi";
import { NotFoundPage } from "./not-found";
import { SSO_PROVIDER, SsoProviderListPage } from "./sso-providers";

const PAGE_SIZE = 25;
const MAX_COLUMNS = 8;

export function EntityListPage() {
  const { meta } = useApp();
  const params = useParams();
  const entity = findEntity(meta, params.entity);
  if (!entity || !canEntity(meta, entity.name, "read")) return <NotFoundPage />;
  if (entity.name === SSO_PROVIDER) return <SsoProviderListPage entity={entity} />;
  return <EntityList key={entity.name} entity={entity} />;
}

function EntityList({ entity }: { entity: EntityMeta }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const q = url.get("q");
  const { sort, page, size, setQuery } = useListUrl(PAGE_SIZE);
  const scoped = isCompanyScoped(entity);
  const searchable = entity.search.length > 0;
  const canCreate =
    (!entity.read_only && canEntity(meta, entity.name, "create")) ||
    (entity.read_only && createCommand(meta, entity.name) !== undefined);
  const waitingForCompany = scoped && company === null;

  const list = useAsync(
    waitingForCompany ? null : `${entity.name}|${company?.id ?? ""}|${q}|${sort.join()}|${page}|${size}`,
    (signal) =>
      api.run(
        "entity.list",
        {
          entity: entity.name,
          q: searchable && q ? q : null,
          sort,
          filters: scoped && company ? { company_id: company.id } : {},
          limit: size,
          offset: (page - 1) * size,
        },
        { signal },
      ),
  );

  const fields = useMemo(() => listFields(meta, entity), [meta, entity]);
  const columns = useMemo(
    () =>
      fields.map((field, index) =>
        fieldColumn<EntityRecord>(
          field,
          // A field the user may read on some records only arrives without a value on the others.
          (record) =>
            !(field.name in record) && fieldReach(meta, entity.name, field.name, "read") === "some"
              ? t("value.withheld")
              : record[field.name],
          // When something was recorded matters rarely in a list; wide tables start with their first columns.
          { hidden: field.type.kind === "timestamp" || index >= MAX_COLUMNS },
        ),
      ),
    [fields, meta, entity.name],
  );
  // Amounts and quantities are shown as the record page shows them: with their currency or unit.
  const decimals = useMemo(
    () => new Map(fields.filter((field) => field.type.kind === "decimal").map((field) => [field.name, field])),
    [fields],
  );

  const setUrl = url.set;
  const onSearch = useCallback((next: string) => setUrl({ q: next, page: null }, { replace: true }), [setUrl]);

  // A company is best started from a template that sets it up ready to work.
  const setupButton =
    entity.name === COMPANY && canRun(meta, SETUP_COMMAND) ? (
      <Button asChild variant="outline">
        <Link to={paths.setup(code)}>{t("setup.new_company")}</Link>
      </Button>
    ) : null;
  // Commands with a screen of their own that make records of this list: receiving an EDI file makes
  // the interchange and its messages, so both lists offer it.
  const makers = meta.commands.filter(
    (command) =>
      hasCommandPage(command.name) &&
      canRun(meta, command.name) &&
      (command.resource === entity.name || (command.name === EDI_RECEIVE && entity.name === EDI_MESSAGE)),
  );
  const makerButtons = makers.map((command) => (
    <Button key={command.name} asChild variant="outline">
      <Link to={paths.run(code, command.name)}>{command.label}</Link>
    </Button>
  ));
  const newButton = canCreate ? (
    <Button asChild>
      <Link to={paths.entityNew(code, entity.name)}>
        <Plus aria-hidden="true" />
        {t("list.new", { label: entity.label })}
      </Link>
    </Button>
  ) : null;

  return (
    <>
      <PageHeader
        title={entity.plural}
        actions={
          <>
            {setupButton}
            {makerButtons}
            {newButton}
          </>
        }
      />
      <ListGrid<EntityRecord>
        key={`${entity.name}|${q}`}
        id={`entity.${entity.name}`}
        label={entity.plural}
        columns={columns}
        rows={waitingForCompany ? [] : list.data?.items}
        total={list.data?.total ?? 0}
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        sort={sort}
        page={page}
        size={size}
        onQuery={setQuery}
        rowHref={(record) => paths.entityRecord(code, entity.name, record.id)}
        linkColumn={columns[0]?.id ?? "id"}
        decorate={(columnId, record) => {
          const field = decimals.get(columnId);
          return field && columnId in record ? <FieldValue field={field} value={record[columnId]} /> : undefined;
        }}
        toolbar={
          <div className="flex flex-wrap items-center gap-2">
            {searchable ? (
              <SearchBox value={q} onChange={onSearch} label={t("list.search", { label: entity.plural })} />
            ) : null}
            <div className="ms-auto">
              <SavedViews tableKey={`entity.${entity.name}`} columns={null} onColumns={() => undefined} />
            </div>
          </div>
        }
        empty={
          waitingForCompany ? (
            <p>{t("list.needs_company", { label: entity.plural })}</p>
          ) : q ? (
            <>
              <p>{t("list.no_match", { query: q })}</p>
              <Button variant="outline" size="sm" onClick={() => url.set({ q: null, page: null })}>
                {t("list.clear_search")}
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
              <p>
                {scoped && company
                  ? t("list.empty_in_company", { label: entity.plural, company: company.name })
                  : t("list.empty", { label: entity.plural })}
              </p>
              {newButton}
            </>
          )
        }
      />
    </>
  );
}
