import { Check, ChevronsUpDown, ExternalLink, Plus } from "lucide-react";
import { type KeyboardEvent, useEffect, useId, useRef, useState } from "react";
import { Link } from "react-router";
import { api } from "@/api/client";
import type { EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { inputClasses } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";
import { recordLabel, rememberRecord, useLinkInfo } from "@/lib/link-labels";
import { canEntity, findEntity, isCompanyScoped } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { recentChoices, rememberChoice } from "@/lib/recent";
import { useAsync } from "@/lib/use-async";
import { QuickCreateDialog } from "./quick-create";

const PAGE = 20;

function useDebounced<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(timer);
  }, [value, delay]);
  return debounced;
}

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

type Option = { kind: "record"; record: EntityRecord; recent: boolean } | { kind: "clear" } | { kind: "create" };

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
  const listId = useId();
  const searchRef = useRef<HTMLInputElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
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
  const options: Option[] = [
    ...(results.data?.recents ?? []).map((record): Option => ({ kind: "record", record, recent: true })),
    ...(results.data?.others ?? []).map((record): Option => ({ kind: "record", record, recent: false })),
    ...(canClear ? [{ kind: "clear" } as const] : []),
    ...(creatable ? [{ kind: "create" } as const] : []),
  ];
  const recordCount = options.filter((option) => option.kind === "record").length;
  const recentCount = results.data?.recents.length ?? 0;

  // biome-ignore lint/correctness/useExhaustiveDependencies: a new result list starts at its first entry
  useEffect(() => setActive(0), [results.data]);

  function close(): void {
    setOpen(false);
    setQuery("");
  }

  function pick(record: EntityRecord): void {
    const next = record[keyField];
    if (typeof next !== "string") return;
    rememberRecord(entity, keyField, record);
    rememberChoice(me.user_id, entity, keyField, next);
    onChange(next, record);
  }

  function choose(index: number): void {
    const option = options[index];
    if (!option) return;
    if (option.kind === "record") pick(option.record);
    else if (option.kind === "clear") onChange(null, null);
    else setCreating(query.trim());
    close();
  }

  function onSearchKey(event: KeyboardEvent<HTMLInputElement>): void {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((n) => (options.length === 0 ? 0 : (n + 1) % options.length));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((n) => (options.length === 0 ? 0 : (n - 1 + options.length) % options.length));
    } else if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      choose(active);
    }
  }

  function onTriggerKey(event: KeyboardEvent<HTMLButtonElement>): void {
    // Typing on the closed field starts a search, so a line can be entered without the mouse.
    if (event.key.length === 1 && event.key !== " " && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      setQuery(searchable ? event.key : "");
      setOpen(true);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      setOpen(true);
    }
  }

  const shownLabel = value === null ? null : info === undefined ? t("value.loading") : (info?.label ?? value);
  const optionClasses =
    "flex cursor-pointer items-center gap-2 rounded-sm px-2 py-1.5 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground";

  return (
    <div className={cn("flex min-w-0 items-center gap-1", className)}>
      <Popover
        open={open}
        onOpenChange={(next) => {
          if (next) setOpen(true);
          else close();
        }}
      >
        <PopoverTrigger asChild>
          <button
            ref={triggerRef}
            type="button"
            id={id}
            role="combobox"
            aria-expanded={open}
            aria-controls={listId}
            aria-haspopup="listbox"
            aria-label={ariaLabel}
            aria-invalid={invalid || undefined}
            disabled={disabled || !readable}
            onKeyDown={onTriggerKey}
            className={cn(inputClasses, "items-center justify-between gap-2 text-start")}
          >
            <span className={cn("truncate", shownLabel === null && "text-muted-foreground")}>
              {shownLabel ?? placeholder ?? t("lookup.placeholder")}
            </span>
            <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
          </button>
        </PopoverTrigger>
        <PopoverContent
          className="w-lookup max-w-[calc(100vw-2rem)] p-0"
          onOpenAutoFocus={(event) => {
            // Put the cursor after the letter that opened the lookup, instead of
            // selecting it, so the next keystroke continues the word.
            event.preventDefault();
            const input = searchRef.current;
            if (!input) return;
            input.focus();
            input.setSelectionRange(input.value.length, input.value.length);
          }}
        >
          <input
            ref={searchRef}
            role="combobox"
            aria-expanded="true"
            aria-label={t("lookup.search", { label: plural })}
            aria-controls={listId}
            aria-activedescendant={options.length > 0 ? `${listId}-${active}` : undefined}
            value={query}
            readOnly={!searchable}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={onSearchKey}
            placeholder={searchable ? t("lookup.search", { label: plural }) : t("lookup.no_search")}
            className="h-9 w-full rounded-t-md border-b bg-transparent px-3 text-sm outline-none placeholder:text-muted-foreground"
          />
          <div id={listId} role="listbox" aria-label={plural} className="max-h-72 overflow-y-auto p-1">
            {results.error ? <p className="px-2 py-1.5 text-sm text-destructive">{results.error.message}</p> : null}
            {results.data === undefined && !results.error ? (
              <p className="px-2 py-1.5 text-sm text-muted-foreground">{t("value.loading")}</p>
            ) : null}
            {results.data && recordCount === 0 ? (
              <p className="px-2 py-1.5 text-sm text-muted-foreground">
                {search ? t("lookup.no_match", { query: search }) : t("lookup.empty", { label: plural })}
              </p>
            ) : null}
            {options.map((option, index) => {
              const common = {
                id: `${listId}-${index}`,
                role: "option" as const,
                tabIndex: -1,
                "aria-selected": index === active,
                onMouseEnter: () => setActive(index),
                onClick: () => choose(index),
              };
              if (option.kind === "record") {
                const { record } = option;
                return (
                  <div key={record.id}>
                    {recentCount > 0 && index === 0 ? (
                      <p className="px-2 pb-0.5 pt-1 text-xs font-medium text-muted-foreground">{t("lookup.recent")}</p>
                    ) : null}
                    {recentCount > 0 && index === recentCount ? (
                      <p className="mt-1 border-t px-2 pb-0.5 pt-2 text-xs font-medium text-muted-foreground">
                        {t("lookup.all", { label: plural })}
                      </p>
                    ) : null}
                    <div {...common} className={optionClasses}>
                      <span className="min-w-0 flex-1 truncate">{recordLabel(entity, record)}</span>
                      {record[keyField] === value ? (
                        <Check className="size-3.5 shrink-0" aria-label={t("lookup.selected")} />
                      ) : null}
                    </div>
                  </div>
                );
              }
              const first = index === recordCount;
              return (
                <div key={option.kind}>
                  {first && results.data && results.data.total > results.data.shown ? (
                    <p className="px-2 py-1.5 text-xs text-muted-foreground">
                      {t("lookup.more", { shown: results.data.shown, total: results.data.total.toLocaleString() })}
                    </p>
                  ) : null}
                  <div
                    {...common}
                    className={cn(
                      optionClasses,
                      first && "mt-1 border-t",
                      option.kind === "clear" && "text-muted-foreground",
                    )}
                  >
                    {option.kind === "clear" ? (
                      t("lookup.clear")
                    ) : (
                      <>
                        <Plus className="size-3.5 shrink-0" aria-hidden="true" />
                        {query.trim()
                          ? t("lookup.new_named", { label, name: query.trim() })
                          : t("lookup.new", { label })}
                      </>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </PopoverContent>
      </Popover>
      {recordLink && info ? (
        <Link
          to={paths.entityRecord(companyCode, entity, info.id)}
          aria-label={t("lookup.open_record", { label: info.label })}
          title={t("lookup.open_record", { label: info.label })}
          className="inline-flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        >
          <ExternalLink className="size-3.5" aria-hidden="true" />
        </Link>
      ) : null}
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
    </div>
  );
}
