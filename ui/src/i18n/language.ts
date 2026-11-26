// The language of the UI's own words, and the locale numbers and dates are written in. It is
// chosen in the browser (per user once signed in, and the last choice for the sign-in page) and
// read once as the page loads: switching reloads the page, so every label made at load time
// (the grid's words, date formats) is made again in the new language.

import { de } from "./de";
import { en, type MessageKey } from "./en";

export type Language = "en" | "de";

export const LANGUAGES: { code: Language; label: string; locale: string | undefined }[] = [
  // English follows the browser's own number and date formats.
  { code: "en", label: "English", locale: undefined },
  { code: "de", label: "Deutsch", locale: "de-DE" },
];

const KEY = "erpcraft.language";

function stored(key: string): Language | null {
  try {
    const value = localStorage.getItem(key);
    return value === "de" || value === "en" ? value : null;
  } catch {
    return null;
  }
}

const current: Language = stored(KEY) ?? "en";

export function language(): Language {
  return current;
}

/** The locale for Intl formats: `de-DE` for German, the browser's own for English. */
export function locale(): string | undefined {
  return LANGUAGES.find((entry) => entry.code === current)?.locale;
}

export function dictionary(): Record<MessageKey, string> {
  return current === "de" ? de : en;
}

/** Choose a language: kept for this browser, and for the user when one is signed in. */
export function setLanguage(next: Language, userId?: string | null): void {
  try {
    localStorage.setItem(KEY, next);
    if (userId) localStorage.setItem(`${KEY}.${userId}`, next);
  } catch {
    // Storage may be refused (a private window); the choice then lasts for this page only.
  }
  if (next !== current) window.location.reload();
}

/** After signing in: the language this user chose before, if it is not the one shown. */
export function adoptUserLanguage(userId: string): void {
  const own = stored(`${KEY}.${userId}`);
  if (own && own !== current) setLanguage(own, userId);
}
