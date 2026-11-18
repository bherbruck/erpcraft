// What a design system supplies. The grid has no look of its own: no CSS,
// no colours, borders, fonts or spacing. Every visible element is drawn by
// a slot, which gets the state it shows and `props` to spread on its own
// root element. The props carry the grid's behaviour: role and ARIA, the
// roving tabindex, data attributes for state, event handlers, and the
// layout the virtual rows need (widths, sticky and absolute positions).
//
//   Cell: ({ props, text, editor }) => <TableCell {...props}>{editor ?? text}</TableCell>
//
// Spread `props` first and merge your own className and style after it,
// keeping `props.style`. `@erpcraft/grid/reference` has a bare version of
// every slot, in browser-default HTML, for tests and the demo only.

import type {
  ChangeEvent,
  ComponentType,
  CSSProperties,
  HTMLAttributes,
  KeyboardEvent,
  ReactNode,
  Ref,
} from "react";
import type { FilterLabels } from "../core/filters";
import type { GridColumn, Option } from "../types";

/** Data attributes the grid sets for state: `data-active`, `data-selected`, `data-sorted`… */
export type DataAttributes = { [key: `data-${string}`]: string | number | boolean | undefined };

/**
 * Props for a slot's root element. Spread them; keep their `style`, `ref` and
 * handlers. The ref is a callback taking any element, so the props fit a
 * div, a td or a table alike.
 */
export type ElementProps<E extends HTMLElement = HTMLElement> = Omit<HTMLAttributes<E>, "ref"> &
  DataAttributes & { ref?: (element: HTMLElement | null) => void; style?: CSSProperties };

export type Align = "start" | "end" | "center";

// ------------------------------------------------------------ structure

/** The grid's containers: `kind` says which one. A design system maps them to its table parts. */
export interface PartSlotProps {
  kind: "root" | "scroller" | "header" | "body" | "footer";
  props: ElementProps;
  children: ReactNode;
  loading: boolean;
}

export interface RowSlotProps<Row> {
  kind: "header" | "body" | "footer";
  props: ElementProps;
  children: ReactNode;
  /** Body rows only. */
  row: Row | null;
  rowIndex: number;
  selected: boolean;
}

export interface SortInfo {
  direction: "asc" | "desc";
  /** 1-based position in a multi-column sort; null when only one column is sorted. */
  order: number | null;
}

export interface HeaderCellSlotProps<Row> {
  column: GridColumn<Row>;
  /** For the header cell element (TableHead). It is the tab stop and the drag source for reordering. */
  props: ElementProps;
  label: string;
  sort: SortInfo | null;
  sortable: boolean;
  /** For a button around the label: clicking sorts, Shift+click adds to the sort. tabIndex is -1. */
  sortProps: ElementProps<HTMLButtonElement> & { type: "button"; disabled: boolean; tabIndex: -1 };
  filtered: boolean;
  active: boolean;
  pinned: "left" | "right" | false;
  align: Align;
  /** The FilterPopover slot for this column, or null if it is not filterable. */
  filter: ReactNode;
  /** The ColumnMenu slot for this column. */
  menu: ReactNode;
  /** The ResizeHandle slot, or null. Place it last; its props position it on the right edge. */
  resizeHandle: ReactNode;
}

export interface CellSlotProps<Row> {
  column: GridColumn<Row>;
  /** For the cell element (TableCell). */
  props: ElementProps;
  row: Row;
  value: unknown;
  /** The cell's value as text, formatted by type. It is also what is copied. */
  text: string;
  /** The Editor slot while the cell is edited, else null. Render it instead of the text. */
  editor: ReactNode;
  rowIndex: number;
  colIndex: number;
  active: boolean;
  selected: boolean;
  editing: boolean;
  editable: boolean;
  invalid: boolean;
  align: Align;
}

export interface FooterCellSlotProps<Row> {
  column: GridColumn<Row>;
  props: ElementProps;
  /** The total as a decimal string; null where the column has none. */
  total: string | null;
  /** The total formatted, a word when it cannot be added up (mixed currencies), or "Total" in the first column. */
  text: string;
  align: Align;
}

export interface ResizeHandleSlotProps<Row> {
  column: GridColumn<Row>;
  /** Pointer handlers (drag to resize, double click to fit) and the position on the column's right edge. Give it a width. */
  props: ElementProps;
  resizing: boolean;
}

// ------------------------------------------------------------ editing

export interface EditorSlotProps<Row> {
  column: GridColumn<Row>;
  row: Row;
  value: unknown;
  /** For an Input: value, onChange, ref (the grid focuses it), aria-label, aria-invalid. */
  inputProps: {
    ref: Ref<HTMLInputElement & HTMLTextAreaElement & HTMLSelectElement>;
    value: string;
    onChange(event: ChangeEvent<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>): void;
    "aria-label": string;
    "aria-invalid": boolean | undefined;
    "data-grid-editor": "";
    inputMode?: "decimal" | "text";
  };
  draft: string;
  setDraft(text: string): void;
  /** Commit the draft and move down, as Enter does. For a select or combobox whose list keeps the keys. */
  commit(text?: string): void;
  cancel(): void;
  /** "enter": started by typing, arrows commit; "edit": started with F2 or Enter, arrows move the caret. */
  mode: "enter" | "edit";
  /** Choices of enum, state and reference columns. */
  options: Option[];
  loadOptions?: (search: string, signal: AbortSignal) => Promise<Option[]>;
  align: Align;
  /** Why the last commit was refused, or null. */
  error: string | null;
}

// ------------------------------------------------------------ menus

/** Props for a menu's trigger button. Spread on the trigger (DropdownMenuTrigger / PopoverTrigger asChild). */
export type TriggerProps = {
  tabIndex: -1;
  "aria-label": string;
  "aria-haspopup": "menu" | "dialog";
  "aria-expanded": boolean;
  "data-grid-trigger": "";
};

/** Props for the menu's content in its portal. `data-grid-menu` keeps the grid's focus while it is open. */
export type ContentProps = { "data-grid-menu": ""; "aria-label": string };

export interface ColumnMenuSlotProps<Row> {
  column: GridColumn<Row>;
  open: boolean;
  onOpenChange(open: boolean): void;
  triggerProps: TriggerProps;
  contentProps: ContentProps;
  sort: SortInfo | null;
  sortable: boolean;
  sortAsc(): void;
  sortDesc(): void;
  clearSort(): void;
  filterable: boolean;
  filtered: boolean;
  /** Close the menu and open the column's filter popover. */
  openFilter(): void;
  clearFilter(): void;
  pinned: "left" | "right" | false;
  pin(side: "left" | "right" | false): void;
  hideable: boolean;
  hide(): void;
  autoFit(): void;
  canMoveLeft: boolean;
  canMoveRight: boolean;
  move(direction: -1 | 1): void;
  labels: GridLabels;
}

export interface FilterPopoverSlotProps<Row> {
  column: GridColumn<Row>;
  open: boolean;
  onOpenChange(open: boolean): void;
  triggerProps: TriggerProps;
  contentProps: ContentProps;
  /** Is a filter set on the column? Show it on the trigger. */
  active: boolean;
  /** The ConditionFilter or ChecklistFilter slot for the column. */
  children: ReactNode;
  labels: GridLabels;
}

/** Props for an input of a filter form: Enter applies the filter and closes the popover. */
export type FilterInputProps = {
  value: string;
  onChange(event: ChangeEvent<HTMLInputElement>): void;
  onKeyDown(event: KeyboardEvent<HTMLInputElement>): void;
  "aria-label": string;
  type: "text" | "date";
  inputMode?: "decimal";
};

/** A condition and its value(s): text, number, money, date and boolean columns. */
export interface ConditionFilterSlotProps<Row> {
  column: GridColumn<Row>;
  kind: "text" | "number" | "date" | "boolean";
  /** Conditions to choose from, labelled ("contains", "≥", "between", "in"…). For a Select. */
  ops: Option[];
  op: string;
  setOp(op: string): void;
  /** Show the value input, the second input of "between", and the range Select of relative dates. */
  needsValue: boolean;
  needsTo: boolean;
  needsRange: boolean;
  valueProps: FilterInputProps;
  toProps: FilterInputProps;
  ranges: Option[];
  range: string;
  setRange(range: string): void;
  /** Is the form complete? Apply is disabled until it is. */
  valid: boolean;
  /** Apply the condition and close. A boolean column applies on choosing; it needs no Apply. */
  apply(): void;
  clear(): void;
  active: boolean;
  labels: GridLabels;
}

export interface ChecklistItem {
  value: string;
  label: string;
  checked: boolean;
  toggle(): void;
}

/** A checklist with search: enum, state and reference columns. Applies as boxes are ticked. */
export interface ChecklistFilterSlotProps<Row> {
  column: GridColumn<Row>;
  search: string;
  setSearch(text: string): void;
  /** For a search Input; with shadcn Command, use `search`/`setSearch` on CommandInput and shouldFilter={false}. */
  searchProps: FilterInputProps;
  items: ChecklistItem[];
  /** "Select all" over the items shown. */
  allChecked: boolean;
  someChecked: boolean;
  toggleAll(): void;
  /** The "(empty)" choice; null while searching. */
  emptyItem: ChecklistItem | null;
  /** Choices are being loaded (reference columns). */
  loading: boolean;
  active: boolean;
  clear(): void;
  /** Close the popover. */
  done(): void;
  labels: GridLabels;
}

// ------------------------------------------------------------ around the grid

export interface FilterChip {
  columnId: string;
  label: string;
  remove(): void;
  /** Open the column's filter popover to change it. */
  edit(): void;
  /** The server cannot apply this filter (see `unsupported` of toErpcraftQuery); set by the consumer. */
  unsupported?: string;
}

export interface ToolbarSlotProps {
  props: ElementProps;
  /** The SearchBox slot, or null when no column is searchable. */
  search: ReactNode;
  /** The FilterChips slot. */
  chips: ReactNode;
  /** The ColumnsMenu slot. */
  columns: ReactNode;
  shownRows: number;
  totalRows: number;
  /** The consumer's `toolbarExtra`. */
  extra: ReactNode;
  labels: GridLabels;
}

export interface SearchBoxSlotProps {
  inputProps: {
    type: "search";
    value: string;
    onChange(event: ChangeEvent<HTMLInputElement>): void;
    "aria-label": string;
    placeholder: string;
  };
  value: string;
  setValue(text: string): void;
  labels: GridLabels;
}

export interface FilterChipsSlotProps {
  chips: FilterChip[];
  /** Remove every filter and the search. */
  clearAll(): void;
  /** Is there anything to clear (filters or a search)? */
  canClear: boolean;
  labels: GridLabels;
}

export interface ColumnsMenuSlotProps<Row> {
  /** Every column in shown order, with whether it is visible. For DropdownMenuCheckboxItem. */
  columns: { column: GridColumn<Row>; visible: boolean; hideable: boolean; toggle(): void }[];
  resetLayout(): void;
  labels: GridLabels;
}

export interface PagerSlotProps {
  page: number;
  pageCount: number;
  pageSize: number;
  total: number;
  setPage(page: number): void;
  canPrevious: boolean;
  canNext: boolean;
  labels: GridLabels;
}

export interface EmptySlotProps {
  filtered: boolean;
  loading: boolean;
  labels: GridLabels;
}

export interface GridSlots<Row> {
  /** Containers: root, scroller (the role=grid element), header, body and footer groups. */
  Part: ComponentType<PartSlotProps>;
  Row: ComponentType<RowSlotProps<Row>>;
  HeaderCell: ComponentType<HeaderCellSlotProps<Row>>;
  Cell: ComponentType<CellSlotProps<Row>>;
  FooterCell: ComponentType<FooterCellSlotProps<Row>>;
  ResizeHandle: ComponentType<ResizeHandleSlotProps<Row>>;
  Editor: ComponentType<EditorSlotProps<Row>>;
  ColumnMenu: ComponentType<ColumnMenuSlotProps<Row>>;
  FilterPopover: ComponentType<FilterPopoverSlotProps<Row>>;
  ConditionFilter: ComponentType<ConditionFilterSlotProps<Row>>;
  ChecklistFilter: ComponentType<ChecklistFilterSlotProps<Row>>;
  Toolbar: ComponentType<ToolbarSlotProps>;
  SearchBox: ComponentType<SearchBoxSlotProps>;
  FilterChips: ComponentType<FilterChipsSlotProps>;
  ColumnsMenu: ComponentType<ColumnsMenuSlotProps<Row>>;
  Pager: ComponentType<PagerSlotProps>;
  Empty: ComponentType<EmptySlotProps>;
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
  editFilter: "Filter…",
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
  condition: "Condition",
  range: "Range",
  columnMenu: "Column menu",
  filters: "Filters",
  pasted: "Pasted {applied} cells",
  cleared: "Cleared {applied} cells",
  undone: "Undone",
  redone: "Redone",
  rejected: "{count} not accepted",
  nothingToUndo: "Nothing to undo",
} as const;

export type GridLabels = Record<keyof typeof DEFAULT_GRID_LABELS, string> & FilterLabels;
