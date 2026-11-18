# @erpcraft/grid

A headless data grid that works like a spreadsheet, for React 18 and 19. It is built on
TanStack Table v8 and TanStack Virtual v3.

The grid owns the structure, state, keyboard, focus, clipboard, sorting, filtering,
column layout and virtual rows. A design system supplies the look through nine small
**slot** components and a set of **class names**. `@erpcraft/grid/reference` holds a plain,
unstyled version of every slot, used by the demo and the tests.

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
import { referenceClassNames, referenceSlots } from "@erpcraft/grid/reference";

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
  slots={referenceSlots<Order>()}        // or your design system's slots
  classNames={referenceClassNames}
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

Pass `slots` (all nine) and, optionally, `classNames`. Each slot only draws what it is
given and calls back. State, focus and keys stay in the grid.

| Slot | Draws | Receives |
| --- | --- | --- |
| `Cell` | the content of a body cell | `column, row, value, text, active, selected, editable, align` |
| `Editor` | the input of the cell being edited | `draft, setDraft, mode, inputRef, options, loadOptions, align, error` |
| `Header` | the content of a header cell | `column, sort {direction, order}, sortable, onSort(multi), filtered, align, menu` |
| `ColumnMenu` | the menu trigger and its popover | `open, onOpenChange, sortAsc/Desc, clearSort, pin(side), hide, autoFit, move(±1), filter, labels` |
| `FilterEditor` | the typed filter form inside the menu | `condition, active, onChange, onClear, onDone, options, loadOptions, rememberLabels` |
| `Toolbar` | quick search, filter chips, "clear all", columns menu | `search, setSearch, chips[{label, remove, edit}], clearAll, columns[{visible, toggle}], resetLayout, shownRows, totalRows` |
| `Pager` | paging in server mode | `page, pageCount, pageSize, total, setPage` |
| `FooterCell` | a total | `column, total, text, align` |
| `Empty` | the empty or loading state | `filtered, loading` |

The rules a skin has to keep:

1. **The editor's input goes inside the cell, and gets `inputRef`.** The grid focuses it,
   puts the caret at the end, and reads the keys that bubble from it (Enter, Tab, Escape,
   arrows in "enter" mode). Call `setDraft` on every change. A combobox may show its list
   in a portal. Keys pressed in that list stay with the list, so commit by putting the
   chosen value into `draft`.
2. **Mark every portal of the grid with `data-grid-menu`** (the popover content of the column
   menu, a filter's date picker, and so on). Focus inside such an element still counts as
   focus in the grid, and the grid ignores keys and clicks that come from it.
3. **Inner header controls take `tabIndex={-1}`.** The header cell is the tab stop.
   Enter sorts and Alt+ArrowDown opens the menu.
4. **Call `onOpenChange(false)` when the menu closes** (Escape, a click outside, an action).
   The grid then puts focus back on the cell or header the menu was opened from.
5. Apply sorting from the header with `onSort(event.shiftKey)`, so Shift+click adds a sort.
6. The `FilterEditor` decides when to apply a condition: the reference applies checklists at
   once and typed conditions on Enter or "Apply". An incomplete condition removes the filter.
   For the checklist of a reference column, call `rememberLabels(options)` so the chips can
   show names instead of ids.

A shadcn/ui `ColumnMenu`, roughly:

```tsx
function ShadcnColumnMenu<Row>(p: ColumnMenuSlotProps<Row>) {
  return (
    <Popover open={p.open} onOpenChange={p.onOpenChange}>
      <PopoverTrigger asChild>
        <Button variant="ghost" size="icon" tabIndex={-1} aria-label={`${p.labels.columnMenu}: ${p.column.header}`}>
          <MoreVertical />
        </Button>
      </PopoverTrigger>
      <PopoverContent data-grid-menu="" align="end" className="w-72">
        {/* sort buttons, p.filter, pin / move / fit / hide */}
      </PopoverContent>
    </Popover>
  );
}
```

### Styling hooks

`classNames` puts classes on the grid's own elements: `root, scroller, header, headerRow,
headerCell, resizeHandle, body, row, cell, footer, footerCell, live`. State is exposed as
data attributes, so a stylesheet or Tailwind's `data-[…]:` variants can style it:

| Element | Attributes |
| --- | --- |
| body cell | `data-active`, `data-selected`, `data-sel-top`, `data-sel-bottom`, `data-sel-left`, `data-sel-right` (edges of the selection), `data-editing`, `data-editable`, `data-pinned="left\|right"`, `data-type`, `data-align`, `aria-invalid` |
| header cell | `data-active`, `data-sorted="asc\|desc"`, `data-filtered`, `data-pinned`, `data-dragging`, `data-drop="before\|after"` (reorder target), `data-type`, `data-align` |
| row | `data-selected`, `data-row-id` |
| resize handle | `data-resizing` |
| root | `data-mode="client\|server"` |

Numbers should be right-aligned with tabular figures (`font-variant-numeric: tabular-nums`),
as the reference CSS does with `[data-type="number"], [data-type="money"]`. Pinned cells
need an opaque background. The grid sets widths, sticky positions and z-index inline.

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
| Alt+ArrowDown, Shift+F10, ContextMenu | Open the column menu (sort, filter, pin, move, fit, hide). Alt+ArrowDown on a body cell opens its column's menu, as with Excel's filter. |
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
header reorders the columns. Dropped among pinned columns, a column joins their side.
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

Active filters appear as chips in the Toolbar slot, each with a remove button and an edit
action, plus "clear all". Quick search matches the shown text of every searchable column
(text, enum, state and reference by default).

## The erpcraft query adapter

`toErpcraftQuery(state, target, { today })` turns a `GridQueryState` into the exact input
of `entity.list` or `document.list` (see `crates/erpcraft-kernel/src/builtin.rs`):

```ts
const q = toErpcraftQuery(state, { kind: "entity", entity: "item", filters: { company_id }, activeOnly: true });
// q.command === "entity.list"
// q.input = { entity, filters, q, where, sort: ["name", "-price"], limit, offset, active_only }
// q.unsupported = [{ columnId, what: "filter" | "sort" | "page", reason }]
const page = await api.run(q.command, q.input, { signal });

toErpcraftQuery(state, { kind: "document", docType: "sales_order", companyId, status, summary });
// q.input = { doc_type, company_id, status, q, where, sort, limit, offset, summary }
```

The filters become one `where` condition in the rule language of `erpcraft-rules`
(comparisons, `in`, `&&`, `||` over `record.<field>`). Values are written as quoted
strings and cast by the server to the column's type, so decimals and days arrive exactly
as typed. Conditions are joined as a balanced tree, which stays well inside the server's
nesting limit of 64. Field names must be plain identifiers. On a document list, `summary.<field>`
filters go to `summary` when they are a single exact value, and the sort takes header
fields and `summary.<field>`. Every generated condition has been checked against the
real `Rule::parse` and `to_sql`.

Anything the API cannot express is **left out of the query and listed in `unsupported`**,
so the UI can say "this filter is not applied" instead of showing unfiltered rows as
filtered. See "What the list API cannot express yet" in the report for the full list.

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
src/reference/          the plain renderer and its CSS
demo/                   the demo page
test/, e2e/             bun unit tests, Playwright tests
```

Every word shown comes from `labels` (`DEFAULT_GRID_LABELS` and `DEFAULT_LABELS`), so a UI
passes its own translations.
