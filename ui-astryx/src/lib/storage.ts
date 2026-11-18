// Per-user conveniences kept in the browser: chosen columns, recent choices.
// Storage can be missing, full or blocked, so every access may fail quietly and
// the UI must work the same without it.

const PREFIX = "erpcraft";

export function readStored<T>(key: string, isValid: (value: unknown) => value is T): T | null {
  try {
    const raw = window.localStorage.getItem(`${PREFIX}.${key}`);
    if (raw === null) return null;
    const parsed: unknown = JSON.parse(raw);
    return isValid(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

export function writeStored(key: string, value: unknown): void {
  try {
    if (value === null) window.localStorage.removeItem(`${PREFIX}.${key}`);
    else window.localStorage.setItem(`${PREFIX}.${key}`, JSON.stringify(value));
  } catch {
    // Not remembered, and that is all.
  }
}

export function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === "string");
}
