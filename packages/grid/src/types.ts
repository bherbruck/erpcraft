// The public shapes of the grid. Everything here is plain data or a plain
// function: no design system, no DOM.

/** What a column holds. It decides sorting, filtering, alignment, parsing and copying. */
export type ColumnType =
  | "text"
  | "number"
  | "money"
  | "date"
  | "datetime"
  | "boolean"
  | "enum"
  | "state"
  | "reference";

/** One choice of an enum, state or reference column. */
export interface Option {
  value: string;
  label: string;
}

/** The result of turning typed or pasted text into a cell value. */
export type ParseResult = { ok: true; value: unknown } | { ok: false; error: string };

/** One column, as the consumer describes it. */
export interface GridColumn<Row> {
  id: string;
  header: string;
  type: ColumnType;
  /** The field the server knows this column by (`summary.total` on a document list). Defaults to `id`. */
  field?: string;
  /** The value of the cell. Defaults to `row[id]`. */
  value?: (row: Row) => unknown;
  /** A row with the cell set to a value, for `applyChanges`. Defaults to `{ ...row, [id]: value }`. */
  setValue?: (row: Row, value: unknown) => Row;
  /** Text shown in the cell and copied to the clipboard. Defaults by type. */
  format?: (value: unknown, row: Row) => string;
  /** Turns typed or pasted text into a value. Defaults by type. */
  parse?: (text: string, row: Row) => ParseResult;
  /** Checks a parsed value before it is accepted; return a message to refuse it. */
  validate?: (value: unknown, row: Row) => string | null;
  editable?: boolean | ((row: Row) => boolean);
  /** Choices of an enum or state column, and the labels of known references. */
  options?: Option[];
  /** Choices of a reference column, found by the consumer as the user searches. */
  loadOptions?: (search: string, signal: AbortSignal) => Promise<Option[]>;
  /** For money: the currency code, or where to find it on the row. Only for display. */
  currency?: string | ((row: Row) => string | null | undefined);
  /** Decimal places a number or money value shows (at least). */
  minFraction?: number;
  width?: number;
  minWidth?: number;
  maxWidth?: number;
  sortable?: boolean;
  filterable?: boolean;
  /** Included in the quick search. Defaults to true for text columns. */
  searchable?: boolean;
  /** Show a total of the column in the footer (number and money). */
  total?: boolean;
  pin?: "left" | "right";
  hidden?: boolean;
  /** Can the user hide it? Defaults to true. */
  hideable?: boolean;
}

// ------------------------------------------------------------------ filters

export type TextOp = "contains" | "equals" | "startsWith" | "endsWith" | "empty" | "notEmpty";
export type NumberOp = "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "between" | "empty" | "notEmpty";
export type DateOp = "on" | "before" | "after" | "between" | "relative" | "empty" | "notEmpty";
export type RelativeRange = "today" | "thisMonth" | "lastMonth" | "thisQuarter" | "thisYear";

/** The condition of one column. Numbers travel as decimal strings, dates as YYYY-MM-DD. */
export type FilterCondition =
  | { kind: "text"; op: TextOp; value?: string }
  | { kind: "number"; op: NumberOp; value?: string; to?: string }
  | { kind: "date"; op: DateOp; value?: string; to?: string; range?: RelativeRange }
  | { kind: "set"; values: string[]; includeEmpty?: boolean }
  | { kind: "boolean"; value: boolean | null };

/** A filter on one column, with what an adapter needs to send it to a server. */
export interface ColumnFilter {
  columnId: string;
  field: string;
  type: ColumnType;
  condition: FilterCondition;
}

export interface SortKey {
  columnId: string;
  field: string;
  desc: boolean;
}

/**
 * Everything a server needs to produce the rows the grid shows: plain,
 * serialisable data. `toErpcraftQuery` turns it into an erpcraft list query.
 */
export interface GridQueryState {
  sort: SortKey[];
  filters: ColumnFilter[];
  search: string;
  page: { index: number; size: number };
}

// ------------------------------------------------------------------ editing

export interface CellChange<Row> {
  rowIndex: number;
  rowId: string;
  row: Row;
  columnId: string;
  oldValue: unknown;
  newValue: unknown;
}

export type EditSource = "edit" | "paste" | "clear" | "fill" | "undo" | "redo";

export interface RejectedCell {
  rowIndex: number;
  columnId: string;
  text: string;
  reason: string;
}

/** What a paste or clear did: the consumer can show it, the grid announces it. */
export interface EditReport {
  source: EditSource;
  applied: number;
  rejected: RejectedCell[];
}

// ------------------------------------------------------------------ layout

/** How the user arranged the columns: saved per grid id. */
export interface ColumnLayout {
  order: string[];
  sizes: Record<string, number>;
  pinning: { left: string[]; right: string[] };
  hidden: string[];
}

export interface LayoutStorage {
  load(gridId: string): ColumnLayout | null | Promise<ColumnLayout | null>;
  save(gridId: string, layout: ColumnLayout): void;
}
