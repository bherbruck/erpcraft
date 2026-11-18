import { useState } from "react";
import { api } from "@/api/client";
import type { EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { quote, recordLabel, rememberRecord, useLinkInfo } from "@/lib/link-labels";
import { canEntity, findEntity, isCompanyScoped } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { recentChoices, rememberChoice } from "@/lib/recent";
import { Picker, type PickerOption } from "./picker";
import { QuickCreateDialog } from "./quick-create";

const PAGE = 20;

export interface LookupProps {
  label: string;
  isLabelHidden?: boolean;
  /** The entity whose records are offered. */
  entity: string;
  /** The field of that entity whose value is stored: `id`, or `code` for a currency. */
  keyField: string;
  value: string | null;
  onChange: (value: string | null, record: EntityRecord | null) => void;
  /** Limits the choice to this company's records, where the entity belongs to a company. */
  companyId?: string | null;
  /** Further limits the field declares, as `entity.list` takes them. */
  filters?: Record<string, string>;
  required?: boolean;
  disabled?: boolean;
  error?: string | null;
  description?: string;
  labelTooltip?: string;
  /** Show a link to the selected record beside the field. Off in grid cells. */
  recordLink?: boolean;
  /** Offer "New …" as the last entry. */
  allowCreate?: boolean;
  size?: "sm" | "md";
}

/**
 * A searchable combobox for a value that comes from the database. The search runs
 * on the server. Before anything is typed, the user's recent choices come first.
 * The last entry creates a new record in a dialog and selects it, without leaving
 * the form underneath.
 */
export function Lookup(props: LookupProps) {
  const { meta, me } = useApp();
  const { code } = useCompany();
  const { entity, keyField, value } = props;
  const target = findEntity(meta, entity);
  const readable = target !== undefined && canEntity(meta, entity, "read");
  const searchable = (target?.search.length ?? 0) > 0;
  const creatable =
    (props.allowCreate ?? true) && target !== undefined && !target.read_only && canEntity(meta, entity, "create");
  const plural = target?.plural ?? entity;
  const info = useLinkInfo(entity, keyField, readable ? value : null);
  const [creating, setCreating] = useState<string | null>(null);

  const scope = target && isCompanyScoped(target) && props.companyId ? props.companyId : null;
  const filters: Record<string, string> = { ...props.filters, ...(scope ? { company_id: scope } : {}) };
  const filterKey = JSON.stringify(filters);

  const option = (record: EntityRecord, group?: string): PickerOption => {
    const code = typeof record.code === "string" ? record.code : undefined;
    const label = recordLabel(entity, record);
    return { key: String(record[keyField] ?? record.id), label, detail: code && !label.includes(code) ? code : undefined, group };
  };

  const pick = (next: string | null, picked: EntityRecord | null) => {
    if (next !== null && picked) {
      rememberRecord(entity, keyField, picked);
      rememberChoice(me.user_id, entity, keyField, next);
    }
    props.onChange(next, picked);
  };

  // The records behind the options, so a choice can be remembered with its label.
  const seen = new Map<string, EntityRecord>();

  return (
    <>
      <Picker
        label={props.label}
        isLabelHidden={props.isLabelHidden}
        description={props.description}
        labelTooltip={props.labelTooltip}
        noun={plural}
        value={value}
        selectedLabel={value === null ? null : info === undefined ? undefined : (info?.label ?? value)}
        onChange={(next) => pick(next, next ? (seen.get(next) ?? null) : null)}
        required={props.required}
        disabled={props.disabled || !readable}
        error={props.error}
        size={props.size}
        searchable={searchable}
        href={info && (props.recordLink ?? true) ? paths.entityRecord(code, entity, info.id) : null}
        extra={(query) =>
          creatable
            ? [
                {
                  key: "__create__",
                  label: query.trim()
                    ? t("lookup.new_named", { label: target?.label ?? entity, name: query.trim() })
                    : t("lookup.new", { label: target?.label ?? entity }),
                  onPick: (typed) => setCreating(typed.trim()),
                },
              ]
            : []
        }
        load={{
          key: readable ? `${entity}|${filterKey}` : null,
          run: async (search, signal) => {
            const text = searchable && search ? search : null;
            const recentKeys = text ? [] : recentChoices(me.user_id, entity, keyField);
            const [page, recent] = await Promise.all([
              api.run("entity.list", { entity, q: text, filters, limit: PAGE, active_only: true }, { signal }),
              recentKeys.length > 0
                ? api
                    .run(
                      "entity.list",
                      {
                        entity,
                        filters,
                        where: `record.${keyField} in [${recentKeys.map(quote).join(", ")}]`,
                        limit: recentKeys.length,
                      },
                      { signal },
                    )
                    .catch(() => ({ items: [] as EntityRecord[], total: 0 }))
                : { items: [] as EntityRecord[], total: 0 },
            ]);
            const recents = recentKeys.flatMap((key) => recent.items.filter((record) => record[keyField] === key));
            const recentIds = new Set(recents.map((record) => record.id));
            for (const record of [...recents, ...page.items]) seen.set(String(record[keyField] ?? record.id), record);
            const others = page.items.filter((record) => !recentIds.has(record.id));
            return {
              total: page.total,
              options: [
                ...recents.map((record) => option(record, t("lookup.recent"))),
                ...others.map((record) => option(record, recents.length > 0 ? t("lookup.all", { label: plural }) : undefined)),
              ],
            };
          },
        }}
      />
      {creating !== null && target ? (
        <QuickCreateDialog
          entity={target}
          companyId={scope}
          suggestedName={creating}
          onClose={() => setCreating(null)}
          onCreated={(record) => {
            setCreating(null);
            const next = record[keyField];
            if (typeof next === "string") pick(next, record);
          }}
        />
      ) : null}
    </>
  );
}
