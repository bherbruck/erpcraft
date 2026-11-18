import { Check, ChevronsUpDown, ExternalLink } from "lucide-react";
import { type KeyboardEvent, type ReactNode, useEffect, useId, useRef, useState } from "react";
import { Link } from "react-router";
import { inputClasses } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";
import { useAsync } from "@/lib/use-async";

export interface PickerOption {
  key: string;
  label: string;
  /** A second, quieter line of text: a date, an amount, a status. */
  detail?: ReactNode;
}

export interface PickerPage {
  options: PickerOption[];
  /** How many there are in all, when more exist than are shown. */
  total?: number;
}

export interface PickerProps {
  value: string | null;
  /** What the selected value shows. `undefined` while it is being found out. */
  selectedLabel: string | null | undefined;
  onChange: (value: string | null, option: PickerOption | null) => void;
  /**
   * Reads the options from the server for a search text. `null` as the cache
   * key means nothing can be offered yet; `blocked` then says why.
   */
  load: { key: string | null; run: (search: string, signal: AbortSignal) => Promise<PickerPage> };
  /** Why nothing can be chosen yet, such as "Choose the purchase order first". */
  blocked?: string | null;
  /** Whether the server can search; otherwise the list is only scrolled. */
  searchable?: boolean;
  /** What is being chosen, for the search box and the list: "Purchase orders". */
  noun: string;
  required?: boolean;
  disabled?: boolean;
  invalid?: boolean;
  id?: string;
  "aria-label"?: string;
  "aria-describedby"?: string;
  /** Where the selected value can be opened. */
  href?: string | null;
  className?: string;
}

function useDebounced<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(timer);
  }, [value, delay]);
  return debounced;
}

type Entry = { kind: "option"; option: PickerOption } | { kind: "clear" };

/**
 * A searchable combobox over options the server lists: documents to refer to,
 * lines of another document, rows of a query command. The same keyboard
 * behaviour as the entity lookup: typing on the closed field starts a search,
 * arrows move, Enter takes.
 */
export function Picker({
  value,
  selectedLabel,
  onChange,
  load,
  blocked = null,
  searchable = true,
  noun,
  required = false,
  disabled = false,
  invalid = false,
  id,
  "aria-label": ariaLabel,
  "aria-describedby": describedBy,
  href,
  className,
}: PickerProps) {
  const listId = useId();
  const searchRef = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const search = useDebounced(query.trim(), 200);

  const results = useAsync<PickerPage>(
    open && load.key !== null ? `${load.key}|${searchable ? search : ""}` : null,
    (signal) => load.run(searchable ? search : "", signal),
  );

  const entries: Entry[] = [
    ...(results.data?.options ?? []).map((option): Entry => ({ kind: "option", option })),
    ...(!required && value !== null ? [{ kind: "clear" } as const] : []),
  ];
  const optionCount = results.data?.options.length ?? 0;

  // biome-ignore lint/correctness/useExhaustiveDependencies: a new result list starts at its first entry
  useEffect(() => setActive(0), [results.data]);

  function close(): void {
    setOpen(false);
    setQuery("");
  }

  function choose(index: number): void {
    const entry = entries[index];
    if (!entry) return;
    if (entry.kind === "option") onChange(entry.option.key, entry.option);
    else onChange(null, null);
    close();
  }

  function onSearchKey(event: KeyboardEvent<HTMLInputElement>): void {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((n) => (entries.length === 0 ? 0 : (n + 1) % entries.length));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((n) => (entries.length === 0 ? 0 : (n - 1 + entries.length) % entries.length));
    } else if (event.key === "Enter") {
      event.preventDefault();
      event.stopPropagation();
      choose(active);
    }
  }

  function onTriggerKey(event: KeyboardEvent<HTMLButtonElement>): void {
    if (event.key.length === 1 && event.key !== " " && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      setQuery(searchable ? event.key : "");
      setOpen(true);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      setOpen(true);
    }
  }

  const shown = value === null ? null : selectedLabel === undefined ? t("value.loading") : (selectedLabel ?? value);
  const optionClasses =
    "flex cursor-pointer flex-col rounded-sm px-2 py-1.5 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground";

  return (
    <div className={cn("flex min-w-0 items-center gap-1", className)}>
      <Popover open={open} onOpenChange={(next) => (next ? setOpen(true) : close())}>
        <PopoverTrigger asChild>
          <button
            type="button"
            id={id}
            role="combobox"
            aria-expanded={open}
            aria-controls={listId}
            aria-haspopup="listbox"
            aria-label={ariaLabel}
            aria-describedby={describedBy}
            aria-invalid={invalid || undefined}
            aria-required={required || undefined}
            disabled={disabled}
            onKeyDown={onTriggerKey}
            className={cn(inputClasses, "items-center justify-between gap-2 text-start")}
          >
            <span className={cn("truncate", shown === null && "text-muted-foreground")}>
              {shown ?? (blocked && value === null ? blocked : t("lookup.placeholder"))}
            </span>
            <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
          </button>
        </PopoverTrigger>
        <PopoverContent
          className="w-lookup max-w-[calc(100vw-2rem)] p-0"
          onOpenAutoFocus={(event) => {
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
            aria-label={t("lookup.search", { label: noun })}
            aria-controls={listId}
            aria-activedescendant={entries.length > 0 ? `${listId}-${active}` : undefined}
            value={query}
            readOnly={!searchable}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={onSearchKey}
            placeholder={searchable ? t("lookup.search", { label: noun }) : t("lookup.no_search")}
            className="h-9 w-full rounded-t-md border-b bg-transparent px-3 text-sm outline-none placeholder:text-muted-foreground"
          />
          <div id={listId} role="listbox" aria-label={noun} className="max-h-72 overflow-y-auto p-1">
            {load.key === null ? (
              <p className="px-2 py-1.5 text-sm text-muted-foreground">{blocked}</p>
            ) : results.error ? (
              <p className="px-2 py-1.5 text-sm text-destructive">{results.error.message}</p>
            ) : results.data === undefined ? (
              <p className="px-2 py-1.5 text-sm text-muted-foreground">{t("value.loading")}</p>
            ) : optionCount === 0 ? (
              <p className="px-2 py-1.5 text-sm text-muted-foreground">
                {search && searchable ? t("lookup.no_match", { query: search }) : t("lookup.empty", { label: noun })}
              </p>
            ) : null}
            {entries.map((entry, index) => {
              const common = {
                id: `${listId}-${index}`,
                role: "option" as const,
                tabIndex: -1,
                "aria-selected": index === active,
                onMouseEnter: () => setActive(index),
                onClick: () => choose(index),
              };
              if (entry.kind === "clear") {
                return (
                  <div key="clear" {...common} className={cn(optionClasses, "mt-1 border-t text-muted-foreground")}>
                    {t("lookup.clear")}
                  </div>
                );
              }
              const { option } = entry;
              return (
                <div key={option.key} {...common} className={optionClasses}>
                  <span className="flex items-center gap-2">
                    <span className="min-w-0 flex-1 truncate">{option.label}</span>
                    {option.key === value ? (
                      <Check className="size-3.5 shrink-0" aria-label={t("lookup.selected")} />
                    ) : null}
                  </span>
                  {option.detail ? <span className="text-xs text-muted-foreground">{option.detail}</span> : null}
                </div>
              );
            })}
            {results.data?.total !== undefined && results.data.total > optionCount ? (
              <p className="px-2 py-1.5 text-xs text-muted-foreground">
                {t("lookup.more", { shown: optionCount, total: results.data.total.toLocaleString() })}
              </p>
            ) : null}
          </div>
        </PopoverContent>
      </Popover>
      {href && value ? (
        <Link
          to={href}
          aria-label={t("lookup.open_record", { label: shown ?? "" })}
          title={t("lookup.open_record", { label: shown ?? "" })}
          className="inline-flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        >
          <ExternalLink className="size-3.5" aria-hidden="true" />
        </Link>
      ) : null}
    </div>
  );
}
