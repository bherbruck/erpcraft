// Decimals stay strings: amounts are compared and added on their digits,
// never through a float, so no total is ever off by a rounding.

const DECIMAL = /^([+-]?)(\d*)(?:\.(\d*))?$/;

interface Parts {
  negative: boolean;
  integer: string;
  fraction: string;
}

function parts(value: string): Parts | null {
  const match = DECIMAL.exec(value.trim());
  if (!match) return null;
  const [, sign = "", integer = "", fraction = ""] = match;
  if (integer === "" && fraction === "") return null;
  return { negative: sign === "-", integer: integer.replace(/^0+/, ""), fraction: fraction.replace(/0+$/, "") };
}

export function isDecimal(value: string): boolean {
  return parts(value) !== null;
}

/** The decimal a value stands for, as a canonical string, or null. Numbers are taken as written. */
export function toDecimal(value: unknown): string | null {
  if (typeof value === "number") return Number.isFinite(value) ? normalise(String(value)) : null;
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "string") return normalise(value);
  if (value && typeof value === "object" && "amount" in value) {
    return toDecimal((value as { amount: unknown }).amount);
  }
  return null;
}

function normalise(value: string): string | null {
  // Exponents (1e-7 from a number) are expanded first.
  const text = /e/i.test(value) ? expand(value) : value;
  if (text === null) return null;
  const p = parts(text);
  if (!p) return null;
  const integer = p.integer || "0";
  const zero = integer === "0" && p.fraction === "";
  const body = p.fraction ? `${integer}.${p.fraction}` : integer;
  return p.negative && !zero ? `-${body}` : body;
}

function expand(value: string): string | null {
  const match = /^([+-]?)(\d*)(?:\.(\d*))?e([+-]?\d+)$/i.exec(value.trim());
  if (!match) return null;
  const [, sign = "", integer = "", fraction = "", exponentText = "0"] = match;
  const digits = `${integer}${fraction}`;
  const point = integer.length + Number(exponentText);
  if (point <= 0) return `${sign}0.${"0".repeat(-point)}${digits}`;
  if (point >= digits.length) return `${sign}${digits}${"0".repeat(point - digits.length)}`;
  return `${sign}${digits.slice(0, point)}.${digits.slice(point)}`;
}

function scaled(value: string, scale: number): bigint {
  const p = parts(value);
  if (!p) return 0n;
  const digits = `${p.integer || "0"}${p.fraction.padEnd(scale, "0")}`;
  const n = BigInt(digits);
  return p.negative ? -n : n;
}

function fractionLength(value: string): number {
  return parts(value)?.fraction.length ?? 0;
}

/** -1, 0 or 1. Values that are not decimals sort after those that are. */
export function compareDecimal(a: string, b: string): number {
  const valid = (s: string) => parts(s) !== null;
  if (!valid(a) || !valid(b)) return Number(!valid(a)) - Number(!valid(b));
  const scale = Math.max(fractionLength(a), fractionLength(b));
  const x = scaled(a, scale);
  const y = scaled(b, scale);
  return x < y ? -1 : x > y ? 1 : 0;
}

/** The exact sum of decimal strings; anything else is skipped. */
export function sumDecimals(values: Iterable<string>): string {
  const list = [...values].filter(isDecimal);
  if (list.length === 0) return "0";
  const scale = Math.max(...list.map(fractionLength));
  let total = 0n;
  for (const value of list) total += scaled(value, scale);
  const negative = total < 0n;
  let digits = (negative ? -total : total).toString().padStart(scale + 1, "0");
  if (scale > 0) digits = `${digits.slice(0, -scale)}.${digits.slice(-scale)}`;
  return normalise(`${negative ? "-" : ""}${digits}`) ?? "0";
}

/** A decimal for display in a locale, without rounding. */
export function formatDecimal(value: string, minFraction = 0, locale?: string): string {
  const p = parts(value);
  if (!p) return value;
  let fraction = p.fraction;
  if (fraction.length < minFraction) fraction = fraction.padEnd(minFraction, "0");
  const integer = p.integer || "0";
  const grouping = new Intl.NumberFormat(locale, { maximumFractionDigits: 0 });
  const point = new Intl.NumberFormat(locale, { minimumFractionDigits: 1 })
    .formatToParts(1.5)
    .find((part) => part.type === "decimal")?.value;
  const zero = /^0+$/.test(integer) && /^0*$/.test(fraction);
  const head = `${p.negative && !zero ? "-" : ""}${grouping.format(BigInt(integer))}`;
  return fraction ? `${head}${point ?? "."}${fraction}` : head;
}

/**
 * Read a number as people type or paste it: `1,234.50`, `(12.00)` for a
 * negative, a currency sign or code around it. Returns a canonical decimal
 * string or null. Grouping is taken from the locale's decimal separator.
 */
export function parseUserDecimal(text: string, locale?: string): string | null {
  const point =
    new Intl.NumberFormat(locale, { minimumFractionDigits: 1 }).formatToParts(1.5).find((p) => p.type === "decimal")
      ?.value ?? ".";
  let s = text.trim();
  if (s === "") return null;
  let negative = false;
  if (/^\(.*\)$/.test(s)) {
    negative = true;
    s = s.slice(1, -1).trim();
  }
  // Currency signs and codes, and spaces used for grouping.
  s = s.replace(/^[A-Z]{3}\s*|\s*[A-Z]{3}$/g, "").replace(/[\p{Sc}\s  ']/gu, "");
  if (s.endsWith("-")) {
    negative = !negative;
    s = s.slice(0, -1);
  }
  if (s.startsWith("-")) {
    negative = !negative;
    s = s.slice(1);
  } else if (s.startsWith("+")) {
    s = s.slice(1);
  }
  const group = point === "," ? "." : ",";
  s = s.split(group).join("");
  if (point !== ".") s = s.replace(point, ".");
  const value = normalise(s);
  if (value === null) return null;
  if (!negative || value === "0") return value;
  return value.startsWith("-") ? value.slice(1) : `-${value}`;
}
