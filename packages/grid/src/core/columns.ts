// What each column type means for reading, writing, sorting and alignment.

import type { ColumnType, GridColumn, ParseResult } from "../types";
import { formatDay, parseUserDate, toDay, toTime } from "./dates";
import { compareDecimal, formatDecimal, parseUserDecimal, toDecimal } from "./decimal";

export function cellValue<Row>(column: GridColumn<Row>, row: Row): unknown {
  if (column.value) return column.value(row);
  return (row as Record<string, unknown>)[column.id];
}

export function isEmptyValue(value: unknown): boolean {
  return value === null || value === undefined || value === "" || (Array.isArray(value) && value.length === 0);
}

export function isNumeric(type: ColumnType): boolean {
  return type === "number" || type === "money";
}

export function isSetType(type: ColumnType): boolean {
  return type === "enum" || type === "state" || type === "reference";
}

export function alignOf(type: ColumnType): "start" | "end" | "center" {
  if (isNumeric(type)) return "end";
  if (type === "boolean") return "center";
  return "start";
}

export function isEditable<Row>(column: GridColumn<Row>, row: Row): boolean {
  const { editable } = column;
  return typeof editable === "function" ? editable(row) : editable === true;
}

export function isSearchable<Row>(column: GridColumn<Row>): boolean {
  return column.searchable ?? (column.type === "text" || isSetType(column.type));
}

export function optionLabel<Row>(column: GridColumn<Row>, value: unknown): string | null {
  if (value === null || value === undefined) return null;
  const text = String(value);
  return column.options?.find((option) => option.value === text)?.label ?? null;
}

/** The text a cell shows, which is also what is copied. */
export function formatCell<Row>(column: GridColumn<Row>, row: Row, locale?: string): string {
  const value = cellValue(column, row);
  if (column.format) return column.format(value, row);
  if (isEmptyValue(value)) return "";
  switch (column.type) {
    case "number":
    case "money": {
      const decimal = toDecimal(value);
      return decimal === null ? String(value) : formatDecimal(decimal, column.minFraction ?? 0, locale);
    }
    case "date": {
      const day = toDay(value);
      return day ? formatDay(day, locale) : String(value);
    }
    case "datetime": {
      const time = toTime(value);
      return time === null
        ? String(value)
        : new Intl.DateTimeFormat(locale, { dateStyle: "short", timeStyle: "short" }).format(new Date(time));
    }
    case "boolean":
      return value ? "TRUE" : "FALSE";
    case "enum":
    case "state":
    case "reference":
      return optionLabel(column, value) ?? String(value);
    default:
      return typeof value === "object" ? JSON.stringify(value) : String(value);
  }
}

/** Typed or pasted text to a value, by the column's own parser or its type. */
export function parseCell<Row>(column: GridColumn<Row>, text: string, row: Row, locale?: string): ParseResult {
  let result: ParseResult;
  if (column.parse) {
    result = column.parse(text, row);
  } else {
    result = parseByType(column, text, locale);
  }
  if (result.ok && column.validate) {
    const error = column.validate(result.value, row);
    if (error) return { ok: false, error };
  }
  return result;
}

function parseByType<Row>(column: GridColumn<Row>, text: string, locale?: string): ParseResult {
  const trimmed = text.trim();
  if (trimmed === "") return { ok: true, value: null };
  switch (column.type) {
    case "number":
    case "money": {
      const decimal = parseUserDecimal(trimmed, locale);
      return decimal === null ? { ok: false, error: `"${text}" is not a number` } : { ok: true, value: decimal };
    }
    case "date": {
      const day = parseUserDate(trimmed, locale);
      return day === null ? { ok: false, error: `"${text}" is not a date` } : { ok: true, value: day };
    }
    case "datetime": {
      const time = Date.parse(trimmed);
      return Number.isNaN(time)
        ? { ok: false, error: `"${text}" is not a date and time` }
        : { ok: true, value: new Date(time).toISOString() };
    }
    case "boolean": {
      const lower = trimmed.toLowerCase();
      if (["true", "yes", "y", "1", "x", "✓"].includes(lower)) return { ok: true, value: true };
      if (["false", "no", "n", "0"].includes(lower)) return { ok: true, value: false };
      return { ok: false, error: `"${text}" is not true or false` };
    }
    case "enum":
    case "state":
    case "reference": {
      // A value or its label, ignoring case.
      const lower = trimmed.toLowerCase();
      const match = column.options?.find(
        (option) => option.value.toLowerCase() === lower || option.label.toLowerCase() === lower,
      );
      if (match) return { ok: true, value: match.value };
      if (column.type === "reference" && !column.options) return { ok: true, value: trimmed };
      return { ok: false, error: `"${text}" is not one of the choices` };
    }
    default:
      return { ok: true, value: text };
  }
}

const collators = new Map<string, Intl.Collator>();
function collator(locale?: string): Intl.Collator {
  const key = locale ?? "";
  let found = collators.get(key);
  if (!found) {
    found = new Intl.Collator(locale, { numeric: true, sensitivity: "base" });
    collators.set(key, found);
  }
  return found;
}

/**
 * Compare two values of a column for sorting, ascending. Empty values are
 * not compared here: the caller keeps them last in both directions.
 */
export function compareValues<Row>(column: GridColumn<Row>, a: unknown, b: unknown, locale?: string): number {
  switch (column.type) {
    case "number":
    case "money":
      return compareDecimal(toDecimal(a) ?? "", toDecimal(b) ?? "");
    case "date":
    case "datetime":
      return (toTime(a) ?? 0) - (toTime(b) ?? 0);
    case "boolean":
      return Number(Boolean(a)) - Number(Boolean(b));
    case "enum":
    case "state": {
      // In the order the choices are declared: a workflow's states in their order.
      const options = column.options;
      if (options) {
        const ia = options.findIndex((o) => o.value === String(a));
        const ib = options.findIndex((o) => o.value === String(b));
        if (ia !== -1 && ib !== -1) return ia - ib;
      }
      return collator(locale).compare(optionLabel(column, a) ?? String(a), optionLabel(column, b) ?? String(b));
    }
    case "reference": {
      const la = optionLabel(column, a) ?? String(a);
      const lb = optionLabel(column, b) ?? String(b);
      return collator(locale).compare(la, lb);
    }
    default:
      return collator(locale).compare(String(a), String(b));
  }
}

/**
 * A value made quick to compare, computed once per row for sorting. Numbers
 * carry a float for the fast comparison and their exact digits for when the
 * floats are equal: rounding to a float never reverses an order, it can only
 * make two different values look the same, and those are compared exactly.
 */
export type CellSortKey = number | string | { n: number; d: string };

export function sortKey<Row>(column: GridColumn<Row>, value: unknown, locale?: string): CellSortKey | undefined {
  if (isEmptyValue(value)) return undefined;
  switch (column.type) {
    case "number":
    case "money": {
      const d = toDecimal(value);
      return d === null ? undefined : { n: Number(d), d };
    }
    case "date":
    case "datetime":
      return toTime(value) ?? undefined;
    case "boolean":
      return value ? 1 : 0;
    case "enum":
    case "state": {
      const at = column.options?.findIndex((o) => o.value === String(value)) ?? -1;
      return at !== -1 ? at : (optionLabel(column, value) ?? String(value));
    }
    case "reference":
      return optionLabel(column, value) ?? String(value);
    default:
      void locale;
      return typeof value === "string" ? value : String(value);
  }
}

/** Compare two sort keys of one column, ascending. */
export function compareKeys(a: CellSortKey, b: CellSortKey, locale?: string): number {
  if (typeof a === "number" && typeof b === "number") return a - b;
  if (typeof a === "object" && typeof b === "object") {
    return a.n < b.n ? -1 : a.n > b.n ? 1 : compareDecimal(a.d, b.d);
  }
  if (typeof a === "string" && typeof b === "string") return collator(locale).compare(a, b);
  // Mixed kinds (an option index and an unknown value): numbers first.
  return typeof a === "number" ? -1 : typeof b === "number" ? 1 : collator(locale).compare(String(a), String(b));
}

export function serverField<Row>(column: GridColumn<Row>): string {
  return column.field ?? column.id;
}
