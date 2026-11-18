// The grid's query state as an erpcraft list query: the input of
// `entity.list` or `document.list` (crates/erpcraft-kernel/src/builtin.rs).
//
// Filters become one `where` condition in the rule language of
// erpcraft-rules: comparisons, `in`, `&&`, `||`, `!` over `record.<field>`.
// Values are written as quoted strings and cast by the server to the
// column's type, so decimals and days arrive exactly as written. What the
// API cannot express is left out of the query and listed in `unsupported`,
// so the consumer can say so instead of showing rows that were not filtered.

import type { ColumnFilter, FilterCondition, GridQueryState } from "../types";
import { addDays } from "./dates";
import { toDecimal } from "./decimal";
import { dateBounds, isComplete } from "./filters";

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
  activeOnly?: boolean;
}

export interface DocumentTarget {
  kind: "document";
  docType: string;
  companyId?: string | null;
  status?: string | null;
  summary?: Record<string, string>;
  where?: string;
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
  /** "Today" for relative date ranges. They are resolved to days when the query is built. */
  today?: Date;
}

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

/** One filter as a condition over `record`. `field` is a plain field name. */
export function filterToWhere(
  field: string,
  type: ColumnFilter["type"],
  condition: FilterCondition,
  options: QueryOptions = {},
): Compiled {
  const f = `record.${field}`;
  const empty = (op: string) =>
    op === "empty" ? `(${f} == null)` : op === "notEmpty" ? `(${f} != null)` : null;
  switch (condition.kind) {
    case "text": {
      if (condition.op === "empty") return { ok: true, where: `(${f} == null || ${f} == '')` };
      if (condition.op === "notEmpty") return { ok: true, where: `(${f} != null && ${f} != '')` };
      if (condition.op === "equals") return { ok: true, where: `${f} == ${celString(condition.value ?? "")}` };
      return {
        ok: false,
        reason: `"${condition.op}" on one field: the list API matches text only by "q", across the entity's search fields`,
      };
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
        return { ok: true, where: `(${f} >= ${celString(a)} && ${f} <= ${celString(b)})` };
      }
      return { ok: true, where: `${f} ${ops[condition.op as keyof typeof ops]} ${celString(a)}` };
    }
    case "date": {
      const e = empty(condition.op);
      if (e) return { ok: true, where: e };
      const bounds = dateBounds(condition, options.today);
      if (!bounds) return { ok: false, reason: "incomplete date" };
      const parts: string[] = [];
      if (type === "datetime") {
        // A timestamp is on a day from that day's midnight to the next one's.
        if (bounds.from) parts.push(`${f} >= ${celString(bounds.from)}`);
        if (bounds.to) parts.push(`${f} < ${celString(addDays(bounds.to, 1))}`);
      } else {
        if (bounds.from && bounds.from === bounds.to) return { ok: true, where: `${f} == ${celString(bounds.from)}` };
        if (bounds.from) parts.push(`${f} >= ${celString(bounds.from)}`);
        if (bounds.to) parts.push(`${f} <= ${celString(bounds.to)}`);
      }
      return { ok: true, where: joinAll(parts, "&&") };
    }
    case "set": {
      const list = `[${condition.values.map(celString).join(", ")}]`;
      if (condition.values.length === 0) return { ok: true, where: condition.includeEmpty ? `(${f} == null)` : "false" };
      const inList = `${f} in ${list}`;
      return { ok: true, where: condition.includeEmpty ? `(${inList} || ${f} == null)` : inList };
    }
    case "boolean":
      return { ok: true, where: condition.value === null ? `(${f} == null)` : `${f} == ${condition.value}` };
  }
}

/** Is this filter one that only `document.list`'s `summary` (exact values) can take? */
function summaryEquality(filter: ColumnFilter): string | null {
  const c = filter.condition;
  if (c.kind === "set" && c.values.length === 1 && !c.includeEmpty) return c.values[0] ?? null;
  if (c.kind === "text" && c.op === "equals" && c.value) return c.value;
  if (c.kind === "number" && c.op === "eq") return toDecimal(c.value ?? "");
  if (c.kind === "date" && c.op === "on" && c.value) return c.value;
  if (c.kind === "boolean" && c.value !== null) return String(c.value);
  return null;
}

/** Map the grid's state to the input of `entity.list` or `document.list`, exactly. */
export function toErpcraftQuery(
  state: GridQueryState,
  target: ErpcraftTarget,
  options: QueryOptions = {},
): ErpcraftQuery {
  const unsupported: Unsupported[] = [];
  const conditions: string[] = [];
  const summary: Record<string, string> = target.kind === "document" ? { ...target.summary } : {};

  for (const filter of state.filters) {
    if (!isComplete(filter.condition)) continue;
    let field = filter.field;
    if (target.kind === "document") {
      if (field.startsWith("summary.")) {
        const value = summaryEquality(filter);
        if (value === null) {
          unsupported.push({
            columnId: filter.columnId,
            what: "filter",
            reason: "a document summary field can only be filtered to one exact value",
          });
        } else {
          summary[field.slice("summary.".length)] = value;
        }
        continue;
      }
      if (!(DOCUMENT_WHERE_FIELDS as readonly string[]).includes(field)) {
        unsupported.push({
          columnId: filter.columnId,
          what: "filter",
          reason: `a document list can filter only by its header fields, not "${field}"`,
        });
        continue;
      }
    }
    if (!/^[a-z_][a-z0-9_]*$/i.test(field)) {
      unsupported.push({ columnId: filter.columnId, what: "filter", reason: `"${field}" is not a plain field name` });
      continue;
    }
    field = field.trim();
    const compiled = filterToWhere(field, filter.type, filter.condition, options);
    if (compiled.ok) conditions.push(compiled.where);
    else unsupported.push({ columnId: filter.columnId, what: "filter", reason: compiled.reason });
  }

  const sort: string[] = [];
  for (const key of state.sort) {
    const allowed =
      target.kind === "entity" ||
      key.field.startsWith("summary.") ||
      (DOCUMENT_SORT_FIELDS as readonly string[]).includes(key.field);
    if (!allowed) {
      unsupported.push({ columnId: key.columnId, what: "sort", reason: `a document list cannot sort by "${key.field}"` });
      continue;
    }
    sort.push(`${key.desc ? "-" : ""}${key.field}`);
  }

  let limit = state.page.size;
  if (limit > ERPCRAFT_MAX_LIMIT) {
    unsupported.push({ columnId: null, what: "page", reason: `at most ${ERPCRAFT_MAX_LIMIT} rows a page` });
    limit = ERPCRAFT_MAX_LIMIT;
  }
  const offset = state.page.index * state.page.size;

  if (target.where?.trim()) conditions.unshift(`(${target.where.trim()})`);
  let where: string | null = conditions.length ? joinAll(conditions, "&&") : null;
  if (where && where.length > ERPCRAFT_MAX_WHERE) {
    unsupported.push({
      columnId: null,
      what: "filter",
      reason: `the filters make a condition of ${where.length} characters; the server takes ${ERPCRAFT_MAX_WHERE}`,
    });
    where = target.where?.trim() || null;
  }
  const q = state.search.trim() || null;

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
      summary,
    },
    unsupported,
  };
}
