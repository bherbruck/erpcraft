// A list on @erpcraft/grid in server mode, shadcn skin: the grid owns sort (multi-column), paging,
// column order, widths, pinning and hiding, the keyboard and copying; the server sorts and pages.
// Filtering stays with the page's own search and filters above the grid, kept in the URL; the
// grid's own column filters are off. Ported from ui-astryx/src/components/list-grid.tsx.

import {
  type CellChange,
  type ColumnType,
  Grid,
  type GridColumn,
  type GridQueryState,
  localStorageLayout,
} from "@erpcraft/grid";
import { type KeyboardEvent, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import type { ApiError } from "@/api/client";
import type { FieldMeta, Json, JsonObject } from "@/api/types";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { peekLinkLabel, shortId, useLinkLabelsVersion } from "@/lib/link-labels";
import { enumLabel } from "@/lib/meta";
import { Alert } from "./alert";
import { Button } from "./button";
import { type GridCheck, type GridDecorate, gridLabels, shadcnGridSlots } from "./grid-skin";
import { fitWidths } from "./grid-widths";

/** The grid's words, translated once. */
const GRID_LABELS = gridLabels();
const ROW_HEIGHT = 34;

function typeOf(field: FieldMeta): ColumnType {
  switch (field.type.kind) {
    case "decimal":
      return field.decimal?.role === "money" || field.decimal?.role === "unit_price" ? "money" : "number";
    case "int":
      return "number";
    case "date":
      return "date";
    case "timestamp":
      return "datetime";
    case "bool":
      return "boolean";
    case "enum":
      return "enum";
    case "link":
      return "reference";
    default:
      return "text";
  }
}

/** A read-only list column for a field of the registry. `read` picks the value out of a row. */
export function fieldColumn<R>(
  field: FieldMeta,
  read: (row: R) => Json | undefined,
  options: { id?: string; sortField?: string | null; decimals?: number; hidden?: boolean; header?: string } = {},
): GridColumn<R> {
  const type = typeOf(field);
  const column: GridColumn<R> = {
    id: options.id ?? field.name,
    header: options.header ?? field.label,
    type,
    field: options.sortField ?? field.name,
    sortable: options.sortField !== null,
    filterable: false,
    searchable: false,
    hidden: options.hidden,
    value: (row) => {
      const value = read(row);
      return typeof value === "number" ? String(value) : (value ?? null);
    },
    minFraction: type === "money" ? (options.decimals ?? 2) : undefined,
  };
  switch (field.type.kind) {
    case "enum": {
      const enumField = field;
      return { ...column, format: (v) => (typeof v === "string" ? enumLabel(enumField, v) : "") };
    }
    case "link": {
      const { entity, key } = field.type;
      return {
        ...column,
        format: (v) => {
          if (typeof v !== "string" || !v) return "";
          const label = peekLinkLabel(entity, key, v);
          return label ?? (label === null ? shortId(v) : t("value.loading"));
        },
      };
    }
    case "bool":
      return { ...column, format: (v) => (v === true ? t("value.yes") : v === false ? t("value.no") : "") };
    case "date":
      return { ...column, format: (v) => (typeof v === "string" ? formatCalendarDate(v) : "") };
    case "timestamp":
      return { ...column, format: (v) => (typeof v === "string" ? formatTimestamp(v) : "") };
    case "decimal":
      return {
        ...column,
        format: (v) =>
          typeof v === "string"
            ? formatDecimal(v, { minFraction: type === "money" ? (options.decimals ?? 2) : 0 })
            : "",
      };
    case "document_link":
    case "line_link":
    case "lookup":
    case "entity_link":
    case "uuid":
    case "posting":
      return { ...column, sortable: false, format: (v) => (typeof v === "string" ? shortId(v) : "") };
    case "json":
      return { ...column, sortable: false, format: (v) => (v == null ? "" : JSON.stringify(v)) };
    default:
      return column;
  }
}

/** The server's sort, from the grid's: `["-doc_date", "summary.total"]`. */
export function serverSort(query: GridQueryState): string[] {
  return query.sort.map((key) => `${key.desc ? "-" : ""}${key.field}`);
}

export function ListGrid<R extends JsonObject>({
  id,
  label,
  columns,
  rows,
  total,
  loading,
  sort,
  page,
  size,
  onQuery,
  rowHref,
  linkColumn,
  empty,
  check,
  onEdit,
  decorate,
  toolbar,
  error = null,
  onRetry,
}: {
  /** Names the saved column layout. */
  id: string;
  label: string;
  columns: GridColumn<R>[];
  rows: R[] | undefined;
  total: number;
  loading: boolean;
  /** As the URL keeps it: `["-doc_date"]`. */
  sort: string[];
  page: number;
  size: number;
  onQuery: (query: { sort: string[]; page: number; size: number }) => void;
  rowHref: (row: R) => string | undefined;
  linkColumn: string;
  /** Shown instead of the grid when there are no rows: what to do next. */
  empty?: ReactNode;
  /** A column of checkboxes to choose rows by (bulk actions). */
  check?: GridCheck<R>;
  decorate?: GridDecorate<R>;
  /** The page's filter bar and views: they share the grid's toolbar row with its columns menu. */
  toolbar?: ReactNode;
  /** Why the page could not be loaded, shown above the grid with a retry. */
  error?: ApiError | null;
  onRetry?: () => void;
  /** Edits of editable columns (only the check column is). */
  onEdit?: (changes: CellChange<R>[]) => void;
}) {
  const navigate = useNavigate();
  const labelsVersion = useLinkLabelsVersion();
  const decorateRef = useRef(decorate);
  decorateRef.current = decorate;
  const checkRef = useRef(check);
  checkRef.current = check;
  // biome-ignore lint/correctness/useExhaustiveDependencies: the link column and target only change with the list; the rest is read through refs
  const slots = useMemo(
    () =>
      shadcnGridSlots<R>({
        link: { columnId: linkColumn, href: rowHref },
        decorate: (columnId, row, text) => decorateRef.current?.(columnId, row, text),
        check: check
          ? {
              columnId: check.columnId,
              label: (row) => checkRef.current?.label(row) ?? "",
              onToggle: (row, on) => checkRef.current?.onToggle(row, on),
            }
          : undefined,
      }),
    [linkColumn, Boolean(check)],
  );
  // The grid starts from the URL's sort and page, then reports every change back to it. When the
  // URL changes by other means (Back, a saved view, a link), the grid starts again from it.
  const fromUrl = `${sort.join()}|${page}|${size}`;
  const reported = useRef(fromUrl);
  const [seen, setSeen] = useState(fromUrl);
  const [epoch, setEpoch] = useState(0);
  if (fromUrl !== seen) {
    setSeen(fromUrl);
    if (fromUrl !== reported.current) {
      reported.current = fromUrl;
      setEpoch((value) => value + 1);
    }
  }
  // biome-ignore lint/correctness/useExhaustiveDependencies: the URL seeds the grid when it starts (again)
  const initialQuery = useMemo<Partial<GridQueryState>>(
    () => ({
      sort: sort.flatMap((key) => {
        const desc = key.startsWith("-");
        const field = desc ? key.slice(1) : key;
        const column = columns.find((c) => (c.field ?? c.id) === field);
        return column ? [{ columnId: column.id, field, desc }] : [];
      }),
      page: { index: page - 1, size },
    }),
    [epoch],
  );
  const [available, setAvailable] = useState(0);
  const boxRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const observer = new ResizeObserver(([entry]) => setAvailable(Math.floor((entry?.contentRect.width ?? 0) - 4)));
    observer.observe(box);
    return () => observer.disconnect();
  }, []);
  // Widths fitted to the space and the page's content (a width the user dragged is kept).
  // biome-ignore lint/correctness/useExhaustiveDependencies: labels that arrive change the widths
  const shown = useMemo(() => {
    const widths = available > 0 ? fitWidths(columns, rows ?? [], available) : null;
    return columns.map((column) => ({ ...column, width: widths?.get(column.id) ?? column.width }));
  }, [columns, labelsVersion, rows, available]);

  // The grid is as tall as its page of rows, so the page scrolls as one and nothing jumps when
  // the rows arrive: before they do, it keeps the room of the rows it is waiting for.
  const shownRows = rows?.length ?? Math.min(size, 10);
  // The header row is a little taller than a body row (h-10), and the scroller has a border.
  const height = ROW_HEIGHT * Math.max(shownRows, 3) + 40 + 4;

  // Enter (or Ctrl+Enter) on a cell of the link column opens the record, as the link would.
  const onKeyDownCapture = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Enter" || event.shiftKey || event.altKey) return;
    const cell = (event.target as HTMLElement).closest<HTMLElement>("[data-grid-cell]");
    const row = cell ? rows?.[parseInt(cell.dataset.row ?? "", 10)] : undefined;
    const href = row ? rowHref(row) : undefined;
    if (!cell || !href || !(cell.hasAttribute("data-link") || event.ctrlKey || event.metaKey)) return;
    event.preventDefault();
    event.stopPropagation();
    navigate(href);
  };

  return (
    <div ref={boxRef} className="flex flex-col gap-3" onKeyDownCapture={onKeyDownCapture}>
      {error ? (
        <Alert
          title={t("list.load_failed")}
          actions={
            onRetry ? (
              <Button variant="outline" size="sm" onClick={onRetry}>
                {t("action.retry")}
              </Button>
            ) : undefined
          }
        >
          {error.message}
        </Alert>
      ) : null}
      {rows !== undefined && rows.length === 0 && !loading && !error && empty ? (
        <>
          {toolbar}
          <div className="flex flex-col items-center gap-3 rounded-lg border bg-card px-4 py-10 text-center text-sm text-muted-foreground">
            {empty}
          </div>
        </>
      ) : (
        <Grid<R>
          key={epoch}
          id={id}
          aria-label={label}
          mode="server"
          columns={shown}
          rows={rows ?? []}
          total={total}
          loading={loading}
          initialQuery={initialQuery}
          pageSize={size}
          rowHeight={ROW_HEIGHT}
          height={height}
          slots={slots}
          labels={GRID_LABELS}
          layoutStorage={localStorageLayout("erpcraft.grid.")}
          onEdit={onEdit}
          toolbarExtra={toolbar}
          onQueryChange={(query) => {
            const next = { sort: serverSort(query), page: query.page.index + 1, size: query.page.size };
            reported.current = `${next.sort.join()}|${next.page}|${next.size}`;
            onQuery(next);
          }}
        />
      )}
    </div>
  );
}
