# ui-astryx: the admin UI on Astryx

The erpcraft admin UI rebuilt on [Astryx](https://github.com/meta/astryx) (`@astryxdesign/core`,
theme `@astryxdesign/theme-neutral`, MIT), as a candidate to replace `../ui`.

Like `../ui`, every screen is generated from the server's registry (`/api/meta`): one list and one
record page for every entity, one list and one page for every document type, the menu from the
installed modules. The design-system-free parts are copied from `../ui/src` unchanged: `api/`,
`lib/` (registry helpers, labels, recent choices, URL state, decimals, dates), `i18n/`,
`app/context.tsx`. Everything visible is an Astryx component used as it comes.

| Screen | Astryx built-ins |
|---|---|
| Shell | AppShell, SideNav (resizable, collapsible, drawer below `lg`), TopNav, Selector (company) |
| Lists | Table (+ sortable plugin), PowerSearch (typed filter chips → server `where`), DropdownMenu (columns), Pagination, EmptyState, Skeleton |
| Record / document | Layout, TabList, Card, MetadataList, inputs per field type (TextInput, DateInput, Selector, CheckboxInput, TextArea, Typeahead), Banner, AlertDialog, Dialog |
| Lookups | Typeahead with a server search source, recent choices, "New …" → quick-create Dialog |
| Lines | Table in children mode with an input per cell (stand-in for `packages/grid`) |

Not ported: reports, approvals, printing, command pages, bulk actions, saved views, command palette.

```sh
bun install
bun run build && bun run preview   # http://127.0.0.1:5174, /api proxied to 127.0.0.1:8600
bun scripts/screenshots.ts         # docs/screenshots/*.png against the running preview
bun scripts/smoke.ts               # create, save (Ctrl+S) and delete a sales order through the UI
```

Component docs come from the locally installed CLI: `./node_modules/.bin/astryx component Table --dense`.
The table lives in one file, `src/components/data-table.tsx`, so the headless grid can replace it.
