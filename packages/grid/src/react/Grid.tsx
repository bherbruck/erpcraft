// The grid: structure, state, keyboard, focus, clipboard and virtual rows.
// What it looks like comes from the slots (see ./slots.ts).

import {
  type Column,
  type ColumnDef,
  type ColumnFiltersState,
  type ColumnOrderState,
  type ColumnPinningState,
  type FilterFn,
  getCoreRowModel,
  getFilteredRowModel,
  getSortedRowModel,
  type SortingState,
  useReactTable,
  type VisibilityState,
} from "@tanstack/react-table";
import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import {
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  memo,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  alignOf,
  cellValue,
  compareKeys,
  formatCell,
  isEditable,
  isEmptyValue,
  isSearchable,
  isSetType,
  type CellSortKey,
  serverField,
  sortKey,
} from "../core/columns";
import { formatDecimal, sumDecimals, toDecimal } from "../core/decimal";
import { copyRange, EditHistory, type HistoryEntry, planClear, planEdit, planPaste, type Sheet } from "../core/editing";
import { DEFAULT_LABELS, describeFilter, filterKind, isComplete, matches } from "../core/filters";
import { defaultLayout, moveColumn, reconcileLayout } from "../core/layout";
import {
  type Cell,
  clampCell,
  type Effect,
  type Edit,
  handleKey,
  initialNav,
  type NavContext,
  type NavState,
  type Range,
  rangeOf,
} from "../core/navigation";
import { parseTsv, toHtmlTable, toTsv } from "../core/tsv";
import type {
  CellChange,
  ColumnLayout,
  ColumnType,
  EditReport,
  EditSource,
  FilterCondition,
  GridColumn,
  GridQueryState,
  LayoutStorage,
  Option,
  RejectedCell,
} from "../types";
import { type ChecklistOptions, useChecklistFilter, useConditionFilter } from "./filters";
import {
  DEFAULT_GRID_LABELS,
  type ElementProps,
  type FilterChip,
  type GridLabels,
  type GridSlots,
  type SortInfo,
} from "./slots";

export interface LineEditing {
  /** Enter or ArrowDown on the last row, or Tab past its last cell: add a row at the end. */
  onAppend(): void;
  /** Ctrl+Shift+= (Ctrl++) inserts above the active row; Ctrl+Enter below. */
  onInsert(at: number): void;
  /** Ctrl+-: delete the rows of the selection. */
  onDelete(rowIds: string[], from: number, to: number): void;
}

export interface GridProps<Row> {
  /** Names the grid: its saved layout and its element ids. */
  id: string;
  columns: readonly GridColumn<Row>[];
  rows: readonly Row[];
  /** A stable id for each row. Defaults to `row.id`, else the index. */
  rowId?: (row: Row, index: number) => string;
  /** "client": all rows are here; sort, filter and search run in memory. "server": `rows` is one page. */
  mode?: "client" | "server";
  /** Server mode: the number of rows the whole list has. */
  total?: number;
  loading?: boolean;
  /** Called with the sort, filters, search and page whenever they change, in both modes. */
  onQueryChange?: (query: GridQueryState) => void;
  initialQuery?: Partial<GridQueryState>;
  pageSize?: number;
  /** Called with each batch of cell changes. Without it no cell is editable. */
  onEdit?: (changes: CellChange<Row>[], source: EditSource) => void;
  /** What a paste, clear or edit did, including the cells it refused. */
  onReport?: (report: EditReport) => void;
  lineEditing?: LineEditing;
  layoutStorage?: LayoutStorage;
  /** Server mode: totals of the columns with `total`, by column id, as decimal strings. */
  totals?: Record<string, string | null>;
  /** Does Enter start editing an editable cell (Google Sheets) rather than move down (Excel)? Default true. */
  enterEdits?: boolean;
  locale?: string;
  /** "Today" for relative date filters. */
  today?: Date;
  labels?: Partial<GridLabels>;
  /** The design system: every visible element. See ./slots.ts and the README's skinning checklist. */
  slots: GridSlots<Row>;
  /** Height of the scrolling area. */
  height?: number | string;
  rowHeight?: number;
  "aria-label": string;
  /** Show the toolbar (search, filter chips, columns). Default true. */
  toolbar?: boolean;
  /** The consumer's own content for the toolbar (buttons, a "new" action). */
  toolbarExtra?: ReactNode;
}

const DEFAULT_WIDTH: Record<ColumnType, number> = {
  text: 180,
  number: 110,
  money: 130,
  date: 120,
  datetime: 170,
  boolean: 80,
  enum: 130,
  state: 130,
  reference: 180,
};

interface ColumnMeta<Row> {
  column: GridColumn<Row>;
}

function defaultRowId<Row>(row: Row, index: number): string {
  const id = (row as { id?: unknown } | null)?.id;
  return id === undefined || id === null ? String(index) : String(id);
}

function interpolate(text: string, values: Record<string, string | number>): string {
  return text.replace(/\{(\w+)\}/g, (_, key: string) => String(values[key] ?? ""));
}

/**
 * Did the event happen in the element's own DOM? React passes events from
 * portals (a menu, a design system's combobox list) up through the
 * components that render them; the grid acts only on its own.
 */
function outsidePortal(event: { currentTarget: EventTarget; target: EventTarget }): boolean {
  return (event.currentTarget as Node).contains(event.target as Node);
}

const EDITOR_SELECTOR = "input, select, textarea, [contenteditable='true'], [data-grid-editor]";

/** Set each changed cell on the rows, for consumers that keep plain objects. */
export function applyChanges<Row>(
  rows: readonly Row[],
  changes: readonly CellChange<Row>[],
  columns: readonly GridColumn<Row>[],
  rowId: (row: Row, index: number) => string = defaultRowId,
): Row[] {
  const byId = new Map<string, CellChange<Row>[]>();
  for (const change of changes) {
    const list = byId.get(change.rowId) ?? [];
    list.push(change);
    byId.set(change.rowId, list);
  }
  return rows.map((row, index) => {
    const list = byId.get(rowId(row, index));
    if (!list) return row;
    let next = row;
    for (const change of list) {
      const column = columns.find((c) => c.id === change.columnId);
      next = column?.setValue ? column.setValue(next, change.newValue) : { ...next, [change.columnId]: change.newValue };
    }
    return next;
  });
}

export function Grid<Row>(props: GridProps<Row>) {
  const {
    id,
    columns,
    rows,
    mode = "client",
    total,
    loading = false,
    onQueryChange,
    initialQuery,
    pageSize = 100,
    onEdit,
    onReport,
    lineEditing,
    layoutStorage,
    totals,
    enterEdits = true,
    locale,
    today,
    slots,
    height = 480,
    rowHeight = 32,
    toolbar = true,
    toolbarExtra,
  } = props;
  const server = mode === "server";
  const labels: GridLabels = useMemo(() => ({ ...DEFAULT_LABELS, ...DEFAULT_GRID_LABELS, ...props.labels }), [props.labels]);
  const rowIdOf = props.rowId ?? defaultRowId;
  const rowIdRef = useRef(rowIdOf);
  rowIdRef.current = rowIdOf;

  // ------------------------------------------------------------ table state
  const byId = useMemo(() => new Map(columns.map((c) => [c.id, c])), [columns]);
  const [sorting, setSorting] = useState<SortingState>(
    () => initialQuery?.sort?.map((s) => ({ id: s.columnId, desc: s.desc })) ?? [],
  );
  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>(
    () => initialQuery?.filters?.map((f) => ({ id: f.columnId, value: f.condition })) ?? [],
  );
  const [search, setSearch] = useState(initialQuery?.search ?? "");
  const [pageIndex, setPageIndex] = useState(initialQuery?.page?.index ?? 0);
  const size = initialQuery?.page?.size ?? pageSize;

  const [layout, setLayout] = useState<ColumnLayout>(() => {
    const stored = layoutStorage?.load(id);
    return reconcileLayout(columns, stored && !(stored instanceof Promise) ? stored : null);
  });
  const layoutLoaded = useRef(false);
  useEffect(() => {
    // A store that answers later (a server) applies when it does.
    const stored = layoutStorage?.load(id);
    if (stored instanceof Promise) {
      let live = true;
      stored.then((saved) => {
        if (live) setLayout(reconcileLayout(columns, saved));
      });
      return () => {
        live = false;
      };
    }
    return undefined;
    // Loaded once per grid id; columns are reconciled below as they change.
  }, [id, layoutStorage]);
  useEffect(() => {
    setLayout((current) => reconcileLayout(columns, current));
  }, [columns]);
  useEffect(() => {
    if (!layoutLoaded.current) {
      layoutLoaded.current = true;
      return undefined;
    }
    if (!layoutStorage) return undefined;
    const timer = setTimeout(() => layoutStorage.save(id, layout), 250);
    return () => clearTimeout(timer);
  }, [id, layout, layoutStorage]);

  const columnVisibility = useMemo<VisibilityState>(
    () => Object.fromEntries(layout.hidden.map((h) => [h, false])),
    [layout.hidden],
  );

  const todayRef = useRef(today);
  todayRef.current = today;
  const localeRef = useRef(locale);
  localeRef.current = locale;

  const defs = useMemo<ColumnDef<Row>[]>(() => {
    const columnFilter: FilterFn<Row> = (row, columnId, value: FilterCondition) => {
      const column = byId.get(columnId);
      if (!column || !isComplete(value)) return true;
      return matches(value, cellValue(column, row.original), todayRef.current);
    };
    return columns.map((column) => ({
      id: column.id,
      header: column.header,
      // The table keeps this per row: a key quick to sort by. Empty values
      // are undefined, which the table keeps last in both directions.
      accessorFn: (row: Row) => sortKey(column, cellValue(column, row), localeRef.current),
      size: column.width ?? DEFAULT_WIDTH[column.type],
      minSize: column.minWidth ?? 48,
      maxSize: column.maxWidth ?? 1200,
      enableSorting: column.sortable !== false,
      enableColumnFilter: column.filterable !== false,
      enableHiding: column.hideable !== false,
      enableGlobalFilter: isSearchable(column),
      sortUndefined: "last",
      sortDescFirst: false,
      sortingFn: (a, b, columnId) =>
        compareKeys(a.getValue<CellSortKey>(columnId), b.getValue<CellSortKey>(columnId), localeRef.current),
      filterFn: columnFilter,
      meta: { column } satisfies ColumnMeta<Row>,
    }));
  }, [columns, byId]);

  const globalFilterFn = useCallback<FilterFn<Row>>(
    (row, columnId, value: string) => {
      const column = byId.get(columnId);
      if (!column) return false;
      return formatCell(column, row.original, localeRef.current).toLocaleLowerCase().includes(value.toLocaleLowerCase());
    },
    [byId],
  );

  const table = useReactTable<Row>({
    data: rows as Row[],
    columns: defs,
    getRowId: (row, index) => rowIdRef.current(row, index),
    state: {
      sorting,
      columnFilters,
      globalFilter: search.trim(),
      columnSizing: layout.sizes,
      columnOrder: layout.order,
      columnPinning: layout.pinning,
      columnVisibility,
    },
    onSortingChange: setSorting,
    onColumnFiltersChange: setColumnFilters,
    onColumnSizingChange: (updater) =>
      setLayout((l) => ({ ...l, sizes: typeof updater === "function" ? updater(l.sizes) : updater })),
    onColumnOrderChange: (updater) =>
      setLayout((l) => ({ ...l, order: typeof updater === "function" ? updater(l.order) : (updater as ColumnOrderState) })),
    onColumnPinningChange: (updater) =>
      setLayout((l) => {
        const next = typeof updater === "function" ? updater(l.pinning) : (updater as ColumnPinningState);
        return { ...l, pinning: { left: next.left ?? [], right: next.right ?? [] } };
      }),
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: server ? undefined : getSortedRowModel(),
    getFilteredRowModel: server ? undefined : getFilteredRowModel(),
    manualSorting: server,
    manualFiltering: server,
    globalFilterFn,
    getColumnCanGlobalFilter: (column) => {
      const meta = column.columnDef.meta as ColumnMeta<Row> | undefined;
      return meta ? isSearchable(meta.column) : false;
    },
    enableMultiSort: true,
    isMultiSortEvent: () => false,
    enableSortingRemoval: true,
    columnResizeMode: "onChange",
    enableColumnResizing: true,
  });

  const shownColumns: Column<Row, unknown>[] = useMemo(
    () => [
      ...table.getLeftVisibleLeafColumns(),
      ...table.getCenterVisibleLeafColumns(),
      ...table.getRightVisibleLeafColumns(),
    ],
    // The table's own memo changes with these.
    [table, layout, columns],
  );
  const shown = useMemo(
    () => shownColumns.map((c) => (c.columnDef.meta as ColumnMeta<Row>).column),
    [shownColumns],
  );
  const tableRows = table.getRowModel().rows;
  // The filtered rows before sorting: totals do not change with the order.
  const filteredRows = table.getFilteredRowModel().rows;
  const displayRows = useMemo(() => tableRows.map((r) => r.original), [tableRows]);
  const rowCount = displayRows.length;
  const colCount = shown.length;
  const filtered = columnFilters.length > 0 || search.trim() !== "";

  // ------------------------------------------------------------ query out
  const [debouncedSearch, setDebouncedSearch] = useState(search);
  useEffect(() => {
    const timer = setTimeout(() => setDebouncedSearch(search), server ? 300 : 0);
    return () => clearTimeout(timer);
  }, [search, server]);

  const query = useMemo<GridQueryState>(
    () => ({
      sort: sorting.flatMap((s) => {
        const column = byId.get(s.id);
        return column ? [{ columnId: s.id, field: serverField(column), desc: s.desc }] : [];
      }),
      filters: columnFilters.flatMap((f) => {
        const column = byId.get(f.id);
        const condition = f.value as FilterCondition;
        return column && isComplete(condition)
          ? [{ columnId: f.id, field: serverField(column), type: column.type, condition }]
          : [];
      }),
      search: debouncedSearch.trim(),
      page: { index: pageIndex, size },
    }),
    [sorting, columnFilters, debouncedSearch, pageIndex, size, byId],
  );
  const queryKey = JSON.stringify(query);
  const onQueryChangeRef = useRef(onQueryChange);
  onQueryChangeRef.current = onQueryChange;
  // biome-ignore lint/correctness/useExhaustiveDependencies: the key stands for the query
  useEffect(() => {
    onQueryChangeRef.current?.(query);
  }, [queryKey]);
  // A new sort, filter or search starts from the first page.
  const criteriaKey = JSON.stringify([query.sort, query.filters, query.search]);
  const firstCriteria = useRef(true);
  // biome-ignore lint/correctness/useExhaustiveDependencies: the key stands for the criteria
  useEffect(() => {
    if (firstCriteria.current) {
      firstCriteria.current = false;
      return;
    }
    setPageIndex(0);
  }, [criteriaKey]);

  // ------------------------------------------------------------ navigation
  const [nav, setNavState] = useState<NavState>(initialNav);
  const navRef = useRef(nav);
  const setNav = useCallback((next: NavState) => {
    navRef.current = next;
    setNavState(next);
  }, []);
  /** The open column menu or filter popover. */
  const [menu, setMenu] = useState<{ columnId: string; kind: "column" | "filter" } | null>(null);
  const menuColumn = menu?.columnId ?? null;
  const [editError, setEditError] = useState<{ row: number; col: number; message: string } | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const history = useRef(new EditHistory()).current;

  const rootRef = useRef<HTMLDivElement>(null);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<HTMLInputElement & HTMLSelectElement & HTMLTextAreaElement>(null);
  const hasFocus = useRef(false);
  // Callback refs: a slot may render any element (a div, a table), so the grid takes whichever it gets.
  const setRootRef = useCallback((el: HTMLElement | null) => {
    rootRef.current = el as HTMLDivElement | null;
  }, []);
  const setScrollerRef = useCallback((el: HTMLElement | null) => {
    scrollerRef.current = el as HTMLDivElement | null;
  }, []);
  const menuRef = useRef(menu);
  menuRef.current = menu;

  const editable = onEdit !== undefined;
  const sheet: Sheet<Row> = useMemo(
    () => ({ rows: displayRows, columns: shown, rowId: rowIdOf, locale }),
    // rowIdOf is read through its ref where it matters.
    [displayRows, shown, locale],
  );
  const sheetRef = useRef(sheet);
  sheetRef.current = sheet;

  const scrollerHeight = typeof height === "number" ? height : 480;
  const pageRows = Math.max(1, Math.floor(scrollerHeight / rowHeight) - 2);

  const navContext: NavContext = useMemo(
    () => ({
      rowCount,
      colCount,
      pageRows,
      headerRow: true,
      lineEditing: lineEditing !== undefined,
      enterEdits,
      isEmpty: (r, c) => {
        const column = shown[c];
        const row = displayRows[r];
        return !column || row === undefined || isEmptyValue(cellValue(column, row));
      },
      isEditable: (r, c) => {
        const column = shown[c];
        const row = displayRows[r];
        return editable && column !== undefined && row !== undefined && isEditable(column, row);
      },
      editText: (r, c) => {
        const column = shown[c];
        const row = displayRows[r];
        if (!column || row === undefined) return "";
        const value = cellValue(column, row);
        // Numbers are edited as the plain decimal, not with grouping.
        if (column.type === "number" || column.type === "money") return toDecimal(value) ?? "";
        if (isSetType(column.type) && !column.options) return isEmptyValue(value) ? "" : String(value);
        return formatCell(column, row, locale);
      },
      sortable: (c) => shownColumns[c]?.getCanSort() ?? false,
    }),
    [rowCount, colCount, pageRows, lineEditing, enterEdits, shown, displayRows, editable, locale, shownColumns],
  );
  const navContextRef = useRef(navContext);
  navContextRef.current = navContext;

  // The position shown: the state clamped to the rows there are now. The
  // state itself keeps its row, so a row the consumer is about to add can be
  // moved to before it arrives.
  const active: Cell = nav.active.row === -1 || rowCount === 0
    ? { row: -1, col: Math.min(Math.max(nav.active.col, 0), Math.max(colCount - 1, 0)) }
    : clampCell(nav.active, navContext);
  const range: Range = useMemo(() => {
    const r = rangeOf(nav);
    const maxRow = Math.max(rowCount - 1, 0);
    const maxCol = Math.max(colCount - 1, 0);
    return {
      top: Math.min(Math.max(r.top, 0), maxRow),
      bottom: Math.min(Math.max(r.bottom, 0), maxRow),
      left: Math.min(r.left, maxCol),
      right: Math.min(r.right, maxCol),
    };
  }, [nav, rowCount, colCount]);
  // When a filter leaves no rows, the header holds focus; the state follows
  // what the user sees, so ArrowDown then goes to the first row.
  const previousRowCount = useRef(rowCount);
  useEffect(() => {
    const before = previousRowCount.current;
    previousRowCount.current = rowCount;
    const state = navRef.current;
    if (rowCount === 0 && before > 0 && state.active.row >= 0) {
      const cell = { row: -1, col: state.active.col };
      setNav({ ...state, active: cell, anchor: { row: 0, col: cell.col }, extent: { row: 0, col: cell.col }, editing: null });
    }
  }, [rowCount, setNav]);
  const editing = nav.editing && nav.editing.row < rowCount && nav.editing.col < colCount ? nav.editing : null;

  // ------------------------------------------------------------ virtual rows
  const [headerHeight, setHeaderHeight] = useState(rowHeight);
  const headerRef = useRef<HTMLDivElement>(null);
  const [headerElement, setHeaderElement] = useState<HTMLElement | null>(null);
  const setHeaderRef = useCallback((el: HTMLElement | null) => {
    headerRef.current = el as HTMLDivElement | null;
    setHeaderElement(el);
  }, []);
  useLayoutEffect(() => {
    const el = headerElement;
    if (!el) return undefined;
    const measure = () => setHeaderHeight(el.offsetHeight || rowHeight);
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(el);
    return () => observer?.disconnect();
  }, [rowHeight, headerElement]);

  const keepRows = useRef<number[]>([]);
  keepRows.current = [active.row, nav.extent.row].filter((r) => r >= 0 && r < rowCount);
  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollerRef.current,
    estimateSize: () => rowHeight,
    overscan: 12,
    scrollMargin: headerHeight,
    // The active cell and the end of the selection stay rendered, so focus
    // is never on an element that scrolled away and was removed.
    rangeExtractor: (r) => {
      const indexes = new Set(defaultRangeExtractor(r));
      for (const keep of keepRows.current) indexes.add(keep);
      return [...indexes].sort((a, b) => a - b);
    },
  });

  // ------------------------------------------------------------ edits
  const announce = useCallback((text: string) => {
    // A repeated message is read again only if it changes.
    setAnnouncement((previous) => (previous === text ? `${text} ` : text));
  }, []);

  const describeRejected = useCallback(
    (rejected: RejectedCell[]) => {
      const first = rejected[0];
      if (!first) return "";
      const column = byId.get(first.columnId);
      const where = `${column?.header ?? first.columnId} ${first.rowIndex + 1}`;
      return `${interpolate(labels.rejected, { count: rejected.length })} (${where}: ${first.reason})`;
    },
    [byId, labels],
  );

  const commitChanges = useCallback(
    (changes: CellChange<Row>[], rejected: RejectedCell[], source: EditSource) => {
      if (changes.length > 0) {
        onEdit?.(changes, source);
        if (source !== "undo" && source !== "redo") history.record(source, changes);
      }
      const report: EditReport = { source, applied: changes.length, rejected };
      if (source !== "edit" || rejected.length > 0) onReport?.(report);
      let text = "";
      if (source === "paste") text = interpolate(labels.pasted, { applied: changes.length });
      else if (source === "clear") text = interpolate(labels.cleared, { applied: changes.length });
      else if (source === "undo") text = labels.undone;
      else if (source === "redo") text = labels.redone;
      if (rejected.length > 0) text = `${text ? `${text}. ` : ""}${describeRejected(rejected)}`;
      if (text) announce(text);
    },
    [onEdit, onReport, history, labels, describeRejected, announce],
  );

  /** History entries back to changes on the rows as they are now. */
  const fromHistory = useCallback(
    (entries: HistoryEntry[]): CellChange<Row>[] => {
      const { rows: shownRows } = sheetRef.current;
      const shownIndex = new Map<string, number>();
      shownRows.forEach((row, index) => {
        shownIndex.set(rowIdRef.current(row, index), index);
      });
      const allIndex = new Map<string, Row>();
      rows.forEach((row, index) => {
        allIndex.set(rowIdRef.current(row, index), row);
      });
      return entries.flatMap((entry) => {
        const at = shownIndex.get(entry.rowId);
        const row = at === undefined ? allIndex.get(entry.rowId) : shownRows[at];
        if (row === undefined) return [];
        return [{ ...entry, rowIndex: at ?? -1, row }];
      });
    },
    [rows],
  );

  const runEffects = useCallback(
    (effects: Effect[], before: NavState) => {
      for (const effect of effects) {
        switch (effect.type) {
          case "commit": {
            const plan = planEdit(sheetRef.current, effect.edit.row, effect.edit.col, effect.edit.draft);
            if (plan.rejected.length > 0) {
              // Refused: stay in the cell with the text, as Excel does.
              const message = plan.rejected[0]?.reason ?? "";
              setNav({ ...before, editing: effect.edit });
              setEditError({ row: effect.edit.row, col: effect.edit.col, message });
              commitChanges([], plan.rejected, "edit");
              return;
            }
            setEditError(null);
            commitChanges(plan.changes, [], "edit");
            break;
          }
          case "cancel":
            setEditError(null);
            break;
          case "clear": {
            const plan = planClear(sheetRef.current, effect.range);
            commitChanges(plan.changes, plan.rejected, "clear");
            break;
          }
          case "undo":
          case "redo": {
            const entries = effect.type === "undo" ? history.undo() : history.redo();
            if (!entries) {
              announce(labels.nothingToUndo);
              break;
            }
            commitChanges(fromHistory(entries), [], effect.type);
            break;
          }
          case "appendRow":
            lineEditing?.onAppend();
            break;
          case "insertRow":
            lineEditing?.onInsert(effect.at);
            break;
          case "deleteRows": {
            const ids = sheetRef.current.rows
              .slice(effect.from, effect.to + 1)
              .map((row, i) => rowIdRef.current(row, effect.from + i));
            lineEditing?.onDelete(ids, effect.from, effect.to);
            break;
          }
          case "sort": {
            const column = shownColumns[effect.col];
            if (column?.getCanSort()) column.toggleSorting(undefined, effect.multi);
            break;
          }
          case "openMenu": {
            const column = shown[effect.col];
            if (!column) break;
            // A column that cannot be filtered opens its menu instead.
            const kind = effect.menu === "filter" && column.filterable === false ? "column" : effect.menu;
            setMenu({ columnId: column.id, kind });
            break;
          }
          case "beginEdit":
            setEditError(null);
            break;
        }
      }
    },
    [commitChanges, history, fromHistory, lineEditing, shownColumns, shown, setNav, announce, labels],
  );

  // ------------------------------------------------------------ focus
  useLayoutEffect(() => {
    if (!hasFocus.current || menuColumn !== null) return;
    const root = rootRef.current;
    const scroller = scrollerRef.current;
    if (!root || !scroller) return;
    const current = document.activeElement;
    // Never take focus from something outside the grid that has it.
    if (current && current !== document.body && !root.contains(current)) return;
    if (editing) {
      const editor =
        editorRef.current ??
        scroller.querySelector<HTMLElement>(
          `[data-row="${editing.row}"][data-col="${editing.col}"] :is(${EDITOR_SELECTOR})`,
        );
      if (editor && document.activeElement !== editor) {
        editor.focus({ preventScroll: true });
        if (editor instanceof HTMLInputElement || editor instanceof HTMLTextAreaElement) {
          const end = editor.value.length;
          try {
            editor.setSelectionRange(end, end);
          } catch {
            // Inputs of some types have no caret.
          }
        }
      }
      return;
    }
    const cell = scroller.querySelector<HTMLElement>(`[data-row="${active.row}"][data-col="${active.col}"]`);
    if (cell && document.activeElement !== cell) cell.focus({ preventScroll: true });
  });

  // Keep the active cell, or the end of a selection being extended, in view.
  const scrollKey = `${active.row}:${active.col}:${nav.extent.row}:${nav.extent.col}`;
  // biome-ignore lint/correctness/useExhaustiveDependencies: the key stands for the position
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller || !hasFocus.current) return;
    const extending = nav.extent.row !== nav.anchor.row || nav.extent.col !== nav.anchor.col;
    const target = extending ? clampCell(nav.extent, navContext) : active;
    if (target.row >= 0) virtualizer.scrollToIndex(target.row, { align: "auto" });
    const el = scroller.querySelector<HTMLElement>(`[data-row="${target.row}"][data-col="${target.col}"]`);
    el?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [scrollKey]);

  const onFocusIn = useCallback(
    (event: React.FocusEvent) => {
      hasFocus.current = true;
      // A control inside a cell (a header's sort or menu button, focused by a
      // click or by a menu closing) hands focus to the active cell, so the
      // grid keeps one tab stop and its keys.
      const target = event.target as HTMLElement;
      if (menuRef.current !== null || target.matches(EDITOR_SELECTOR)) return;
      const owner = target.closest<HTMLElement>("[data-grid-cell]");
      if (!owner || owner === target || !rootRef.current?.contains(owner)) return;
      const { active: at } = navRef.current;
      const cell =
        scrollerRef.current?.querySelector<HTMLElement>(`[data-row="${at.row}"][data-col="${at.col}"]`) ?? owner;
      cell.focus({ preventScroll: true });
    },
    [],
  );
  const onFocusOut = useCallback(
    (event: React.FocusEvent) => {
      const next = event.relatedTarget as Node | null;
      if (next && rootRef.current?.contains(next)) return;
      // A menu of the grid (in a portal) keeps the grid's focus.
      if (next instanceof Element && next.closest("[data-grid-menu]")) return;
      hasFocus.current = false;
      // Leaving the grid while editing commits the edit, as clicking away in Excel does.
      const state = navRef.current;
      if (state.editing && next) {
        const edit = state.editing;
        setNav({ ...state, editing: null });
        runEffects([{ type: "commit", edit }], state);
      }
    },
    [runEffects, setNav],
  );

  // ------------------------------------------------------------ keyboard
  const onKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>) => {
      if (event.nativeEvent.isComposing) return;
      const target = event.target as HTMLElement;
      // Keys in a column menu or anything else that is not a cell are not the grid's.
      if (!target.closest("[data-grid-cell]") || target.closest("[data-grid-menu]")) return;
      if (!rootRef.current?.contains(target)) return;
      const before = navRef.current;
      const outcome = handleKey(
        before,
        { key: event.key, shift: event.shiftKey, ctrl: event.ctrlKey || event.metaKey, alt: event.altKey },
        navContextRef.current,
      );
      if (outcome.handled) event.preventDefault();
      if (outcome.state !== before) setNav(outcome.state);
      if (outcome.effects.length) runEffects(outcome.effects, before);
    },
    [runEffects, setNav],
  );

  // ------------------------------------------------------------ clipboard
  const inEditor = (target: EventTarget | null) =>
    target instanceof HTMLElement && target.closest("[data-grid-cell]") !== null && target.matches(EDITOR_SELECTOR);

  const copySelection = useCallback(
    (event: React.ClipboardEvent) => {
      const state = navRef.current;
      let cells: string[][];
      if (state.active.row === -1) {
        const column = sheetRef.current.columns[state.active.col];
        cells = [[column?.header ?? ""]];
      } else {
        cells = copyRange(sheetRef.current, range);
      }
      event.clipboardData.setData("text/plain", toTsv(cells));
      event.clipboardData.setData("text/html", toHtmlTable(cells));
      event.preventDefault();
    },
    [range],
  );

  const onCopy = useCallback(
    (event: React.ClipboardEvent) => {
      if (!outsidePortal(event) || inEditor(event.target) || menuColumn) return;
      copySelection(event);
    },
    [copySelection, menuColumn],
  );

  const onCut = useCallback(
    (event: React.ClipboardEvent) => {
      if (!outsidePortal(event) || inEditor(event.target) || menuColumn || navRef.current.active.row === -1) return;
      copySelection(event);
      const plan = planClear(sheetRef.current, range);
      commitChanges(plan.changes, plan.rejected, "clear");
    },
    [copySelection, range, commitChanges, menuColumn],
  );

  const onPaste = useCallback(
    (event: React.ClipboardEvent) => {
      if (!outsidePortal(event) || inEditor(event.target) || menuColumn || navRef.current.active.row === -1) return;
      event.preventDefault();
      const cells = parseTsv(event.clipboardData.getData("text/plain"));
      if (cells.length === 0) return;
      if (!editable) {
        commitChanges(
          [],
          [{ rowIndex: range.top, columnId: shown[range.left]?.id ?? "", text: "", reason: "read-only" }],
          "paste",
        );
        return;
      }
      const plan = planPaste(sheetRef.current, range, cells);
      commitChanges(plan.changes, plan.rejected, "paste");
      // The pasted block becomes the selection, as in Excel.
      const single = cells.length === 1 && cells[0]?.length === 1;
      if (!single) {
        const width = Math.max(...cells.map((line) => line.length));
        const extent = clampCell({ row: range.top + cells.length - 1, col: range.left + width - 1 }, navContextRef.current);
        const anchor = { row: range.top, col: range.left };
        setNav({ ...navRef.current, active: anchor, anchor, extent });
      }
    },
    [range, commitChanges, editable, shown, setNav, menuColumn],
  );

  // ------------------------------------------------------------ mouse
  const dragging = useRef(false);
  useEffect(() => {
    const stop = () => {
      dragging.current = false;
    };
    window.addEventListener("mouseup", stop);
    return () => window.removeEventListener("mouseup", stop);
  }, []);

  const onCellMouseDown = useCallback(
    (event: ReactMouseEvent, row: number, col: number) => {
      if (event.button !== 0) return;
      const state = navRef.current;
      if (state.editing && state.editing.row === row && state.editing.col === col) return;
      event.preventDefault();
      hasFocus.current = true;
      let next = state;
      if (state.editing) {
        // Clicking another cell commits the edit first.
        runEffects([{ type: "commit", edit: state.editing }], state);
        if (navRef.current.editing) return; // refused: stay
        next = { ...navRef.current, editing: null };
      }
      const cell = { row, col };
      if (event.shiftKey) setNav({ ...next, extent: cell, editing: null });
      else setNav({ ...next, active: cell, anchor: cell, extent: cell, editing: null, tabColumn: null });
      dragging.current = true;
    },
    [runEffects, setNav],
  );

  const onCellMouseEnter = useCallback(
    (row: number, col: number) => {
      if (!dragging.current) return;
      const state = navRef.current;
      if (state.extent.row === row && state.extent.col === col) return;
      setNav({ ...state, extent: { row, col }, editing: null });
    },
    [setNav],
  );

  const onCellDoubleClick = useCallback(
    (row: number, col: number) => {
      const ctx = navContextRef.current;
      if (!ctx.isEditable(row, col)) return;
      const state = navRef.current;
      const original = ctx.editText(row, col);
      const edit: Edit = { row, col, mode: "edit", draft: original, original };
      setNav({ ...state, active: { row, col }, anchor: { row, col }, extent: { row, col }, editing: edit });
    },
    [setNav],
  );

  /** For an editor whose own list keeps the keys (a select, a combobox): commit as Enter would. */
  const commitEdit = useCallback(
    (text?: string) => {
      const state = navRef.current;
      if (!state.editing) return;
      const edit = text === undefined ? state.editing : { ...state.editing, draft: text };
      const outcome = handleKey({ ...state, editing: edit }, { key: "Enter" }, navContextRef.current);
      setNav(outcome.state);
      runEffects(outcome.effects, { ...state, editing: edit });
    },
    [runEffects, setNav],
  );
  const cancelEdit = useCallback(() => {
    const state = navRef.current;
    if (!state.editing) return;
    setNav({ ...state, editing: null });
    setEditError(null);
  }, [setNav]);

  const setDraft = useCallback(
    (text: string) => {
      const state = navRef.current;
      if (!state.editing) return;
      setNav({ ...state, editing: { ...state.editing, draft: text } });
    },
    [setNav],
  );

  // ------------------------------------------------------------ columns
  const [dragColumn, setDragColumn] = useState<string | null>(null);
  // Reordering by dragging a header, with pointer events: a native drag
  // does not start from a button, and every header has one.
  const headerDrag = useRef<{ columnId: string; startX: number; moving: boolean } | null>(null);
  const dropHintRef = useRef<{ id: string; after: boolean } | null>(null);
  const shownRef = useRef(shown);
  shownRef.current = shown;
  useEffect(() => {
    const move = (event: MouseEvent) => {
      const drag = headerDrag.current;
      if (!drag) return;
      if (!drag.moving) {
        if (Math.abs(event.clientX - drag.startX) < 6) return;
        drag.moving = true;
        setDragColumn(drag.columnId);
      }
      const over = document
        .elementFromPoint(event.clientX, event.clientY)
        ?.closest<HTMLElement>('[role="columnheader"][data-col]');
      const target = over && scrollerRef.current?.contains(over) ? shownRef.current[Number(over.dataset.col)] : undefined;
      let hint: { id: string; after: boolean } | null = null;
      if (over && target && target.id !== drag.columnId) {
        const rect = over.getBoundingClientRect();
        hint = { id: target.id, after: event.clientX > rect.left + rect.width / 2 };
      }
      if (hint?.id !== dropHintRef.current?.id || hint?.after !== dropHintRef.current?.after) {
        dropHintRef.current = hint;
        setDropHint(hint);
      }
    };
    const up = () => {
      const drag = headerDrag.current;
      headerDrag.current = null;
      if (!drag?.moving) return;
      const hint = dropHintRef.current;
      dropHintRef.current = null;
      setDragColumn(null);
      setDropHint(null);
      // The click that ends a drag does not sort.
      const swallow = (event: Event) => {
        event.stopPropagation();
        event.preventDefault();
      };
      window.addEventListener("click", swallow, { capture: true, once: true });
      setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
      if (!hint) return;
      const dragged = drag.columnId;
      setLayout((l) => {
        // Dropped among pinned columns, a column takes their side.
        const side = l.pinning.left.includes(hint.id) ? "left" : l.pinning.right.includes(hint.id) ? "right" : null;
        const left = l.pinning.left.filter((x) => x !== dragged);
        const right = l.pinning.right.filter((x) => x !== dragged);
        if (side === "left") left.splice(left.indexOf(hint.id) + (hint.after ? 1 : 0), 0, dragged);
        if (side === "right") right.splice(right.indexOf(hint.id) + (hint.after ? 1 : 0), 0, dragged);
        return { ...l, order: moveColumn(l.order, dragged, hint.id, hint.after), pinning: { left, right } };
      });
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
  }, []);
  const [dropHint, setDropHint] = useState<{ id: string; after: boolean } | null>(null);

  const measureCanvas = useRef<HTMLCanvasElement | null>(null);
  const autoFit = useCallback(
    (columnId: string) => {
      const column = byId.get(columnId);
      const scroller = scrollerRef.current;
      if (!column || !scroller) return;
      const canvas = measureCanvas.current ?? document.createElement("canvas");
      measureCanvas.current = canvas;
      const context = canvas.getContext("2d");
      if (!context) return;
      const col = shown.findIndex((c) => c.id === columnId);
      const sample = scroller.querySelector<HTMLElement>(`[data-row][data-col="${col}"]:not([data-row="-1"])`);
      const header = scroller.querySelector<HTMLElement>(`[data-row="-1"][data-col="${col}"]`);
      const style = sample ? getComputedStyle(sample) : null;
      context.font = style?.font || "14px sans-serif";
      const padding = style ? Number.parseInt(style.paddingLeft, 10) + Number.parseInt(style.paddingRight, 10) : 16;
      let widest = 0;
      const step = Math.max(1, Math.floor(displayRows.length / 2000));
      for (let i = 0; i < displayRows.length; i += step) {
        const row = displayRows[i];
        if (row === undefined) continue;
        widest = Math.max(widest, context.measureText(formatCell(column, row, locale)).width);
      }
      if (header) {
        context.font = getComputedStyle(header).font || context.font;
        // Room for the sort badge and the menu button.
        widest = Math.max(widest, context.measureText(column.header).width + 48);
      }
      const width = Math.ceil(widest + padding + 4);
      const min = column.minWidth ?? 48;
      const max = column.maxWidth ?? 800;
      setLayout((l) => ({ ...l, sizes: { ...l.sizes, [columnId]: Math.max(min, Math.min(max, width)) } }));
    },
    [byId, shown, displayRows, locale],
  );

  const pinColumn = useCallback((columnId: string, side: "left" | "right" | false) => {
    setLayout((l) => {
      const left = l.pinning.left.filter((x) => x !== columnId);
      const right = l.pinning.right.filter((x) => x !== columnId);
      if (side === "left") left.push(columnId);
      if (side === "right") right.unshift(columnId);
      return { ...l, pinning: { left, right } };
    });
  }, []);

  const hideColumn = useCallback((columnId: string, hidden: boolean) => {
    setLayout((l) => ({
      ...l,
      hidden: hidden ? [...new Set([...l.hidden, columnId])] : l.hidden.filter((x) => x !== columnId),
    }));
  }, []);

  const moveShown = useCallback(
    (columnId: string, direction: -1 | 1) => {
      const at = shown.findIndex((c) => c.id === columnId);
      const neighbour = shown[at + direction];
      if (!neighbour) return;
      setLayout((l) => ({ ...l, order: moveColumn(l.order, columnId, neighbour.id, direction === 1) }));
      // The active cell moves with its column.
      const state = navRef.current;
      if (state.active.col === at) {
        const cell = { row: state.active.row, col: at + direction };
        setNav({ ...state, active: cell, anchor: cell, extent: cell });
      }
    },
    [shown, setNav],
  );

  const closeMenu = useCallback(() => {
    setMenu(null);
    hasFocus.current = true;
  }, []);

  // ------------------------------------------------------------ filters
  const [referenceLabels, setReferenceLabels] = useState<Map<string, string>>(() => new Map());
  const rememberLabels = useCallback((options: Option[]) => {
    setReferenceLabels((current) => {
      if (options.every((o) => current.get(o.value) === o.label)) return current;
      const next = new Map(current);
      for (const o of options) next.set(o.value, o.label);
      return next;
    });
  }, []);

  const setFilter = useCallback((columnId: string, condition: FilterCondition | null) => {
    setColumnFilters((current) => {
      const rest = current.filter((f) => f.id !== columnId);
      return condition && isComplete(condition) ? [...rest, { id: columnId, value: condition }] : rest;
    });
  }, []);

  /** Open a column's menu or filter by pointer or from a chip. Opened from the keyboard, the active cell stays. */
  const openMenuFor = useCallback(
    (columnId: string, kind: "column" | "filter") => {
      const col = shown.findIndex((c) => c.id === columnId);
      if (col === -1) return;
      const state = navRef.current;
      if (state.active.col !== col || state.active.row !== -1) {
        const cell = { row: -1, col };
        setNav({ ...state, active: cell, anchor: { row: 0, col }, extent: { row: 0, col }, editing: null });
      }
      setMenu({ columnId, kind });
    },
    [shown, setNav],
  );

  const chips: FilterChip[] = useMemo(
    () =>
      columnFilters.flatMap((f) => {
        const column = byId.get(f.id);
        if (!column) return [];
        return [
          {
            columnId: f.id,
            label: describeFilter(column, f.value as FilterCondition, labels, locale, referenceLabels),
            remove: () => setFilter(f.id, null),
            edit: () => openMenuFor(f.id, "filter"),
          },
        ];
      }),
    [columnFilters, byId, labels, locale, referenceLabels, setFilter, openMenuFor],
  );

  // ------------------------------------------------------------ totals
  const footerTotals = useMemo(() => {
    const wanted = shown.filter((c) => c.total && (c.type === "number" || c.type === "money"));
    if (wanted.length === 0) return null;
    const out = new Map<string, { total: string | null; text: string }>();
    for (const column of wanted) {
      if (server) {
        const value = totals?.[column.id] ?? null;
        out.set(column.id, {
          total: value,
          text: value === null ? "" : formatDecimal(value, column.minFraction ?? 0, locale),
        });
        continue;
      }
      const currencies = new Set<string>();
      const values: string[] = [];
      for (const { original: row } of filteredRows) {
        const decimal = toDecimal(cellValue(column, row));
        if (decimal === null) continue;
        values.push(decimal);
        const currency = typeof column.currency === "function" ? column.currency(row) : column.currency;
        if (currency) currencies.add(currency);
      }
      if (currencies.size > 1) {
        out.set(column.id, { total: null, text: labels.mixed });
      } else {
        const sum = sumDecimals(values);
        out.set(column.id, { total: sum, text: formatDecimal(sum, column.minFraction ?? 0, locale) });
      }
    }
    return out;
  }, [shown, server, totals, filteredRows, locale, labels]);

  // ------------------------------------------------------------ render
  const totalWidth = table.getTotalSize();
  const leftWidth = table.getLeftTotalSize();
  const rightWidth = table.getRightTotalSize();
  const footerHeight = footerTotals ? rowHeight : 0;
  const pageOffset = server ? pageIndex * size : 0;
  const totalRows = server ? (total ?? rowCount) : rows.length;
  const ariaRowCount = (server ? totalRows : rowCount) + 1 + (footerTotals ? 1 : 0);

  // biome-ignore lint/correctness/useExhaustiveDependencies: sizes and pinning live in the layout
  const cellStyle = useCallback((column: Column<Row, unknown>, header = false): CSSProperties => {
    const pinned = column.getIsPinned();
    const style: CSSProperties = {
      width: column.getSize(),
      minWidth: column.getSize(),
      maxWidth: column.getSize(),
      boxSizing: "border-box",
      flex: "none",
      position: pinned ? "sticky" : "relative",
      overflow: "hidden",
    };
    if (pinned === "left") style.left = column.getStart("left");
    if (pinned === "right") style.right = column.getAfter("right");
    if (pinned) style.zIndex = header ? 3 : 1;
    return style;
  }, [layout]);

  const virtualItems = virtualizer.getVirtualItems();
  const sortInfo = (column: Column<Row, unknown>): SortInfo | null => {
    const direction = column.getIsSorted();
    if (!direction) return null;
    return { direction, order: sorting.length > 1 ? column.getSortIndex() + 1 : null };
  };

  const { Part, Row: RowSlot, HeaderCell, FooterCell, ResizeHandle, ColumnMenu, FilterPopover, Toolbar } = slots;
  const { SearchBox, FilterChips, ColumnsMenu, Pager, Empty } = slots;

  const headerCells = shownColumns.map((tc, col) => {
    const column = shown[col] as GridColumn<Row>;
    const isActive = active.row === -1 && active.col === col;
    const condition = (columnFilters.find((f) => f.id === column.id)?.value as FilterCondition | undefined) ?? null;
    const align = alignOf(column.type);
    const sort = sortInfo(tc);
    const pinned = tc.getIsPinned();
    const filterable = column.filterable !== false;
    const menuOpen = menu?.columnId === column.id && menu.kind === "column";
    const filterOpen = menu?.columnId === column.id && menu.kind === "filter";
    const filterLabel = `${labels.filter}: ${column.header}`;
    const menuLabel = `${labels.columnMenu}: ${column.header}`;

    const filter = filterable ? (
      <FilterPopover
        column={column}
        open={filterOpen}
        onOpenChange={(open) => (open ? openMenuFor(column.id, "filter") : closeMenu())}
        triggerProps={{
          tabIndex: -1,
          "aria-label": filterLabel,
          "aria-haspopup": "dialog",
          "aria-expanded": filterOpen,
          "data-grid-trigger": "",
        }}
        contentProps={{ "data-grid-menu": "", "aria-label": filterLabel }}
        active={condition !== null}
        labels={labels}
      >
        {filterOpen ? (
          <FilterBody<Row>
            column={column}
            condition={condition}
            onChange={(c) => setFilter(column.id, c)}
            onDone={closeMenu}
            labels={labels}
            knownLabels={referenceLabels}
            rememberLabels={rememberLabels}
            slots={slots}
          />
        ) : null}
      </FilterPopover>
    ) : null;

    const menuNode = (
      <ColumnMenu
        column={column}
        open={menuOpen}
        onOpenChange={(open) => (open ? openMenuFor(column.id, "column") : closeMenu())}
        triggerProps={{
          tabIndex: -1,
          "aria-label": menuLabel,
          "aria-haspopup": "menu",
          "aria-expanded": menuOpen,
          "data-grid-trigger": "",
        }}
        contentProps={{ "data-grid-menu": "", "aria-label": menuLabel }}
        sort={sort}
        sortable={tc.getCanSort()}
        sortAsc={() => tc.toggleSorting(false, tc.getIsSorted() !== false && sorting.length > 1)}
        sortDesc={() => tc.toggleSorting(true, tc.getIsSorted() !== false && sorting.length > 1)}
        clearSort={() => tc.clearSorting()}
        filterable={filterable}
        filtered={condition !== null}
        openFilter={() => openMenuFor(column.id, "filter")}
        clearFilter={() => setFilter(column.id, null)}
        pinned={pinned}
        pin={(side) => pinColumn(column.id, side)}
        hideable={tc.getCanHide()}
        hide={() => {
          hideColumn(column.id, true);
          closeMenu();
        }}
        autoFit={() => autoFit(column.id)}
        canMoveLeft={col > 0}
        canMoveRight={col < shownColumns.length - 1}
        move={(direction) => moveShown(column.id, direction)}
        labels={labels}
      />
    );

    const header = table.getHeaderGroups()[0]?.headers.find((h) => h.column.id === column.id);
    const resizeHandle =
      tc.getCanResize() && header ? (
        <ResizeHandle
          column={column}
          resizing={tc.getIsResizing()}
          props={{
            "aria-hidden": true,
            "data-grid-resize": "",
            "data-resizing": tc.getIsResizing() || undefined,
            style: { position: "absolute", top: 0, bottom: 0, right: 0, cursor: "col-resize", touchAction: "none" },
            onMouseDown: (event) => {
              event.stopPropagation();
              header.getResizeHandler()(event);
            },
            onTouchStart: (event) => {
              event.stopPropagation();
              header.getResizeHandler()(event);
            },
            onDoubleClick: (event) => {
              event.stopPropagation();
              autoFit(column.id);
            },
          }}
        />
      ) : null;

    const props: ElementProps = {
      role: "columnheader",
      id: `${id}-h-${column.id}`,
      "aria-colindex": col + 1,
      "aria-sort": sort ? (sort.direction === "asc" ? "ascending" : "descending") : tc.getCanSort() ? "none" : undefined,
      tabIndex: isActive ? 0 : -1,
      "data-grid-cell": "",
      "data-row": -1,
      "data-col": col,
      "data-active": isActive || undefined,
      "data-pinned": pinned || undefined,
      "data-sorted": sort?.direction,
      "data-filtered": condition !== null || undefined,
      "data-type": column.type,
      "data-align": align,
      "data-dragging": dragColumn === column.id || undefined,
      "data-drop": dropHint?.id === column.id ? (dropHint.after ? "after" : "before") : undefined,
      style: cellStyle(tc, true),
      onMouseDown: (event) => {
        // React passes on events from portals (the menus): only the header's own count.
        if (!outsidePortal(event)) return;
        // Pressing on the header (not its menu triggers) may start a drag that reorders columns.
        const target = event.target as HTMLElement;
        if (event.button === 0 && !target.closest("[data-grid-trigger], [data-grid-resize]")) {
          headerDrag.current = { columnId: column.id, startX: event.clientX, moving: false };
        }
        hasFocus.current = true;
        const state = navRef.current;
        if (state.editing) runEffects([{ type: "commit", edit: state.editing }], state);
        setNav({ ...navRef.current, active: { row: -1, col }, editing: null });
      },
    };

    return (
      <HeaderCell
        key={column.id}
        column={column}
        props={props}
        label={column.header}
        sort={sort}
        sortable={tc.getCanSort()}
        sortProps={{
          type: "button",
          tabIndex: -1,
          disabled: !tc.getCanSort(),
          onClick: (event) => tc.toggleSorting(undefined, event.shiftKey),
        }}
        filtered={condition !== null}
        active={isActive}
        pinned={pinned}
        align={align}
        filter={filter}
        menu={menuNode}
        resizeHandle={resizeHandle}
      />
    );
  });

  const searchable = shown.some(isSearchable);
  const columnsMenu = (
    <ColumnsMenu
      columns={table.getAllLeafColumns().map((tc) => {
        const column = (tc.columnDef.meta as ColumnMeta<Row>).column;
        return {
          column,
          visible: tc.getIsVisible(),
          hideable: tc.getCanHide(),
          toggle: () => hideColumn(column.id, tc.getIsVisible()),
        };
      })}
      resetLayout={() => setLayout(defaultLayout(columns))}
      labels={labels}
    />
  );
  const clearAll = () => {
    setColumnFilters([]);
    setSearch("");
  };

  const pageCount = Math.max(1, Math.ceil(totalRows / size));
  const groupStyle: CSSProperties = { width: totalWidth, minWidth: "100%" };

  return (
    <Part
      kind="root"
      loading={loading}
      props={{ ref: setRootRef, "data-grid-root": "", "data-mode": mode, onFocus: onFocusIn, onBlur: onFocusOut }}
    >
      {toolbar ? (
        <Toolbar
          props={{ "data-grid-toolbar": "" }}
          search={
            searchable ? (
              <SearchBox
                value={search}
                setValue={setSearch}
                inputProps={{
                  type: "search",
                  value: search,
                  onChange: (event) => setSearch(event.target.value),
                  "aria-label": labels.search,
                  placeholder: labels.search,
                }}
                labels={labels}
              />
            ) : null
          }
          chips={<FilterChips chips={chips} clearAll={clearAll} canClear={chips.length > 0 || search !== ""} labels={labels} />}
          columns={columnsMenu}
          shownRows={server ? totalRows : rowCount}
          totalRows={totalRows}
          extra={toolbarExtra}
          labels={labels}
        />
      ) : (
        toolbarExtra
      )}
      <Part
        kind="scroller"
        loading={loading}
        props={{
          ref: setScrollerRef,
          role: "grid",
          id,
          "aria-label": props["aria-label"],
          "aria-rowcount": ariaRowCount,
          "aria-colcount": colCount,
          "aria-multiselectable": true,
          "aria-busy": loading || undefined,
          "data-grid-scroller": "",
          style: {
            height,
            overflow: "auto",
            position: "relative",
            scrollPaddingTop: headerHeight,
            scrollPaddingBottom: footerHeight,
            scrollPaddingLeft: leftWidth,
            scrollPaddingRight: rightWidth,
            overscrollBehavior: "contain",
          },
          onKeyDown,
          onCopy,
          onCut,
          onPaste,
        }}
      >
        <Part
          kind="header"
          loading={loading}
          props={{
            ref: setHeaderRef,
            role: "rowgroup",
            "data-grid-header": "",
            style: { ...groupStyle, position: "sticky", top: 0, zIndex: 2 },
          }}
        >
          <RowSlot
            kind="header"
            row={null}
            rowIndex={-1}
            selected={false}
            props={{ role: "row", "aria-rowindex": 1, style: { display: "flex", width: totalWidth } }}
          >
            {headerCells}
          </RowSlot>
        </Part>
        <Part
          kind="body"
          loading={loading}
          props={{
            role: "rowgroup",
            "data-grid-body": "",
            style: { height: virtualizer.getTotalSize(), width: totalWidth, position: "relative" },
          }}
        >
          {virtualItems.map((item) => {
            const r = item.index;
            const row = displayRows[r];
            if (row === undefined) return null;
            const inRows = r >= range.top && r <= range.bottom;
            return (
              <GridRow<Row>
                key={item.key}
                row={row}
                rowIndex={r}
                ariaRowIndex={pageOffset + r + 2}
                top={item.start - headerHeight}
                height={rowHeight}
                width={totalWidth}
                shownColumns={shownColumns}
                shown={shown}
                cellStyle={cellStyle}
                selLeft={inRows ? range.left : -1}
                selRight={inRows ? range.right : -1}
                selTop={r === range.top}
                selBottom={r === range.bottom}
                activeCol={active.row === r ? active.col : -1}
                editing={editing && editing.row === r ? editing : null}
                error={editError && editError.row === r ? editError : null}
                editable={editable}
                locale={locale}
                slots={slots}
                rowIdOf={rowIdOf}
                gridId={id}
                editorRef={editorRef}
                setDraft={setDraft}
                commitEdit={commitEdit}
                cancelEdit={cancelEdit}
                onMouseDown={onCellMouseDown}
                onMouseEnter={onCellMouseEnter}
                onDoubleClick={onCellDoubleClick}
              />
            );
          })}
          {rowCount === 0 ? (
            <div style={{ position: "sticky", left: 0, width: "100%" }}>
              <Empty filtered={filtered} loading={loading} labels={labels} />
            </div>
          ) : null}
        </Part>
        {footerTotals ? (
          <Part
            kind="footer"
            loading={loading}
            props={{
              role: "rowgroup",
              "data-grid-footer": "",
              style: { ...groupStyle, position: "sticky", bottom: 0, zIndex: 2 },
            }}
          >
            <RowSlot
              kind="footer"
              row={null}
              rowIndex={-1}
              selected={false}
              props={{ role: "row", "aria-rowindex": ariaRowCount, style: { display: "flex", width: totalWidth } }}
            >
              {shownColumns.map((tc, col) => {
                const column = shown[col] as GridColumn<Row>;
                const found = footerTotals.get(column.id);
                const align = alignOf(column.type);
                return (
                  <FooterCell
                    key={column.id}
                    column={column}
                    total={found?.total ?? null}
                    text={found ? found.text : col === 0 ? labels.total : ""}
                    align={align}
                    props={{
                      role: "gridcell",
                      "aria-colindex": col + 1,
                      "data-type": column.type,
                      "data-align": align,
                      "data-pinned": tc.getIsPinned() || undefined,
                      style: cellStyle(tc, true),
                    }}
                  />
                );
              })}
            </RowSlot>
          </Part>
        ) : null}
      </Part>
      {server ? (
        <Pager
          page={pageIndex}
          pageCount={pageCount}
          pageSize={size}
          total={totalRows}
          setPage={(page) => setPageIndex(Math.max(0, Math.min(page, pageCount - 1)))}
          canPrevious={pageIndex > 0}
          canNext={pageIndex < pageCount - 1}
          labels={labels}
        />
      ) : null}
      <div aria-live="polite" role="status" data-grid-live="" style={VISUALLY_HIDDEN}>
        {announcement}
      </div>
    </Part>
  );
}

/** Read by screen readers, never seen: not a look, so the grid keeps it. */
const VISUALLY_HIDDEN: CSSProperties = {
  position: "absolute",
  width: 1,
  height: 1,
  margin: -1,
  padding: 0,
  border: 0,
  overflow: "hidden",
  clip: "rect(0 0 0 0)",
  whiteSpace: "nowrap",
};

/** The filter form for a column: a checklist or a condition, through the slots. */
function FilterBody<Row>(
  props: ChecklistOptions<Row> & { slots: GridSlots<Row> },
) {
  const kind = filterKind(props.column.type);
  return kind === "set" ? <ChecklistHost {...props} /> : <ConditionHost {...props} />;
}

function ChecklistHost<Row>({ slots, ...options }: ChecklistOptions<Row> & { slots: GridSlots<Row> }) {
  const state = useChecklistFilter(options);
  const { ChecklistFilter } = slots;
  return <ChecklistFilter {...state} />;
}

function ConditionHost<Row>({ slots, ...options }: ChecklistOptions<Row> & { slots: GridSlots<Row> }) {
  const state = useConditionFilter(options);
  const { ConditionFilter } = slots;
  return <ConditionFilter {...state} />;
}

interface GridRowProps<Row> {
  row: Row;
  rowIndex: number;
  ariaRowIndex: number;
  top: number;
  height: number;
  width: number;
  shownColumns: Column<Row, unknown>[];
  shown: GridColumn<Row>[];
  cellStyle: (column: Column<Row, unknown>) => CSSProperties;
  selLeft: number;
  selRight: number;
  /** Is this the first (last) row of the selection? For drawing its edges. */
  selTop: boolean;
  selBottom: boolean;
  activeCol: number;
  editing: Edit | null;
  error: { col: number; message: string } | null;
  editable: boolean;
  locale: string | undefined;
  slots: GridSlots<Row>;
  rowIdOf: (row: Row, index: number) => string;
  gridId: string;
  editorRef: React.RefObject<(HTMLInputElement & HTMLSelectElement & HTMLTextAreaElement) | null>;
  setDraft(text: string): void;
  commitEdit(text?: string): void;
  cancelEdit(): void;
  onMouseDown(event: ReactMouseEvent, row: number, col: number): void;
  onMouseEnter(row: number, col: number): void;
  onDoubleClick(row: number, col: number): void;
}

function GridRowInner<Row>(props: GridRowProps<Row>) {
  const { row, rowIndex: r, shownColumns, shown, slots, editing, error } = props;
  const { Row: RowSlot, Cell: CellSlot, Editor } = slots;
  const rowId = props.rowIdOf(row, r);
  const selectedRow = props.selLeft !== -1;
  return (
    <RowSlot
      kind="body"
      row={row}
      rowIndex={r}
      selected={selectedRow}
      props={{
        role: "row",
        "aria-rowindex": props.ariaRowIndex,
        "data-row-id": rowId,
        "data-selected": selectedRow || undefined,
        style: {
          position: "absolute",
          top: 0,
          left: 0,
          display: "flex",
          width: props.width,
          height: props.height,
          transform: `translateY(${props.top}px)`,
        },
      }}
    >
      {shownColumns.map((tc, c) => {
        const column = shown[c] as GridColumn<Row>;
        const value = cellValue(column, row);
        const isActive = props.activeCol === c;
        const selected = selectedRow && c >= props.selLeft && c <= props.selRight;
        const canEdit = props.editable && isEditable(column, row);
        const align = alignOf(column.type);
        const isEditing = editing !== null && editing.col === c;
        const cellError = error && error.col === c ? error.message : null;
        const editor =
          isEditing && editing ? (
            <Editor
              column={column}
              row={row}
              value={value}
              draft={editing.draft}
              setDraft={props.setDraft}
              commit={props.commitEdit}
              cancel={props.cancelEdit}
              mode={editing.mode}
              inputProps={{
                ref: props.editorRef,
                value: editing.draft,
                onChange: (event) => props.setDraft(event.target.value),
                "aria-label": column.header,
                "aria-invalid": cellError ? true : undefined,
                "data-grid-editor": "",
                ...(column.type === "number" || column.type === "money" ? { inputMode: "decimal" as const } : {}),
              }}
              options={column.options ?? []}
              loadOptions={column.loadOptions}
              align={align}
              error={cellError}
            />
          ) : null;
        return (
          <CellSlot
            key={column.id}
            column={column}
            row={row}
            value={value}
            text={formatCell(column, row, props.locale)}
            editor={editor}
            rowIndex={r}
            colIndex={c}
            active={isActive}
            selected={selected}
            editing={isEditing}
            editable={canEdit}
            invalid={cellError !== null}
            align={align}
            props={{
              role: "gridcell",
              id: `${props.gridId}-${r}-${c}`,
              "aria-colindex": c + 1,
              "aria-selected": selected,
              "aria-readonly": !canEdit || undefined,
              "aria-invalid": cellError ? true : undefined,
              tabIndex: isActive ? 0 : -1,
              "data-grid-cell": "",
              "data-row": r,
              "data-col": c,
              "data-active": isActive || undefined,
              "data-selected": selected || undefined,
              "data-sel-top": (selected && props.selTop) || undefined,
              "data-sel-bottom": (selected && props.selBottom) || undefined,
              "data-sel-left": (selected && c === props.selLeft) || undefined,
              "data-sel-right": (selected && c === props.selRight) || undefined,
              "data-editing": isEditing || undefined,
              "data-editable": canEdit || undefined,
              "data-pinned": tc.getIsPinned() || undefined,
              "data-type": column.type,
              "data-align": align,
              style: { ...props.cellStyle(tc), height: props.height },
              onMouseDown: (event) => {
                if (outsidePortal(event)) props.onMouseDown(event, r, c);
              },
              onMouseEnter: () => props.onMouseEnter(r, c),
              onDoubleClick: (event) => {
                if (outsidePortal(event)) props.onDoubleClick(r, c);
              },
            }}
          />
        );
      })}
    </RowSlot>
  );
}

const GridRow = memo(GridRowInner) as typeof GridRowInner;
