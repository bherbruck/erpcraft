// The Astryx skin of @erpcraft/grid: its 17 slots drawn with Astryx components, as the grid's
// README asks. The grid owns the behaviour (keys, focus, selection, clipboard, layout, virtual
// rows); this file only draws. State is read from the grid's data attributes in globals.css
// (`.ag-*` rules), never from class names.

import { Badge } from "@astryxdesign/core/Badge";
import { Button } from "@astryxdesign/core/Button";
import { CheckboxInput } from "@astryxdesign/core/CheckboxInput";
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuDivider, DropdownMenuItem } from "@astryxdesign/core/DropdownMenu";
import { EmptyState } from "@astryxdesign/core/EmptyState";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { IconButton } from "@astryxdesign/core/IconButton";
import { Link } from "@astryxdesign/core/Link";
import { Pagination } from "@astryxdesign/core/Pagination";
import { Popover } from "@astryxdesign/core/Popover";
import { RadioList, RadioListItem } from "@astryxdesign/core/RadioList";
import { Selector } from "@astryxdesign/core/Selector";
import { Spinner } from "@astryxdesign/core/Spinner";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import { type SearchableItem, type SearchSource, Typeahead } from "@astryxdesign/core/Typeahead";
import { VStack } from "@astryxdesign/core/VStack";
import type {
  CellSlotProps,
  ChecklistFilterSlotProps,
  ColumnMenuSlotProps,
  ColumnsMenuSlotProps,
  ConditionFilterSlotProps,
  EditorSlotProps,
  EmptySlotProps,
  FilterChipsSlotProps,
  FilterPopoverSlotProps,
  FooterCellSlotProps,
  GridSlots,
  Option,
  HeaderCellSlotProps,
  PagerSlotProps,
  PartSlotProps,
  ResizeHandleSlotProps,
  RowSlotProps,
  SearchBoxSlotProps,
  ToolbarSlotProps,
} from "@erpcraft/grid";
import { ArrowDown, ArrowUp, Columns3, Funnel, MoreVertical } from "lucide-react";
import { type KeyboardEvent, type ReactNode, useMemo, useRef, useState } from "react";

// ------------------------------------------------------------ structure

function Part({ kind, props, children, loading }: PartSlotProps) {
  return (
    <div {...props} className={`ag-${kind}`} aria-busy={kind === "scroller" && loading ? true : undefined}>
      {children}
    </div>
  );
}

function Row<R>({ props, children, kind }: RowSlotProps<R>) {
  return (
    <div {...props} className={`ag-row ag-row-${kind}`}>
      {children}
    </div>
  );
}

function HeaderCell<R>(p: HeaderCellSlotProps<R>) {
  return (
    <div {...p.props} className="ag-hcell">
      <button {...p.sortProps} className="ag-sort">
        <Text type="label" weight="semibold" maxLines={1} hasTruncateTooltip={false}>
          {p.label}
        </Text>
        {p.sort ? <Icon icon={p.sort.direction === "asc" ? ArrowUp : ArrowDown} size="xsm" color="secondary" /> : null}
        {p.sort?.order != null ? <Badge label={String(p.sort.order)} /> : null}
      </button>
      <span className="ag-hcell-tools">
        {p.filter}
        {p.menu}
      </span>
      {p.resizeHandle}
    </div>
  );
}

/** A column whose cells link to the row's record (a list's first column). */
export interface GridLink<R> {
  columnId: string;
  href: (row: R) => string | undefined;
}

/** A yes/no column drawn as a checkbox the mouse can tick (the keyboard types y/n, Delete clears). */
export interface GridCheck<R> {
  columnId: string;
  label: (row: R) => string;
  onToggle: (row: R, checked: boolean) => void;
}

/** Draws a cell's value as something richer than its text (a status badge); the text is still what is copied. */
export type GridDecorate<R> = (columnId: string, row: R, text: string) => ReactNode | undefined;

function makeCell<R>(link?: GridLink<R>, check?: GridCheck<R>, decorate?: GridDecorate<R>) {
  return function Cell({ props, editor, text, column, row, value }: CellSlotProps<R>) {
    if (check && column.id === check.columnId && !editor) {
      return (
        <div {...props} className="ag-cell ag-check">
          <CheckboxInput label={check.label(row)} isLabelHidden value={value === true} onChange={(on) => check.onToggle(row, on)} />
        </div>
      );
    }
    const href = link && column.id === link.columnId && !editor ? link.href(row) : undefined;
    const decorated = !editor && !href ? decorate?.(column.id, row, text) : undefined;
    if (decorated !== undefined) {
      return (
        <div {...props} className="ag-cell">
          {decorated}
        </div>
      );
    }
    return (
      <div {...props} className="ag-cell" data-link={href ? "" : undefined}>
        {editor ??
          (href ? (
            // Not a tab stop: the cell is. Enter on it opens the record (see ListGrid).
            <Link href={href} tabIndex={-1} className="ag-text">
              {text}
            </Link>
          ) : (
            <span className="ag-text">{text}</span>
          ))}
      </div>
    );
  };
}

function FooterCell<R>({ props, text }: FooterCellSlotProps<R>) {
  return (
    <div {...props} className="ag-cell ag-fcell">
      <Text weight="semibold" hasTabularNumbers>
        {text}
      </Text>
    </div>
  );
}

function ResizeHandle<R>({ props }: ResizeHandleSlotProps<R>) {
  return <div {...props} className="ag-resize" />;
}

// ------------------------------------------------------------ editing

interface Choice extends SearchableItem {}

/**
 * Enum, state and reference cells pick with Astryx's own pickers. They sit in a
 * `data-grid-menu` wrapper, so the grid leaves their keys alone (Enter chooses, arrows
 * move in the list), and they commit or cancel themselves.
 */
/** Choices that depend on the row being edited (a line of the order this line's header names). */
export interface GridRowPickers<R> {
  options: (columnId: string, row: R) => ((search: string, signal: AbortSignal) => Promise<Option[]>) | undefined;
  /** Why nothing can be chosen yet in this cell, or null. */
  blocked: (columnId: string, row: R) => string | null;
}

function PickerEditor<R>(p: EditorSlotProps<R> & { pickers?: GridRowPickers<R> }) {
  const [open, setOpen] = useState(false);
  // The grid hands new option arrays and functions on every render; the source reads the latest
  // through a ref, so Astryx's Typeahead keeps one source and its search in flight.
  const latest = useRef(p);
  latest.current = p;
  const source = useMemo<SearchSource<Choice>>(() => {
    let controller: AbortController | null = null;
    const run = async (q: string): Promise<Choice[]> => {
      const { options, pickers, column, row } = latest.current;
      const loadOptions = pickers?.options(column.id, row) ?? latest.current.loadOptions;
      if (!loadOptions) {
        const needle = q.trim().toLowerCase();
        return options.filter((o) => !needle || o.label.toLowerCase().includes(needle)).map((o) => ({ id: o.value, label: o.label }));
      }
      controller?.abort();
      controller = new AbortController();
      const found = await loadOptions(q, controller.signal);
      return found.map((o) => ({ id: o.value, label: o.label }));
    };
    return { search: run, bootstrap: () => run(""), cancel: () => controller?.abort() };
  }, []);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (open) return;
    if (event.key === "Escape") {
      event.preventDefault();
      p.cancel();
    } else if (event.key === "Tab") {
      event.preventDefault();
      p.cancel();
    }
  };
  const blocked = p.pickers?.blocked(p.column.id, p.row) ?? null;
  if (blocked) {
    return (
      // biome-ignore lint/a11y/noStaticElementInteractions: Escape and Tab close the note, as they would the editor
      <div data-grid-menu="" className="ag-editor ag-blocked" onKeyDown={onKeyDown} tabIndex={-1} ref={(element) => element?.focus()}>
        <Text type="supporting" maxLines={1}>
          {blocked}
        </Text>
      </div>
    );
  }
  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: closes the editor on Escape and Tab while the list is shut
    <div data-grid-menu="" className="ag-editor" onKeyDown={onKeyDown}>
      <Typeahead<Choice>
        label={p.inputProps["aria-label"]}
        isLabelHidden
        size="sm"
        width="100%"
        value={null}
        hasAutoFocus
        hasEntriesOnFocus
        minQueryLength={0}
        debounceMs={150}
        searchSource={source}
        onOpenChange={setOpen}
        onChange={(item) => {
          if (item) p.commit(item.id);
        }}
        status={p.error ? { type: "error", message: p.error } : undefined}
        statusVariant="tooltip"
      />
    </div>
  );
}

function makeEditor<R>(pickers?: GridRowPickers<R>) {
  return function Editor(p: EditorSlotProps<R>) {
    if (p.column.type === "enum" || p.column.type === "state" || p.column.type === "reference") return <PickerEditor {...p} pickers={pickers} />;
    return <TextEditor {...p} />;
  };
}

function TextEditor<R>(p: EditorSlotProps<R>) {
  const { ref, value, onChange, inputMode, ...rest } = p.inputProps;
  return (
    <div className={p.align === "end" ? "ag-editor num" : "ag-editor"}>
      <TextInput
        {...rest}
        ref={ref}
        label={p.inputProps["aria-label"]}
        isLabelHidden
        size="sm"
        width="100%"
        value={value}
        inputMode={inputMode}
        hasSpellCheck={false}
        autoComplete="off"
        onChange={(_, event) => onChange(event)}
        status={p.error ? { type: "error", message: p.error } : undefined}
        statusVariant="tooltip"
      />
    </div>
  );
}

// ------------------------------------------------------------ menus

function ColumnMenu<R>(p: ColumnMenuSlotProps<R>) {
  const { labels } = p;
  return (
    <DropdownMenu
      isMenuOpen={p.open}
      onOpenChange={p.onOpenChange}
      alignment="end"
      renderTrigger={(trigger) => (
        <IconButton {...trigger} {...p.triggerProps} label={p.triggerProps["aria-label"]} icon={<Icon icon={MoreVertical} />} variant="ghost" size="sm" />
      )}
    >
      {p.sortable ? <DropdownMenuItem label={labels.sortAsc} icon={ArrowUp} onClick={p.sortAsc} /> : null}
      {p.sortable ? <DropdownMenuItem label={labels.sortDesc} icon={ArrowDown} onClick={p.sortDesc} /> : null}
      {p.sortable && p.sort ? <DropdownMenuItem label={labels.clearSort} onClick={p.clearSort} /> : null}
      {p.filterable ? <DropdownMenuItem label={labels.editFilter} icon={Funnel} onClick={p.openFilter} /> : null}
      {p.filtered ? <DropdownMenuItem label={labels.clearFilter} onClick={p.clearFilter} /> : null}
      <DropdownMenuDivider />
      {p.pinned !== "left" ? <DropdownMenuItem label={labels.pinLeft} onClick={() => p.pin("left")} /> : null}
      {p.pinned !== "right" ? <DropdownMenuItem label={labels.pinRight} onClick={() => p.pin("right")} /> : null}
      {p.pinned ? <DropdownMenuItem label={labels.unpin} onClick={() => p.pin(false)} /> : null}
      <DropdownMenuItem label={labels.moveLeft} isDisabled={!p.canMoveLeft} onClick={() => p.move(-1)} />
      <DropdownMenuItem label={labels.moveRight} isDisabled={!p.canMoveRight} onClick={() => p.move(1)} />
      <DropdownMenuItem label={labels.autoFit} onClick={p.autoFit} />
      {p.hideable ? <DropdownMenuItem label={labels.hide} onClick={p.hide} /> : null}
    </DropdownMenu>
  );
}

function FilterPopover<R>(p: FilterPopoverSlotProps<R>) {
  return (
    <Popover
      isOpen={p.open}
      onOpenChange={p.onOpenChange}
      label={p.contentProps["aria-label"]}
      alignment="end"
      width={300}
      content={<div {...p.contentProps}>{p.children}</div>}
    >
      <IconButton
        {...p.triggerProps}
        label={p.triggerProps["aria-label"]}
        icon={<Icon icon={Funnel} color={p.active ? "accent" : "secondary"} />}
        variant="ghost"
        size="sm"
        onClick={() => p.onOpenChange(!p.open)}
      />
    </Popover>
  );
}

function ConditionFilter<R>(p: ConditionFilterSlotProps<R>) {
  const { labels } = p;
  if (p.kind === "boolean") {
    return (
      <VStack gap={2}>
        <RadioList label={p.column.header} value={p.op} onChange={p.setOp}>
          {p.ops.map((op) => (
            <RadioListItem key={op.value} value={op.value} label={op.label} />
          ))}
        </RadioList>
        {p.active ? <Button label={labels.clearFilter} size="sm" variant="ghost" onClick={p.clear} /> : null}
      </VStack>
    );
  }
  const input = (props: typeof p.valueProps) => (
    <TextInput
      label={props["aria-label"]}
      isLabelHidden
      size="sm"
      width="100%"
      value={props.value}
      inputMode={props.inputMode}
      placeholder={props.type === "date" ? "YYYY-MM-DD" : undefined}
      onChange={(_, event) => props.onChange(event)}
      onKeyDown={props.onKeyDown}
    />
  );
  return (
    <VStack gap={2}>
      <Text weight="semibold">{p.column.header}</Text>
      <Selector label={labels.condition} isLabelHidden size="sm" width="100%" value={p.op} onChange={p.setOp} options={p.ops} />
      {p.needsRange ? <Selector label={labels.range} isLabelHidden size="sm" width="100%" value={p.range} onChange={p.setRange} options={p.ranges} /> : null}
      {p.needsValue ? input(p.valueProps) : null}
      {p.needsTo ? input(p.toProps) : null}
      <HStack gap={2} hAlign="end">
        {p.active ? <Button label={labels.clearFilter} size="sm" variant="ghost" onClick={p.clear} /> : null}
        <Button label={labels.apply} size="sm" variant="primary" isDisabled={!p.valid} onClick={p.apply} />
      </HStack>
    </VStack>
  );
}

function ChecklistFilter<R>(p: ChecklistFilterSlotProps<R>) {
  const { labels } = p;
  return (
    <VStack gap={2}>
      <Text weight="semibold">{p.column.header}</Text>
      <TextInput
        label={p.searchProps["aria-label"]}
        isLabelHidden
        type="search"
        size="sm"
        width="100%"
        placeholder={labels.search}
        value={p.searchProps.value}
        onChange={(_, event) => p.searchProps.onChange(event)}
        onKeyDown={p.searchProps.onKeyDown}
      />
      {p.loading ? <Spinner size="sm" aria-label={labels.loading} /> : null}
      <div className="ag-checklist">
        {p.items.length > 0 ? (
          <CheckboxInput label={labels.selectAll} value={p.allChecked ? true : p.someChecked ? "indeterminate" : false} onChange={p.toggleAll} />
        ) : null}
        {p.items.map((item) => (
          <CheckboxInput key={item.value} label={item.label} value={item.checked} onChange={item.toggle} />
        ))}
        {p.emptyItem ? <CheckboxInput label={p.emptyItem.label} value={p.emptyItem.checked} onChange={p.emptyItem.toggle} /> : null}
        {p.items.length === 0 && !p.loading ? <Text type="supporting">{labels.noOptions}</Text> : null}
      </div>
      <HStack gap={2} hAlign="end">
        {p.active ? <Button label={labels.clearFilter} size="sm" variant="ghost" onClick={p.clear} /> : null}
        <Button label={labels.done} size="sm" variant="primary" onClick={p.done} />
      </HStack>
    </VStack>
  );
}

// ------------------------------------------------------------ around the grid

function Toolbar(p: ToolbarSlotProps) {
  return (
    <div {...p.props} className="ag-toolbar">
      {p.search}
      {p.chips}
      {/* Where the grid has no search of its own, the page's filter bar already counts the rows. */}
      <span className="push-end">
        {p.search ? (
          <Text type="supporting">
            {p.shownRows === p.totalRows ? `${p.totalRows} ${p.labels.rows}` : `${p.shownRows} ${p.labels.of} ${p.totalRows} ${p.labels.rows}`}
          </Text>
        ) : null}
      </span>
      {p.extra}
      {p.columns}
    </div>
  );
}

function SearchBox({ inputProps }: SearchBoxSlotProps) {
  return (
    <TextInput
      label={inputProps["aria-label"]}
      isLabelHidden
      type="search"
      size="sm"
      width={260}
      placeholder={inputProps.placeholder}
      value={inputProps.value}
      onChange={(_, event) => inputProps.onChange(event)}
    />
  );
}

function FilterChips({ chips, clearAll, canClear, labels }: FilterChipsSlotProps) {
  return (
    <HStack gap={1} wrap="wrap" vAlign="center">
      {chips.map((chip) => (
        <HStack key={chip.columnId} gap={0.5} vAlign="center">
          <Button label={chip.label} size="sm" variant="secondary" onClick={chip.edit} tooltip={chip.unsupported} />
          <IconButton label={`${labels.clearFilter}: ${chip.label}`} icon={<Icon icon="close" />} size="sm" variant="ghost" onClick={chip.remove} />
        </HStack>
      ))}
      {canClear ? <Button label={labels.clearAll} size="sm" variant="ghost" onClick={clearAll} /> : null}
    </HStack>
  );
}

function ColumnsMenu<R>({ columns, resetLayout, labels }: ColumnsMenuSlotProps<R>) {
  return (
    <DropdownMenu button={{ label: labels.columns, size: "sm", variant: "secondary", icon: <Icon icon={Columns3} /> }} alignment="end">
      {columns.map(({ column, visible, hideable, toggle }) => (
        <DropdownMenuCheckboxItem key={column.id} label={column.header} value={visible} isDisabled={!hideable} onChange={toggle} />
      ))}
      <DropdownMenuDivider />
      <DropdownMenuItem label={labels.resetLayout} onClick={resetLayout} />
    </DropdownMenu>
  );
}

function Pager({ page, pageCount, pageSize, total, setPage, labels }: PagerSlotProps) {
  if (pageCount <= 1) return null;
  return (
    <div className="ag-pager">
      <Pagination page={page + 1} onChange={(next) => setPage(next - 1)} totalItems={total} pageSize={pageSize} size="sm" label={labels.page} />
    </div>
  );
}

function Empty({ filtered, loading, labels }: EmptySlotProps) {
  if (loading) return <Spinner label={labels.loading} />;
  return <EmptyState title={filtered ? labels.emptyFiltered : labels.empty} isCompact />;
}

/** The Astryx skin of the grid. */
export function astryxGridSlots<R>(
  options: { link?: GridLink<R>; check?: GridCheck<R>; decorate?: GridDecorate<R>; pickers?: GridRowPickers<R> } = {},
): GridSlots<R> {
  return {
    Part,
    Row,
    HeaderCell,
    Cell: makeCell(options.link, options.check, options.decorate),
    FooterCell,
    ResizeHandle,
    Editor: makeEditor(options.pickers),
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
