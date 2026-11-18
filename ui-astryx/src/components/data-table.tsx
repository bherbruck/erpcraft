// The one table for every list, on Astryx's Table. Kept in one file on purpose:
// the headless grid (packages/grid) is to replace what is inside, not its props.

import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { DropdownMenu, DropdownMenuCheckboxItem } from "@astryxdesign/core/DropdownMenu";
import { EmptyState } from "@astryxdesign/core/EmptyState";
import { HStack } from "@astryxdesign/core/HStack";
import { Link } from "@astryxdesign/core/Link";
import { Pagination } from "@astryxdesign/core/Pagination";
import { Skeleton } from "@astryxdesign/core/Skeleton";
import { pixel, proportional, Table, type TableColumn, useTableSortable } from "@astryxdesign/core/Table";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { Columns3 } from "lucide-react";
import { type ReactNode, useMemo } from "react";
import type { ApiError } from "@/api/client";
import { t } from "@/i18n/t";
import { isStringArray } from "@/lib/storage";
import { useStoredState } from "./use-stored";

export interface Column<Row> {
  id: string;
  header: string;
  cell: (row: Row) => ReactNode;
  align?: "start" | "end";
  /** The field the server sorts by when the header is clicked. */
  sortField?: string;
  /** Not shown until the user asks for it. */
  defaultHidden?: boolean;
  /** Fixed width in pixels, for short values such as dates and statuses. */
  width?: number;
}

export interface DataTableProps<Row> {
  caption: string;
  columns: Column<Row>[];
  /** `undefined` while the first page loads. */
  rows: Row[] | undefined;
  rowKey: (row: Row) => string;
  /** Makes the row's first cell a link to its record. */
  rowHref?: (row: Row) => string | undefined;
  loading?: boolean;
  error?: ApiError | null;
  onRetry?: () => void;
  /** What to say, and offer, when there are no rows. */
  empty: { title: string; description?: string; action?: ReactNode };
  /** Current sort as the server takes it: a field name, `-` first for descending. */
  sort?: string;
  onSort?: (sort: string) => void;
  page?: number;
  pageSize?: number;
  total?: number;
  onPage?: (page: number) => void;
  onPageSize?: (size: number) => void;
  /** Where the user's column choice is kept, per list. */
  storageKey?: string;
}

type Cells = Record<string, unknown> & { __row: unknown; __key: string };

/** The columns the user chose for a list, kept in the browser. */
export function useColumnChoice<Row>(storageKey: string | undefined, columns: Column<Row>[]) {
  const defaults = useMemo(() => columns.filter((c) => !c.defaultHidden).map((c) => c.id), [columns]);
  const [chosen, setChosen] = useStoredState<string[]>(
    storageKey ? `columns.${storageKey}` : null,
    defaults,
    isStringArray,
  );
  const known = new Set(columns.map((c) => c.id));
  const active = chosen.filter((id) => known.has(id));
  const visible = (active.length ? active : defaults).flatMap((id) => columns.filter((c) => c.id === id));
  const chooser = (
    <DropdownMenu
      button={{ label: t("columns.button"), size: "sm", variant: "secondary", icon: <Columns3 size={14} aria-hidden /> }}
      alignment="end"
    >
      {columns.map((column) => (
        <DropdownMenuCheckboxItem
          key={column.id}
          label={column.header}
          value={visible.some((v) => v.id === column.id)}
          onChange={(on) => {
            const current = visible.map((v) => v.id);
            const next = on ? columns.map((c) => c.id).filter((id) => id === column.id || current.includes(id)) : current.filter((id) => id !== column.id);
            if (next.length > 0) setChosen(next);
          }}
        />
      ))}
    </DropdownMenu>
  );
  return {
    visible,
    chooser,
    /** The user's choice, or null for the defaults: what a saved view keeps. */
    chosen: active.length ? active : null,
    choose: (next: string[] | null) => setChosen(next ?? defaults),
  };
}

export function DataTable<Row>(props: DataTableProps<Row>) {
  const { columns, rows, rowKey, rowHref, sort = "" } = props;
  const descending = sort.startsWith("-");
  const sortField = descending ? sort.slice(1) : sort;

  const sortable = useTableSortable<Cells>({
    sort: sortField ? [{ sortKey: sortField, direction: descending ? "descending" : "ascending" }] : [],
    onSortChange: (next) => {
      const first = next[0];
      if (first && props.onSort) props.onSort(`${first.direction === "descending" ? "-" : ""}${first.sortKey}`);
    },
  });

  const tableColumns: TableColumn<Cells>[] = columns.map((column, index) => ({
    key: column.id,
    header: column.header,
    align: column.align === "end" ? "end" : "start",
    width: column.width ? pixel(column.width) : proportional(1, { minWidth: 120 }),
    sortable: column.sortField && props.onSort ? { sortKey: column.sortField } : undefined,
    renderCell: (cells) => {
      const row = cells.__row as Row;
      const content = column.cell(row);
      const href = index === 0 ? rowHref?.(row) : undefined;
      return href ? <Link href={href}>{content}</Link> : content;
    },
  }));

  const data: Cells[] = (rows ?? []).map((row) => ({ __row: row, __key: rowKey(row) }));
  const paged = props.page !== undefined && props.pageSize !== undefined && props.total !== undefined && props.onPage;

  let body: ReactNode;
  if (rows === undefined && !props.error) {
    body = (
      <VStack gap={2} padding={4}>
        {Array.from({ length: 8 }, (_, i) => (
          <Skeleton key={String(i)} height={24} index={i} />
        ))}
      </VStack>
    );
  } else if (rows && rows.length === 0) {
    body = (
      <EmptyState
        title={props.empty.title}
        description={props.empty.description}
        actions={props.empty.action}
        isCompact
      />
    );
  } else {
    body = (
      <div className={props.loading ? "reloading" : undefined} aria-busy={props.loading || undefined}>
        <Table<Cells>
          data={data}
          columns={tableColumns}
          idKey="__key"
          density="compact"
          hasHover
          textOverflow="truncate"
          plugins={props.onSort ? { sortable } : undefined}
          rowIndexStart={paged ? ((props.page ?? 1) - 1) * (props.pageSize ?? 0) + 1 : undefined}
          rowCount={props.total}
        />
      </div>
    );
  }

  return (
    <VStack gap={0} height="100%">
      {props.error ? (
        <VStack padding={3}>
          <Banner
            status="error"
            title={t("list.load_failed")}
            description={props.error.message}
            endContent={props.onRetry ? <Button label={t("action.retry")} size="sm" onClick={props.onRetry} /> : undefined}
          />
        </VStack>
      ) : null}
      {/* A scrolling region is a tab stop, so the keyboard can scroll a table without links in it. */}
      {/* biome-ignore lint/a11y/noNoninteractiveTabindex: scrollable region must be reachable by keyboard */}
      <div className="table-scroll" role="region" aria-label={props.caption} tabIndex={0}>
        {body}
      </div>
      {paged ? (
        <div className="table-footer">
          <HStack hAlign="between" vAlign="center" gap={3} wrap="wrap">
            <Text type="supporting">
              {props.total
                ? t("list.count", {
                    first: ((props.page ?? 1) - 1) * (props.pageSize ?? 0) + 1,
                    last: Math.min((props.page ?? 1) * (props.pageSize ?? 0), props.total ?? 0),
                    total: (props.total ?? 0).toLocaleString(),
                  })
                : t("list.count_none")}
            </Text>
            <Pagination
              page={props.page ?? 1}
              onChange={(next) => props.onPage?.(next)}
              totalItems={props.total ?? 0}
              pageSize={props.pageSize}
              pageSizeOptions={props.onPageSize ? [25, 50, 100] : undefined}
              onPageSizeChange={props.onPageSize}
              size="sm"
            />
          </HStack>
        </div>
      ) : null}
    </VStack>
  );
}

