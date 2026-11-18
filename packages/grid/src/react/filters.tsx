// The filter forms without their look: the state of a condition form and of
// a checklist with search. The grid renders them through the
// ConditionFilter and ChecklistFilter slots; the hooks are exported for a
// UI that wants the same forms elsewhere.

import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { DATE_OPS, emptyCondition, filterKind, isComplete, NUMBER_OPS, RELATIVE_RANGES, TEXT_OPS } from "../core/filters";
import type { FilterCondition, GridColumn, Option, RelativeRange } from "../types";
import type {
  ChecklistFilterSlotProps,
  ChecklistItem,
  ConditionFilterSlotProps,
  FilterInputProps,
  GridLabels,
} from "./slots";

export interface FilterFormOptions<Row> {
  column: GridColumn<Row>;
  /** The condition set on the column, or null. */
  condition: FilterCondition | null;
  /** Set the column's condition; an incomplete one removes the filter. */
  onChange(condition: FilterCondition | null): void;
  /** Close the popover holding the form. */
  onDone(): void;
  labels: GridLabels;
}

const BOOLEAN_OPS = ["true", "false", "empty"] as const;

export function useConditionFilter<Row>(options: FilterFormOptions<Row>): ConditionFilterSlotProps<Row> {
  const { column, condition, onChange, onDone, labels } = options;
  const kind = filterKind(column.type) as ConditionFilterSlotProps<Row>["kind"];
  const start = condition ?? emptyCondition(column.type);
  const [draft, setDraft] = useState<FilterCondition>(start);
  // A filter changed elsewhere (a chip removed) resets the form.
  const key = JSON.stringify(condition);
  // biome-ignore lint/correctness/useExhaustiveDependencies: the key stands for the condition
  useEffect(() => setDraft(condition ?? emptyCondition(column.type)), [key]);

  const label = (op: string) => (labels as Record<string, string>)[op] ?? op;
  const opNames: readonly string[] =
    kind === "text" ? TEXT_OPS : kind === "number" ? NUMBER_OPS : kind === "date" ? DATE_OPS : BOOLEAN_OPS;
  const op =
    draft.kind === "boolean"
      ? condition === null
        ? ""
        : draft.value === null
          ? "empty"
          : String(draft.value)
      : "op" in draft
        ? draft.op
        : "";

  const apply = (next: FilterCondition = draft) => {
    onChange(isComplete(next) ? next : null);
    onDone();
  };

  const setOp = (next: string) => {
    if (draft.kind === "boolean") {
      const value = next === "empty" ? null : next === "true";
      const updated: FilterCondition = { kind: "boolean", value };
      setDraft(updated);
      onChange(updated);
      return;
    }
    setDraft({ ...draft, op: next } as FilterCondition);
  };

  const field = (which: "value" | "to"): FilterInputProps => {
    const current = draft.kind === "text" || draft.kind === "number" || draft.kind === "date" ? draft : null;
    const text = which === "value" ? (current?.value ?? "") : ((current as { to?: string } | null)?.to ?? "");
    return {
      value: text,
      onChange: (event) => {
        if (current) setDraft({ ...current, [which]: event.target.value } as FilterCondition);
      },
      onKeyDown: (event: KeyboardEvent<HTMLInputElement>) => {
        if (event.key === "Enter") {
          event.preventDefault();
          apply();
        }
      },
      "aria-label": which === "to" ? labels.to : op === "between" ? labels.from : labels.value,
      type: kind === "date" ? "date" : "text",
      ...(kind === "number" ? { inputMode: "decimal" as const } : {}),
    };
  };

  const noValue = op === "empty" || op === "notEmpty" || op === "relative" || kind === "boolean";
  return {
    column,
    kind,
    ops: opNames.map((value) => ({ value, label: label(value) })),
    op,
    setOp,
    needsValue: !noValue,
    needsTo: op === "between",
    needsRange: op === "relative",
    valueProps: field("value"),
    toProps: field("to"),
    ranges: RELATIVE_RANGES.map((value) => ({ value, label: label(value) })),
    range: draft.kind === "date" ? (draft.range ?? "") : "",
    setRange: (range) => {
      if (draft.kind === "date") setDraft({ ...draft, range: range as RelativeRange });
    },
    valid: isComplete(draft),
    apply: () => apply(),
    clear: () => {
      onChange(null);
      setDraft(emptyCondition(column.type));
    },
    active: condition !== null,
    labels,
  };
}

export interface ChecklistOptions<Row> extends FilterFormOptions<Row> {
  /** Labels of references seen before, so chosen ones show by name. */
  knownLabels: ReadonlyMap<string, string>;
  rememberLabels(options: Option[]): void;
}

export function useChecklistFilter<Row>(options: ChecklistOptions<Row>): ChecklistFilterSlotProps<Row> {
  const { column, condition, onChange, onDone, labels, knownLabels, rememberLabels } = options;
  const loadOptions = column.loadOptions;
  const [search, setSearch] = useState("");
  const [loaded, setLoaded] = useState<Option[] | null>(null);
  const [loading, setLoading] = useState(false);
  const rememberRef = useRef(rememberLabels);
  rememberRef.current = rememberLabels;

  useEffect(() => {
    if (!loadOptions) return undefined;
    const controller = new AbortController();
    setLoading(true);
    const timer = setTimeout(() => {
      loadOptions(search, controller.signal)
        .then((found) => {
          setLoaded(found);
          rememberRef.current(found);
        })
        .catch(() => undefined)
        .finally(() => {
          if (!controller.signal.aborted) setLoading(false);
        });
    }, 200);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [search, loadOptions]);

  const set = condition?.kind === "set" ? condition : null;
  const all = useMemo(() => (loadOptions ? (loaded ?? []) : (column.options ?? [])), [loadOptions, loaded, column.options]);
  const lower = search.trim().toLocaleLowerCase();
  const visible = loadOptions ? all : all.filter((o) => o.label.toLocaleLowerCase().includes(lower));
  // Without a filter a fixed list shows everything ticked, as Excel's checklist does.
  const everything = !loadOptions && set === null;
  const values = everything ? all.map((o) => o.value) : (set?.values ?? []);
  const includeEmpty = everything ? true : set?.includeEmpty === true;
  // Chosen references that the current search does not list stay in view.
  const chosenElsewhere: Option[] = loadOptions
    ? values
        .filter((v) => !visible.some((o) => o.value === v))
        .map((v) => ({ value: v, label: knownLabels.get(v) ?? v }))
    : [];

  const commit = (nextValues: string[], nextEmpty: boolean) => {
    const unique = [...new Set(nextValues)];
    // Everything ticked is no filter at all.
    if (!loadOptions && unique.length === all.length && nextEmpty) {
      onChange(null);
      return;
    }
    const next: FilterCondition = { kind: "set", values: unique, includeEmpty: nextEmpty };
    onChange(isComplete(next) ? next : null);
  };

  const items: ChecklistItem[] = [...chosenElsewhere, ...visible].map((o) => {
    const checked = values.includes(o.value);
    return {
      value: o.value,
      label: o.label,
      checked,
      toggle: () => commit(checked ? values.filter((v) => v !== o.value) : [...values, o.value], includeEmpty),
    };
  });
  const shown = visible.map((o) => o.value);
  const allChecked = shown.length > 0 && shown.every((v) => values.includes(v));
  const someChecked = shown.some((v) => values.includes(v));

  return {
    column,
    search,
    setSearch,
    searchProps: {
      value: search,
      onChange: (event) => setSearch(event.target.value),
      onKeyDown: (event) => {
        if (event.key === "Enter") event.preventDefault();
      },
      "aria-label": labels.search,
      type: "text",
    },
    items,
    allChecked,
    someChecked,
    toggleAll: () =>
      commit(allChecked ? values.filter((v) => !shown.includes(v)) : [...values, ...shown], includeEmpty),
    emptyItem: lower
      ? null
      : {
          value: "",
          label: labels.emptyValue,
          checked: includeEmpty,
          toggle: () => commit(values, !includeEmpty),
        },
    loading: loading && loadOptions !== undefined,
    active: set !== null,
    clear: () => onChange(null),
    done: onDone,
    labels,
  };
}
