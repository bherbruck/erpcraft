// The shadcn/ui skin of @erpcraft/grid: its 17 slots drawn with this app's shadcn components and
// table classes, as the grid's README asks (packages/grid/README.md, "Skinning checklist"). The
// grid owns the behaviour (keys, focus, selection, clipboard, layout, virtual rows); this file
// only draws. State comes from the grid's data attributes (Tailwind `data-*` variants), never
// from class names. Ported from ui-astryx/src/components/grid-skin.tsx.
//
// The grid lays rows out with flex and absolute positions, so the parts are divs carrying the
// classes of shadcn's Table (TableHead, TableRow, TableCell), not table elements.

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
  HeaderCellSlotProps,
  Option,
  PagerSlotProps,
  PartSlotProps,
  ResizeHandleSlotProps,
  RowSlotProps,
  SearchBoxSlotProps,
  ToolbarSlotProps,
} from "@erpcraft/grid";
import { ArrowDown, ArrowUp, ChevronLeft, ChevronRight, Columns3, EllipsisVertical, Funnel, X } from "lucide-react";
import { type KeyboardEvent, type ReactNode, useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { cn } from "@/lib/utils";
import { Button } from "./button";
import { Badge } from "./ui/badge";
import { Checkbox } from "./ui/checkbox";
import { Command, CommandEmpty, CommandInput, CommandItem, CommandList } from "./ui/command";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "./ui/dropdown-menu";
import { Empty as EmptyBox, EmptyHeader, EmptyTitle } from "./ui/empty";
import { Input } from "./ui/input";
import { NativeSelect, NativeSelectOption } from "./ui/native-select";
import { Popover, PopoverAnchor, PopoverContent, PopoverTrigger } from "./ui/popover";
import { RadioGroup, RadioGroupItem } from "./ui/radio-group";
import { Spinner } from "./ui/spinner";

// ------------------------------------------------------------ structure

const PART: Record<PartSlotProps["kind"], string> = {
  // `group/grid`: the selection and the active cell show while the grid has the focus.
  root: "group/grid flex min-h-0 flex-col gap-2",
  scroller:
    "rounded-md border bg-card text-sm text-card-foreground focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-1",
  header: "bg-card",
  body: "",
  footer: "bg-muted font-medium",
};

function Part({ kind, props, children }: PartSlotProps) {
  return (
    <div {...props} className={PART[kind]}>
      {children}
    </div>
  );
}

const ROW: Record<RowSlotProps<unknown>["kind"], string> = {
  header: "border-b",
  body: "border-b transition-colors hover:bg-muted/50",
  footer: "border-t",
};

function Row<R>({ props, children, kind }: RowSlotProps<R>) {
  return (
    <div {...props} className={ROW[kind]}>
      {children}
    </div>
  );
}

/** A cell's own background, so pinned cells cover what scrolls under them. */
const CELL_BASE = "flex min-w-0 items-center gap-1 bg-card px-2 outline-none data-pinned:z-[1]";

function HeaderCell<R>(p: HeaderCellSlotProps<R>) {
  return (
    <div
      {...p.props}
      className={cn(
        CELL_BASE,
        "group/hcell relative h-10 font-medium whitespace-nowrap text-foreground",
        "data-[align=end]:flex-row-reverse data-dragging:opacity-60",
        "data-[drop=before]:shadow-[inset_2px_0_0_var(--ring)] data-[drop=after]:shadow-[inset_-2px_0_0_var(--ring)]",
        "group-focus-within/grid:data-active:shadow-[inset_0_0_0_2px_var(--ring)]",
      )}
    >
      <button
        {...p.sortProps}
        className={cn(
          "inline-flex min-w-0 flex-auto items-center gap-1 text-start disabled:cursor-default",
          p.align === "end" && "justify-end",
          p.sortable && "cursor-pointer",
        )}
      >
        <span className="truncate">{p.label}</span>
        {p.sort ? (
          p.sort.direction === "asc" ? (
            <ArrowUp aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
          ) : (
            <ArrowDown aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
          )
        ) : null}
        {p.sort?.order != null ? (
          <Badge variant="secondary" className="h-4 min-w-4 px-1 text-[0.625rem] tabular-nums">
            {p.sort.order}
          </Badge>
        ) : null}
      </button>
      {/* The column's tools show on hover or focus, and stay while a filter is set. */}
      <span
        className={cn(
          "absolute flex bg-card opacity-0 group-hover/hcell:opacity-100 group-focus-within/hcell:opacity-100",
          "group-data-active/hcell:opacity-100 group-data-filtered/hcell:opacity-100",
          p.align === "end" ? "start-1" : "end-1",
        )}
      >
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

const CELL = cn(
  CELL_BASE,
  "whitespace-nowrap",
  "data-[align=end]:justify-end data-[align=end]:tabular-nums data-editable:cursor-cell data-editing:px-0",
  "group-focus-within/grid:data-selected:bg-accent group-focus-within/grid:data-selected:text-accent-foreground",
  "group-focus-within/grid:data-active:shadow-[inset_0_0_0_2px_var(--ring)]",
  "aria-invalid:shadow-[inset_0_0_0_2px_var(--destructive)]",
);

/** The server's message for a cell it refused (a line's field), or undefined. */
export type GridProblem<R> = (columnId: string, row: R) => string | undefined;

function makeCell<R>(link?: GridLink<R>, check?: GridCheck<R>, decorate?: GridDecorate<R>, problem?: GridProblem<R>) {
  // biome-ignore lint/correctness/noComponentHookFactories: made once per grid (its slots are memoised), so its identity is stable
  return function Cell({ props, editor, text, column, row, value }: CellSlotProps<R>) {
    if (check && column.id === check.columnId && !editor) {
      return (
        <div {...props} className={cn(CELL, "justify-center")}>
          <Checkbox
            aria-label={check.label(row)}
            checked={value === true}
            tabIndex={-1}
            onCheckedChange={(on) => check.onToggle(row, on === true)}
          />
        </div>
      );
    }
    const href = link && column.id === link.columnId && !editor ? link.href(row) : undefined;
    const decorated = !editor && !href ? decorate?.(column.id, row, text) : undefined;
    if (decorated !== undefined) {
      return (
        <div {...props} className={CELL}>
          {decorated}
        </div>
      );
    }
    // A cell the server refused is marked, and says why on hover; the list above the lines says it too.
    const refused = editor ? undefined : problem?.(column.id, row);
    return (
      <div
        {...props}
        {...(refused ? { "aria-invalid": true, title: refused } : {})}
        className={CELL}
        data-link={href ? "" : undefined}
      >
        {editor ??
          (href ? (
            // Not a tab stop: the cell is. Enter on it opens the record (see ListGrid).
            <Link to={href} tabIndex={-1} className="truncate underline-offset-2 hover:underline">
              {text}
            </Link>
          ) : (
            <span className="truncate">{text}</span>
          ))}
      </div>
    );
  };
}

function FooterCell<R>({ props, text }: FooterCellSlotProps<R>) {
  return (
    <div {...props} className={cn(CELL_BASE, "bg-muted whitespace-nowrap tabular-nums data-[align=end]:justify-end")}>
      {text}
    </div>
  );
}

function ResizeHandle<R>({ props }: ResizeHandleSlotProps<R>) {
  return <div {...props} className="w-1.5 hover:bg-ring data-resizing:bg-ring" />;
}

// ------------------------------------------------------------ editing

/** Choices that depend on the row being edited (a line of the order this line's header names). */
export interface GridRowPickers<R> {
  options: (columnId: string, row: R) => ((search: string, signal: AbortSignal) => Promise<Option[]>) | undefined;
  /** Why nothing can be chosen yet in this cell, or null. */
  blocked: (columnId: string, row: R) => string | null;
}

/**
 * Enum, state and reference cells pick from a Command list in a popover over the cell. The
 * popover is a `data-grid-menu`, so the grid leaves its keys alone (Enter chooses, arrows move in
 * the list); it commits or cancels itself.
 */
function PickerEditor<R>(p: EditorSlotProps<R> & { pickers?: GridRowPickers<R> }) {
  // Typed into the cell, the first letters are the search.
  const [search, setSearch] = useState(p.mode === "enter" ? p.draft : "");
  const [found, setFound] = useState<Option[]>([]);
  const [loading, setLoading] = useState(false);
  // The highlighted choice: the first of each new list, so Enter takes the first match even when
  // the list arrives after the letters were typed (cmdk highlights nothing in a list that loads late).
  const [active, setActive] = useState("");
  useEffect(() => setActive(found[0]?.value ?? ""), [found]);
  const latest = useRef(p);
  latest.current = p;
  const blocked = p.pickers?.blocked(p.column.id, p.row) ?? null;

  useEffect(() => {
    if (blocked) return undefined;
    const { options, pickers, column, row } = latest.current;
    const load = pickers?.options(column.id, row) ?? latest.current.loadOptions;
    if (!load) {
      const needle = search.trim().toLowerCase();
      setFound(options.filter((o) => !needle || o.label.toLowerCase().includes(needle)));
      return undefined;
    }
    const controller = new AbortController();
    setLoading(true);
    const timer = setTimeout(() => {
      load(search, controller.signal)
        .then(setFound)
        .catch(() => undefined)
        .finally(() => {
          if (!controller.signal.aborted) setLoading(false);
        });
    }, 150);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [search, blocked]);

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape" || event.key === "Tab") {
      event.preventDefault();
      p.cancel();
    }
  };

  return (
    <Popover open onOpenChange={(open) => (open ? undefined : p.cancel())}>
      <PopoverAnchor asChild>
        <span className="block h-full w-full" />
      </PopoverAnchor>
      <PopoverContent data-grid-menu="" align="start" className="w-72 gap-0 p-0" onKeyDown={onKeyDown}>
        {blocked ? (
          // biome-ignore lint/a11y/noNoninteractiveTabindex: holds the focus so Escape and Tab close the note
          <p tabIndex={0} ref={(element) => element?.focus()} className="p-3 text-sm text-muted-foreground">
            {blocked}
          </p>
        ) : (
          <Command shouldFilter={false} value={active} onValueChange={setActive}>
            <CommandInput
              autoFocus
              aria-label={p.inputProps["aria-label"]}
              value={search}
              onValueChange={setSearch}
              placeholder={p.inputProps["aria-label"]}
            />
            <CommandList>
              {loading ? (
                <div className="flex justify-center p-3">
                  <Spinner />
                </div>
              ) : null}
              {found.map((option) => (
                <CommandItem key={option.value} value={option.value} onSelect={() => p.commit(option.value)}>
                  {option.label}
                </CommandItem>
              ))}
              {!loading ? <CommandEmpty>{p.error ?? p.inputProps["aria-label"]}</CommandEmpty> : null}
            </CommandList>
          </Command>
        )}
      </PopoverContent>
    </Popover>
  );
}

function TextEditor<R>(p: EditorSlotProps<R>) {
  return (
    <Input
      {...p.inputProps}
      autoComplete="off"
      spellCheck={false}
      title={p.error ?? undefined}
      className={cn(
        "h-full rounded-none border-0 px-2 shadow-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
        p.align === "end" && "text-end tabular-nums",
        p.error && "ring-2 ring-destructive ring-inset",
      )}
    />
  );
}

function makeEditor<R>(pickers?: GridRowPickers<R>) {
  // biome-ignore lint/correctness/noComponentHookFactories: made once per grid (its slots are memoised), so its identity is stable
  return function Editor(p: EditorSlotProps<R>) {
    if (p.column.type === "enum" || p.column.type === "state" || p.column.type === "reference") {
      return <PickerEditor {...p} pickers={pickers} />;
    }
    return <TextEditor {...p} />;
  };
}

// ------------------------------------------------------------ menus

function ColumnMenu<R>(p: ColumnMenuSlotProps<R>) {
  const { labels } = p;
  return (
    <DropdownMenu open={p.open} onOpenChange={p.onOpenChange}>
      <DropdownMenuTrigger asChild>
        <Button {...p.triggerProps} variant="ghost" size="icon-sm" className="size-6">
          <EllipsisVertical aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent {...p.contentProps} align="end">
        {p.sortable ? (
          <>
            <DropdownMenuItem onSelect={p.sortAsc}>
              <ArrowUp aria-hidden="true" />
              {labels.sortAsc}
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={p.sortDesc}>
              <ArrowDown aria-hidden="true" />
              {labels.sortDesc}
            </DropdownMenuItem>
            {p.sort ? <DropdownMenuItem onSelect={p.clearSort}>{labels.clearSort}</DropdownMenuItem> : null}
          </>
        ) : null}
        {p.filterable ? (
          <DropdownMenuItem onSelect={p.openFilter}>
            <Funnel aria-hidden="true" />
            {labels.editFilter}
          </DropdownMenuItem>
        ) : null}
        {p.filtered ? <DropdownMenuItem onSelect={p.clearFilter}>{labels.clearFilter}</DropdownMenuItem> : null}
        <DropdownMenuSeparator />
        {p.pinned !== "left" ? (
          <DropdownMenuItem onSelect={() => p.pin("left")}>{labels.pinLeft}</DropdownMenuItem>
        ) : null}
        {p.pinned !== "right" ? (
          <DropdownMenuItem onSelect={() => p.pin("right")}>{labels.pinRight}</DropdownMenuItem>
        ) : null}
        {p.pinned ? <DropdownMenuItem onSelect={() => p.pin(false)}>{labels.unpin}</DropdownMenuItem> : null}
        <DropdownMenuItem disabled={!p.canMoveLeft} onSelect={() => p.move(-1)}>
          {labels.moveLeft}
        </DropdownMenuItem>
        <DropdownMenuItem disabled={!p.canMoveRight} onSelect={() => p.move(1)}>
          {labels.moveRight}
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={p.autoFit}>{labels.autoFit}</DropdownMenuItem>
        {p.hideable ? <DropdownMenuItem onSelect={p.hide}>{labels.hide}</DropdownMenuItem> : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function FilterPopover<R>(p: FilterPopoverSlotProps<R>) {
  return (
    <Popover open={p.open} onOpenChange={p.onOpenChange}>
      <PopoverTrigger asChild>
        <Button {...p.triggerProps} variant="ghost" size="icon-sm" className="size-6">
          <Funnel aria-hidden="true" className={cn(p.active ? "text-primary" : "text-muted-foreground")} />
        </Button>
      </PopoverTrigger>
      <PopoverContent {...p.contentProps} align="end" className="w-72">
        {p.children}
      </PopoverContent>
    </Popover>
  );
}

function ConditionFilter<R>(p: ConditionFilterSlotProps<R>) {
  const { labels } = p;
  if (p.kind === "boolean") {
    return (
      <div className="flex flex-col gap-3">
        <p className="font-medium">{p.column.header}</p>
        <RadioGroup aria-label={p.column.header} value={p.op} onValueChange={p.setOp}>
          {p.ops.map((op) => (
            // biome-ignore lint/a11y/noLabelWithoutControl: the radio item is inside the label
            <label key={op.value} className="flex items-center gap-2">
              <RadioGroupItem value={op.value} />
              {op.label}
            </label>
          ))}
        </RadioGroup>
        {p.active ? (
          <Button variant="ghost" size="sm" onClick={p.clear}>
            {labels.clearFilter}
          </Button>
        ) : null}
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <p className="font-medium">{p.column.header}</p>
      {/* Native selects: a Radix Select's list would be a click outside the popover. */}
      <NativeSelect aria-label={labels.condition} value={p.op} onChange={(event) => p.setOp(event.target.value)}>
        {p.ops.map((op) => (
          <NativeSelectOption key={op.value} value={op.value}>
            {op.label}
          </NativeSelectOption>
        ))}
      </NativeSelect>
      {p.needsRange ? (
        <NativeSelect aria-label={labels.range} value={p.range} onChange={(event) => p.setRange(event.target.value)}>
          {p.ranges.map((range) => (
            <NativeSelectOption key={range.value} value={range.value}>
              {range.label}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      ) : null}
      {p.needsValue ? <Input {...p.valueProps} /> : null}
      {p.needsTo ? <Input {...p.toProps} /> : null}
      <div className="flex justify-end gap-2">
        {p.active ? (
          <Button variant="ghost" size="sm" onClick={p.clear}>
            {labels.clearFilter}
          </Button>
        ) : null}
        <Button size="sm" disabled={!p.valid} onClick={p.apply}>
          {labels.apply}
        </Button>
      </div>
    </div>
  );
}

function ChecklistFilter<R>(p: ChecklistFilterSlotProps<R>) {
  const { labels } = p;
  return (
    <div className="flex flex-col gap-2">
      <p className="font-medium">{p.column.header}</p>
      <Command shouldFilter={false} className="rounded-md border">
        <CommandInput
          aria-label={p.searchProps["aria-label"]}
          placeholder={labels.search}
          value={p.search}
          onValueChange={p.setSearch}
        />
        <CommandList className="max-h-64">
          {p.loading ? (
            <div className="flex justify-center p-2">
              <Spinner />
            </div>
          ) : null}
          {p.items.length > 0 ? (
            <CommandItem value="__all" onSelect={p.toggleAll}>
              <Checkbox
                tabIndex={-1}
                aria-hidden="true"
                checked={p.allChecked ? true : p.someChecked ? "indeterminate" : false}
              />
              {labels.selectAll}
            </CommandItem>
          ) : null}
          {p.items.map((item) => (
            <CommandItem key={item.value} value={item.value} onSelect={item.toggle}>
              <Checkbox tabIndex={-1} aria-hidden="true" checked={item.checked} />
              {item.label}
            </CommandItem>
          ))}
          {p.emptyItem ? (
            <CommandItem value="__empty" onSelect={p.emptyItem.toggle}>
              <Checkbox tabIndex={-1} aria-hidden="true" checked={p.emptyItem.checked} />
              {p.emptyItem.label}
            </CommandItem>
          ) : null}
          {!p.loading ? <CommandEmpty>{labels.noOptions}</CommandEmpty> : null}
        </CommandList>
      </Command>
      <div className="flex justify-end gap-2">
        {p.active ? (
          <Button variant="ghost" size="sm" onClick={p.clear}>
            {labels.clearFilter}
          </Button>
        ) : null}
        <Button size="sm" onClick={p.done}>
          {labels.done}
        </Button>
      </div>
    </div>
  );
}

// ------------------------------------------------------------ around the grid

function Toolbar(p: ToolbarSlotProps) {
  // The page's own controls (a heading, a filter bar) come first and take the room; the grid's
  // search, filter chips and columns menu share the row.
  return (
    <div {...p.props} className="flex min-h-8 flex-wrap items-center gap-2">
      <div className="min-w-0 flex-1">{p.extra}</div>
      {p.search}
      {p.chips}
      {p.search ? (
        <span className="text-sm text-muted-foreground tabular-nums">
          {p.shownRows === p.totalRows
            ? `${p.totalRows} ${p.labels.rows}`
            : `${p.shownRows} ${p.labels.of} ${p.totalRows} ${p.labels.rows}`}
        </span>
      ) : null}
      {p.columns}
    </div>
  );
}

function SearchBox({ inputProps }: SearchBoxSlotProps) {
  return <Input {...inputProps} className="h-8 w-64" />;
}

function FilterChips({ chips, clearAll, canClear, labels }: FilterChipsSlotProps) {
  if (chips.length === 0 && !canClear) return null;
  return (
    <ul aria-label={labels.filters} className="flex flex-wrap items-center gap-1">
      {chips.map((chip) => (
        <li key={chip.columnId}>
          <Badge variant="secondary" className="gap-0 py-0 pe-0" title={chip.unsupported}>
            <button type="button" className="py-0.5" onClick={chip.edit}>
              {chip.label}
            </button>
            <button
              type="button"
              aria-label={`${labels.clearFilter}: ${chip.label}`}
              className="rounded-full p-0.5 hover:bg-muted"
              onClick={chip.remove}
            >
              <X aria-hidden="true" className="size-3" />
            </button>
          </Badge>
        </li>
      ))}
      {canClear ? (
        <li>
          <Button variant="ghost" size="sm" onClick={clearAll}>
            {labels.clearAll}
          </Button>
        </li>
      ) : null}
    </ul>
  );
}

function ColumnsMenu<R>({ columns, resetLayout, labels }: ColumnsMenuSlotProps<R>) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size="sm">
          <Columns3 aria-hidden="true" />
          {labels.columns}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {columns.map(({ column, visible, hideable, toggle }) => (
          <DropdownMenuCheckboxItem
            key={column.id}
            checked={visible}
            disabled={!hideable}
            onCheckedChange={toggle}
            // The menu stays open while columns are chosen.
            onSelect={(event) => event.preventDefault()}
          >
            {column.header}
          </DropdownMenuCheckboxItem>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={resetLayout}>{labels.resetLayout}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function Pager({ page, pageCount, total, setPage, canPrevious, canNext, labels }: PagerSlotProps) {
  if (pageCount <= 1) return null;
  return (
    <nav aria-label={labels.page} className="flex items-center justify-end gap-2 text-sm text-muted-foreground">
      <span className="tabular-nums">{`${labels.page} ${page + 1} ${labels.of} ${pageCount} · ${total} ${labels.rows}`}</span>
      <Button
        variant="outline"
        size="icon-sm"
        aria-label={labels.previous}
        disabled={!canPrevious}
        onClick={() => setPage(page - 1)}
      >
        <ChevronLeft aria-hidden="true" />
      </Button>
      <Button
        variant="outline"
        size="icon-sm"
        aria-label={labels.next}
        disabled={!canNext}
        onClick={() => setPage(page + 1)}
      >
        <ChevronRight aria-hidden="true" />
      </Button>
    </nav>
  );
}

function Empty({ filtered, loading, labels }: EmptySlotProps) {
  if (loading) {
    return (
      <div className="flex justify-center p-6">
        <Spinner aria-label={labels.loading} />
      </div>
    );
  }
  return (
    <EmptyBox className="p-6">
      <EmptyHeader>
        <EmptyTitle className="text-sm font-normal text-muted-foreground">
          {filtered ? labels.emptyFiltered : labels.empty}
        </EmptyTitle>
      </EmptyHeader>
    </EmptyBox>
  );
}

/** The shadcn skin of the grid. */
export function shadcnGridSlots<R>(
  options: {
    link?: GridLink<R>;
    check?: GridCheck<R>;
    decorate?: GridDecorate<R>;
    pickers?: GridRowPickers<R>;
    problem?: GridProblem<R>;
  } = {},
): GridSlots<R> {
  return {
    Part,
    Row,
    HeaderCell,
    Cell: makeCell(options.link, options.check, options.decorate, options.problem),
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
