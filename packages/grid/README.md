# @erpcraft/grid

A headless data grid that works like a spreadsheet, for React 18 and 19. It is built on
TanStack Table v8 and TanStack Virtual v3.

The grid has **no look of its own**. It ships no CSS and no visual defaults: no colours,
borders, fonts or spacing. It owns the behaviour: state, keyboard, focus, clipboard,
sorting, filtering, column layout, virtual rows, ARIA and data attributes for state.
Every visible element is drawn by a **slot** from the UI's own design system. The slot
receives the state it shows and `props` to spread on its root element, so in each UI
the grid looks exactly like that design system's table.

`@erpcraft/grid/reference` holds bare, browser-default HTML versions of the slots, for
the tests and the demo only. It is not a look to ship or start from: each UI writes its
own skin from its own components (see the skinning checklist below). The demo adds a
few lines of CSS of its own (`demo/demo.css`) so the active cell and the selection are
visible; the package itself has none.

```sh
bun install
bun run demo        # http://127.0.0.1:5175  (#client 100k rows, #server, #lines)
bun test test       # unit tests: keyboard, selection, TSV, paste, filters, query adapter
bun run e2e         # Playwright against the demo (uses /usr/bin/google-chrome, or $CHROME)
bun run typecheck
```

## Using it

```tsx
import { applyChanges, Grid, type GridColumn, localStorageLayout } from "@erpcraft/grid";
import { gridSlots } from "@/components/grid-slots"; // the UI's own skin

const columns: GridColumn<Order>[] = [
  { id: "number", header: "Number", type: "text", pin: "left", hideable: false },
  { id: "customer_id", header: "Customer", type: "reference", format: (v) => names.get(v) ?? "",
    loadOptions: (search, signal) => api.searchCustomers(search, signal) },
  { id: "status", header: "Status", type: "state", options: states },
  { id: "doc_date", header: "Date", type: "date", editable: true },
  { id: "total", header: "Total", type: "money", currency: (r) => r.currency, minFraction: 2, total: true },
];

<Grid<Order>
  id="sales-orders"                      // names the saved layout and the element ids
  aria-label="Sales orders"
  columns={columns}
  rows={rows}
  slots={gridSlots<Order>()}
  onEdit={(changes) => setRows((r) => applyChanges(r, changes, columns))}
  onReport={(report) => report.rejected.length && toast(report)}
  layoutStorage={localStorageLayout()}
/>;
```

### Columns

| Field | Meaning |
| --- | --- |
| `id`, `header`, `type` | `type` is one of `text`, `number`, `money`, `date`, `datetime`, `boolean`, `enum`, `state`, `reference`. It sets sorting, filtering, alignment, parsing and copying. |
| `field` | The field the server knows the column by (`summary.total` on a document list). Defaults to `id`. |
| `value(row)`, `setValue(row, v)` | Read and write the cell when it is not `row[id]`. |
| `format(value, row)` | The text shown, which is also what is copied. The default formats by type. |
| `parse(text, row)` | Typed or pasted text to a value, `{ ok, value }` or `{ ok: false, error }`. The default parses by type: decimals as exact strings (`1,234.50`, `(12.00)`, `€ 9.99`), dates in ISO or the locale's order, and Excel serial day numbers. |
| `validate(value, row)` | Return a message to refuse a value. It applies to typing and pasting. |
| `editable` | `true` or `(row) => boolean`. Cells are only editable when the grid has `onEdit`. |
| `options` | The choices of `enum` and `state` columns (in workflow order: a state sorts in this order). |
| `loadOptions(search, signal)` | The choices of a `reference` column, loaded by the consumer as the user types. |
| `currency`, `minFraction` | For display only. Amounts stay decimal strings: they are never turned into floats, and totals are exact. |
| `width`, `minWidth`, `maxWidth`, `pin`, `hidden`, `hideable`, `sortable`, `filterable`, `searchable` | Defaults for the layout and the features of the column. |
| `total` | Footer total for a `number` or `money` column. In client mode it sums the filtered rows; in server mode it comes from the `totals` prop. Mixed currencies show "mixed". |

### Modes

- **Client** (`mode="client"`, the default): every row is in `rows`. Sorting, filtering and
  quick search run in memory. With 100k rows in a production build, scrolling runs at
  60 fps, an arrow key takes about 7 ms, and a sort takes 0.2 to 0.6 s.
- **Server** (`mode="server"`): `rows` is one page. The grid reports its state through
  `onQueryChange` as a plain, serialisable `GridQueryState`:

  ```ts
  { sort: [{ columnId, field, desc }], filters: [{ columnId, field, type, condition }],
    search: string, page: { index, size } }
  ```

  The consumer fetches the page and passes `rows`, `total`, `loading` and, if it wants
  them, `totals`. Search is debounced by 300 ms. A change of sort, filter or search goes
  back to the first page. `initialQuery` starts the grid from a saved state, such as one
  read from the URL. `onQueryChange` is called in client mode too.

### Edits

`onEdit(changes, source)` receives one batch per typed edit, paste, clear, undo or redo.
Each change has `rowId`, `rowIndex`, `row`, `columnId`, `oldValue` and `newValue`. The
grid never changes `rows` itself. `applyChanges(rows, changes, columns)` does it for plain
objects.

`onReport({ source, applied, rejected })` reports what a paste or clear did, including
every refused cell with its row, column, text and reason ("read-only", "outside the grid",
"\"abc\" is not a number", or your `validate` message). The grid also announces it in its
`aria-live` region. A refused typed edit keeps the editor open, marks the cell
`aria-invalid` and passes the reason to the Editor slot as `error`.

Undo history is kept by row id, so sorting or filtering between an edit and its undo
does not matter. It holds 100 batches.

### Line editing (document lines)

Pass `lineEditing={{ onAppend, onInsert(at), onDelete(rowIds, from, to) }}`. The grid asks
for rows to be added or removed, and the consumer changes `rows`. The active cell moves
to the new row as soon as it arrives.

### Layout

Column order, widths, pinning and hidden columns are saved per grid `id` through
`layoutStorage: { load(gridId), save(gridId, layout) }`. `load` may return a promise, for a
server-side store. `localStorageLayout(prefix)` is the browser store. A saved layout is
reconciled with the columns as they are now: columns that no longer exist are dropped,
and new ones appear where they are defined.

## How a design system skins it

A skin is an object with the 17 slots. Each slot gets **state** (what to show) and,
where the grid needs an element of its own, **`props` to spread** on the slot's root
element: role and ARIA, the roving `tabIndex`, `data-*` state attributes, event handlers,
a callback `ref`, and the layout the virtual rows need in `style` (widths, sticky and
absolute positions, `display: flex` on rows). Spread `props` first, then add the design
system's classes, and keep `props.style`:

```tsx
const Cell: GridSlots<Row>["Cell"] = ({ props, text, editor, align }) => (
  <TableCell {...props} className={cn("h-9 px-2", align === "end" && "text-right tabular-nums",
    "data-[selected]:bg-muted data-[active]:ring-2 data-[active]:ring-ring")}>
    {editor ?? text}
  </TableCell>
);
```

Rows are positioned absolutely and laid out with flex, so a skin that uses `<table>`
elements (shadcn's `Table`) keeps working: the `style` in `props` overrides their table
display. Draw state from the data attributes (Tailwind `data-[active]:`, `data-[selected]:`),
not from class names: the grid gives none.

### Skinning checklist

| Slot | Draws | shadcn/ui | Gets |
| --- | --- | --- | --- |
| `Part` kind `root` | wrapper around toolbar, grid and pager | `div` | `props` |
| `Part` kind `scroller` | the scrolling `role="grid"` element | `Table` (or its container `div`) | `props`, `loading` |
| `Part` kind `header` / `body` / `footer` | row groups | `TableHeader` / `TableBody` / `TableFooter` | `props` |
| `Row` | header, body and footer rows | `TableRow` | `props`, `kind`, `row`, `selected` |
| `HeaderCell` | a column header | `TableHead` with a ghost `Button` (`sortProps`) and sort icon (`ArrowUp`/`ArrowDown`, a small `Badge` for the order) | `props`, `label`, `sort`, `sortProps`, `filtered`, `filter`, `menu`, `resizeHandle` |
| `Cell` | a body cell | `TableCell` | `props`, `text`, `editor`, `active`, `selected`, `editing`, `invalid`, `align` |
| `FooterCell` | a total | `TableCell` in `TableFooter` | `props`, `text`, `total` |
| `ResizeHandle` | the drag edge of a header | `div` (give it a width and a hover colour) | `props`, `resizing` |
| `Editor` | the input of the cell being edited | `Input`; `Select` or a `Command` combobox for enum, state and reference (call `commit(value)` on choosing) | `inputProps`, `draft`, `setDraft`, `commit`, `cancel`, `options`, `loadOptions`, `error` |
| `ColumnMenu` | the column menu and its trigger | `DropdownMenu` (`DropdownMenuTrigger asChild` + `triggerProps`, `DropdownMenuContent {...contentProps}`, items for sort, `openFilter`, pin, move, fit, hide) | `open`, `onOpenChange`, `triggerProps`, `contentProps`, actions |
| `FilterPopover` | the filter button and popover | `Popover` (`PopoverTrigger asChild` + a ghost icon `Button` with `triggerProps`, `PopoverContent {...contentProps}`) | `open`, `onOpenChange`, `triggerProps`, `contentProps`, `active`, `children` |
| `ConditionFilter` | text, number, date and boolean conditions | `Select` for `ops` and `ranges`, `Input {...valueProps}` / `{...toProps}`, `Button` Apply / Clear; `RadioGroup` for boolean | `ops`, `op`, `setOp`, `needsValue`, `needsTo`, `needsRange`, `valid`, `apply`, `clear` |
| `ChecklistFilter` | the checklist with search | `Command` with `shouldFilter={false}`: `CommandInput value={search} onValueChange={setSearch}`, `CommandList` of `CommandItem onSelect={item.toggle}` with a `Checkbox`; `CommandEmpty`; a "Select all" item | `items`, `toggleAll`, `allChecked`, `emptyItem`, `loading`, `search`, `setSearch` |
| `Toolbar` | the bar above the grid | `div` laying out its parts | `props`, `search`, `chips`, `columns`, `shownRows`, `totalRows`, `extra` |
| `SearchBox` | quick search | `Input` | `inputProps` |
| `FilterChips` | active filters, "clear all" | `Badge` per chip (a `button` to edit, an `X` button to remove), ghost `Button` "Clear all" | `chips`, `clearAll`, `canClear` |
| `ColumnsMenu` | show and hide columns | `DropdownMenu` with `DropdownMenuCheckboxItem`s and a reset item | `columns`, `resetLayout` |
| `Pager` | paging (server mode) | `Pagination` | `page`, `pageCount`, `setPage`, `canPrevious`, `canNext` |
| `Empty` | no rows, loading | the UI's empty state, `Skeleton` | `filtered`, `loading` |

For Astryx, map each row to its counterpart in the same way.

The rules a skin has to keep:

1. **Spread `props` on the slot's own element and keep `props.style`**, `props.ref` and the
   handlers. Merge, don't replace.
2. **The editor's input goes inside the cell** and gets `inputProps` (with its `ref`). The grid
   focuses it and reads the keys that bubble from it (Enter, Tab, Escape, arrows in "enter"
   mode). An editor whose list lives in a portal (`Select`, a `Command` combobox) calls
   `commit(value)` when a choice is made and `cancel()` on Escape, since its keys do not
   reach the grid.
3. **Put `contentProps` on every popover's content.** It carries `data-grid-menu`, which
   keeps the grid's focus while the menu is open and makes the grid ignore keys and clicks
   from it. A date picker or anything else the grid opens in a portal needs `data-grid-menu`
   too.
4. **Put `triggerProps` on the trigger buttons.** They are `tabIndex={-1}`: the header cell is
   the tab stop. Clicking a trigger or the sort button hands focus back to the cell, so the
   grid keeps its keys. Keys open them: Alt+ArrowDown the filter, Shift+F10 the menu.
5. **Call `onOpenChange(false)` when a menu closes** (Escape, a click outside, an action).
   The grid then puts focus back on its cell. Radix's own close autofocus is harmless: the
   grid moves focus from the trigger to the cell.
6. Sorting: spread `sortProps` on the header's button. It sorts on click and adds to the
   sort on Shift+click. Show `sort.direction` and, in a multi-sort, `sort.order`.
7. The filter forms are headless (`useConditionFilter`, `useChecklistFilter`, both
   exported): a checklist applies as boxes are ticked, and a condition applies on Enter in
   its inputs (`valueProps` handle it) or on `apply()`.

Data attributes the grid sets:

| Element | Attributes |
| --- | --- |
| body cell | `data-active`, `data-selected`, `data-sel-top`, `data-sel-bottom`, `data-sel-left`, `data-sel-right` (edges of the selection), `data-editing`, `data-editable`, `data-pinned="left\|right"`, `data-type`, `data-align`, `aria-invalid`, `aria-selected`, `aria-readonly` |
| header cell | `data-active`, `data-sorted="asc\|desc"`, `data-filtered`, `data-pinned`, `data-dragging`, `data-drop="before\|after"` (reorder target), `data-type`, `data-align`, `aria-sort` |
| row | `data-selected`, `data-row-id` |
| resize handle | `data-resizing` |
| root | `data-mode="client\|server"`; groups `data-grid-header`, `data-grid-body`, `data-grid-footer`; toolbar `data-grid-toolbar` |

Right-align numbers with tabular figures (`data-[align=end]:text-right tabular-nums`).
Pinned cells and the sticky header and footer need an opaque background.

## Keyboard

The model is Excel's. The active cell is where typing goes. Shift extends a selection
while the active cell stays where it is. An edit started by typing ("enter" mode) is
committed by an arrow key. An edit started with F2, Enter or a double click ("edit" mode)
leaves the arrow keys to the text, and F2 switches between the two modes.

### Moving and selecting

| Key | Action |
| --- | --- |
| Arrow keys | Move one cell |
| Ctrl+Arrow | Jump to the edge of the data, as in Excel |
| Shift+Arrow, Ctrl+Shift+Arrow | Extend the selection |
| Tab / Shift+Tab | Move right / left. At the last or first column, focus leaves the grid. |
| Enter / Shift+Enter | Edit an editable cell (Google Sheets). On a read-only cell, move down / up. `enterEdits={false}` makes Enter always move down, as in Excel. After a run of Tabs, Enter goes back to the column the run started in. |
| Home / End | First / last column of the row |
| Ctrl+Home / Ctrl+End | First cell / last cell |
| PageDown / PageUp | Down / up one screen |
| Shift+Space | Select the row |
| Ctrl+Space | Select the column |
| Ctrl+A | Select all |
| Escape | Collapse the selection to the active cell |
| Shift+click, click and drag | Select a range |
| ArrowUp from the first row | Go to the header row |

### Editing

| Key | Action |
| --- | --- |
| Any character | Replace the value and start editing |
| F2, Enter, double click | Edit the current value |
| Backspace | Clear the value and start editing |
| Enter / Shift+Enter (editing) | Commit and move down / up |
| Tab / Shift+Tab (editing) | Commit and move right / left |
| Arrow keys (editing, "enter" mode) | Commit and move |
| Escape (editing) | Cancel |
| Delete | Clear the editable cells of the selection |
| Ctrl+C / Ctrl+X | Copy / cut the selection as TSV (and an HTML table). It pastes into Excel and Sheets cell by cell. |
| Ctrl+V | Paste TSV from Excel or Sheets into the editable cells, starting at the selection's top left. A single copied cell fills the whole selection. Every refused cell is reported. |
| Ctrl+Z / Ctrl+Y (Ctrl+Shift+Z) | Undo / redo the last edit batch |

### Header row

| Key | Action |
| --- | --- |
| Enter / Space | Sort by the column (ascending, descending, none) |
| Shift+Enter | Add the column to a multi-column sort |
| Alt+ArrowDown | Open the column's filter popover, as Excel's filter button does. It also works on a body cell. |
| Shift+F10, ContextMenu | Open the column menu (sort, filter, pin, move, fit, hide). It also works on a body cell. |
| ArrowLeft / ArrowRight / Home / End | Move along the header |
| ArrowDown | Back to the first row |

### Line editing (with `lineEditing`)

| Key | Action |
| --- | --- |
| ArrowDown on the last row, Enter committing on the last row | Add a row at the end and move into it |
| Tab past the last column | Go on to the first column of the next row, adding one at the end |
| Ctrl+Shift+= (Ctrl++) | Insert a row above |
| Ctrl+Enter | Insert a row below |
| Ctrl+- | Delete the selected rows |

The mouse: a click on a header sorts it and Shift+click adds it to the sort. Dragging a
header reorders the columns. This uses pointer events, so it works when the header is a
button. Dropped among pinned columns, a column joins their side.
Dragging a header's right edge resizes the column, and a double click on the edge fits
it to its content.

Accessibility follows the WAI-ARIA grid pattern. The grid has `role="grid"`,
`aria-rowcount`, `aria-colcount` and `aria-multiselectable`. Rows and cells carry
`aria-rowindex` and `aria-colindex`, which in server mode count from the page's start.
Cells have `aria-selected` and `aria-readonly`, and headers have `aria-sort`. There is a
roving tabindex: exactly one cell (or header) has `tabIndex=0`. Focus is put back on the
active cell after every re-render, sort, filter and edit. When a filter leaves no rows,
the header holds focus. The active row is always rendered, even when it is scrolled out
of view, so the focused element is never removed. Results of paste, clear and undo are
announced in an `aria-live` region.

## Filters

| Type | Conditions |
| --- | --- |
| text | contains, equals, starts with, is empty, is not empty (in memory, case-insensitive) |
| number, money | =, ≠, <, ≤, >, ≥, between, is empty, is not empty (exact decimal comparison) |
| date, datetime | on, before, after, between, relative (today, this month, last month, this quarter, this year), is empty, is not empty |
| enum, state | checklist with search. With no filter set, every box is ticked, as in Excel. |
| reference | checklist with search, loaded through `loadOptions` |
| boolean | yes, no, empty |

Active filters appear in the FilterChips slot, each with a remove action and an edit
action that opens the filter popover, plus "clear all". Quick search matches the shown text of every searchable column
(text, enum, state and reference by default).

## The erpcraft query adapter

`toErpcraftQuery(state, target, { timeZone })` turns a `GridQueryState` into the exact input
of `entity.list` or `document.list` (see `crates/erpcraft-kernel/src/builtin.rs`):

```ts
const q = toErpcraftQuery(state, { kind: "entity", entity: "item", filters: { company_id }, activeOnly: true });
// q.command === "entity.list"
// q.input = { entity, filters, q, where, params, time_zone, sort: ["name nulls last"], limit, offset, active_only }
// q.unsupported = [{ columnId, what: "filter" | "sort" | "page", reason }]
const page = await api.run(q.command, q.input, { signal });

toErpcraftQuery(state, { kind: "document", docType: "sales_order", companyId, status });
// q.input = { doc_type, company_id, status, q, where, params, time_zone, sort, limit, offset, summary }
```

How each filter is sent (the server needs commit fb678ec or later):

| Grid filter | `where` |
| --- | --- |
| text contains, equals, starts with, ends with | `record.f.containsIgnoreCase('x')`, `equalsIgnoreCase`, `startsWithIgnoreCase`, `endsWithIgnoreCase`: case-insensitive like the grid in memory, `ILIKE` on the server with wildcards escaped |
| text empty / not empty | `(record.f == null \|\| record.f == '')` and its opposite |
| number, money | `record.f >= '1000.5'`, between as two bounds; values are exact decimal strings |
| date on, before, after, between | `record.f == '2026-11-18'`, `<`, `>=` the next day, two bounds |
| date relative | `record.f >= startOfMonth(addMonths(today(), -1)) && record.f <= endOfMonth(addMonths(today(), -1))`: worked out by the server each time, so a saved view stays relative |
| datetime | from a day's midnight to the next one's in `time_zone` (the browser's zone unless `timeZone` says otherwise) |
| checklist (enum, state, reference) | `record.f in params.g0` with the values in `params`: bound, so 500 ids cost the condition nothing |
| boolean | `record.f == true`, `== false`, `== null` |
| a document's summary field | the same, on `record.summary.<field>` |

Every sort is sent with ` nulls last`, as the grid sorts empty values last in both
directions. The server sorts a choice (state, enum) in its declared order and a reference
by the linked record's name. Conditions are joined as a balanced tree, which stays well
inside the server's nesting limit of 64. A target's own `where` and `params` are kept, and
the grid's params are named `g0`, `g1` and so on.

Anything the API cannot express is **left out of the query and listed in `unsupported`**,
so the UI can say "this filter is not applied" instead of showing unfiltered rows as
filtered. What is still left out:

- On a document list, filters and sorts on fields that are neither header fields
  (`number`, `status`, `doc_date`, `memo`, …) nor summary fields. Sorting also excludes
  `memo` and the other header fields outside `DOCUMENT_SORT_FIELDS`.
- A page larger than 500 rows. It is clamped and reported.
- A condition longer than 4,000 characters. With checklists in `params`, this takes dozens
  of long text filters.
- Quick search goes to `q`, which the server matches against the entity's own search
  fields rather than against every visible text column.

## Layout of the package

```
src/types.ts            public shapes: columns, filters, query state, edits, layout
src/core/navigation.ts  the keyboard as a pure state machine (handleKey)
src/core/editing.ts     copy, paste, clear and typed-edit planning; undo history
src/core/tsv.ts         Excel/Sheets clipboard text
src/core/filters.ts     conditions, matching, chips
src/core/columns.ts     per-type format, parse, compare, sort keys
src/core/decimal.ts     exact decimal compare, sum, parse, format
src/core/dates.ts       days, user dates, relative ranges
src/core/layout.ts      layout reconcile, localStorage store, column move
src/core/erpcraft.ts    toErpcraftQuery
src/react/Grid.tsx      the component
src/react/slots.ts      slot contracts and labels
src/react/filters.tsx   headless filter forms (useConditionFilter, useChecklistFilter)
src/reference/          bare browser-default slots, for tests and the demo only
demo/                   the demo page
test/, e2e/             bun unit tests, Playwright tests
```

Every word shown comes from `labels` (`DEFAULT_GRID_LABELS` and `DEFAULT_LABELS`), so a UI
passes its own translations.
