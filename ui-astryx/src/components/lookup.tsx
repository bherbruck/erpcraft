import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { Link } from "@astryxdesign/core/Link";
import { type SearchableItem, type SearchSource, Typeahead, TypeaheadItem } from "@astryxdesign/core/Typeahead";
import { ExternalLink } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { api } from "@/api/client";
import type { EntityRecord } from "@/api/types";
import { t } from "@/i18n/t";

export interface LookupItem extends SearchableItem<{ code: string; name: string }> {}

interface LookupProps {
  label: string;
  entity: string;
  /** The field the stored value refers to: `id` for most, `code` for a currency. */
  keyField?: string;
  value: string | null;
  onChange: (value: string | null) => void;
  /** Where the selected record opens, if it has a page in this app. */
  recordHref?: (id: string) => string;
  isRequired?: boolean;
  isLabelHidden?: boolean;
  isDisabled?: boolean;
  size?: "sm" | "md";
  error?: string | null;
  width?: number | string;
}

const RECENT_LIMIT = 5;

function recentKey(entity: string): string {
  return `astryx-spike.recent.${entity}`;
}

function readRecent(entity: string): LookupItem[] {
  try {
    const raw = window.localStorage.getItem(recentKey(entity));
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? (parsed as LookupItem[]) : [];
  } catch {
    return [];
  }
}

function rememberRecent(entity: string, item: LookupItem): void {
  try {
    const next = [item, ...readRecent(entity).filter((r) => r.id !== item.id)].slice(0, RECENT_LIMIT);
    window.localStorage.setItem(recentKey(entity), JSON.stringify(next));
  } catch {
    // Recent choices are a convenience; without storage there are none.
  }
}

function toItem(record: EntityRecord, keyField: string): LookupItem {
  const code = String(record.code ?? "");
  const name = String(record.name ?? code);
  return { id: String(record[keyField] ?? record.id), label: name, auxiliaryData: { code, name } };
}

const cache = new Map<string, LookupItem>();

/** Resolves a stored value to its name and code, once per value. */
async function resolve(entity: string, keyField: string, value: string): Promise<LookupItem | null> {
  const cacheKey = `${entity}:${keyField}:${value}`;
  const hit = cache.get(cacheKey);
  if (hit) return hit;
  const page = await api.run("entity.list", { entity, filters: { [keyField]: value }, limit: 1 });
  const record = page.items[0];
  if (!record) return null;
  const item = toItem(record, keyField);
  cache.set(cacheKey, item);
  return item;
}

/**
 * A searchable combobox over an entity. Search runs on the server; before typing it
 * shows the user's recent choices, then the first records.
 */
export function Lookup(props: LookupProps) {
  const keyField = props.keyField ?? "id";
  const { entity, value } = props;
  const [selected, setSelected] = useState<LookupItem | null>(null);

  useEffect(() => {
    if (!value) {
      setSelected(null);
      return;
    }
    if (selected?.id === value) return;
    let live = true;
    resolve(entity, keyField, value).then(
      (item) => {
        if (live) setSelected(item ?? { id: value, label: value });
      },
      () => {
        if (live) setSelected({ id: value, label: value });
      },
    );
    return () => {
      live = false;
    };
  }, [entity, keyField, value, selected?.id]);

  const source = useMemo<SearchSource<LookupItem>>(() => {
    let controller: AbortController | null = null;
    const query = async (q: string): Promise<LookupItem[]> => {
      controller?.abort();
      controller = new AbortController();
      const page = await api.run(
        "entity.list",
        { entity, q: q || null, active_only: true, limit: 20 },
        { signal: controller.signal },
      );
      return page.items.map((r) => toItem(r, keyField));
    };
    return {
      search: (q) => query(q),
      bootstrap: async () => {
        const recent = readRecent(entity);
        const first = await query("");
        return [...recent, ...first.filter((f) => !recent.some((r) => r.id === f.id))].slice(0, 10);
      },
      cancel: () => controller?.abort(),
    };
  }, [entity, keyField]);

  const change = (item: LookupItem | null) => {
    setSelected(item);
    if (item) {
      cache.set(`${entity}:${keyField}:${item.id}`, item);
      rememberRecent(entity, item);
    }
    props.onChange(item?.id ?? null);
  };

  const field = (
    <Typeahead<LookupItem>
      label={props.label}
      isLabelHidden={props.isLabelHidden}
      isRequired={props.isRequired}
      isDisabled={props.isDisabled}
      size={props.size ?? "md"}
      width={props.width ?? "100%"}
      value={selected}
      onChange={change}
      searchSource={source}
      hasEntriesOnFocus
      minQueryLength={0}
      debounceMs={200}
      placeholder={t("lookup.search")}
      emptySearchText={t("lookup.none")}
      status={props.error ? { type: "error", message: props.error } : undefined}
      renderItem={(item) => <TypeaheadItem item={item} description={item.auxiliaryData?.code} />}
    />
  );

  if (!props.recordHref || !selected || selected.id !== value) return field;
  return (
    <HStack gap={1} vAlign="end">
      {field}
      <Link href={props.recordHref(selected.id)} label={t("lookup.open", { label: selected.label })}>
        <Icon icon={ExternalLink} size="sm" />
      </Link>
    </HStack>
  );
}
