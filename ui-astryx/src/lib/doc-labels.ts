// Documents that other documents refer to: their headers for a label, and,
// where lines of them are chosen, the whole document. Read once, remembered
// for the session of the page, refreshed when the user saves something.

import { useEffect, useSyncExternalStore } from "react";
import { api } from "@/api/client";
import type { DocTypeMeta, DocumentRecord, DocumentSummary, Json } from "@/api/types";
import { t } from "@/i18n/t";
import { quote } from "./link-labels";

const headers = new Map<string, DocumentSummary | null>();
const bodies = new Map<string, DocumentRecord | null>();
const queued = new Map<string, Set<string>>();
const loading = new Set<string>();
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setTimeout> | null = null;

function notify(): void {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

async function flush(): Promise<void> {
  timer = null;
  const batches = [...queued.entries()];
  queued.clear();
  await Promise.all(
    batches.map(async ([docType, ids]) => {
      const wanted = [...ids];
      let items: DocumentSummary[] = [];
      try {
        const page = await api.run("document.list", {
          doc_type: docType,
          where: `record.id in [${wanted.map(quote).join(", ")}]`,
          limit: Math.min(wanted.length, 500),
        });
        items = page.items;
      } catch {
        // Not readable: shown by its id.
      }
      for (const id of wanted) headers.set(id, null);
      for (const item of items) headers.set(item.id, item);
    }),
  );
  notify();
}

/** Forget what was read, after a save or a posting changed documents. */
export function forgetDocuments(): void {
  headers.clear();
  bodies.clear();
  notify();
}

/** Keep a document the page already holds, such as one just read or created. */
export function rememberDocument(doc: DocumentRecord): void {
  bodies.set(doc.id, doc);
  headers.set(doc.id, doc);
  notify();
}

/** The header of a document. `undefined` while it is read, `null` if it cannot be. */
export function useDocumentHeader(docType: string, id: string | null): DocumentSummary | null | undefined {
  const info = useSyncExternalStore(subscribe, () => (id ? headers.get(id) : null));
  // biome-ignore lint/correctness/useExhaustiveDependencies: `info` re-queues the id after the cache was cleared
  useEffect(() => {
    if (!id || headers.has(id)) return;
    const ids = queued.get(docType) ?? new Set<string>();
    ids.add(id);
    queued.set(docType, ids);
    if (timer === null) timer = setTimeout(() => void flush(), 15);
  }, [docType, id, info]);
  return id ? info : null;
}

/** A whole document, lines included. */
export async function readDocument(docType: string, id: string, signal?: AbortSignal): Promise<DocumentRecord> {
  const known = bodies.get(id);
  if (known) return known;
  const doc = await api.run("document.get", { doc_type: docType, id }, { signal });
  rememberDocument(doc);
  return doc;
}

/** A whole document, read once. `undefined` while it is read, `null` if it cannot be. */
export function useDocumentBody(docType: string, id: string | null): DocumentRecord | null | undefined {
  const info = useSyncExternalStore(subscribe, () => (id ? bodies.get(id) : null));
  // biome-ignore lint/correctness/useExhaustiveDependencies: `info` re-reads the document after the cache was cleared
  useEffect(() => {
    if (!id || bodies.has(id) || loading.has(id)) return;
    loading.add(id);
    readDocument(docType, id).then(
      () => loading.delete(id),
      () => {
        loading.delete(id);
        bodies.set(id, null);
        notify();
      },
    );
  }, [docType, id, info]);
  return id ? info : null;
}

function textOf(value: Json | undefined): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

/**
 * How a document is named where it is referred to: its number, and who it is
 * with where its summary says so ("PO-2026-00001 · Parts Inc").
 */
export function documentLabel(docType: DocTypeMeta | undefined, doc: DocumentSummary): string {
  const summary =
    typeof doc.summary === "object" && doc.summary !== null && !Array.isArray(doc.summary) ? doc.summary : {};
  const party = (docType?.summary ?? [])
    .filter((field) => field.type.kind === "text")
    .map((field) => textOf(summary[field.name]))
    .find((value) => value !== null);
  return [doc.number ?? t("document.unnumbered"), party].filter(Boolean).join(" · ");
}
