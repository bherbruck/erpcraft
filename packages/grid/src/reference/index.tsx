// The plain reference renderer: every slot drawn with unstyled HTML, so the
// grid's behaviour can be tried and tested without a design system. A
// design system writes its own versions of these; each is small.

import { type KeyboardEvent, type ReactNode, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { DATE_OPS, NUMBER_OPS, RELATIVE_RANGES, TEXT_OPS } from "../core/filters";
import type {
  CellSlotProps,
  ColumnMenuSlotProps,
  EditorSlotProps,
  EmptySlotProps,
  FilterEditorSlotProps,
  FooterCellSlotProps,
  GridClassNames,
  GridSlots,
  HeaderSlotProps,
  PagerSlotProps,
  ToolbarSlotProps,
} from "../react/slots";
import type { DateOp, FilterCondition, NumberOp, Option, RelativeRange, TextOp } from "../types";

export function ReferenceCell<Row>({ column, row, text, align }: CellSlotProps<Row>) {
  const currency = typeof column.currency === "function" ? column.currency(row) : column.currency;
  return (
    <span className="rg-cell-text" data-align={align}>
      {text}
      {column.type === "money" && currency && text ? <span className="rg-currency"> {currency}</span> : null}
    </span>
  );
}

export function ReferenceEditor<Row>({
  column,
  draft,
  setDraft,
  inputRef,
  options,
  loadOptions,
  align,
  error,
}: EditorSlotProps<Row>) {
  const listId = useId();
  const [found, setFound] = useState<Option[]>(options);
  useEffect(() => {
    if (!loadOptions) return undefined;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      loadOptions(draft, controller.signal)
        .then(setFound)
        .catch(() => undefined);
    }, 150);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [draft, loadOptions]);
  const choices = loadOptions ? found : options;
  return (
    <>
      <input
        ref={inputRef}
        className="rg-editor"
        data-align={align}
        aria-label={column.header}
        aria-invalid={error ? true : undefined}
        title={error ?? undefined}
        value={draft}
        list={choices.length ? listId : undefined}
        onChange={(event) => setDraft(event.target.value)}
      />
      {choices.length ? (
        <datalist id={listId}>
          {choices.map((o) => (
            <option key={o.value} value={o.label} />
          ))}
        </datalist>
      ) : null}
      {error ? (
        <span className="rg-error" role="alert">
          {error}
        </span>
      ) : null}
    </>
  );
}

export function ReferenceHeader<Row>({ column, sort, sortable, onSort, filtered, align, menu }: HeaderSlotProps<Row>) {
  return (
    <div className="rg-header" data-align={align}>
      <button
        type="button"
        tabIndex={-1}
        className="rg-header-label"
        disabled={!sortable}
        onClick={(event) => onSort(event.shiftKey)}
        title={column.header}
      >
        <span className="rg-header-text">{column.header}</span>
        {sort ? (
          <span className="rg-sort" aria-hidden="true">
            {sort.direction === "asc" ? "▲" : "▼"}
            {sort.order !== null ? <sup className="rg-sort-order">{sort.order}</sup> : null}
          </span>
        ) : null}
        {filtered ? (
          <span className="rg-filtered" aria-hidden="true">
            ⧩
          </span>
        ) : null}
      </button>
      {menu}
    </div>
  );
}

/** A panel in a portal, placed under its trigger, closed by Escape or a click outside. */
function Popover({
  open,
  onClose,
  anchor,
  children,
  label,
}: {
  open: boolean;
  onClose(): void;
  anchor: HTMLElement | null;
  children: ReactNode;
  label: string;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ top: number; left: number } | null>(null);
  useLayoutEffect(() => {
    if (!open || !anchor) return;
    const rect = anchor.getBoundingClientRect();
    const width = 280;
    setPosition({ top: rect.bottom + 4, left: Math.max(8, Math.min(rect.right - width, window.innerWidth - width - 8)) });
  }, [open, anchor]);
  useEffect(() => {
    if (!open) return undefined;
    const first = panel.current?.querySelector<HTMLElement>("input, select, button, [tabindex='0']");
    first?.focus();
    const outside = (event: MouseEvent) => {
      const target = event.target as Node;
      if (panel.current?.contains(target) || anchor?.contains(target)) return;
      onClose();
    };
    document.addEventListener("mousedown", outside);
    return () => document.removeEventListener("mousedown", outside);
  }, [open, anchor, onClose]);
  if (!open || !position) return null;
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onClose();
    }
  };
  return createPortal(
    // biome-ignore lint/a11y/noStaticElementInteractions: Escape closes the dialog
    <div
      ref={panel}
      role="dialog"
      aria-label={label}
      data-grid-menu=""
      className="rg-popover"
      style={{ position: "fixed", top: position.top, left: position.left, width: 280, zIndex: 1000 }}
      onKeyDown={onKeyDown}
    >
      {children}
    </div>,
    document.body,
  );
}

export function ReferenceColumnMenu<Row>(props: ColumnMenuSlotProps<Row>) {
  const { column, open, onOpenChange, labels, filter } = props;
  const trigger = useRef<HTMLButtonElement>(null);
  const close = () => onOpenChange(false);
  return (
    <>
      <button
        ref={trigger}
        type="button"
        tabIndex={-1}
        className="rg-menu-button"
        aria-label={`${labels.columnMenu}: ${column.header}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => onOpenChange(!open)}
      >
        ⋮
      </button>
      <Popover open={open} onClose={close} anchor={trigger.current} label={`${labels.columnMenu}: ${column.header}`}>
        <div className="rg-menu">
          {props.sortable ? (
            <div className="rg-menu-group">
              <button type="button" onClick={props.sortAsc} aria-pressed={props.sort?.direction === "asc"}>
                {labels.sortAsc}
              </button>
              <button type="button" onClick={props.sortDesc} aria-pressed={props.sort?.direction === "desc"}>
                {labels.sortDesc}
              </button>
              {props.sort ? (
                <button type="button" onClick={props.clearSort}>
                  {labels.clearSort}
                </button>
              ) : null}
            </div>
          ) : null}
          {filter ? (
            <fieldset className="rg-menu-group rg-filter">
              <legend>{labels.filter}</legend>
              {filter}
            </fieldset>
          ) : null}
          <div className="rg-menu-group">
            {props.pinned !== "left" ? (
              <button type="button" onClick={() => props.pin("left")}>
                {labels.pinLeft}
              </button>
            ) : null}
            {props.pinned !== "right" ? (
              <button type="button" onClick={() => props.pin("right")}>
                {labels.pinRight}
              </button>
            ) : null}
            {props.pinned ? (
              <button type="button" onClick={() => props.pin(false)}>
                {labels.unpin}
              </button>
            ) : null}
            <button type="button" onClick={() => props.move(-1)} disabled={!props.canMoveLeft}>
              {labels.moveLeft}
            </button>
            <button type="button" onClick={() => props.move(1)} disabled={!props.canMoveRight}>
              {labels.moveRight}
            </button>
            <button type="button" onClick={props.autoFit}>
              {labels.autoFit}
            </button>
            {props.hideable ? (
              <button type="button" onClick={props.hide}>
                {labels.hide}
              </button>
            ) : null}
          </div>
        </div>
      </Popover>
    </>
  );
}

function OpSelect<T extends string>({
  value,
  ops,
  onChange,
  labels,
}: {
  value: T;
  ops: readonly T[];
  onChange(op: T): void;
  labels: Record<string, string>;
}) {
  return (
    <select aria-label="Condition" value={value} onChange={(event) => onChange(event.target.value as T)}>
      {ops.map((op) => (
        <option key={op} value={op}>
          {labels[op] ?? op}
        </option>
      ))}
    </select>
  );
}

/** The typed filter form: a condition and value(s), or a checklist with search. */
export function ReferenceFilterEditor<Row>(props: FilterEditorSlotProps<Row>) {
  const { condition, onChange, onClear, onDone, labels, active } = props;
  const [draft, setDraft] = useState<FilterCondition>(condition);
  // A filter changed elsewhere (a chip removed) resets the form.
  const key = JSON.stringify(condition);
  // biome-ignore lint/correctness/useExhaustiveDependencies: the key stands for the condition
  useEffect(() => setDraft(condition), [key]);

  const apply = (next: FilterCondition = draft) => {
    onChange(next);
  };
  const onEnter = (event: KeyboardEvent) => {
    if (event.key === "Enter") {
      event.preventDefault();
      apply();
      onDone();
    }
  };
  const actions = (
    <div className="rg-filter-actions">
      {draft.kind !== "set" && draft.kind !== "boolean" ? (
        <button
          type="button"
          onClick={() => {
            apply();
            onDone();
          }}
        >
          {labels.apply}
        </button>
      ) : (
        <button type="button" onClick={onDone}>
          {labels.done}
        </button>
      )}
      {active ? (
        <button type="button" onClick={onClear}>
          {labels.clearFilter}
        </button>
      ) : null}
    </div>
  );

  switch (draft.kind) {
    case "text":
      return (
        <div className="rg-filter-form">
          <OpSelect<TextOp> value={draft.op} ops={TEXT_OPS} labels={labels} onChange={(op) => setDraft({ ...draft, op })} />
          {draft.op !== "empty" && draft.op !== "notEmpty" ? (
            <input
              aria-label={labels.value}
              value={draft.value ?? ""}
              onChange={(event) => setDraft({ ...draft, value: event.target.value })}
              onKeyDown={onEnter}
            />
          ) : null}
          {actions}
        </div>
      );
    case "number":
      return (
        <div className="rg-filter-form">
          <OpSelect<NumberOp>
            value={draft.op}
            ops={NUMBER_OPS}
            labels={labels}
            onChange={(op) => setDraft({ ...draft, op })}
          />
          {draft.op !== "empty" && draft.op !== "notEmpty" ? (
            <input
              aria-label={draft.op === "between" ? labels.from : labels.value}
              inputMode="decimal"
              value={draft.value ?? ""}
              onChange={(event) => setDraft({ ...draft, value: event.target.value })}
              onKeyDown={onEnter}
            />
          ) : null}
          {draft.op === "between" ? (
            <input
              aria-label={labels.to}
              inputMode="decimal"
              value={draft.to ?? ""}
              onChange={(event) => setDraft({ ...draft, to: event.target.value })}
              onKeyDown={onEnter}
            />
          ) : null}
          {actions}
        </div>
      );
    case "date":
      return (
        <div className="rg-filter-form">
          <OpSelect<DateOp> value={draft.op} ops={DATE_OPS} labels={labels} onChange={(op) => setDraft({ ...draft, op })} />
          {draft.op === "relative" ? (
            <OpSelect<RelativeRange>
              value={draft.range ?? "thisMonth"}
              ops={RELATIVE_RANGES}
              labels={labels}
              onChange={(range) => setDraft({ ...draft, range })}
            />
          ) : null}
          {["on", "before", "after", "between"].includes(draft.op) ? (
            <input
              type="date"
              aria-label={draft.op === "between" ? labels.from : labels.value}
              value={draft.value ?? ""}
              onChange={(event) => setDraft({ ...draft, value: event.target.value })}
              onKeyDown={onEnter}
            />
          ) : null}
          {draft.op === "between" ? (
            <input
              type="date"
              aria-label={labels.to}
              value={draft.to ?? ""}
              onChange={(event) => setDraft({ ...draft, to: event.target.value })}
              onKeyDown={onEnter}
            />
          ) : null}
          {actions}
        </div>
      );
    case "boolean":
      return (
        <div className="rg-filter-form" role="radiogroup" aria-label={labels.filter}>
          {(
            [
              [true, labels.true],
              [false, labels.false],
              [null, labels.emptyValue],
            ] as const
          ).map(([value, label]) => (
            <label key={String(value)}>
              <input
                type="radio"
                name={`rg-bool-${props.column.id}`}
                checked={active && draft.value === value}
                onChange={() => {
                  const next = { kind: "boolean", value } as const;
                  setDraft(next);
                  apply(next);
                }}
              />
              {label}
            </label>
          ))}
          {actions}
        </div>
      );
    case "set":
      return <Checklist {...props} draft={draft} setDraft={setDraft} actions={actions} />;
  }
}

function Checklist<Row>({
  column,
  draft,
  setDraft,
  onChange,
  options: given,
  loadOptions,
  rememberLabels,
  labels,
  active,
  actions,
}: FilterEditorSlotProps<Row> & {
  draft: Extract<FilterCondition, { kind: "set" }>;
  setDraft(next: FilterCondition): void;
  actions: ReactNode;
}) {
  const [search, setSearch] = useState("");
  const [loaded, setLoaded] = useState<Option[] | null>(null);
  const [busy, setBusy] = useState(false);
  const remembered = useRef(new Map<string, string>());
  useEffect(() => {
    if (!loadOptions) return undefined;
    const controller = new AbortController();
    setBusy(true);
    const timer = setTimeout(() => {
      loadOptions(search, controller.signal)
        .then((found) => {
          setLoaded(found);
          rememberLabels(found);
          for (const o of found) remembered.current.set(o.value, o.label);
        })
        .catch(() => undefined)
        .finally(() => setBusy(false));
    }, 200);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [search, loadOptions, rememberLabels]);

  const all = loadOptions ? (loaded ?? []) : given;
  const lower = search.trim().toLocaleLowerCase();
  const visible = loadOptions ? all : all.filter((o) => o.label.toLocaleLowerCase().includes(lower));
  // An enum without a filter shows everything ticked, as Excel's checklist does.
  const everything = !loadOptions && !active;
  const checked = (value: string) => (everything ? true : draft.values.includes(value));
  // Chosen references that the current search does not list stay visible.
  const chosenElsewhere = loadOptions
    ? draft.values.filter((v) => !visible.some((o) => o.value === v)).map((v) => ({ value: v, label: remembered.current.get(v) ?? v }))
    : [];

  const set = (values: string[], includeEmpty = draft.includeEmpty) => {
    const unique = [...new Set(values)];
    // Everything ticked is no filter at all.
    const isAll = !loadOptions && unique.length === all.length && includeEmpty;
    const next: FilterCondition = isAll ? { kind: "set", values: [] } : { kind: "set", values: unique, includeEmpty };
    setDraft(next);
    onChange(next);
  };
  const toggle = (value: string) => {
    const current = everything ? all.map((o) => o.value) : draft.values;
    set(current.includes(value) ? current.filter((v) => v !== value) : [...current, value], everything ? true : draft.includeEmpty);
  };
  const allVisibleChecked = visible.length > 0 && visible.every((o) => checked(o.value));

  return (
    <div className="rg-filter-form rg-checklist">
      <input
        type="search"
        aria-label={labels.search}
        placeholder={labels.search}
        value={search}
        onChange={(event) => setSearch(event.target.value)}
      />
      {busy && loadOptions ? <div className="rg-muted">{labels.loading}</div> : null}
      <div className="rg-checklist-items" role="group" aria-label={column.header}>
        {visible.length > 0 ? (
          <label className="rg-check-all">
            <input
              type="checkbox"
              checked={allVisibleChecked}
              onChange={() => {
                const current = everything ? all.map((o) => o.value) : draft.values;
                const values = visible.map((o) => o.value);
                set(
                  allVisibleChecked ? current.filter((v) => !values.includes(v)) : [...current, ...values],
                  everything ? true : draft.includeEmpty,
                );
              }}
            />
            {labels.selectAll}
          </label>
        ) : null}
        {[...chosenElsewhere, ...visible].map((o) => (
          <label key={o.value}>
            <input type="checkbox" checked={checked(o.value)} onChange={() => toggle(o.value)} />
            {o.label}
          </label>
        ))}
        {!lower ? (
          <label>
            <input
              type="checkbox"
              checked={everything || draft.includeEmpty === true}
              onChange={() =>
                set(everything ? all.map((o) => o.value) : draft.values, everything ? false : !draft.includeEmpty)
              }
            />
            {labels.emptyValue}
          </label>
        ) : null}
        {visible.length === 0 && !busy ? <div className="rg-muted">{labels.noOptions}</div> : null}
      </div>
      {actions}
    </div>
  );
}

export function ReferenceToolbar<Row>(props: ToolbarSlotProps<Row>) {
  const { labels, chips } = props;
  const [columnsOpen, setColumnsOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  return (
    <div className="rg-toolbar">
      {props.searchable ? (
        <input
          type="search"
          className="rg-search"
          aria-label={labels.search}
          placeholder={labels.search}
          value={props.search}
          onChange={(event) => props.setSearch(event.target.value)}
        />
      ) : null}
      <ul className="rg-chips" aria-label={labels.filter}>
        {chips.map((chip) => (
          <li key={chip.columnId} className="rg-chip">
            <button type="button" className="rg-chip-label" onClick={chip.edit}>
              {chip.label}
            </button>
            <button type="button" className="rg-chip-remove" aria-label={`${labels.clearFilter}: ${chip.label}`} onClick={chip.remove}>
              ×
            </button>
          </li>
        ))}
      </ul>
      {chips.length > 0 || props.search ? (
        <button type="button" onClick={props.clearAll}>
          {labels.clearAll}
        </button>
      ) : null}
      <span className="rg-count" aria-live="polite">
        {props.shownRows === props.totalRows
          ? `${props.totalRows} ${labels.rows}`
          : `${props.shownRows} ${labels.of} ${props.totalRows} ${labels.rows}`}
      </span>
      <button
        ref={trigger}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={columnsOpen}
        onClick={() => setColumnsOpen((o) => !o)}
      >
        {labels.columns}
      </button>
      <Popover open={columnsOpen} onClose={() => setColumnsOpen(false)} anchor={trigger.current} label={labels.columns}>
        <div className="rg-menu">
          {props.columns.map(({ column, visible, hideable, toggle }) => (
            <label key={column.id}>
              <input type="checkbox" checked={visible} disabled={!hideable} onChange={toggle} />
              {column.header}
            </label>
          ))}
          <button type="button" onClick={props.resetLayout}>
            {labels.resetLayout}
          </button>
        </div>
      </Popover>
    </div>
  );
}

export function ReferencePager({ page, pageCount, total, setPage, labels }: PagerSlotProps) {
  return (
    <nav className="rg-pager" aria-label={labels.page}>
      <button type="button" onClick={() => setPage(page - 1)} disabled={page <= 0}>
        {labels.previous}
      </button>
      <span>
        {labels.page} {page + 1} {labels.of} {pageCount} ({total} {labels.rows})
      </span>
      <button type="button" onClick={() => setPage(page + 1)} disabled={page >= pageCount - 1}>
        {labels.next}
      </button>
    </nav>
  );
}

export function ReferenceFooterCell<Row>({ text, align }: FooterCellSlotProps<Row>) {
  return (
    <span className="rg-cell-text" data-align={align}>
      {text}
    </span>
  );
}

export function ReferenceEmpty({ filtered, loading, labels }: EmptySlotProps) {
  return <div className="rg-empty">{loading ? labels.loading : filtered ? labels.emptyFiltered : labels.empty}</div>;
}

export function referenceSlots<Row>(): GridSlots<Row> {
  return {
    Cell: ReferenceCell,
    Editor: ReferenceEditor,
    Header: ReferenceHeader,
    ColumnMenu: ReferenceColumnMenu,
    FilterEditor: ReferenceFilterEditor,
    Toolbar: ReferenceToolbar,
    Pager: ReferencePager,
    FooterCell: ReferenceFooterCell,
    Empty: ReferenceEmpty,
  };
}

export const referenceClassNames: GridClassNames = {
  root: "rg-root",
  scroller: "rg-scroller",
  header: "rg-head",
  headerRow: "rg-head-row",
  headerCell: "rg-th",
  resizeHandle: "rg-resize",
  body: "rg-body",
  row: "rg-row",
  cell: "rg-td",
  footer: "rg-foot",
  footerCell: "rg-tf",
  live: "rg-live",
};
