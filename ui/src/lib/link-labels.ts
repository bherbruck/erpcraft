// Names for the records that link fields point to.
//
// A list shows many links to the same few records, so lookups are collected
// for a moment and fetched in one request per entity, then remembered. How a
// record is named is what its entity declares as `display`.

import { useEffect, useSyncExternalStore } from "react";
import { ApiError, api } from "@/api/client";
import type { EntityMeta, EntityRecord, Json, Meta } from "@/api/types";

export interface LinkInfo {
  id: string;
  label: string;
  /** The record itself, for what else a screen needs from it (an item's unit). */
  record: EntityRecord;
}

let registry: Meta | null = null;

/** The registry the names are built from. Set once the user is signed in. */
export function setLabelRegistry(meta: Meta): void {
  registry = meta;
}

export function shortId(id: string): string {
  return id.length > 13 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;
}

function shown(value: Json | undefined): string | null {
  if (value === null || value === undefined || value === "") return null;
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return null;
}

/** How a record is named where it is referred to: its entity's display fields, in order. */
export function displayLabel(entity: EntityMeta | undefined, record: { [key: string]: Json | undefined }): string {
  const parts = (entity?.display ?? [])
    .map((name) => {
      const field = entity?.fields.find((candidate) => candidate.name === name);
      const value = shown(record[name]);
      // A display field that is itself a reference names nothing a person can read.
      if (value === null || field?.type.kind === "link" || field?.type.kind === "uuid") return null;
      if (field?.type.kind === "enum") return field.type.labels[value] ?? value;
      return value;
    })
    .filter((part): part is string => part !== null);
  if (parts.length > 0) return parts.join(" · ");
  return typeof record.id === "string" ? shortId(record.id) : "";
}

/** The label of a record of a named entity. */
export function recordLabel(entityName: string, record: { [key: string]: Json | undefined }): string {
  return displayLabel(
    registry?.entities.find((entity) => entity.name === entityName),
    record,
  );
}

const cache = new Map<string, LinkInfo | null>();
const queued = new Map<string, Set<string>>();
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setTimeout> | null = null;

const cacheKey = (entity: string, key: string, value: string): string => `${entity}|${key}|${value}`;

function notify(): void {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function quote(value: string): string {
  return `'${value.replaceAll("\\", "\\\\").replaceAll("'", "\\'")}'`;
}

async function flush(): Promise<void> {
  timer = null;
  const batches = [...queued.entries()];
  queued.clear();
  await Promise.all(
    batches.map(async ([group, values]) => {
      const [entity = "", key = ""] = group.split("|");
      const wanted = [...values];
      let items: EntityRecord[] = [];
      try {
        const page = await api.run("entity.list", {
          entity,
          where: `record.${key} in [${wanted.map(quote).join(", ")}]`,
          limit: Math.min(wanted.length, 500),
        });
        items = page.items;
      } catch (cause) {
        // Refused: the value is shown as it is, and is not asked for again. Any other
        // failure (the network, an expired session) is not an answer: ask again later.
        const refused = cause instanceof ApiError && (cause.status === 403 || cause.status === 404);
        if (!refused) {
          for (const value of wanted) cache.delete(cacheKey(entity, key, value));
          return;
        }
      }
      for (const value of wanted) cache.set(cacheKey(entity, key, value), null);
      for (const item of items) {
        const value = item[key];
        if (typeof value === "string")
          cache.set(cacheKey(entity, key, value), { id: item.id, label: recordLabel(entity, item), record: item });
      }
    }),
  );
  notify();
}

function request(entity: string, key: string, value: string): void {
  if (cache.has(cacheKey(entity, key, value))) return;
  const group = `${entity}|${key}`;
  const values = queued.get(group) ?? new Set<string>();
  values.add(value);
  queued.set(group, values);
  if (timer === null) timer = setTimeout(() => void flush(), 15);
}

/** Remember a record the UI already holds, such as one just picked in a lookup. */
export function rememberRecord(entity: string, key: string, record: EntityRecord): void {
  const value = record[key];
  if (typeof value !== "string") return;
  cache.set(cacheKey(entity, key, value), { id: record.id, label: recordLabel(entity, record), record });
  notify();
}

/**
 * Fill in records read elsewhere, before anything shows them. Nothing is told:
 * this runs while the app is first rendered.
 */
export function seedRecords(entity: string, key: string, records: EntityRecord[]): void {
  for (const record of records) {
    const value = record[key];
    if (typeof value === "string")
      cache.set(cacheKey(entity, key, value), { id: record.id, label: recordLabel(entity, record), record });
  }
}

/** Forget everything, after a record was saved or the user changed. */
export function forgetLinkLabels(): void {
  cache.clear();
  notify();
}

/**
 * The record a link value points to.
 * `undefined` while it is being looked up, `null` if it cannot be read.
 */
export function useLinkInfo(
  entity: string | null,
  key: string,
  value: string | null | undefined,
): LinkInfo | null | undefined {
  const id = value && entity ? cacheKey(entity, key, value) : null;
  const info = useSyncExternalStore(subscribe, () => (id ? cache.get(id) : null));
  // biome-ignore lint/correctness/useExhaustiveDependencies: `info` re-queues the value after the cache was cleared
  useEffect(() => {
    if (value && entity) request(entity, key, value);
  }, [entity, key, value, info]);
  return value && entity ? info : null;
}
