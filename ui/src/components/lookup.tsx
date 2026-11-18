import { ExternalLink, Plus } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { api } from "@/api/client";
import type { EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Button } from "@/components/button";
import { Combobox, ComboboxNote, useDebounced } from "@/components/combobox";
import { CommandGroup, CommandItem, CommandSeparator } from "@/components/ui/command";
import { t } from "@/i18n/t";
import { recordLabel, rememberRecord, useLinkInfo } from "@/lib/link-labels";
import { canEntity, findEntity, isCompanyScoped } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { recentChoices, rememberChoice } from "@/lib/recent";
import { useAsync } from "@/lib/use-async";
import { QuickCreateDialog } from "./quick-create";

const PAGE = 20;
// The values of the two entries that are not records. Record ids are UUIDs, so these cannot collide.
const CLEAR = "__clear__";
const CREATE = "__create__";

function quote(value: string): string {
  return `'${value.replaceAll("\\", "\\\\").replaceAll("'", "\\'")}'`;
}

export interface LookupProps {
  /** The entity whose records are offered. */
  entity: string;
  /** The field of that entity whose value is stored: `id`, or `code` for a currency. */
  keyField: string;
  value: string | null;
  onChange: (value: string | null, record: EntityRecord | null) => void;
  /** Limits the choice to this company's records, where the entity belongs to a company. */
  companyId?: string | null;
  /** Further limits the field declares, as `entity.list` takes them: `{"is_bank": "true"}`. */
  filters?: Record<string, string>;
  required?: boolean;
  disabled?: boolean;
  invalid?: boolean;
  id?: string;
  "aria-label"?: string;
  /** Show a link to the selected record beside the field. */
  recordLink?: boolean;
  /** Offer "New …" as the last entry. Off where creating on the spot makes no sense, as for the company switcher. */
  allowCreate?: boolean;
  /** What the empty field says, such as "Any vendor" in a filter. */
  placeholder?: string;
  className?: string;
}

/**
 * A searchable combobox for a value that comes from the database.
 *
 * The search runs on the server, so it behaves the same for 4 records and for
 * 40,000. Before anything is typed, the user's recent choices come first. The
 * last entry creates a new record in a dialog and selects it, without leaving
 * the form underneath.
 */
export function Lookup({
  entity,
  keyField,
  value,
  onChange,
  companyId,
  filters: declared,
  required = false,
  disabled = false,
  invalid = false,
  id,
  "aria-label": ariaLabel,
  recordLink = true,
  allowCreate = true,
  placeholder,
  className,
}: LookupProps) {
  const { meta, me } = useApp();
  const { code: companyCode } = useCompany();
  const target = findEntity(meta, entity);
  const readable = target !== undefined && canEntity(meta, entity, "read");
  const searchable = (target?.search.length ?? 0) > 0;
  const creatable = allowCreate && target !== undefined && !target.read_only && canEntity(meta, entity, "create");
  const label = target?.label ?? entity;
  const plural = target?.plural ?? entity;
  const triggerRef = useRef<HTMLButtonElement>(null);

  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState("");
  const [creating, setCreating] = useState<string | null>(null);
  const search = useDebounced(query.trim(), 200);
  const info = useLinkInfo(entity, keyField, readable ? value : null);

  const scope = target && isCompanyScoped(target) && companyId ? companyId : null;
  const filters: Record<string, string> = { ...declared, ...(scope ? { company_id: scope } : {}) };
  const results = useAsync(
    open && readable ? `${entity}|${JSON.stringify(filters)}|${search}` : null,
    async (signal) => {
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
              // The recent choices are a convenience; the list works without them.
              .catch(() => ({ items: [] as EntityRecord[], total: 0 }))
          : { items: [] as EntityRecord[], total: 0 },
      ]);
      // Newest choice first; records the user may no longer see are simply absent.
      const recents = recentKeys.flatMap((key) => recent.items.filter((record) => record[keyField] === key));
      const recentIds = new Set(recents.map((record) => record.id));
      return {
        recents,
        others: page.items.filter((record) => !recentIds.has(record.id)),
        total: page.total,
        shown: page.items.length,
      };
    },
  );

  const canClear = !required && value !== null;
  const recents = results.data?.recents ?? [];
  const others = results.data?.others ?? [];
  const recordCount = recents.length + others.length;

  // A new result list starts at its first entry.
  const first = recents[0]?.id ?? others[0]?.id ?? (canClear ? CLEAR : creatable ? CREATE : "");
  // biome-ignore lint/correctness/useExhaustiveDependencies: only a new result list moves the highlight
  useEffect(() => setActive(first), [results.data]);

  function openChange(next: boolean): void {
    setOpen(next);
    if (!next) setQuery("");
  }

  function pick(record: EntityRecord): void {
    const next = record[keyField];
    if (typeof next !== "string") return;
    rememberRecord(entity, keyField, record);
    rememberChoice(me.user_id, entity, keyField, next);
    onChange(next, record);
  }

  function item(record: EntityRecord) {
    return (
      <CommandItem
        key={record.id}
        value={record.id}
        data-checked={record[keyField] === value}
        onSelect={() => {
          pick(record);
          openChange(false);
        }}
      >
        <span className="min-w-0 flex-1 truncate">{recordLabel(entity, record)}</span>
        {record[keyField] === value ? <span className="sr-only">{t("lookup.selected")}</span> : null}
      </CommandItem>
    );
  }

  const shownLabel = value === null ? null : info === undefined ? t("value.loading") : (info?.label ?? value);

  return (
    <>
      <Combobox
        open={open}
        onOpenChange={openChange}
        query={query}
        onQueryChange={setQuery}
        active={active}
        onActiveChange={setActive}
        searchable={searchable}
        searchLabel={t("lookup.search", { label: plural })}
        searchPlaceholder={searchable ? t("lookup.search", { label: plural }) : t("lookup.no_search")}
        listLabel={plural}
        shown={shownLabel}
        placeholder={placeholder ?? t("lookup.placeholder")}
        id={id}
        aria-label={ariaLabel}
        invalid={invalid}
        disabled={disabled || !readable}
        triggerRef={triggerRef}
        className={className}
        trailing={
          recordLink && info ? (
            <Button asChild variant="ghost" size="icon-sm" className="text-muted-foreground">
              <Link
                to={paths.entityRecord(companyCode, entity, info.id)}
                aria-label={t("lookup.open_record", { label: info.label })}
                title={t("lookup.open_record", { label: info.label })}
              >
                <ExternalLink aria-hidden="true" />
              </Link>
            </Button>
          ) : null
        }
      >
        {results.error ? <ComboboxNote tone="error">{results.error.message}</ComboboxNote> : null}
        {results.data === undefined && !results.error ? <ComboboxNote>{t("value.loading")}</ComboboxNote> : null}
        {results.data && recordCount === 0 ? (
          <ComboboxNote>
            {search ? t("lookup.no_match", { query: search }) : t("lookup.empty", { label: plural })}
          </ComboboxNote>
        ) : null}
        {recents.length > 0 ? <CommandGroup heading={t("lookup.recent")}>{recents.map(item)}</CommandGroup> : null}
        {others.length > 0 ? (
          <CommandGroup heading={recents.length > 0 ? t("lookup.all", { label: plural }) : undefined}>
            {others.map(item)}
          </CommandGroup>
        ) : null}
        {results.data && results.data.total > results.data.shown ? (
          <ComboboxNote>
            {t("lookup.more", { shown: results.data.shown, total: results.data.total.toLocaleString() })}
          </ComboboxNote>
        ) : null}
        {canClear || creatable ? (
          <>
            {recordCount > 0 ? <CommandSeparator /> : null}
            <CommandGroup>
              {canClear ? (
                <CommandItem
                  value={CLEAR}
                  className="text-muted-foreground"
                  onSelect={() => {
                    onChange(null, null);
                    openChange(false);
                  }}
                >
                  {t("lookup.clear")}
                </CommandItem>
              ) : null}
              {creatable ? (
                <CommandItem
                  value={CREATE}
                  onSelect={() => {
                    setCreating(query.trim());
                    openChange(false);
                  }}
                >
                  <Plus aria-hidden="true" />
                  {query.trim() ? t("lookup.new_named", { label, name: query.trim() }) : t("lookup.new", { label })}
                </CommandItem>
              ) : null}
            </CommandGroup>
          </>
        ) : null}
      </Combobox>
      {creating !== null && target ? (
        <QuickCreateDialog
          entity={target}
          companyId={scope}
          suggestedName={creating}
          onClose={() => {
            setCreating(null);
            triggerRef.current?.focus();
          }}
          onCreated={(record) => {
            setCreating(null);
            pick(record);
            triggerRef.current?.focus();
          }}
        />
      ) : null}
    </>
  );
}
