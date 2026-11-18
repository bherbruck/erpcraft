// The reference renderer, for tests and the demo only. Every slot is bare,
// browser-default HTML: no classes, no stylesheet, no look. Popovers are
// native modal <dialog> elements. A design system writes its own slots from
// its own components; see the README's skinning checklist.

import { type ReactNode, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type {
  CellSlotProps,
  ChecklistFilterSlotProps,
  ColumnMenuSlotProps,
  ColumnsMenuSlotProps,
  ConditionFilterSlotProps,
  ContentProps,
  EditorSlotProps,
  EmptySlotProps,
  FilterChipsSlotProps,
  FilterPopoverSlotProps,
  FooterCellSlotProps,
  GridSlots,
  HeaderCellSlotProps,
  PagerSlotProps,
  PartSlotProps,
  ResizeHandleSlotProps,
  RowSlotProps,
  SearchBoxSlotProps,
  ToolbarSlotProps,
} from "../react/slots";
import type { Option } from "../types";

function Part({ props, children }: PartSlotProps) {
  return <div {...props}>{children}</div>;
}

function Row<R>({ props, children }: RowSlotProps<R>) {
  return <div {...props}>{children}</div>;
}

function HeaderCell<R>(p: HeaderCellSlotProps<R>) {
  return (
    <div {...p.props}>
      <button {...p.sortProps}>
        {p.label}
        {p.sort ? (p.sort.direction === "asc" ? " ▲" : " ▼") : null}
        {p.sort?.order != null ? <sup data-sort-order="">{p.sort.order}</sup> : null}
      </button>
      {p.filter}
      {p.menu}
      {p.resizeHandle}
    </div>
  );
}

function Cell<R>({ props, editor, text, column, row }: CellSlotProps<R>) {
  const currency = typeof column.currency === "function" ? column.currency(row) : column.currency;
  return (
    <div {...props}>
      {editor ?? (column.type === "money" && currency && text ? `${text} ${currency}` : text)}
    </div>
  );
}

function FooterCell<R>({ props, text }: FooterCellSlotProps<R>) {
  return <div {...props}>{text}</div>;
}

function ResizeHandle<R>({ props }: ResizeHandleSlotProps<R>) {
  // A hit area on the column's edge; it draws nothing.
  return <div {...props} style={{ ...props.style, width: 6 }} />;
}

function Editor<R>({ inputProps, options, loadOptions, draft, error }: EditorSlotProps<R>) {
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
      <input {...inputProps} list={choices.length ? listId : undefined} />
      {choices.length ? (
        <datalist id={listId}>
          {choices.map((o) => (
            <option key={o.value} value={o.label} />
          ))}
        </datalist>
      ) : null}
      {error ? <span role="alert">{error}</span> : null}
    </>
  );
}

/** A native modal dialog in a portal: Escape and a click on the backdrop close it. */
function Dialog({ contentProps, onClose, children }: { contentProps: ContentProps; onClose(): void; children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);
  return createPortal(
    // biome-ignore lint/a11y/useKeyWithClickEvents: Escape closes it through the cancel event
    <dialog
      ref={ref}
      {...contentProps}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      {children}
    </dialog>,
    document.body,
  );
}

function ColumnMenu<R>(p: ColumnMenuSlotProps<R>) {
  const { labels } = p;
  const act = (action: () => void) => () => {
    action();
    p.onOpenChange(false);
  };
  return (
    <>
      <button type="button" {...p.triggerProps} onClick={() => p.onOpenChange(!p.open)}>
        ⋮
      </button>
      {p.open ? (
        <Dialog contentProps={p.contentProps} onClose={() => p.onOpenChange(false)}>
          <div role="menu" aria-label={p.contentProps["aria-label"]}>
            {p.sortable ? (
              <>
                <button type="button" role="menuitem" onClick={act(p.sortAsc)}>
                  {labels.sortAsc}
                </button>
                <button type="button" role="menuitem" onClick={act(p.sortDesc)}>
                  {labels.sortDesc}
                </button>
                {p.sort ? (
                  <button type="button" role="menuitem" onClick={act(p.clearSort)}>
                    {labels.clearSort}
                  </button>
                ) : null}
              </>
            ) : null}
            {p.filterable ? (
              <button type="button" role="menuitem" onClick={p.openFilter}>
                {labels.editFilter}
              </button>
            ) : null}
            {p.filtered ? (
              <button type="button" role="menuitem" onClick={act(p.clearFilter)}>
                {labels.clearFilter}
              </button>
            ) : null}
            {p.pinned !== "left" ? (
              <button type="button" role="menuitem" onClick={act(() => p.pin("left"))}>
                {labels.pinLeft}
              </button>
            ) : null}
            {p.pinned !== "right" ? (
              <button type="button" role="menuitem" onClick={act(() => p.pin("right"))}>
                {labels.pinRight}
              </button>
            ) : null}
            {p.pinned ? (
              <button type="button" role="menuitem" onClick={act(() => p.pin(false))}>
                {labels.unpin}
              </button>
            ) : null}
            <button type="button" role="menuitem" disabled={!p.canMoveLeft} onClick={act(() => p.move(-1))}>
              {labels.moveLeft}
            </button>
            <button type="button" role="menuitem" disabled={!p.canMoveRight} onClick={act(() => p.move(1))}>
              {labels.moveRight}
            </button>
            <button type="button" role="menuitem" onClick={act(p.autoFit)}>
              {labels.autoFit}
            </button>
            {p.hideable ? (
              <button type="button" role="menuitem" onClick={act(p.hide)}>
                {labels.hide}
              </button>
            ) : null}
          </div>
        </Dialog>
      ) : null}
    </>
  );
}

function FilterPopover<R>(p: FilterPopoverSlotProps<R>) {
  return (
    <>
      <button type="button" {...p.triggerProps} onClick={() => p.onOpenChange(!p.open)}>
        {p.active ? "▼" : "▽"}
      </button>
      {p.open ? (
        <Dialog contentProps={p.contentProps} onClose={() => p.onOpenChange(false)}>
          {p.children}
        </Dialog>
      ) : null}
    </>
  );
}

function ConditionFilter<R>(p: ConditionFilterSlotProps<R>) {
  const { labels } = p;
  if (p.kind === "boolean") {
    return (
      <fieldset>
        <legend>{p.column.header}</legend>
        {p.ops.map((op) => (
          <label key={op.value}>
            <input
              type="radio"
              name={`filter-${p.column.id}`}
              checked={p.op === op.value}
              onChange={() => p.setOp(op.value)}
            />
            {op.label}
          </label>
        ))}
        {p.active ? (
          <button type="button" onClick={p.clear}>
            {labels.clearFilter}
          </button>
        ) : null}
      </fieldset>
    );
  }
  return (
    <fieldset>
      <legend>{p.column.header}</legend>
      <select aria-label={labels.condition} value={p.op} onChange={(event) => p.setOp(event.target.value)}>
        {p.ops.map((op) => (
          <option key={op.value} value={op.value}>
            {op.label}
          </option>
        ))}
      </select>
      {p.needsRange ? (
        <select aria-label={labels.range} value={p.range} onChange={(event) => p.setRange(event.target.value)}>
          <option value="" disabled>
            —
          </option>
          {p.ranges.map((r) => (
            <option key={r.value} value={r.value}>
              {r.label}
            </option>
          ))}
        </select>
      ) : null}
      {p.needsValue ? <input {...p.valueProps} /> : null}
      {p.needsTo ? <input {...p.toProps} /> : null}
      <button type="button" disabled={!p.valid} onClick={p.apply}>
        {labels.apply}
      </button>
      {p.active ? (
        <button type="button" onClick={p.clear}>
          {labels.clearFilter}
        </button>
      ) : null}
    </fieldset>
  );
}

function ChecklistFilter<R>(p: ChecklistFilterSlotProps<R>) {
  const { labels } = p;
  return (
    <fieldset>
      <legend>{p.column.header}</legend>
      <input {...p.searchProps} type="search" placeholder={labels.search} />
      {p.loading ? <p>{labels.loading}</p> : null}
      {p.items.length > 0 ? (
        <label>
          <input type="checkbox" checked={p.allChecked} onChange={p.toggleAll} />
          {labels.selectAll}
        </label>
      ) : null}
      {p.items.map((item) => (
        <label key={item.value}>
          <input type="checkbox" checked={item.checked} onChange={item.toggle} />
          {item.label}
        </label>
      ))}
      {p.emptyItem ? (
        <label>
          <input type="checkbox" checked={p.emptyItem.checked} onChange={p.emptyItem.toggle} />
          {p.emptyItem.label}
        </label>
      ) : null}
      {p.items.length === 0 && !p.loading ? <p>{labels.noOptions}</p> : null}
      <button type="button" onClick={p.done}>
        {labels.done}
      </button>
      {p.active ? (
        <button type="button" onClick={p.clear}>
          {labels.clearFilter}
        </button>
      ) : null}
    </fieldset>
  );
}

function Toolbar(p: ToolbarSlotProps) {
  return (
    <div {...p.props}>
      {p.search}
      {p.chips}
      <output>
        {p.shownRows === p.totalRows
          ? `${p.totalRows} ${p.labels.rows}`
          : `${p.shownRows} ${p.labels.of} ${p.totalRows} ${p.labels.rows}`}
      </output>
      {p.columns}
      {p.extra}
    </div>
  );
}

function SearchBox({ inputProps }: SearchBoxSlotProps) {
  return <input {...inputProps} />;
}

function FilterChips({ chips, clearAll, canClear, labels }: FilterChipsSlotProps) {
  return (
    <>
      {chips.length ? (
        <ul aria-label={labels.filters}>
          {chips.map((chip) => (
            <li key={chip.columnId}>
              <button type="button" onClick={chip.edit}>
                {chip.label}
              </button>
              <button type="button" aria-label={`${labels.clearFilter}: ${chip.label}`} onClick={chip.remove}>
                ×
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      {canClear ? (
        <button type="button" onClick={clearAll}>
          {labels.clearAll}
        </button>
      ) : null}
    </>
  );
}

function ColumnsMenu<R>({ columns, resetLayout, labels }: ColumnsMenuSlotProps<R>) {
  return (
    <details>
      <summary>{labels.columns}</summary>
      {columns.map(({ column, visible, hideable, toggle }) => (
        <label key={column.id}>
          <input type="checkbox" checked={visible} disabled={!hideable} onChange={toggle} />
          {column.header}
        </label>
      ))}
      <button type="button" onClick={resetLayout}>
        {labels.resetLayout}
      </button>
    </details>
  );
}

function Pager({ page, pageCount, total, setPage, canPrevious, canNext, labels }: PagerSlotProps) {
  return (
    <nav aria-label={labels.page}>
      <button type="button" onClick={() => setPage(page - 1)} disabled={!canPrevious}>
        {labels.previous}
      </button>
      {` ${labels.page} ${page + 1} ${labels.of} ${pageCount} (${total} ${labels.rows}) `}
      <button type="button" onClick={() => setPage(page + 1)} disabled={!canNext}>
        {labels.next}
      </button>
    </nav>
  );
}

function Empty({ filtered, loading, labels }: EmptySlotProps) {
  return <p>{loading ? labels.loading : filtered ? labels.emptyFiltered : labels.empty}</p>;
}

/** The bare slots. For tests and the demo; not a look to ship. */
export function referenceSlots<R>(): GridSlots<R> {
  return {
    Part,
    Row,
    HeaderCell,
    Cell,
    FooterCell,
    ResizeHandle,
    Editor,
    ColumnMenu,
    FilterPopover,
    ConditionFilter,
    ChecklistFilter,
    Toolbar,
    SearchBox,
    FilterChips,
    ColumnsMenu,
    Pager,
    Empty,
  };
}
