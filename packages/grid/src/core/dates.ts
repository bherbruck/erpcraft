// Calendar days as YYYY-MM-DD strings. A day has no time zone; a
// timestamp is reduced to its day in the viewer's zone.

import type { RelativeRange } from "../types";

const pad = (n: number, width = 2) => String(n).padStart(width, "0");

export function dayOf(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

function isValidDay(y: number, m: number, d: number): boolean {
  if (m < 1 || m > 12 || d < 1) return false;
  const days = new Date(y, m, 0).getDate();
  return d <= days;
}

/** The day a cell value stands for: a YYYY-MM-DD string, an ISO timestamp, a Date. */
export function toDay(value: unknown): string | null {
  if (value instanceof Date) return Number.isNaN(value.getTime()) ? null : dayOf(value);
  if (typeof value !== "string" || value === "") return null;
  const plain = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (plain) return isValidDay(Number(plain[1]), Number(plain[2]), Number(plain[3])) ? value : null;
  const time = Date.parse(value);
  return Number.isNaN(time) ? null : dayOf(new Date(time));
}

/** Milliseconds since the epoch for sorting: days and timestamps alike. */
export function toTime(value: unknown): number | null {
  if (value instanceof Date) return Number.isNaN(value.getTime()) ? null : value.getTime();
  if (typeof value !== "string" || value === "") return null;
  const plain = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (plain) return new Date(Number(plain[1]), Number(plain[2]) - 1, Number(plain[3])).getTime();
  const time = Date.parse(value);
  return Number.isNaN(time) ? null : time;
}

/** The order the locale writes day, month and year in. */
function localeOrder(locale?: string): ("d" | "m" | "y")[] {
  const parts = new Intl.DateTimeFormat(locale, { year: "numeric", month: "2-digit", day: "2-digit" }).formatToParts(
    new Date(2001, 10, 22),
  );
  const order: ("d" | "m" | "y")[] = [];
  for (const part of parts) {
    if (part.type === "day") order.push("d");
    else if (part.type === "month") order.push("m");
    else if (part.type === "year") order.push("y");
  }
  return order.length === 3 ? order : ["m", "d", "y"];
}

/**
 * A day as people type or paste it: 2026-01-31, or the locale's order with
 * any of / . - between (1/31/2026 in en-US, 31.01.2026 in de). Two-digit
 * years are this century. An Excel serial day number (45000) is also read.
 */
export function parseUserDate(text: string, locale?: string): string | null {
  const s = text.trim();
  if (s === "") return null;
  const iso = /^(\d{4})-(\d{1,2})-(\d{1,2})(?:[T ].*)?$/.exec(s);
  if (iso) {
    const [y, m, d] = [Number(iso[1]), Number(iso[2]), Number(iso[3])];
    return isValidDay(y, m, d) ? `${y}-${pad(m)}-${pad(d)}` : null;
  }
  const local = /^(\d{1,4})[/.\-\s](\d{1,2})[/.\-\s](\d{1,4})\.?$/.exec(s);
  if (local) {
    const numbers = [Number(local[1]), Number(local[2]), Number(local[3])];
    const order = local[1] && local[1].length === 4 ? (["y", "m", "d"] as const) : localeOrder(locale);
    const at = (k: "d" | "m" | "y") => numbers[order.indexOf(k)] ?? 0;
    let y = at("y");
    if (y < 100) y += 2000;
    const [m, d] = [at("m"), at("d")];
    return isValidDay(y, m, d) ? `${y}-${pad(m)}-${pad(d)}` : null;
  }
  if (/^\d{5}$/.test(s)) {
    // Excel's serial days count from 1899-12-30.
    const date = new Date(1899, 11, 30 + Number(s));
    return dayOf(date);
  }
  return null;
}

export function addDays(day: string, n: number): string {
  const [y, m, d] = day.split("-").map(Number);
  return dayOf(new Date(y ?? 1970, (m ?? 1) - 1, (d ?? 1) + n));
}

/** The first and last day (both included) of a relative range, as of `today`. */
export function resolveRange(range: RelativeRange, today: Date = new Date()): { from: string; to: string } {
  const y = today.getFullYear();
  const m = today.getMonth();
  const first = (year: number, month: number) => dayOf(new Date(year, month, 1));
  const last = (year: number, month: number) => dayOf(new Date(year, month + 1, 0));
  switch (range) {
    case "today":
      return { from: dayOf(today), to: dayOf(today) };
    case "thisMonth":
      return { from: first(y, m), to: last(y, m) };
    case "lastMonth":
      return { from: first(y, m - 1), to: last(y, m - 1) };
    case "thisQuarter": {
      const q = Math.floor(m / 3) * 3;
      return { from: first(y, q), to: last(y, q + 2) };
    }
    case "thisYear":
      return { from: first(y, 0), to: last(y, 11) };
  }
}

export function formatDay(day: string, locale?: string): string {
  const [y, m, d] = day.split("-").map(Number);
  return new Intl.DateTimeFormat(locale, { year: "numeric", month: "2-digit", day: "2-digit" }).format(
    new Date(y ?? 1970, (m ?? 1) - 1, d ?? 1),
  );
}
