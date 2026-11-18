import { Button } from "@astryxdesign/core/Button";
import { Center } from "@astryxdesign/core/Center";
import { EmptyState } from "@astryxdesign/core/EmptyState";
import { HStack } from "@astryxdesign/core/HStack";
import { useMemo } from "react";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { EntityMeta, EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { FilterBar, type FilterSpec, filterSpec, parseFilters, toQuery } from "@/components/filter-bar";
import { fieldColumn, ListGrid } from "@/components/list-grid";
import { Page } from "@/components/page";
import { SavedViews } from "@/components/saved-views";
import { t } from "@/i18n/t";
import { COMPANY, canEntity, canRun, fieldReach, findEntity, isCompanyScoped, listFields } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useListUrl } from "@/lib/list-url";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

const MAX_COLUMNS = 8;

export function EntityListPage() {
  const { meta } = useApp();
  const params = useParams();
  const entity = findEntity(meta, params.entity);
  if (!entity || !canEntity(meta, entity.name, "read")) return <NotFoundPage />;
  return <EntityList key={entity.name} entity={entity} />;
}

export default EntityListPage;

/** The list of any entity, generated from the registry: its fields are the grid's columns. */
function EntityList({ entity }: { entity: EntityMeta }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const { url, sort, page, size, setQuery } = useListUrl();
  const filters = parseFilters(url.get("f"));
  const specs = useMemo(() => listFields(meta, entity).flatMap((field) => filterSpec(field) ?? []) as FilterSpec[], [meta, entity]);
  const query = toQuery(filters, specs);
  const scoped = isCompanyScoped(entity);
  const searchable = entity.search.length > 0;
  const canCreate = !entity.read_only && canEntity(meta, entity.name, "create");
  const waitingForCompany = scoped && company === null;
  const filterKey = `${query.q}|${query.where}`;

  const list = useAsync(
    waitingForCompany ? null : `${entity.name}|${company?.id ?? ""}|${filterKey}|${sort.join()}|${page}|${size}`,
    (signal) =>
      api.run(
        "entity.list",
        {
          entity: entity.name,
          q: searchable && query.q ? query.q : null,
          where: query.where,
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
            !(field.name in record) && fieldReach(meta, entity.name, field.name, "read") === "some" ? t("value.withheld") : record[field.name],
          { hidden: field.type.kind === "timestamp" || index >= MAX_COLUMNS },
        ),
      ),
    [fields, meta, entity.name],
  );
  const firstColumn = columns[0]?.id ?? "id";

  const setupButton =
    entity.name === COMPANY && canRun(meta, "company.setup") ? <Button label={t("setup.new_company")} href={paths.setup(code)} /> : null;
  const newButton = canCreate ? (
    <Button label={t("list.new", { label: entity.label })} variant="primary" href={paths.entityNew(code, entity.name)} />
  ) : null;

  const emptyTitle = waitingForCompany
    ? t("list.needs_company", { label: entity.plural })
    : filters.length > 0
      ? t("list.no_match_filtered")
      : scoped && company
        ? t("list.empty_in_company", { label: entity.plural, company: company.name })
        : t("list.empty", { label: entity.plural });

  return (
    <Page
      title={entity.plural}
      actions={
        <>
          {setupButton}
          {newButton}
        </>
      }
    >
      <ListGrid<EntityRecord>
        key={`${entity.name}|${filterKey}`}
        id={`entity.${entity.name}`}
        label={entity.plural}
        columns={columns}
        rows={waitingForCompany ? [] : list.data?.items}
        total={list.data?.total ?? 0}
        loading={list.loading}
        sort={sort}
        page={page}
        size={size}
        onQuery={setQuery}
        rowHref={(record) => paths.entityRecord(code, entity.name, record.id)}
        linkColumn={firstColumn}
        toolbar={<HStack gap={2} vAlign="center" width="100%">
          <FilterBar
            meta={meta}
            specs={specs}
            filters={filters}
            companyId={company?.id ?? null}
            resultCount={list.data?.total}
            label={t("filter.placeholder", { label: entity.plural })}
            onChange={(next) => url.set({ f: next.length ? JSON.stringify(next) : null, page: null })}
          />
          <SavedViews tableKey={`entity.${entity.name}`} columns={null} onColumns={() => {}} />
        </HStack>}
        empty={
          <Center padding={6}>
            <EmptyState
              title={emptyTitle}
              actions={
                filters.length > 0 ? (
                  <Button label={t("list.clear_filters")} size="sm" onClick={() => url.set({ f: null, page: null })} />
                ) : (
                  (newButton ?? undefined)
                )
              }
            />
          </Center>
        }
      />
    </Page>
  );
}
