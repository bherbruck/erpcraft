# Astryx vs shadcn for erpcraft

`ui-astryx/` covers every screen in `ui/`, generated from `/api/meta` the same way. The lines, lists and reports run on `@erpcraft/grid` with an Astryx skin. Preview: http://127.0.0.1:5174. These pass: smoke 11/11, grid 20/20 and invoice 9/9; axe (WCAG 2.1 AA, light and dark, 16 pages) found 0 violations.

## What Astryx gave us
- **A finished look with almost no CSS.** Dark mode works with no extra work, and it doesn't look like every other shadcn app.
- **Strong built-ins, used as they come:**
  - shell and navigation: AppShell, SideNav (collapses to a drawer below `lg`);
  - pickers: Typeahead (async server search, grouping, recent choices), DateInput;
  - filtering: **PowerSearch**, typed filter chips that map 1:1 to the server's `where`;
  - CommandPalette (Ctrl+K), Dialog/AlertDialog, MetadataList, Banner, Pagination.
- **Accessibility you get for free:** every control needs a label; also skip link, landmarks, live regions.
- **Agent-friendly docs:** `astryx component X --dense` prints full prop docs. It made the spike fast.

## What it cost
- **Bundle (gzip):** first paint 178 KB (all routes are lazy). A working page is **~640 KB** with the grid. All of `ui/` is ~240 KB. Motion, gestures, BottomSheet and Layer are always loaded, because SideNav, Popover and PowerSearch import them; only swizzling (ejecting components) would avoid that.
- **Beta churn:** 0.7.x, releases every few days, upgrades via codemods. Pin versions; expect upgrade work every quarter.
- **Workarounds I needed:**
  - NumberInput holds a JS number, so I wrote our own string `DecimalInput`.
  - Layout regions must be slot props, not children.
  - Field errors come in three styles (a big box, a small box below, or a tooltip); there's no plain error line.
  - The theme loads Figtree from Google Fonts; I bundled it with `@fontsource` to stay within the CSP.
  - `astryx.css` is one 50 KB stylesheet that can't be split.

## What the grid adds
- **Excel-style editing:** arrow keys move, typing edits a cell, range selection, copy and paste with Excel, undo.
- **Line editing:** Enter adds a line, Ctrl+Enter inserts, Ctrl+- deletes. Every line field is editable, including links to other documents' lines.
- **Lists:** sorting and paging run on the server. Column layout (order, widths, pinning, hiding) is saved per list.
- **Everywhere:** reports and history copy to Excel too. Column widths fit 1280px, with net pinned in view.
- **Skin cost:** all 17 slots are built on Astryx in about 450 lines. Astryx's own Table is only a read-only table.

## Astryx bugs worth reporting upstream (`astryx gap-report`)
1. **Selector contrast (dark mode):** with `variant="secondary"`, the placeholder is 4.11:1 (`#9e9e9e` on `#3c3c3c`), below AA.
2. **Table header widths:** in children mode, widths on `TableHeaderCell` are ignored; you need a `colgroup` with `table-layout: fixed`.
3. **Table bleed:** `.astryx-table-scroll-wrapper` has `margin: -16px` to reach into the padding around it, so it overflows cards and padded pages.
4. **Typeahead resets when its search source changes identity.** A source rebuilt on every render (the grid hands new option arrays each time) drops the search in flight. This is a docs note rather than a bug: say the source must stay stable.

## Switching `ui/` to Astryx: about 25–35 hours
- 8–12 h: move `ui-astryx` into `ui/`, then bring over biome lint, `guidelines.test.ts`, the dist check and the remaining i18n keys.
- 6–8 h: ui/'s own pages for bank import and e-invoice import (the spike uses one generic command page), the line pickers for actions, and the approval facts view.
- 6–8 h: port the Playwright e2e suite from `ui/scripts/e2e.ts` to the new markup. Most selectors change; the scripts here (`smoke`, `grid-smoke`, `invoice-smoke`, `axe`) show the patterns.
- 3–5 h: density pass (form spacing, line rows), mobile check, and one more axe and screenshot sweep.
- Ongoing: Astryx upgrades.

**My recommendation:** if the larger bundle and beta churn are acceptable, Astryx is the faster route to a polished, accessible UI, and the grid already fills its biggest gap.

## Screenshots
- [Sales orders on the grid, with filter chips](docs/screenshots/03-sales-orders-filter-chips-sort.png)
- [Order lines: live totals from the server's preview](docs/screenshots/07-sales-order-edited.png)
- [Vendor invoice: order lines picked in the grid](docs/screenshots/31-invoice-order-line-picker.png)
- [Balance sheet on the grid](docs/screenshots/22-balance-sheet.png)
- [Command palette](docs/screenshots/21-command-palette.png)
- [Dark mode](docs/screenshots/13-sales-order-dark.png)
