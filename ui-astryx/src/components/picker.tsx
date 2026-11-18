import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { Link } from "@astryxdesign/core/Link";
import { Text } from "@astryxdesign/core/Text";
import { type SearchableItem, type SearchSource, Typeahead } from "@astryxdesign/core/Typeahead";
import { VStack } from "@astryxdesign/core/VStack";
import { ExternalLink } from "lucide-react";
import { type ReactNode, useMemo, useRef } from "react";
import { t } from "@/i18n/t";

export interface PickerOption {
  key: string;
  label: string;
  /** A second, quieter line of text: a date, an amount, a status. */
  detail?: ReactNode;
  /** A heading the option is listed under, such as "Recent". */
  group?: string;
  /** An entry that is not a value, such as "New customer…": picking it calls this instead. */
  onPick?: (query: string) => void;
}

export interface PickerPage {
  options: PickerOption[];
  /** How many there are in all, when more exist than are shown. */
  total?: number;
}

export interface PickerProps {
  /** The field's label. Astryx renders it; hide it where a column header names the field. */
  label: string;
  isLabelHidden?: boolean;
  value: string | null;
  /** What the selected value shows. `undefined` while it is being found out. */
  selectedLabel: string | null | undefined;
  onChange: (value: string | null, option: PickerOption | null) => void;
  /** Reads the options from the server for a search text. `null` as the key: nothing can be offered yet. */
  load: { key: string | null; run: (search: string, signal: AbortSignal) => Promise<PickerPage> };
  /** Why nothing can be chosen yet, such as "Choose the purchase order first". */
  blocked?: string | null;
  /** Whether the server can search; otherwise the options are filtered here. */
  searchable?: boolean;
  /** Entries after the options, always offered: "New customer…". */
  extra?: (query: string) => PickerOption[];
  noun: string;
  required?: boolean;
  disabled?: boolean;
  error?: string | null;
  description?: string;
  labelTooltip?: string;
  /** Where the selected value can be opened. */
  href?: string | null;
  size?: "sm" | "md";
}

interface Item extends SearchableItem<{ option: PickerOption; group?: string }> {}

function toItem(option: PickerOption): Item {
  return { id: option.key, label: option.label, auxiliaryData: { option, group: option.group } };
}

/**
 * A combobox over options the server lists: records, documents, lines of another
 * document, rows of a query command. It is Astryx's Typeahead as it comes; this
 * only turns the server's answers into its search source.
 */
export function Picker(props: PickerProps) {
  const { load, searchable = true, extra } = props;
  const queryRef = useRef("");
  // biome-ignore lint/correctness/useExhaustiveDependencies: the load key names everything the source depends on
  const source = useMemo<SearchSource<Item>>(() => {
    let controller: AbortController | null = null;
    const fetch = async (query: string): Promise<Item[]> => {
      queryRef.current = query;
      const extras = (extra?.(query) ?? []).map(toItem);
      if (load.key === null) return extras;
      controller?.abort();
      controller = new AbortController();
      const page = await load.run(searchable ? query : "", controller.signal);
      const needle = query.trim().toLowerCase();
      const options = searchable || !needle ? page.options : page.options.filter((o) => o.label.toLowerCase().includes(needle));
      return [...options.map(toItem), ...extras];
    };
    return {
      search: (query) => fetch(query),
      bootstrap: () => fetch(""),
      cancel: () => controller?.abort(),
    };
  }, [load.key, searchable]);

  const selected: Item | null =
    props.value === null
      ? null
      : { id: props.value, label: props.selectedLabel === undefined ? t("value.loading") : (props.selectedLabel ?? props.value) };

  const field = (
    <Typeahead<Item>
      label={props.label}
      isLabelHidden={props.isLabelHidden}
      description={props.description}
      labelTooltip={props.labelTooltip}
      isRequired={props.required}
      isDisabled={props.disabled || Boolean(props.blocked)}
      disabledMessage={props.blocked ?? undefined}
      size={props.size ?? "md"}
      width="100%"
      value={selected}
      hasClear={!props.required}
      onChange={(item) => {
        const option = item?.auxiliaryData?.option ?? null;
        if (option?.onPick) {
          option.onPick(queryRef.current);
          return;
        }
        props.onChange(item?.id ?? null, option);
      }}
      searchSource={source}
      hasEntriesOnFocus
      minQueryLength={0}
      maxMenuItems={25}
      debounceMs={200}
      placeholder={t("lookup.search", { label: props.noun })}
      emptySearchText={t("spike.no_results")}
      status={props.error ? { type: "error", message: props.error } : undefined}
      statusVariant={props.isLabelHidden ? "tooltip" : "detached"}
      renderItem={(item) => {
        const detail = item.auxiliaryData?.option.detail;
        return (
          <VStack gap={0} paddingBlock={0.5}>
            <Text maxLines={1}>{item.label}</Text>
            {detail ? (
              <Text type="supporting" maxLines={1}>
                {detail}
              </Text>
            ) : null}
          </VStack>
        );
      }}
    />
  );
  if (!props.href || props.value === null) return field;
  return (
    <HStack gap={1} vAlign="end">
      <div className="grow">{field}</div>
      <div className="open-link">
        <Link href={props.href} label={t("lookup.open_record", { label: props.selectedLabel ?? props.noun })}>
          <Icon icon={ExternalLink} size="sm" color="secondary" />
        </Link>
      </div>
    </HStack>
  );
}
