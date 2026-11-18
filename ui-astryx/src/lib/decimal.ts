// Decimals arrive from the server as strings and stay strings. Nothing here
// turns one into a JavaScript number: formatting is done on the digits.

const DECIMAL = /^(-?)(\d+)(?:\.(\d+))?$/;

export function isDecimal(value: string): boolean {
  return DECIMAL.test(value.trim());
}

/** What a decimal input lets the user type: digits, one sign, one separator. */
export function isDecimalDraft(value: string): boolean {
  return /^-?\d*\.?\d*$/.test(value);
}

interface Separators {
  group: (integer: string) => string;
  decimal: string;
  minus: string;
}

const separatorCache = new Map<string, Separators>();

function separators(locale: string | undefined): Separators {
  const key = locale ?? "";
  const cached = separatorCache.get(key);
  if (cached) return cached;
  const grouping = new Intl.NumberFormat(locale, { maximumFractionDigits: 0 });
  const parts = new Intl.NumberFormat(locale, { minimumFractionDigits: 1 }).formatToParts(-1.5);
  const found: Separators = {
    // BigInt keeps every digit of an integer part of any length.
    group: (integer) => grouping.format(BigInt(integer)),
    decimal: parts.find((part) => part.type === "decimal")?.value ?? ".",
    minus: parts.find((part) => part.type === "minusSign")?.value ?? "-",
  };
  separatorCache.set(key, found);
  return found;
}

export interface DecimalFormat {
  /** Decimals always shown. Trailing zeros beyond these are dropped. */
  minFraction?: number;
  locale?: string;
}

/**
 * A decimal string for display in the user's locale. Never rounds: digits the
 * server sent beyond `minFraction` are kept unless they are zeros.
 */
export function formatDecimal(value: string, options: DecimalFormat = {}): string {
  const match = DECIMAL.exec(value.trim());
  if (!match) return value;
  const [, sign = "", integer = "0", rawFraction = ""] = match;
  const min = options.minFraction ?? 0;
  let fraction = rawFraction.replace(/0+$/, "");
  if (fraction.length < min) fraction = fraction.padEnd(min, "0");
  const { group, decimal, minus } = separators(options.locale);
  const isZero = /^0+$/.test(integer) && /^0*$/.test(fraction);
  const head = `${sign && !isZero ? minus : ""}${group(integer)}`;
  return fraction ? `${head}${decimal}${fraction}` : head;
}

/** True for "0", "0.00", "-0.0". */
export function isZeroDecimal(value: string): boolean {
  const match = DECIMAL.exec(value.trim());
  if (!match) return false;
  return /^0+$/.test(match[2] ?? "") && /^0*$/.test(match[3] ?? "");
}

/** Whether two decimal strings denote the same number ("1.50" and "1.5"). */
export function sameDecimal(a: string, b: string): boolean {
  const normal = (value: string): string | null => {
    const match = DECIMAL.exec(value.trim());
    if (!match) return null;
    const integer = (match[2] ?? "0").replace(/^0+(?=\d)/, "");
    const fraction = (match[3] ?? "").replace(/0+$/, "");
    const zero = integer === "0" && fraction === "";
    return `${zero ? "" : (match[1] ?? "")}${integer}.${fraction}`;
  };
  const left = normal(a);
  return left !== null && left === normal(b);
}

/** "25.000000" -> "25", "9.750000" -> "9.75". For showing a stored value in an input. */
export function trimDecimal(value: string): string {
  const match = DECIMAL.exec(value.trim());
  if (!match) return value;
  const fraction = (match[3] ?? "").replace(/0+$/, "");
  return `${match[1] ?? ""}${match[2] ?? "0"}${fraction ? `.${fraction}` : ""}`;
}
