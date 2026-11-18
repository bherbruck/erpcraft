import { Button } from "@astryxdesign/core/Button";
import { HStack } from "@astryxdesign/core/HStack";
import { useMemo } from "react";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { EntityMeta, EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable, useColumnChoice } from "@/components/data-table";
import { FieldValue, isNumeric, Withheld } from "@/components/field-value";
import { Page } from "@/components/page";
import { FilterBar, filterSpec, parseFilters, toQuery, type FilterSpec } from "@/components/filter-bar";
import { t } from "@/i18n/t";
import { canEntity, fieldReach, findEntity, isCompanyScoped, listFields } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { pageFromUrl, useUrlState } from "@/lib/url-state";
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

/** The list of any entity, generated from the registry: its fields are the columns. */
function EntityList({ entity }: { entity: EntityMeta }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const filters = parseFilters(url.get("f"));
  const specs = useMemo(
    () => listFields(meta, entity).flatMap((field) => filterSpec(field) ?? []) as FilterSpec[],
    [meta, entity],
  );
  const query = toQuery(filters, specs);
  const q = query.q;
  const where = query.where;
  const sort = url.get("sort");
  const page = pageFromUrl(url.get("page"));
  const size = Number(url.get("size")) || 25;
  const scoped = isCompanyScoped(entity);
  const searchable = entity.search.length > 0;
  const canCreate = !entity.read_only && canEntity(meta, entity.name, "create");
  const waitingForCompany = scoped && company === null;

  const list = useAsync(
    waitingForCompany ? null : `${entity.name}|${company?.id ?? ""}|${q}|${where}|${sort}|${page}|${size}`,
    (signal) =>
      api.run(
        "entity.list",
        {
          entity: entity.name,
          q: searchable && q ? q : null,
          where,
          sort: sort ? [sort] : [],
          filters: scoped && company ? { company_id: company.id } : {},
          limit: size,
          offset: (page - 1) * size,
        },
        { signal },
      ),
  );

  const columns = useMemo<Column<EntityRecord>[]>(
    () =>
      listFields(meta, entity).map((field, index) => ({
        id: field.name,
        header: field.label,
        align: isNumeric(field) ? "end" : "start",
        sortField: field.name,
        width: field.type.kind === "bool" ? 90 : field.type.kind === "date" ? 130 : undefined,
        defaultHidden: field.type.kind === "timestamp" || index >= MAX_COLUMNS,
        cell: (record) =>
          !(field.name in record) && fieldReach(meta, entity.name, field.name, "read") === "some" ? (
            <Withheld />
          ) : (
            <FieldValue field={field} value={record[field.name]} />
          ),
      })),
    [meta, entity],
  );
  const { visible, chooser } = useColumnChoice(`entity.${entity.name}`, columns);

  const newButton = canCreate ? (
    <Button label={t("list.new", { label: entity.label })} variant="primary" href={paths.entityNew(code, entity.name)} />
  ) : null;

  return (
    <Page
      title={entity.plural}
      actions={newButton}
      padded={false}
      toolbar={
        <HStack gap={2} vAlign="center">
          <FilterBar
            meta={meta}
            specs={specs}
            filters={filters}
            companyId={company?.id ?? null}
            resultCount={list.data?.total}
            label={t("filter.placeholder", { label: entity.plural })}
            onChange={(next) => url.set({ f: next.length ? JSON.stringify(next) : null, page: null })}
          />
          {chooser}
        </HStack>
      }
    >
      <DataTable
        caption={entity.plural}
        columns={visible}
        rows={waitingForCompany ? [] : list.data?.items}
        rowKey={(record) => record.id}
        rowHref={(record) => paths.entityRecord(code, entity.name, record.id)}
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
          waitingForCompany
            ? { title: t("list.needs_company", { label: entity.plural }) }
            : filters.length > 0
              ? {
                  title: t("list.no_match_filtered"),
                  action: <Button label={t("list.clear_filters")} size="sm" onClick={() => url.set({ f: null, page: null })} />,
                }
              : page > 1
                ? {
                    title: t("list.past_end"),
                    action: <Button label={t("list.first_page")} size="sm" onClick={() => url.set({ page: null })} />,
                  }
                : {
                    title:
                      scoped && company
                        ? t("list.empty_in_company", { label: entity.plural, company: company.name })
                        : t("list.empty", { label: entity.plural }),
                    action: newButton ?? undefined,
                  }
        }
      />
    </Page>
  );
}

export default EntityListPage;
