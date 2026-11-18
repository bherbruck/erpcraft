// Searchable lookups for fields whose values come from the database, built on Astryx's
// Typeahead as its docs describe for a remote source: recent choices on focus, search on
// the server as the user types, each result shown with its name and its code.

import { Typeahead, TypeaheadItem } from "@astryxdesign/core/Typeahead";
import type { SearchableItem, SearchSource } from "@astryxdesign/core/Typeahead";
import { useMemo } from "react";
import { api } from "@/api/client";
import type { EntityRecord, JsonObject } from "@/api/types";
import { useApp } from "@/app/context";
import { t } from "@/i18n/t";
import { rememberRecord, recordLabel, useLinkInfo } from "@/lib/link-labels";
import { findEntity, isCompanyScoped } from "@/lib/meta";
import { recentChoices, rememberChoice } from "@/lib/recent";
import type { LineKind } from "./order-model";

interface Choice extends SearchableItem<{ code: string; group?: string; kind?: LineKind; entity: string }> {}

const PAGE = 8;

function toChoice(entity: string, record: EntityRecord, key: string, group?: string, kind?: LineKind): Choice {
  const code = typeof record.code === "string" ? record.code : "";
  const name = typeof record.name === "string" ? record.name : recordLabel(entity, record);
  return { id: String(record[key] ?? record.id), label: name, auxiliaryData: { code, group, kind, entity } };
}

/** Records of an entity matching `q`; `companyId` limits an entity that belongs to a company. */
async function findRecords(entity: string, q: string, companyId: string | null, where?: string): Promise<EntityRecord[]> {
  const filters: Record<string, string> = companyId ? { company_id: companyId } : {};
  const page = await api.run("entity.list", { entity, q: q || null, filters, where: where ?? null, limit: PAGE });
  return page.items;
}

/** The records this user picked last, then the first page, for before anything is typed. */
async function bootstrap(
  userId: string,
  entity: string,
  key: string,
  companyId: string | null,
  allGroup: string,
  where?: string,
): Promise<Choice[]> {
  const recent = recentChoices(userId, entity, key);
  const first = await findRecords(entity, "", companyId, where);
  let picked: EntityRecord[] = [];
  if (recent.length > 0) {
    const page = await api.run("entity.list", {
      entity,
      where: `record.${key} in params.recent`,
      params: { recent },
      limit: recent.length,
    });
    picked = recent.map((value) => page.items.find((item) => item[key] === value)).filter((item): item is EntityRecord => !!item);
  }
  const seen = new Set(picked.map((item) => String(item[key])));
  const recentGroup = t("lookup.recent");
  return [
    ...picked.map((record) => toChoice(entity, record, key, recentGroup)),
    ...first
      .filter((record) => !seen.has(String(record[key])))
      .map((record) => toChoice(entity, record, key, picked.length > 0 ? allGroup : undefined)),
  ];
}

function renderChoice(item: Choice) {
  return <TypeaheadItem item={item} description={item.auxiliaryData?.code || undefined} />;
}

export interface RecordLookupProps {
  entity: string;
  label: string;
  value: string;
  onChange: (value: string, record: EntityRecord | null) => void;
  /** The field of the record the value holds: `id`, or `code` for a currency. */
  keyField?: string;
  companyId: string | null;
  where?: string;
  size?: "sm" | "md";
  isLabelHidden?: boolean;
  isRequired?: boolean;
  isDisabled?: boolean;
  description?: string;
  error?: string;
}

/** A link field as a searchable combobox: the record's name, its code under it. */
export function RecordLookup({
  entity,
  label,
  value,
  onChange,
  keyField = "id",
  companyId,
  where,
  size = "md",
  isLabelHidden,
  isRequired,
  isDisabled,
  description,
  error,
}: RecordLookupProps) {
  const { meta, me } = useApp();
  const definition = findEntity(meta, entity);
  const scope = definition && isCompanyScoped(definition) ? companyId : null;
  const allGroup = t("lookup.all", { label: definition?.plural ?? entity });
  const info = useLinkInfo(entity, keyField, value || null);
  const shownLabel = info?.label ?? null;
  const records = useMemo(() => new Map<string, EntityRecord>(), []);

  const source = useMemo<SearchSource<Choice>>(
    () => ({
      async search(query) {
        // Editing a chosen value starts from its label ("C001 · Buyer LLC"): that is not a
        // search yet, so it shows what focusing an empty field shows.
        if (query === shownLabel) return bootstrap(me.user_id, entity, keyField, scope, allGroup, where);
        const found = await findRecords(entity, query, scope, where);
        for (const record of found) records.set(String(record[keyField]), record);
        return found.map((record) => toChoice(entity, record, keyField));
      },
      async bootstrap() {
        const found = await bootstrap(me.user_id, entity, keyField, scope, allGroup, where);
        return found;
      },
    }),
    [entity, keyField, scope, allGroup, where, me.user_id, records, shownLabel],
  );

  const selected: Choice | null = value
    ? {
        id: value,
        // A selected value shows its name and its code (GUIDELINES, Lookups 5).
        label: info?.label ?? value,
        auxiliaryData: { code: info?.record && typeof info.record.code === "string" ? info.record.code : "", entity },
      }
    : null;

  return (
    <Typeahead<Choice>
      label={label}
      isLabelHidden={isLabelHidden}
      value={selected}
      searchSource={source}
      hasEntriesOnFocus
      minQueryLength={1}
      size={size}
      width="100%"
      isRequired={isRequired}
      isDisabled={isDisabled}
      description={description}
      placeholder={t("lookup.search", { label: label.toLowerCase() })}
      status={error ? { type: "error", message: error } : undefined}
      statusVariant={isLabelHidden ? "tooltip" : "attached"}
      renderItem={renderChoice}
      onChange={(item) => {
        if (!item) return onChange("", null);
        const record = records.get(item.id) ?? null;
        if (record) rememberRecord(entity, keyField, record);
        rememberChoice(me.user_id, entity, keyField, item.id);
        onChange(item.id, record);
      }}
    />
  );
}

export interface LineWhatProps {
  kind: LineKind | null;
  value: string;
  label: string;
  companyId: string | null;
  error?: string;
  onChange: (kind: LineKind | null, value: string) => void;
}

const KIND_ENTITY: Record<LineKind, string> = { item: "item", charge: "charge", account: "account" };

/**
 * What a line sells: one search over items, charges and revenue accounts, the results grouped
 * by kind, so a line needs one control instead of three.
 */
export function LineWhatLookup({ kind, value, label, companyId, error, onChange }: LineWhatProps) {
  const { meta } = useApp();
  const entity = kind ? KIND_ENTITY[kind] : null;
  const info = useLinkInfo(entity, "id", value || null);
  const shownLabel = info?.label ?? null;

  const source = useMemo<SearchSource<Choice>>(() => {
    const kinds: LineKind[] = ["item", "charge", "account"];
    const groupOf = (k: LineKind) => findEntity(meta, KIND_ENTITY[k])?.plural ?? k;
    const run = async (query: string) => {
      const pages = await Promise.all(
        kinds.map(async (k) => {
          const where = k === "account" ? "!record.is_group" : undefined;
          try {
            const definition = findEntity(meta, KIND_ENTITY[k]);
            const scope = definition && isCompanyScoped(definition) ? companyId : null;
            const found = await findRecords(KIND_ENTITY[k], query, scope, where);
            for (const record of found) rememberRecord(KIND_ENTITY[k], "id", record);
            return found.slice(0, k === "item" ? 6 : 3).map((record) => toChoice(KIND_ENTITY[k], record, "id", groupOf(k), k));
          } catch {
            return [];
          }
        }),
      );
      return pages.flat();
    };
    return { search: (query: string) => run(query === shownLabel ? "" : query), bootstrap: () => run("") };
  }, [meta, companyId, shownLabel]);

  const selected: Choice | null =
    value && kind
      ? {
          id: value,
          // A selected value shows its name and its code (GUIDELINES, Lookups 5).
        label: info?.label ?? value,
          auxiliaryData: { code: info?.record && typeof info.record.code === "string" ? info.record.code : "", kind, entity: KIND_ENTITY[kind] },
        }
      : null;

  return (
    <Typeahead<Choice>
      label={label}
      isLabelHidden
      value={selected}
      searchSource={source}
      hasEntriesOnFocus
      size="sm"
      width="100%"
      maxMenuItems={12}
      placeholder={t("fresh.line_what_placeholder")}
      status={error ? { type: "error", message: error } : undefined}
      statusVariant="tooltip"
      renderItem={renderChoice}
      onChange={(item) => onChange(item?.auxiliaryData?.kind ?? null, item?.id ?? "")}
    />
  );
}

/** One of the customer's addresses, by kind, for the bill-to and ship-to fields. */
export interface Address extends JsonObject {
  id: string;
}

export function addressLine(address: JsonObject): string {
  return ["street", "city", "postcode", "country_code"]
    .map((name) => address[name])
    .filter((part): part is string => typeof part === "string" && part !== "")
    .join(", ");
}
