// Typed filters with chips, from the registry, on Astryx's PowerSearch as it comes.
// Each field of a list becomes a filter field; the chips become the server's
// `where` expression (or a document's summary filter), so filtering stays on the server.

import {
  type EnumItem,
  PowerSearch,
  type PowerSearchConfig,
  type PowerSearchField,
  type PowerSearchFilter,
} from "@astryxdesign/core/PowerSearch";
import type { SearchableItem, SearchSource } from "@astryxdesign/core/Typeahead";
import { useMemo } from "react";
import { api } from "@/api/client";
import type { FieldMeta, Meta } from "@/api/types";
import { t } from "@/i18n/t";
import { quote, recordLabel } from "@/lib/link-labels";
import { enumLabel, findEntity, isCompanyScoped } from "@/lib/meta";

/** The free-text field: what is typed without choosing a field searches the list (`q`). */
export const TEXT_FIELD = "q";

export interface FilterSpec {
  /** The field's name as the server filters it (`doc_date`, `summary.vendor_id`). */
  key: string;
  label: string;
  kind: "text" | "number" | "date" | "bool" | "enum" | "link";
  values?: EnumItem[];
  entity?: string;
  keyField?: string;
  /** A document's summary field: filtered by exact value, not by expression. */
  summary?: boolean;
}

/** What a field of the registry can be filtered as, if at all. */
export function filterSpec(field: FieldMeta, key = field.name, summary = false): FilterSpec | null {
  const base = { key, label: field.label, summary };
  switch (field.type.kind) {
    case "text":
      return summary ? null : { ...base, kind: "text" };
    case "int":
    case "decimal":
      return summary ? null : { ...base, kind: "number" };
    case "date":
      return summary ? null : { ...base, kind: "date" };
    case "bool":
      return summary ? null : { ...base, kind: "bool" };
    case "enum": {
      const type = field.type;
      return summary ? null : { ...base, kind: "enum", values: type.values.map((v) => ({ value: v, label: enumLabel(field, v) })) };
    }
    case "link":
      return { ...base, kind: "link", entity: field.type.entity, keyField: field.type.key };
    default:
      return null;
  }
}

function entitySource(meta: Meta, entity: string, keyField: string, companyId: string | null): SearchSource {
  const target = findEntity(meta, entity);
  const filters: Record<string, string> = target && isCompanyScoped(target) && companyId ? { company_id: companyId } : {};
  const run = async (q: string): Promise<SearchableItem[]> => {
    const page = await api.run("entity.list", { entity, q: q || null, filters, limit: 20 });
    return page.items.map((record) => ({ id: String(record[keyField] ?? record.id), label: recordLabel(entity, record) }));
  };
  return { search: run, bootstrap: () => run("") };
}

function config(meta: Meta, specs: FilterSpec[], companyId: string | null): PowerSearchConfig {
  const fields: PowerSearchField[] = [
    {
      key: TEXT_FIELD,
      label: t("filter.text"),
      operators: [{ key: "contains", label: t("filter.contains"), value: { type: "string" } }],
    },
    ...specs.map((spec): PowerSearchField => {
      switch (spec.kind) {
        case "text":
          return {
            key: spec.key,
            label: spec.label,
            operators: [
              { key: "is", label: t("filter.is"), value: { type: "string" } },
              { key: "any", label: t("filter.is_any"), value: { type: "string_list" } },
            ],
          };
        case "number":
          return {
            key: spec.key,
            label: spec.label,
            operators: [
              { key: "==", label: "=", value: { type: "float" } },
              { key: ">=", label: "≥", value: { type: "float" } },
              { key: "<=", label: "≤", value: { type: "float" } },
              { key: ">", label: ">", value: { type: "float" } },
              { key: "<", label: "<", value: { type: "float" } },
            ],
          };
        case "date":
          return {
            key: spec.key,
            label: spec.label,
            operators: [
              { key: ">=", label: t("filter.on_or_after"), value: { type: "date_absolute", isDateOnly: true } },
              { key: "<=", label: t("filter.on_or_before"), value: { type: "date_absolute", isDateOnly: true } },
            ],
          };
        case "bool":
          return {
            key: spec.key,
            label: spec.label,
            operators: [
              {
                key: "is",
                label: t("filter.is"),
                value: { type: "enum", values: [{ value: "true", label: t("value.yes") }, { value: "false", label: t("value.no") }] },
              },
            ],
          };
        case "enum":
          return {
            key: spec.key,
            label: spec.label,
            operators: [{ key: "any", label: t("filter.is_any"), value: { type: "enum_list", values: spec.values ?? [] } }],
          };
        case "link":
          return {
            key: spec.key,
            label: spec.label,
            operators: [
              {
                key: "any",
                label: spec.summary ? t("filter.is") : t("filter.is_any"),
                value: { type: "entity_list", searchSource: entitySource(meta, spec.entity ?? "", spec.keyField ?? "id", companyId) },
              },
            ],
          };
      }
    }),
  ];
  return { name: "list", fields, contentSearchFieldKey: TEXT_FIELD };
}

function day(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toISOString().slice(0, 10);
}

/** The chips as the server takes them: free text, a `where` expression, and exact summary values. */
export function toQuery(filters: readonly PowerSearchFilter[], specs: FilterSpec[]) {
  let q = "";
  const conditions: string[] = [];
  const summary: Record<string, string> = {};
  for (const filter of filters) {
    if (filter.field === TEXT_FIELD && filter.value.type === "string") {
      q = filter.value.value;
      continue;
    }
    const spec = specs.find((s) => s.key === filter.field);
    if (!spec) continue;
    const field = `record.${spec.key}`;
    const v = filter.value;
    if (spec.summary) {
      if (v.type === "entity_list" && v.value[0]) summary[spec.key.replace(/^summary\./, "")] = v.value[0].id;
      continue;
    }
    switch (v.type) {
      case "string":
        conditions.push(`${field} == ${quote(v.value)}`);
        break;
      case "string_list":
        if (v.value.length) conditions.push(`${field} in [${v.value.map(quote).join(", ")}]`);
        break;
      case "float":
      case "integer":
        // Decimals are compared by the server; the number only travels as text in the expression.
        conditions.push(`${field} ${filter.operator} ${String(v.value)}`);
        break;
      case "date_absolute":
        conditions.push(`${field} ${filter.operator} ${quote(day(v.unixSeconds))}`);
        break;
      case "enum":
        conditions.push(spec.kind === "bool" ? `${field} == ${v.value === "true" ? "true" : "false"}` : `${field} == ${quote(v.value)}`);
        break;
      case "enum_list":
        if (v.value.length) conditions.push(`${field} in [${v.value.map(quote).join(", ")}]`);
        break;
      case "entity_list":
        if (v.value.length) conditions.push(`${field} in [${v.value.map((e) => quote(e.id)).join(", ")}]`);
        break;
      default:
        break;
    }
  }
  return { q, where: conditions.length ? conditions.join(" && ") : null, summary };
}

/** The chips kept in the URL, so Back, reload and a shared link restore them. */
export function parseFilters(raw: string): PowerSearchFilter[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as PowerSearchFilter[]) : [];
  } catch {
    return [];
  }
}

export function FilterBar({
  meta,
  specs,
  filters,
  onChange,
  companyId,
  resultCount,
  label,
}: {
  meta: Meta;
  specs: FilterSpec[];
  filters: PowerSearchFilter[];
  onChange: (filters: PowerSearchFilter[]) => void;
  companyId: string | null;
  resultCount?: number;
  label: string;
}) {
  // biome-ignore lint/correctness/useExhaustiveDependencies: the specs follow the list's type
  const searchConfig = useMemo(() => config(meta, specs, companyId), [meta, companyId, specs.map((s) => s.key).join()]);
  return (
    <div className="filter-bar">
      <PowerSearch
        config={searchConfig}
        filters={filters}
        onChange={(next) => onChange([...next])}
        label={label}
        placeholder={label}
        size="sm"
        presentation="popover"
        resultCount={resultCount}
        tokenOverflowBehavior="unfocusedInline"
      />
    </div>
  );
}
