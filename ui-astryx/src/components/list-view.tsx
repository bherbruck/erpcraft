import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { EmptyState } from "@astryxdesign/core/EmptyState";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Layout, LayoutContent, LayoutFooter, LayoutHeader } from "@astryxdesign/core/Layout";
import { Pagination } from "@astryxdesign/core/Pagination";
import { Skeleton } from "@astryxdesign/core/Skeleton";
import { Table, type TableColumn, useTableSortable } from "@astryxdesign/core/Table";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import { VStack } from "@astryxdesign/core/VStack";
import { Search } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { t } from "@/i18n/t";
import type { Async } from "@/lib/use-async";
import type { useListState } from "@/lib/url-state";

type ListState = ReturnType<typeof useListState>;

interface ListViewProps<T extends Record<string, unknown>> {
  title: string;
  plural: string;
  searchLabel: string;
  state: ListState;
  result: Async<{ items: T[]; total: number }>;
  columns: TableColumn<T>[];
  idKey: keyof T & string;
  newHref: string;
  newLabel: string;
  filters?: ReactNode;
}

/** One list for every screen: server-side search, sort and paging, all in the URL. */
export function ListView<T extends Record<string, unknown>>(props: ListViewProps<T>) {
  const { state, result, columns } = props;
  // The input shows what is typed at once; the URL (and the request) follows.
  const [draft, setDraft] = useState(state.q);
  useEffect(() => setDraft(state.q), [state.q]);
  useEffect(() => {
    if (draft === state.q) return;
    const timer = setTimeout(() => state.setQ(draft), 250);
    return () => clearTimeout(timer);
  }, [draft, state]);

  const sortKey = state.sort.replace(/^-/, "");
  const sortable = useTableSortable<T>({
    sort: [{ sortKey, direction: state.sort.startsWith("-") ? "descending" : "ascending" }],
    onSortChange: (next) => {
      const first = next[0];
      if (first) state.setSort(`${first.direction === "descending" ? "-" : ""}${first.sortKey}`);
    },
  });

  const data = result.data;
  const loadingFirst = result.state === "loading" && data === null;

  let body: ReactNode;
  if (loadingFirst) {
    body = (
      <VStack gap={2} padding={4}>
        {Array.from({ length: 8 }, (_, i) => (
          <Skeleton key={String(i)} height={28} index={i} />
        ))}
      </VStack>
    );
  } else if (data && data.items.length === 0) {
    body = state.filtered ? (
      <EmptyState
        title={t("list.emptyFiltered", { plural: props.plural.toLowerCase() })}
        actions={<Button label={t("list.clearFilters")} onClick={state.clear} />}
      />
    ) : (
      <EmptyState
        title={t("list.empty", { plural: props.plural.toLowerCase() })}
        description={t("list.emptyHint")}
        actions={<Button label={props.newLabel} variant="primary" href={props.newHref} />}
      />
    );
  } else {
    body = (
      <Table<T>
        data={data?.items ?? []}
        columns={columns}
        idKey={props.idKey}
        density="compact"
        hasHover
        textOverflow="truncate"
        plugins={{ sortable }}
        rowIndexStart={(state.page - 1) * state.size + 1}
        rowCount={data?.total}
      />
    );
  }

  return (
    <Layout
      height="fill"
      header={<LayoutHeader hasDivider padding={4}>
        <VStack gap={3}>
          <HStack hAlign="between" vAlign="center" gap={3} wrap="wrap">
            <HStack gap={2} vAlign="center">
              <Heading level={1}>{props.title}</Heading>
              {data ? <Text type="supporting">{t("list.count", { total: data.total })}</Text> : null}
            </HStack>
            <Button label={props.newLabel} variant="primary" href={props.newHref} />
          </HStack>
          <HStack gap={2} wrap="wrap" vAlign="end">
            <TextInput
              label={t("list.search")}
              isLabelHidden
              type="search"
              placeholder={props.searchLabel}
              value={draft}
              onChange={setDraft}
              startIcon={Search}
              hasClear
              isLoading={result.state === "loading" && data !== null}
              width={320}
              size="sm"
            />
            {props.filters}
          </HStack>
        </VStack>
      </LayoutHeader>}
      content={<LayoutContent padding={0}>
        {result.state === "error" ? (
          <VStack padding={4}>
            <Banner status="error" title={t("list.loadFailed")} description={result.error.message} />
          </VStack>
        ) : null}
        {body}
      </LayoutContent>}
      footer={<LayoutFooter hasDivider padding={2}>
        <Pagination
          page={state.page}
          onChange={state.setPage}
          totalItems={data?.total ?? 0}
          pageSize={state.size}
          pageSizeOptions={[25, 50, 100, 200]}
          onPageSizeChange={state.setSize}
          size="sm"
        />
      </LayoutFooter>}
    />
  );
}
