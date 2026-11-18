// A list on @erpcraft/grid in server mode, Astryx skin: the grid owns sort (multi-column), paging,
// column order, widths, pinning and hiding, the keyboard and copying; the server sorts and pages.
// Filtering stays with PowerSearch above the grid (see FilterBar): one place to filter, with typed
// operators and server-searched records, kept in the URL. The grid's own column filters are off.

import { type CellChange, type ColumnType, Grid, type GridColumn, type GridQueryState, localStorageLayout } from "@erpcraft/grid";
import { type KeyboardEvent, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { VStack } from "@astryxdesign/core/VStack";
import type { FieldMeta, Json, JsonObject } from "@/api/types";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { peekLinkLabel, shortId, useLinkLabelsVersion } from "@/lib/link-labels";
import { enumLabel } from "@/lib/meta";
import { astryxGridSlots, type GridCheck, type GridDecorate } from "./grid-skin";
import { fitWidths } from "./grid-widths";

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
      return { ...column, format: (v) => (typeof v === "string" ? formatDecimal(v, { minFraction: type === "money" ? (options.decimals ?? 2) : 0 }) : "") };
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
  /** Edits of editable columns (only the check column is). */
  onEdit?: (changes: CellChange<R>[]) => void;
}) {
  const navigate = useNavigate();
  const labelsVersion = useLinkLabelsVersion();
  // biome-ignore lint/correctness/useExhaustiveDependencies: the link column and target only change with the list
  const decorateRef = useRef(decorate);
  decorateRef.current = decorate;
  const checkRef = useRef(check);
  checkRef.current = check;
  const slots = useMemo(
    () =>
      astryxGridSlots<R>({
        link: { columnId: linkColumn, href: rowHref },
        decorate: (columnId, row, text) => decorateRef.current?.(columnId, row, text),
        check: check
          ? { columnId: check.columnId, label: (row) => checkRef.current?.label(row) ?? "", onToggle: (row, on) => checkRef.current?.onToggle(row, on) }
          : undefined,
      }),
    [linkColumn, Boolean(check)],
  );
  // The grid starts from the URL's sort and page, then reports every change back to it.
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
    // biome-ignore lint/correctness/useExhaustiveDependencies: only the first value seeds the grid
    [],
  );
  // biome-ignore lint/correctness/useExhaustiveDependencies: labels that arrive are re-read through a new column list
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
  const shown = useMemo(() => {
    const widths = available > 0 ? fitWidths(columns, rows ?? [], available) : null;
    return columns.map((column) => ({ ...column, width: widths?.get(column.id) ?? column.width }));
  }, [columns, labelsVersion, rows, available]);

  // Enter (or Ctrl+Enter) on a cell of the link column opens the record, as the link would.
  const onKeyDownCapture = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Enter" || event.shiftKey || event.altKey) return;
    const cell = (event.target as HTMLElement).closest<HTMLElement>("[data-grid-cell]");
    const row = cell ? rows?.[Number(cell.dataset.row)] : undefined;
    const href = row ? rowHref(row) : undefined;
    if (!cell || !href || !(cell.hasAttribute("data-link") || event.ctrlKey || event.metaKey)) return;
    event.preventDefault();
    event.stopPropagation();
    navigate(href);
  };

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: Enter on a row's link cell opens it; the grid owns the other keys
    <div ref={boxRef} className="list-grid" onKeyDownCapture={onKeyDownCapture}>
      {rows !== undefined && rows.length === 0 && !loading && empty ? (
        <VStack gap={6}>
          {toolbar}
          {empty}
        </VStack>
      ) : (
        <Grid<R>
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
          height="100%"
          slots={slots}
          layoutStorage={localStorageLayout("astryx-spike.grid.")}
          onEdit={onEdit}
          toolbarExtra={toolbar}
          onQueryChange={(query) => onQuery({ sort: serverSort(query), page: query.page.index + 1, size: query.page.size })}
        />
      )}
    </div>
  );
}
