import { ExternalLink } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { Link } from "react-router";
import { Button } from "@/components/button";
import { Combobox, ComboboxNote, useDebounced } from "@/components/combobox";
import { CommandGroup, CommandItem, CommandSeparator } from "@/components/ui/command";
import { t } from "@/i18n/t";
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

const CLEAR = "__clear__";

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
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState("");
  const search = useDebounced(query.trim(), 200);

  const results = useAsync<PickerPage>(
    open && load.key !== null ? `${load.key}|${searchable ? search : ""}` : null,
    (signal) => load.run(searchable ? search : "", signal),
  );

  const options = results.data?.options ?? [];
  const canClear = !required && value !== null;
  const optionCount = options.length;
  // Options are told apart by their place: their keys are the server's, and may be any text.
  const first = optionCount > 0 ? "0" : canClear ? CLEAR : "";

  // biome-ignore lint/correctness/useExhaustiveDependencies: only a new result list moves the highlight
  useEffect(() => setActive(first), [results.data]);

  function openChange(next: boolean): void {
    setOpen(next);
    if (!next) setQuery("");
  }

  const shown = value === null ? null : selectedLabel === undefined ? t("value.loading") : (selectedLabel ?? value);

  return (
    <Combobox
      open={open}
      onOpenChange={openChange}
      query={query}
      onQueryChange={setQuery}
      active={active}
      onActiveChange={setActive}
      searchable={searchable}
      searchLabel={t("lookup.search", { label: noun })}
      searchPlaceholder={searchable ? t("lookup.search", { label: noun }) : t("lookup.no_search")}
      listLabel={noun}
      shown={shown}
      placeholder={blocked && value === null ? blocked : t("lookup.placeholder")}
      id={id}
      aria-label={ariaLabel}
      aria-describedby={describedBy}
      invalid={invalid}
      required={required}
      disabled={disabled}
      className={className}
      trailing={
        href && value ? (
          <Button asChild variant="ghost" size="icon-sm" className="text-muted-foreground">
            <Link
              to={href}
              aria-label={t("lookup.open_record", { label: shown ?? "" })}
              title={t("lookup.open_record", { label: shown ?? "" })}
            >
              <ExternalLink aria-hidden="true" />
            </Link>
          </Button>
        ) : null
      }
    >
      {load.key === null ? (
        <ComboboxNote>{blocked}</ComboboxNote>
      ) : results.error ? (
        <ComboboxNote tone="error">{results.error.message}</ComboboxNote>
      ) : results.data === undefined ? (
        <ComboboxNote>{t("value.loading")}</ComboboxNote>
      ) : optionCount === 0 ? (
        <ComboboxNote>
          {search && searchable ? t("lookup.no_match", { query: search }) : t("lookup.empty", { label: noun })}
        </ComboboxNote>
      ) : null}
      {optionCount > 0 ? (
        <CommandGroup>
          {options.map((option, index) => (
            <CommandItem
              key={option.key}
              value={String(index)}
              data-checked={option.key === value}
              onSelect={() => {
                onChange(option.key, option);
                openChange(false);
              }}
            >
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="truncate">{option.label}</span>
                {option.detail ? <span className="text-sm text-muted-foreground">{option.detail}</span> : null}
              </span>
              {option.key === value ? <span className="sr-only">{t("lookup.selected")}</span> : null}
            </CommandItem>
          ))}
        </CommandGroup>
      ) : null}
      {results.data?.total !== undefined && results.data.total > optionCount ? (
        <ComboboxNote>
          {t("lookup.more", { shown: optionCount, total: results.data.total.toLocaleString() })}
        </ComboboxNote>
      ) : null}
      {canClear ? (
        <>
          {optionCount > 0 ? <CommandSeparator /> : null}
          <CommandGroup>
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
          </CommandGroup>
        </>
      ) : null}
    </Combobox>
  );
}
