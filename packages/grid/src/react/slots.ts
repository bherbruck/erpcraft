// What a design system supplies: small components that draw the parts of
// the grid. The grid owns structure, state, focus and keyboard; a slot only
// draws what it is given and calls back. Every slot has a plain default in
// `@erpcraft/grid/reference`.
//
// Inner controls of a header (sort button, menu button) take tabIndex={-1}:
// the header cell itself is the tab stop, and the keyboard reaches its
// actions with Enter (sort) and Alt+ArrowDown (menu).

import type { ComponentType, ReactNode, Ref } from "react";
import type { FilterLabels } from "../core/filters";
import type { ColumnFilter, FilterCondition, GridColumn, Option } from "../types";

export interface CellSlotProps<Row> {
  column: GridColumn<Row>;
  row: Row;
  value: unknown;
  /** The cell's value as text, formatted by type. */
  text: string;
  rowIndex: number;
  colIndex: number;
  active: boolean;
  selected: boolean;
  editable: boolean;
  align: "start" | "end" | "center";
}

export interface EditorSlotProps<Row> {
  column: GridColumn<Row>;
  row: Row;
  value: unknown;
  draft: string;
  setDraft(text: string): void;
  /** "enter": started by typing, arrows commit; "edit": started with F2 or Enter, arrows move the caret. */
  mode: "enter" | "edit";
  /** The grid focuses this element when the edit starts. Keys bubble to the grid, which commits and cancels. */
  inputRef: Ref<HTMLInputElement & HTMLSelectElement & HTMLTextAreaElement>;
  /** Choices for enum, state and reference columns. */
  options: Option[];
  /** Search choices of a reference column. */
  loadOptions?: (search: string, signal: AbortSignal) => Promise<Option[]>;
  align: "start" | "end" | "center";
  /** Set when the last commit of this cell was refused. */
  error: string | null;
}

export interface SortInfo {
  direction: "asc" | "desc";
  /** 1-based position in a multi-column sort; null when only one column is sorted. */
  order: number | null;
}

export interface HeaderSlotProps<Row> {
  column: GridColumn<Row>;
  sort: SortInfo | null;
  sortable: boolean;
  /** Sort by this column; `multi` (Shift+click) adds it to the sort. */
  onSort(multi: boolean): void;
  filtered: boolean;
  align: "start" | "end" | "center";
  /** The column menu, rendered by the ColumnMenu slot. Place it in the header. */
  menu: ReactNode;
}

export interface ColumnMenuSlotProps<Row> {
  column: GridColumn<Row>;
  open: boolean;
  onOpenChange(open: boolean): void;
  sort: SortInfo | null;
  sortable: boolean;
  sortAsc(): void;
  sortDesc(): void;
  clearSort(): void;
  pinned: "left" | "right" | false;
  pin(side: "left" | "right" | false): void;
  hideable: boolean;
  hide(): void;
  autoFit(): void;
  canMoveLeft: boolean;
  canMoveRight: boolean;
  move(direction: -1 | 1): void;
  /** The typed filter form for this column, rendered by the FilterEditor slot; null if not filterable. */
  filter: ReactNode;
  labels: GridLabels;
}

export interface FilterEditorSlotProps<Row> {
  column: GridColumn<Row>;
  /** The current condition, or a blank one for the column's type. */
  condition: FilterCondition;
  active: boolean;
  /** Apply a condition (the grid ignores one that is not complete). */
  onChange(condition: FilterCondition): void;
  onClear(): void;
  /** Close the menu that holds the editor. */
  onDone(): void;
  /** Choices of enum and state columns, and of a reference column once loaded. */
  options: Option[];
  /** Search choices of a reference column on the consumer's side. */
  loadOptions?: (search: string, signal: AbortSignal) => Promise<Option[]>;
  /** Remember labels of chosen references, for the chips. */
  rememberLabels(options: Option[]): void;
  labels: GridLabels;
}

export interface FilterChip {
  columnId: string;
  label: string;
  remove(): void;
  /** Open the column's menu to change the filter. */
  edit(): void;
}

export interface ToolbarSlotProps<Row> {
  search: string;
  setSearch(text: string): void;
  searchable: boolean;
  chips: FilterChip[];
  clearAll(): void;
  /** Every column, in shown order, with whether it is visible. */
  columns: { column: GridColumn<Row>; visible: boolean; hideable: boolean; toggle(): void }[];
  resetLayout(): void;
  labels: GridLabels;
  /** Rows after filtering, and before (client mode) or on the server (server mode). */
  shownRows: number;
  totalRows: number;
}

export interface PagerSlotProps {
  page: number;
  pageCount: number;
  pageSize: number;
  total: number;
  setPage(page: number): void;
  labels: GridLabels;
}

export interface FooterCellSlotProps<Row> {
  column: GridColumn<Row>;
  /** The total as a decimal string; null where the column has none. */
  total: string | null;
  /** The total formatted, or a word when it cannot be added up (mixed currencies). */
  text: string;
  align: "start" | "end" | "center";
}

export interface EmptySlotProps {
  filtered: boolean;
  loading: boolean;
  labels: GridLabels;
}

export interface GridSlots<Row> {
  Cell: ComponentType<CellSlotProps<Row>>;
  Editor: ComponentType<EditorSlotProps<Row>>;
  Header: ComponentType<HeaderSlotProps<Row>>;
  ColumnMenu: ComponentType<ColumnMenuSlotProps<Row>>;
  FilterEditor: ComponentType<FilterEditorSlotProps<Row>>;
  Toolbar: ComponentType<ToolbarSlotProps<Row>>;
  Pager: ComponentType<PagerSlotProps>;
  FooterCell: ComponentType<FooterCellSlotProps<Row>>;
  Empty: ComponentType<EmptySlotProps>;
}

/** Class names put on the grid's own elements, for a design system to style. */
export interface GridClassNames {
  root: string;
  scroller: string;
  header: string;
  headerRow: string;
  headerCell: string;
  resizeHandle: string;
  body: string;
  row: string;
  cell: string;
  footer: string;
  footerCell: string;
  live: string;
}

export const DEFAULT_GRID_LABELS = {
  sortAsc: "Sort ascending",
  sortDesc: "Sort descending",
  clearSort: "Clear sort",
  pinLeft: "Pin left",
  pinRight: "Pin right",
  unpin: "Unpin",
  hide: "Hide column",
  autoFit: "Fit to content",
  moveLeft: "Move left",
  moveRight: "Move right",
  filter: "Filter",
  clearFilter: "Clear filter",
  clearAll: "Clear all",
  search: "Search",
  columns: "Columns",
  resetLayout: "Reset columns",
  apply: "Apply",
  done: "Done",
  selectAll: "Select all",
  noOptions: "No choices",
  loading: "Loading…",
  empty: "No rows",
  emptyFiltered: "No rows match the filters",
  previous: "Previous",
  next: "Next",
  page: "Page",
  of: "of",
  rows: "rows",
  total: "Total",
  mixed: "mixed",
  from: "From",
  to: "To",
  value: "Value",
  columnMenu: "Column menu",
  pasted: "Pasted {applied} cells",
  cleared: "Cleared {applied} cells",
  undone: "Undone",
  redone: "Redone",
  rejected: "{count} not accepted",
  nothingToUndo: "Nothing to undo",
} as const;

export type GridLabels = Record<keyof typeof DEFAULT_GRID_LABELS, string> & FilterLabels;

export type { ColumnFilter };
