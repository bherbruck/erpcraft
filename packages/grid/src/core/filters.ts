// Column filters: which conditions a type offers, how a row is tested
// against one in memory, and how a filter reads as a chip.

import type { ColumnType, DateOp, FilterCondition, GridColumn, NumberOp, RelativeRange, TextOp } from "../types";
import { cellValue, formatCell, isEmptyValue, isSearchable, optionLabel } from "./columns";
import { addDays, formatDay, resolveRange, toDay } from "./dates";
import { compareDecimal, formatDecimal, toDecimal } from "./decimal";

export const TEXT_OPS: TextOp[] = ["contains", "equals", "startsWith", "empty", "notEmpty"];
export const NUMBER_OPS: NumberOp[] = ["eq", "ne", "lt", "le", "gt", "ge", "between", "empty", "notEmpty"];
export const DATE_OPS: DateOp[] = ["on", "before", "after", "between", "relative", "empty", "notEmpty"];
export const RELATIVE_RANGES: RelativeRange[] = ["today", "thisMonth", "lastMonth", "thisQuarter", "thisYear"];

/** Words for operators and ranges. A consumer passes its own translations. */
export const DEFAULT_LABELS = {
  contains: "contains",
  equals: "equals",
  startsWith: "starts with",
  empty: "is empty",
  notEmpty: "is not empty",
  eq: "=",
  ne: "≠",
  lt: "<",
  le: "≤",
  gt: ">",
  ge: "≥",
  between: "between",
  on: "on",
  before: "before",
  after: "after",
  relative: "in",
  today: "today",
  thisMonth: "this month",
  lastMonth: "last month",
  thisQuarter: "this quarter",
  thisYear: "this year",
  and: "and",
  true: "Yes",
  false: "No",
  emptyValue: "(empty)",
} as const;

export type FilterLabels = Record<keyof typeof DEFAULT_LABELS, string>;

/** The kind of condition a column type is filtered by. */
export function filterKind(type: ColumnType): FilterCondition["kind"] {
  switch (type) {
    case "number":
    case "money":
      return "number";
    case "date":
    case "datetime":
      return "date";
    case "enum":
    case "state":
    case "reference":
      return "set";
    case "boolean":
      return "boolean";
    default:
      return "text";
  }
}

/** A condition with nothing chosen yet, for a filter editor to start from. */
export function emptyCondition(type: ColumnType): FilterCondition {
  switch (filterKind(type)) {
    case "number":
      return { kind: "number", op: "eq", value: "" };
    case "date":
      return { kind: "date", op: "on", value: "" };
    case "set":
      return { kind: "set", values: [] };
    case "boolean":
      return { kind: "boolean", value: true };
    default:
      return { kind: "text", op: "contains", value: "" };
  }
}

/** Is the condition complete enough to filter by? An editor half filled in filters nothing. */
export function isComplete(condition: FilterCondition): boolean {
  switch (condition.kind) {
    case "text":
      return condition.op === "empty" || condition.op === "notEmpty" || Boolean(condition.value);
    case "number":
      if (condition.op === "empty" || condition.op === "notEmpty") return true;
      if (toDecimal(condition.value ?? "") === null) return false;
      return condition.op !== "between" || toDecimal(condition.to ?? "") !== null;
    case "date":
      if (condition.op === "empty" || condition.op === "notEmpty") return true;
      if (condition.op === "relative") return condition.range !== undefined;
      if (!toDay(condition.value ?? "")) return false;
      return condition.op !== "between" || Boolean(toDay(condition.to ?? ""));
    case "set":
      // Nothing ticked filters nothing, as an untouched checklist does.
      return condition.values.length > 0 || condition.includeEmpty === true;
    case "boolean":
      return true;
  }
}

/** The first and last day a date condition lets through, both included; null bounds are open. */
export function dateBounds(
  condition: Extract<FilterCondition, { kind: "date" }>,
  today?: Date,
): { from: string | null; to: string | null } | null {
  switch (condition.op) {
    case "on":
      return { from: condition.value ?? null, to: condition.value ?? null };
    case "before":
      return { from: null, to: condition.value ? addDays(condition.value, -1) : null };
    case "after":
      return { from: condition.value ? addDays(condition.value, 1) : null, to: null };
    case "between": {
      const [a, b] = [condition.value ?? "", condition.to ?? ""];
      return a <= b ? { from: a, to: b } : { from: b, to: a };
    }
    case "relative":
      return condition.range ? resolveRange(condition.range, today) : null;
    default:
      return null;
  }
}

/** Does a cell value pass a condition? */
export function matches(condition: FilterCondition, value: unknown, today?: Date): boolean {
  switch (condition.kind) {
    case "text": {
      if (condition.op === "empty") return isEmptyValue(value);
      if (condition.op === "notEmpty") return !isEmptyValue(value);
      if (isEmptyValue(value)) return false;
      const text = String(value).toLocaleLowerCase();
      const wanted = (condition.value ?? "").toLocaleLowerCase();
      if (condition.op === "equals") return text === wanted;
      if (condition.op === "startsWith") return text.startsWith(wanted);
      return text.includes(wanted);
    }
    case "number": {
      if (condition.op === "empty") return isEmptyValue(value);
      if (condition.op === "notEmpty") return !isEmptyValue(value);
      const n = toDecimal(value);
      const a = toDecimal(condition.value ?? "");
      if (n === null || a === null) return false;
      const c = compareDecimal(n, a);
      switch (condition.op) {
        case "eq":
          return c === 0;
        case "ne":
          return c !== 0;
        case "lt":
          return c < 0;
        case "le":
          return c <= 0;
        case "gt":
          return c > 0;
        case "ge":
          return c >= 0;
        case "between": {
          const b = toDecimal(condition.to ?? "");
          if (b === null) return false;
          const [lo, hi] = compareDecimal(a, b) <= 0 ? [a, b] : [b, a];
          return compareDecimal(n, lo) >= 0 && compareDecimal(n, hi) <= 0;
        }
      }
      return false;
    }
    case "date": {
      if (condition.op === "empty") return isEmptyValue(value);
      if (condition.op === "notEmpty") return !isEmptyValue(value);
      const day = toDay(value);
      const bounds = dateBounds(condition, today);
      if (!day || !bounds) return false;
      return (bounds.from === null || day >= bounds.from) && (bounds.to === null || day <= bounds.to);
    }
    case "set":
      if (isEmptyValue(value)) return condition.includeEmpty === true;
      return condition.values.includes(String(value));
    case "boolean":
      if (condition.value === null) return isEmptyValue(value);
      return Boolean(value) === condition.value;
  }
}

/** Does a row contain the quick search text in any searchable column? */
export function matchesSearch<Row>(
  columns: readonly GridColumn<Row>[],
  row: Row,
  search: string,
  locale?: string,
): boolean {
  const wanted = search.trim().toLocaleLowerCase();
  if (wanted === "") return true;
  return columns.some(
    (column) => isSearchable(column) && formatCell(column, row, locale).toLocaleLowerCase().includes(wanted),
  );
}

/** A filter as a chip reads it: "Total ≥ 100", "Status: Draft, Posted". */
export function describeFilter<Row>(
  column: GridColumn<Row>,
  condition: FilterCondition,
  labels: FilterLabels = DEFAULT_LABELS,
  locale?: string,
  referenceLabels?: ReadonlyMap<string, string>,
): string {
  const name = column.header;
  const number = (s?: string) => {
    const d = toDecimal(s ?? "");
    return d === null ? (s ?? "") : formatDecimal(d, 0, locale);
  };
  const day = (s?: string) => (s && toDay(s) ? formatDay(s, locale) : (s ?? ""));
  switch (condition.kind) {
    case "text":
      if (condition.op === "empty" || condition.op === "notEmpty") return `${name} ${labels[condition.op]}`;
      return `${name} ${labels[condition.op]} "${condition.value ?? ""}"`;
    case "number":
      if (condition.op === "empty" || condition.op === "notEmpty") return `${name} ${labels[condition.op]}`;
      if (condition.op === "between")
        return `${name} ${labels.between} ${number(condition.value)} ${labels.and} ${number(condition.to)}`;
      return `${name} ${labels[condition.op]} ${number(condition.value)}`;
    case "date":
      if (condition.op === "empty" || condition.op === "notEmpty") return `${name} ${labels[condition.op]}`;
      if (condition.op === "relative") return `${name}: ${condition.range ? labels[condition.range] : ""}`;
      if (condition.op === "between")
        return `${name} ${labels.between} ${day(condition.value)} ${labels.and} ${day(condition.to)}`;
      return `${name} ${labels[condition.op]} ${day(condition.value)}`;
    case "set": {
      const names = condition.values.map(
        (value) => optionLabel(column, value) ?? referenceLabels?.get(value) ?? value,
      );
      if (condition.includeEmpty) names.push(labels.emptyValue);
      const shown = names.length > 3 ? `${names.slice(0, 3).join(", ")} +${names.length - 3}` : names.join(", ");
      return `${name}: ${shown || "—"}`;
    }
    case "boolean":
      return `${name}: ${condition.value === null ? labels.emptyValue : condition.value ? labels.true : labels.false}`;
  }
}

/** Test a row against every filter and the search: the client-side filter. */
export function rowPasses<Row>(
  columns: readonly GridColumn<Row>[],
  filters: readonly { columnId: string; condition: FilterCondition }[],
  row: Row,
  today?: Date,
): boolean {
  for (const filter of filters) {
    const column = columns.find((c) => c.id === filter.columnId);
    if (!column || !isComplete(filter.condition)) continue;
    if (!matches(filter.condition, cellValue(column, row), today)) return false;
  }
  return true;
}
