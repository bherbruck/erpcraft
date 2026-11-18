// The grid's query state as an erpcraft list query: the input of
// `entity.list` or `document.list` (crates/erpcraft-kernel/src/builtin.rs).
//
// Filters become one `where` condition in the rule language of
// erpcraft-rules: comparisons, `in`, `&&`, `||`, text tests that ignore
// case (`containsIgnoreCase`…), and dates relative to `today()`, over
// `record.<field>` and a document's `record.summary.<field>`. Values are
// written as quoted strings and cast by the server to the column's type, so
// decimals and days arrive exactly as written; checklists travel as bound
// `params`, so a long one does not lengthen the condition. The reader's time
// zone goes along, for `today()` and for days against timestamps. What the
// API cannot express is left out of the query and listed in `unsupported`,
// so the consumer can say so instead of showing rows that were not filtered.

import type { ColumnFilter, FilterCondition, GridQueryState } from "../types";
import type { RelativeRange } from "../types";
import { addDays } from "./dates";
import { compareDecimal, toDecimal } from "./decimal";
import { isComplete } from "./filters";

/** Most rows one call returns (`MAX_LIMIT` in erpcraft-kernel). */
export const ERPCRAFT_MAX_LIMIT = 500;
/** Longest `where` the server parses (`MAX_SOURCE_LEN` in erpcraft-rules). */
export const ERPCRAFT_MAX_WHERE = 4000;

/** Header fields of a document that `where` can name (`document::rule_columns`). */
export const DOCUMENT_WHERE_FIELDS = [
  "id",
  "company_id",
  "doc_type",
  "number",
  "status",
  "locked",
  "version",
  "doc_date",
  "memo",
  "created_by",
  "created_at",
  "updated_at",
] as const;

/** Header fields a document list sorts by (`document::SORTABLE`), besides `summary.<field>`. */
export const DOCUMENT_SORT_FIELDS = ["number", "status", "doc_date", "created_at", "updated_at"] as const;

export interface EntityTarget {
  kind: "entity";
  entity: string;
  /** Exact values every row must have, such as the company: `{ company_id: "…" }`. */
  filters?: Record<string, string>;
  /** A condition of the consumer's own, joined to the grid's with `&&`. */
  where?: string;
  /** Params the consumer's own `where` reads. The grid's are named `g0`, `g1`… */
  params?: Record<string, unknown>;
  activeOnly?: boolean;
}

export interface DocumentTarget {
  kind: "document";
  docType: string;
  companyId?: string | null;
  status?: string | null;
  summary?: Record<string, string>;
  where?: string;
  params?: Record<string, unknown>;
}

export type ErpcraftTarget = EntityTarget | DocumentTarget;

export interface EntityListInput {
  entity: string;
  filters: Record<string, string>;
  q: string | null;
  where: string | null;
  sort: string[];
  limit: number;
  offset: number;
  active_only: boolean;
  params: Record<string, unknown>;
  time_zone: string | null;
}

export interface DocumentListInput {
  doc_type: string;
  company_id: string | null;
  status: string | null;
  q: string | null;
  where: string | null;
  sort: string[];
  limit: number;
  offset: number;
  summary: Record<string, string>;
  params: Record<string, unknown>;
  time_zone: string | null;
}

/** A part of the grid's state the query leaves out, and why. */
export interface Unsupported {
  columnId: string | null;
  what: "filter" | "sort" | "page";
  reason: string;
}

export type ErpcraftQuery =
  | { command: "entity.list"; input: EntityListInput; unsupported: Unsupported[] }
  | { command: "document.list"; input: DocumentListInput; unsupported: Unsupported[] };

export interface QueryOptions {
  /**
   * The reader's time zone (IANA), sent as `time_zone`: the day `today()` is
   * on the server, and where a day starts for timestamps. Defaults to the
   * browser's; `null` sends none, and the database's zone applies.
   */
  timeZone?: string | null;
}

function browserZone(): string | null {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || null;
  } catch {
    return null;
  }
}

/** The first and last day of a relative range, as expressions the server works out each time. */
const RELATIVE: Record<RelativeRange, [string, string]> = {
  today: ["today()", "today()"],
  thisMonth: ["startOfMonth(today())", "endOfMonth(today())"],
  lastMonth: ["startOfMonth(addMonths(today(), -1))", "endOfMonth(addMonths(today(), -1))"],
  thisQuarter: ["startOfQuarter(today())", "endOfQuarter(today())"],
  thisYear: ["startOfYear(today())", "endOfYear(today())"],
};

/** A string literal of the rule language. */
export function celString(value: string): string {
  const escaped = value
    .replaceAll("\\", "\\\\")
    .replaceAll("'", "\\'")
    .replaceAll("\n", "\\n")
    .replaceAll("\r", "\\r")
    .replaceAll("\t", "\\t");
  return `'${escaped}'`;
}

/** Join conditions with && as a balanced tree, so the depth the server allows (64) is not reached by length. */
export function joinAll(parts: readonly string[], op: "&&" | "||"): string {
  if (parts.length === 0) return "";
  if (parts.length === 1) return parts[0] ?? "";
  const mid = Math.ceil(parts.length / 2);
  return `(${joinAll(parts.slice(0, mid), op)} ${op} ${joinAll(parts.slice(mid), op)})`;
}

type Compiled = { ok: true; where: string } | { ok: false; reason: string };

/** Collects the values filters send as `params`. */
export class Params {
  readonly values: Record<string, unknown> = {};
  private next = 0;

  /** A name for a value, as the condition reads it: `params.g0`. */
  add(value: unknown): string {
    const name = `g${this.next}`;
    this.next += 1;
    this.values[name] = value;
    return `params.${name}`;
  }
}

/** One filter as a condition over `record`. `field` is a field name, or `summary.<field>` on a document. */
export function filterToWhere(
  field: string,
  type: ColumnFilter["type"],
  condition: FilterCondition,
  params: Params = new Params(),
): Compiled {
  const f = `record.${field}`;
  const empty = (op: string) =>
    op === "empty" ? `(${f} == null)` : op === "notEmpty" ? `(${f} != null)` : null;
  switch (condition.kind) {
    case "text": {
      if (condition.op === "empty") return { ok: true, where: `(${f} == null || ${f} == '')` };
      if (condition.op === "notEmpty") return { ok: true, where: `(${f} != null && ${f} != '')` };
      // As the grid filters in memory: ignoring case.
      const method = {
        equals: "equalsIgnoreCase",
        contains: "containsIgnoreCase",
        startsWith: "startsWithIgnoreCase",
        endsWith: "endsWithIgnoreCase",
      }[condition.op];
      return { ok: true, where: `${f}.${method}(${celString(condition.value ?? "")})` };
    }
    case "number": {
      const e = empty(condition.op);
      if (e) return { ok: true, where: e };
      const a = toDecimal(condition.value ?? "");
      if (a === null) return { ok: false, reason: "not a number" };
      const ops = { eq: "==", ne: "!=", lt: "<", le: "<=", gt: ">", ge: ">=" } as const;
      if (condition.op === "between") {
        const b = toDecimal(condition.to ?? "");
        if (b === null) return { ok: false, reason: "not a number" };
        const [lo, hi] = compareDecimal(a, b) <= 0 ? [a, b] : [b, a];
        return { ok: true, where: `(${f} >= ${celString(lo)} && ${f} <= ${celString(hi)})` };
      }
      return { ok: true, where: `${f} ${ops[condition.op as keyof typeof ops]} ${celString(a)}` };
    }
    case "date": {
      const e = empty(condition.op);
      if (e) return { ok: true, where: e };
      // The first and last day, both included, as expressions: a quoted day or one relative to today().
      let from: string | null = null;
      let to: string | null = null;
      const day = (d: string | undefined) => (d ? celString(d) : null);
      switch (condition.op) {
        case "on":
          from = to = day(condition.value);
          break;
        case "before":
          // The day before, as the last day; for a timestamp, up to that day's midnight.
          if (!condition.value) break;
          return { ok: true, where: `${f} < ${celString(condition.value)}` };
        case "after":
          from = condition.value ? celString(addDays(condition.value, 1)) : null;
          break;
        case "between": {
          const [a, b] = [condition.value ?? "", condition.to ?? ""].sort();
          from = day(a);
          to = day(b);
          break;
        }
        case "relative":
          if (!condition.range) break;
          [from, to] = RELATIVE[condition.range];
          break;
      }
      if (from === null && to === null) return { ok: false, reason: "incomplete date" };
      const literal = /^'(\d{4}-\d{2}-\d{2})'$/;
      const nextDay = (d: string) => {
        const plain = literal.exec(d);
        return plain?.[1] ? celString(addDays(plain[1], 1)) : `addDays(${d}, 1)`;
      };
      const parts: string[] = [];
      if (type === "datetime") {
        // A timestamp is on a day from that day's midnight to the next one's, in the reader's zone.
        if (from) parts.push(`${f} >= ${from}`);
        if (to) parts.push(`${f} < ${nextDay(to)}`);
      } else {
        if (from && from === to) return { ok: true, where: `${f} == ${from}` };
        if (from) parts.push(`${f} >= ${from}`);
        if (to) parts.push(`${f} <= ${to}`);
      }
      return { ok: true, where: joinAll(parts, "&&") };
    }
    case "set": {
      if (condition.values.length === 0) return { ok: true, where: condition.includeEmpty ? `(${f} == null)` : "false" };
      const inList = `${f} in ${params.add([...condition.values])}`;
      return { ok: true, where: condition.includeEmpty ? `(${inList} || ${f} == null)` : inList };
    }
    case "boolean":
      return { ok: true, where: condition.value === null ? `(${f} == null)` : `${f} == ${condition.value}` };
  }
}

const PLAIN = /^[a-z_][a-z0-9_]*$/i;
const SUMMARY = /^summary\.[a-z_][a-z0-9_]*$/i;

/** Map the grid's state to the input of `entity.list` or `document.list`, exactly. */
export function toErpcraftQuery(
  state: GridQueryState,
  target: ErpcraftTarget,
  options: QueryOptions = {},
): ErpcraftQuery {
  const unsupported: Unsupported[] = [];
  const conditions: string[] = [];
  const params = new Params();

  for (const filter of state.filters) {
    if (!isComplete(filter.condition)) continue;
    const field = filter.field.trim();
    const allowed =
      target.kind === "entity"
        ? PLAIN.test(field)
        : SUMMARY.test(field) || (DOCUMENT_WHERE_FIELDS as readonly string[]).includes(field);
    if (!allowed) {
      unsupported.push({
        columnId: filter.columnId,
        what: "filter",
        reason:
          target.kind === "document"
            ? `a document list filters by its header and summary fields, not "${field}"`
            : `"${field}" is not a plain field name`,
      });
      continue;
    }
    const compiled = filterToWhere(field, filter.type, filter.condition, params);
    if (compiled.ok) conditions.push(compiled.where);
    else unsupported.push({ columnId: filter.columnId, what: "filter", reason: compiled.reason });
  }

  const sort: string[] = [];
  for (const key of state.sort) {
    const allowed =
      target.kind === "entity"
        ? PLAIN.test(key.field)
        : SUMMARY.test(key.field) || (DOCUMENT_SORT_FIELDS as readonly string[]).includes(key.field);
    if (!allowed) {
      unsupported.push({ columnId: key.columnId, what: "sort", reason: `a list cannot sort by "${key.field}"` });
      continue;
    }
    // Empty values last in both directions, as the grid sorts in memory.
    sort.push(`${key.desc ? "-" : ""}${key.field} nulls last`);
  }

  let limit = state.page.size;
  if (limit > ERPCRAFT_MAX_LIMIT) {
    unsupported.push({ columnId: null, what: "page", reason: `at most ${ERPCRAFT_MAX_LIMIT} rows a page` });
    limit = ERPCRAFT_MAX_LIMIT;
  }
  const offset = state.page.index * state.page.size;

  if (target.where?.trim()) conditions.unshift(`(${target.where.trim()})`);
  let where: string | null = conditions.length ? joinAll(conditions, "&&") : null;
  let sent: Record<string, unknown> = { ...target.params, ...params.values };
  if (where && where.length > ERPCRAFT_MAX_WHERE) {
    unsupported.push({
      columnId: null,
      what: "filter",
      reason: `the filters make a condition of ${where.length} characters; the server takes ${ERPCRAFT_MAX_WHERE}`,
    });
    where = target.where?.trim() || null;
    sent = { ...target.params };
  }
  const q = state.search.trim() || null;
  const time_zone = options.timeZone === undefined ? browserZone() : options.timeZone;

  if (target.kind === "entity") {
    return {
      command: "entity.list",
      input: {
        entity: target.entity,
        filters: { ...target.filters },
        q,
        where,
        sort,
        limit,
        offset,
        active_only: target.activeOnly ?? false,
        params: sent,
        time_zone,
      },
      unsupported,
    };
  }
  return {
    command: "document.list",
    input: {
      doc_type: target.docType,
      company_id: target.companyId ?? null,
      status: target.status ?? null,
      q,
      where,
      sort,
      limit,
      offset,
      summary: { ...target.summary },
      params: sent,
      time_zone,
    },
    unsupported,
  };
}
