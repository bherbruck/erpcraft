// The read-only tables of the app (reports, history, bank reconciliation, approvals) on
// @erpcraft/grid in client mode, Astryx skin: sorting, column layout, the keyboard and copying a
// block to Excel work the same as in the lists. A report the server pages runs in server mode.

import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { EmptyState } from "@astryxdesign/core/EmptyState";
import { VStack } from "@astryxdesign/core/VStack";
import { type ColumnType, Grid, type GridColumn, localStorageLayout } from "@erpcraft/grid";
import { type KeyboardEvent, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import type { ApiError } from "@/api/client";
import { t } from "@/i18n/t";
import { astryxGridSlots } from "./grid-skin";
import { fitWidths } from "./grid-widths";

export interface Column<Row> {
  id: string;
  header: string;
  /** The value: what sorts, and (formatted by `text` or by type) what is shown and copied. */
  value: (row: Row) => unknown;
  type?: ColumnType;
  /** The text shown and copied, where the type's own formatting is not it. */
  text?: (row: Row) => string;
  /** Something richer to draw than the text (a badge, a link); the text is still what is copied. */
  cell?: (row: Row) => ReactNode;
  width?: number;
  /** Decimals of a money column. */
  minFraction?: number;
  /** A total under the column (number and money). */
  total?: boolean;
  hidden?: boolean;
}

export interface DataTableProps<Row> {
  caption: string;
  /** Names the saved column layout. */
  id: string;
  columns: Column<Row>[];
  /** `undefined` while the first page loads. */
  rows: Row[] | undefined;
  rowKey: (row: Row) => string;
  /** The first column's cells link to the row's record; Enter on one opens it. */
  rowHref?: (row: Row) => string | undefined;
  loading?: boolean;
  error?: ApiError | null;
  onRetry?: () => void;
  empty: { title: string; description?: string; action?: ReactNode };
  /** Paging by the server; leave out to show every row. */
  page?: number;
  pageSize?: number;
  total?: number;
  onPage?: (page: number) => void;
  /** Take the height of the space around it rather than of the rows. */
  fill?: boolean;
  /** The page's own controls (filters, tabs): they share the table's toolbar row with its columns menu. */
  toolbar?: ReactNode;
}

const ROW_HEIGHT = 34;

export function DataTable<Row>(props: DataTableProps<Row>) {
  const { columns, rows, rowKey, rowHref } = props;
  const navigate = useNavigate();
  const paged = props.page !== undefined && props.pageSize !== undefined && props.total !== undefined && props.onPage !== undefined;
  const latest = useRef(props);
  latest.current = props;
  const first = columns[0]?.id ?? "";
  // One set of slots for the table's life (see ListGrid).
  // biome-ignore lint/correctness/useExhaustiveDependencies: the props are read through a ref
  const slots = useMemo(
    () =>
      astryxGridSlots<Row>({
        link: { columnId: first, href: (row) => latest.current.rowHref?.(row) },
        decorate: (columnId, row) => latest.current.columns.find((c) => c.id === columnId)?.cell?.(row),
      }),
    [first],
  );
  const gridColumns = useMemo<GridColumn<Row>[]>(
    () =>
      columns.map((column) => ({
        id: column.id,
        header: column.header,
        type: column.type ?? "text",
        value: column.value,
        format: column.text ? (_value, row) => column.text?.(row) ?? "" : undefined,
        width: column.width,
        minFraction: column.minFraction,
        total: column.total,
        hidden: column.hidden,
        filterable: false,
        searchable: false,
        sortable: !paged,
      })),
    [columns, paged],
  );

  // Widths that fit the space there is (see fitWidths), measured once it is known.
  const [available, setAvailable] = useState(0);
  const boxRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const observer = new ResizeObserver(([entry]) => setAvailable(Math.floor((entry?.contentRect.width ?? 0) - 4)));
    observer.observe(box);
    return () => observer.disconnect();
  }, []);
  const fitted = useMemo(() => {
    if (available <= 0) return gridColumns;
    const widths = fitWidths(gridColumns, rows ?? [], available);
    return gridColumns.map((column) => ({ ...column, width: widths.get(column.id) ?? column.width }));
  }, [gridColumns, rows, available]);

  const onKeyDownCapture = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Enter" || event.shiftKey || event.altKey) return;
    const cell = (event.target as HTMLElement).closest<HTMLElement>("[data-grid-cell]");
    const row = cell ? rows?.[Number(cell.dataset.row)] : undefined;
    const href = row ? rowHref?.(row) : undefined;
    if (!cell || !href || !(cell.hasAttribute("data-link") || event.ctrlKey || event.metaKey)) return;
    event.preventDefault();
    event.stopPropagation();
    navigate(href);
  };

  const box = (children: ReactNode) => (
    <div ref={boxRef} className={props.fill ? "list-grid" : undefined}>
      {children}
    </div>
  );
  if (props.error) {
    return box(
      <Banner
        status="error"
        title={t("list.load_failed")}
        description={props.error.message}
        endContent={props.onRetry ? <Button label={t("action.retry")} size="sm" onClick={props.onRetry} /> : undefined}
      />,
    );
  }
  if (rows !== undefined && rows.length === 0 && !props.loading) {
    return box(
      <VStack gap={6}>
        {props.toolbar}
        <EmptyState title={props.empty.title} description={props.empty.description} actions={props.empty.action} isCompact />
      </VStack>,
    );
  }
  const footer = columns.some((column) => column.total) ? 1 : 0;
  const height = props.fill ? "100%" : Math.min(ROW_HEIGHT * (Math.max(rows?.length ?? 6, 1) + 1 + footer) + 12 + (paged ? 48 : 0), 640);

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: Enter on a row's link cell opens it; the grid owns the other keys
    <div ref={boxRef} className={props.fill ? "list-grid" : undefined} onKeyDownCapture={onKeyDownCapture}>
      <VStack gap={0}>
        <Grid<Row>
          id={props.id}
          aria-label={props.caption}
          mode={paged ? "server" : "client"}
          columns={fitted}
          // A small table needs no toolbar; a page-sized one gets the columns menu.
          toolbar={Boolean(props.fill)}
          toolbarExtra={props.toolbar}
          rows={rows ?? []}
          rowId={(row) => rowKey(row)}
          total={paged ? props.total : undefined}
          loading={props.loading}
          pageSize={paged ? props.pageSize : 1000}
          initialQuery={paged ? { page: { index: (props.page ?? 1) - 1, size: props.pageSize ?? 50 } } : undefined}
          onQueryChange={paged ? (query) => query.page.index + 1 !== latest.current.page && latest.current.onPage?.(query.page.index + 1) : undefined}
          rowHeight={ROW_HEIGHT}
          height={height}
          slots={slots}
          layoutStorage={localStorageLayout("astryx-spike.grid.")}
        />
      </VStack>
    </div>
  );
}
