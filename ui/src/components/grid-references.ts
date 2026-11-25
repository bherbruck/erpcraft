// Link-like fields of document lines in the grid: a line of another document (line_link), a row
// of a query command (lookup), a document (document_link) and a record of an entity another field
// names (entity_link). Each gets a picker for the cell editor, which depends on the row (and the
// header), and labels the grid can format synchronously, from a cache filled as options load.
// Ported unchanged from ui-astryx/src/components/grid-references.ts.
import type { Option } from "@erpcraft/grid";
import { api } from "@/api/client";
import type { DocTypeMeta, FieldMeta, FieldType, Json, JsonObject, Meta } from "@/api/types";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { documentLabel, readDocument } from "@/lib/doc-labels";
import { peekLinkLabel, quote, recordLabel, rememberRecord, shortId } from "@/lib/link-labels";
import { findCommand, findDocType, findEntity } from "@/lib/meta";
import type { FieldValues } from "./field-input";
import {
  contextValue,
  type FieldContext,
  linesOf,
  linkLabels,
  lookupParams,
  lookupRows,
  rowFormats,
  withLimit,
} from "./references";

type Kind = "line_link" | "lookup" | "document_link" | "entity_link";
export const REFERENCE_KINDS = new Set<string>(["line_link", "lookup", "document_link", "entity_link"]);

const text = (value: Json | undefined): string | null => (typeof value === "string" && value !== "" ? value : null);

/** Labels of link-like values, by field and value; `version` changes when new ones arrive. */
export class ReferenceLabels {
  private labels = new Map<string, string>();
  version = 0;
  constructor(private notify: () => void) {}
  get(field: string, value: string): string | undefined {
    return this.labels.get(`${field}|${value}`);
  }
  set(field: string, entries: Option[]): void {
    let changed = false;
    for (const entry of entries) {
      const key = `${field}|${entry.value}`;
      if (this.labels.get(key) !== entry.label) {
        this.labels.set(key, entry.label);
        changed = true;
      }
    }
    if (changed) {
      this.version += 1;
      this.notify();
    }
  }
  /** The value for a label or code typed or pasted, among those offered so far. */
  find(field: string, typed: string): string | undefined {
    const needle = typed.trim().toLowerCase();
    for (const [key, label] of this.labels) {
      if (!key.startsWith(`${field}|`)) continue;
      if (label.toLowerCase() === needle || label.toLowerCase().startsWith(`${needle} `))
        return key.slice(field.length + 1);
    }
    return undefined;
  }
}

export interface ReferenceSource {
  /** The choices for one row, searched by what is typed. */
  options(values: FieldValues, header: FieldValues, search: string, signal: AbortSignal): Promise<Option[]>;
  /** What a value shows, once known. */
  label(value: string, values: FieldValues): string;
  /** Read the labels of the values the lines hold now. */
  prefetch(lines: FieldValues[], header: FieldValues, signal: AbortSignal): Promise<void>;
  /** Why nothing can be chosen yet for this row, or null. */
  blocked(values: FieldValues, header: FieldValues): string | null;
}

const matches = (search: string) => (option: Option) =>
  !search.trim() || option.label.toLowerCase().includes(search.trim().toLowerCase());

export function referenceSource(
  meta: Meta,
  field: FieldMeta,
  fields: FieldMeta[],
  labels: ReferenceLabels,
  options: { companyId: string | null; decimals: number },
): ReferenceSource | null {
  const type = field.type;
  const kind = type.kind as Kind;
  if (!REFERENCE_KINDS.has(kind)) return null;
  const context = (values: FieldValues, header: FieldValues): FieldContext => ({ header, line: values, fields });
  const labelOf = (name: string) => fields.find((f) => f.name === name)?.label ?? name;

  if (type.kind === "line_link") {
    const { document_field, doc_type, collection } = type;
    const docType = findDocType(meta, doc_type);
    const lineFields = docType?.collections.find((c) => c.name === collection)?.fields ?? [];
    const load = async (documentId: string, signal: AbortSignal): Promise<Option[]> => {
      const doc = await readDocument(doc_type, documentId, signal);
      const lines = linesOf(doc.body, collection);
      const names = await linkLabels(lineFields, lines, signal);
      const found = lines.map((line): Option => {
        const links = lineFields.flatMap((f) => {
          const value = text(line[f.name]);
          return f.type.kind === "link" && value ? [names.get(`${f.type.entity}|${value}`) ?? shortId(value)] : [];
        });
        const figures = lineFields.flatMap((f) => {
          const value = text(line[f.name]);
          return f.type.kind === "decimal" && value && !f.readonly
            ? [`${f.label} ${formatDecimal(value, { minFraction: 0 })}`]
            : [];
        });
        const number = typeof line.line_no === "number" ? t("lines.line_n", { line: line.line_no }) : null;
        return { value: text(line.id) ?? "", label: [number, ...links, ...figures].filter(Boolean).join(" · ") };
      });
      labels.set(field.name, found);
      return found;
    };
    const documentOf = (values: FieldValues, header: FieldValues) =>
      text(contextValue(context(values, header), document_field));
    return {
      options: async (values, header, search, signal) => {
        const id = documentOf(values, header);
        return id ? (await load(id, signal)).filter(matches(search)) : [];
      },
      label: (value) => labels.get(field.name, value) ?? shortId(value),
      prefetch: async (lines, header, signal) => {
        const ids = new Set(
          lines.map((values) => documentOf(values, header)).filter((id): id is string => id !== null),
        );
        await Promise.all([...ids].map((id) => load(id, signal).catch(() => [])));
      },
      blocked: (values, header) =>
        documentOf(values, header) ? null : t("reference.choose_document_first", { label: docType?.label ?? doc_type }),
    };
  }

  if (type.kind === "lookup") {
    const command = findCommand(meta, type.command);
    const formats = rowFormats(command, type.rows);
    const describe = (row: JsonObject): Option => {
      const parts = type.display.flatMap((name) => {
        const value = text(row[name]) ?? (typeof row[name] === "number" ? String(row[name]) : null);
        if (value === null) return [];
        if (formats[name] === "date") return [formatCalendarDate(value)];
        if (formats[name] === "decimal") return [formatDecimal(value, { minFraction: options.decimals })];
        return [value];
      });
      return { value: text(row[type.key]) ?? "", label: parts.join(" · ") };
    };
    const load = async (input: JsonObject, signal: AbortSignal) => {
      // `limit` only for a command that takes one (party.get lists everything and refuses it).
      const { rows } = await lookupRows(type, withLimit(command, input, 100), signal);
      const found = rows.map(describe);
      labels.set(field.name, found);
      return found;
    };
    return {
      options: async (values, header, search, signal) => {
        const input = lookupParams(type, context(values, header));
        return input ? (await load(input, signal)).filter(matches(search)) : [];
      },
      label: (value) => labels.get(field.name, value) ?? shortId(value),
      prefetch: async (lines, header, signal) => {
        const inputs = new Map<string, JsonObject>();
        for (const values of lines) {
          const input = lookupParams(type, context(values, header));
          if (input && values[field.name]) inputs.set(JSON.stringify(input), input);
        }
        await Promise.all([...inputs.values()].map((input) => load(input, signal).catch(() => [])));
      },
      blocked: (values, header) => {
        if (lookupParams(type, context(values, header))) return null;
        const missing = Object.values(type.params).flatMap((param) =>
          param.from === "field" ? [labelOf(param.field)] : [],
        );
        return t("reference.fill_first", { fields: missing.join(", ") });
      },
    };
  }

  if (type.kind === "document_link") {
    const docType = findDocType(meta, type.doc_type);
    const states = type.states;
    const describe = (doc: Parameters<typeof documentLabel>[1]): Option => ({
      value: doc.id,
      label: documentLabel(docType as DocTypeMeta | undefined, doc),
    });
    return {
      options: async (_values, header, search, signal) => {
        const page = await api.run(
          "document.list",
          {
            doc_type: type.doc_type,
            company_id: text(header.company_id) ?? options.companyId,
            q: search || null,
            status: states.length === 1 ? (states[0] ?? null) : null,
            where: states.length > 1 ? `record.status in [${states.map(quote).join(", ")}]` : null,
            sort: ["-doc_date"],
            limit: 20,
          },
          { signal },
        );
        const found = page.items.map(describe);
        labels.set(field.name, found);
        return found;
      },
      label: (value) => labels.get(field.name, value) ?? shortId(value),
      prefetch: async (lines, _header, signal) => {
        const ids = [
          ...new Set(lines.map((values) => text(values[field.name])).filter((id): id is string => id !== null)),
        ];
        if (ids.length === 0) return;
        const page = await api.run(
          "document.list",
          { doc_type: type.doc_type, where: `record.id in [${ids.map(quote).join(", ")}]`, limit: ids.length },
          { signal },
        );
        labels.set(field.name, page.items.map(describe));
      },
      blocked: () => null,
    };
  }

  // entity_link: a record of the entity another field of the row names.
  const entityType = type as Extract<FieldType, { kind: "entity_link" }>;
  const entityOf = (values: FieldValues, header: FieldValues) => {
    const name = text(contextValue(context(values, header), entityType.entity_field));
    return name && findEntity(meta, name) ? name : null;
  };
  return {
    options: async (values, header, search, signal) => {
      const entity = entityOf(values, header);
      if (!entity) return [];
      const page = await api.run(
        "entity.list",
        { entity, q: search || null, limit: 20, active_only: true },
        { signal },
      );
      return page.items.map((record) => {
        rememberRecord(entity, "id", record);
        return { value: record.id, label: recordLabel(entity, record) };
      });
    },
    label: (value, values) => {
      const entity = entityOf(values, {});
      return (entity ? peekLinkLabel(entity, "id", value) : null) ?? shortId(value);
    },
    prefetch: async () => {},
    blocked: (values, header) =>
      entityOf(values, header) ? null : t("reference.fill_first", { fields: labelOf(entityType.entity_field) }),
  };
}
