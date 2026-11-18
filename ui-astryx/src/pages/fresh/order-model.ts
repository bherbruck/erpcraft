// The sales order as the page edits it: a header, lines, and what the server said was wrong.
// Plain data and pure functions; the page owns the state.

import type { ApiError } from "@/api/client";
import type { DocumentRecord, Json, JsonObject } from "@/api/types";
import { todayCalendarDate } from "@/lib/dates";
import { trimDecimal } from "@/lib/decimal";
import { newKey } from "@/lib/text";

/** The header fields this page edits, as the sales order type names them. */
export const HEADER_FIELDS = ["customer_id", "currency", "payment_terms_id", "billing_address_id", "shipping_address_id"] as const;
export type HeaderField = (typeof HEADER_FIELDS)[number];

/** The line fields this page edits. */
export const LINE_FIELDS = [
  "item_id",
  "warehouse_id",
  "charge_id",
  "account_id",
  "description",
  "qty",
  "unit_price",
  "discount_percent",
  "discount_amount",
] as const;
export type LineField = (typeof LINE_FIELDS)[number];

/** What a line is: a stock item, a charge, or a revenue account. */
export type LineKind = "item" | "charge" | "account";
export const KIND_FIELD: Record<LineKind, LineField> = { item: "item_id", charge: "charge_id", account: "account_id" };

export interface OrderLine {
  /** Stable for React: the line's id once saved, a fresh key before. */
  key: string;
  /** The line as the server last sent it, so fields this page does not edit go back unchanged. */
  stored: JsonObject;
  values: Record<LineField, string>;
}

export interface OrderForm {
  docDate: string;
  memo: string;
  /** The header as the server last sent it. */
  stored: JsonObject;
  header: Record<HeaderField, string>;
  lines: OrderLine[];
}

/** Errors the server placed: at a header field, or at a field (or the whole) of one line. */
export interface Problems {
  header: Partial<Record<string, string>>;
  /** By line number, counted from 1. */
  lines: Record<number, { line?: string; fields: Partial<Record<string, string>> }>;
}

export const NO_PROBLEMS: Problems = { header: {}, lines: {} };

const isObject = (value: Json | undefined): value is JsonObject =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** A stored value as an input holds it: text, with decimals trimmed of trailing zeros. */
function editable(value: Json | undefined): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return /^-?\d+\.\d+$/.test(value) ? trimDecimal(value) : value;
  return String(value);
}

function emptyLineValues(): Record<LineField, string> {
  return Object.fromEntries(LINE_FIELDS.map((name) => [name, ""])) as Record<LineField, string>;
}

export function blankLine(): OrderLine {
  return { key: newKey(), stored: {}, values: emptyLineValues() };
}

export function blankOrder(): OrderForm {
  return {
    docDate: todayCalendarDate(),
    memo: "",
    stored: {},
    header: Object.fromEntries(HEADER_FIELDS.map((name) => [name, ""])) as Record<HeaderField, string>,
    lines: [blankLine()],
  };
}

export function orderFromDocument(doc: DocumentRecord): OrderForm {
  const { lines, ...stored } = doc.body;
  return {
    docDate: doc.doc_date,
    memo: doc.memo ?? "",
    stored,
    header: Object.fromEntries(HEADER_FIELDS.map((name) => [name, editable(doc.body[name])])) as Record<HeaderField, string>,
    lines: (Array.isArray(lines) ? lines : []).filter(isObject).map((line) => ({
      key: typeof line.id === "string" ? line.id : newKey(),
      stored: line,
      values: Object.fromEntries(LINE_FIELDS.map((name) => [name, editable(line[name])])) as Record<LineField, string>,
    })),
  };
}

export function lineKind(line: OrderLine): LineKind | null {
  if (line.values.item_id) return "item";
  if (line.values.charge_id) return "charge";
  if (line.values.account_id) return "account";
  return null;
}

/** A new line nothing was typed into: it is not sent. */
export const isUntouched = (line: OrderLine): boolean =>
  Object.keys(line.stored).length === 0 && LINE_FIELDS.every((name) => line.values[name] === "");

/** The body as `document.create` and `document.update` take it. A new line left blank is not sent. */
export function bodyOf(form: OrderForm): JsonObject {
  const body: JsonObject = { ...form.stored };
  for (const name of HEADER_FIELDS) body[name] = form.header[name] || null;
  body.lines = form.lines
    .filter((line) => !isUntouched(line))
    .map((line) => {
      const out: JsonObject = { ...line.stored };
      for (const name of LINE_FIELDS) out[name] = line.values[name] || null;
      return out;
    });
  return body;
}

/** What would be saved, as one string: equal strings mean nothing changed. */
export function snapshot(form: OrderForm): string {
  return JSON.stringify({ doc_date: form.docDate, memo: form.memo, body: bodyOf(form) });
}

/** Where the server said the error is, so it can be shown next to its field. Null when it names no place. */
export function placeProblem(error: ApiError): Problems | null {
  if (error.collection === "lines" && error.line !== null) {
    const entry = error.field ? { fields: { [error.field]: error.message } } : { line: error.message, fields: {} };
    return { header: {}, lines: { [error.line]: entry } };
  }
  if (error.field) return { header: { [error.field]: error.message }, lines: {} };
  return null;
}
