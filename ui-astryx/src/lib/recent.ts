// The records a user picked most recently in lookups, per entity, kept in the
// browser. A lookup offers them first, before anything is typed.

import { isStringArray, readStored, writeStored } from "./storage";

const KEEP = 8;

const storageKey = (userId: string, entity: string, keyField: string): string =>
  `recent.${userId}.${entity}.${keyField}`;

/** Key values of the records picked last, newest first. */
export function recentChoices(userId: string, entity: string, keyField: string): string[] {
  return readStored(storageKey(userId, entity, keyField), isStringArray) ?? [];
}

export function rememberChoice(userId: string, entity: string, keyField: string, value: string): void {
  const next = [value, ...recentChoices(userId, entity, keyField).filter((other) => other !== value)].slice(0, KEEP);
  writeStored(storageKey(userId, entity, keyField), next);
}
