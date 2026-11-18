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

## Polish pass
The question was whether Astryx looked worse because agents know shadcn and not Astryx. So I ran `astryx init`, read what it generates and the `astryx docs layout/*` pages, condensed both into [AGENTS.md](AGENTS.md), and reworked every screen against it and `ui/GUIDELINES.md`. Checks are still green: smoke 11/11, grid 20/20, invoice 9/9, and axe found 0 violations on 16 pages.

### What the guidance changed
- **One page frame** (`page.tsx`), following Astryx's `detail-page` and `table-page` templates:
  - LayoutHeader with a back link ("← Sales orders") above the title;
  - the number in quieter type after the title;
  - one metadata line under it (status · who and when);
  - actions on the right and tabs sitting on the divider;
  - one padding token (4) for the header and the content.
- **No card soup.** I removed the Cards around header fields and records ("Section by default, Card only for a hard boundary"). Fields are on an Astryx `Grid` (`field-grid.tsx`) instead of a CSS grid.
- **Status is a StatusDot with its word**, not a Badge: "Badge is for counts only".
- **Controls share the table's toolbar row**, so lists, approvals and reports have no stacked control rows:
  - filter bar, Views, Columns;
  - approval scope tabs and status;
  - report filters and their figures.
- **Fewer buttons at once.** Print and Delete moved into a "⋯" menu, which leaves one primary action.
- **Totals are right-aligned rows** under a divider, not a card.
- **Rhythm:** tight gaps inside a field, 8 between sections.
- **Type and widths:**
  - read-only text is no longer greyed out;
  - dense grids use the small type size, as Astryx's own compact Table does;
  - column tools show on hover over the label's end, so headers no longer read "Quan…";
  - widths fit the header too.

### Before and after, with shadcn alongside
| Screen | shadcn (`ui/`) | Astryx before | Astryx after |
|---|---|---|---|
| Sales order list | [04](../ui/docs/screenshots/04-document-list.png) | [02](docs/screenshots/02-sales-orders.png) | [polish-01](docs/screenshots/polish-01-sales-order-list.png) |
| Sales order | [06](../ui/docs/screenshots/06-document-draft.png) | [05](docs/screenshots/05-sales-order.png) | [polish-02](docs/screenshots/polish-02-sales-order.png) |
| Customer | [03](../ui/docs/screenshots/03-entity-record.png) | [11](docs/screenshots/11-customer.png) | [polish-03](docs/screenshots/polish-03-customer.png) |
| Trial balance | [08](../ui/docs/screenshots/08-trial-balance.png) | [16](docs/screenshots/16-report-trial-balance.png) | [polish-04](docs/screenshots/polish-04-trial-balance.png) |
| Approvals | [40](../ui/docs/screenshots/40-approvals-inbox.png) | [17](docs/screenshots/17-approvals.png) | [polish-05](docs/screenshots/polish-05-approvals.png) |
| Palette | [20](../ui/docs/screenshots/20-palette.png) | [21](docs/screenshots/21-command-palette.png) | [polish-06](docs/screenshots/polish-06-palette.png) |
| Dark | [14](../ui/docs/screenshots/14-dark-document.png) | [13](docs/screenshots/13-sales-order-dark.png) | [polish-07](docs/screenshots/polish-07-dark-sales-order.png), [polish-08](docs/screenshots/polish-08-dark-sales-order-list.png) |
| Tablet | [12](../ui/docs/screenshots/12-tablet-document.png) | [15](docs/screenshots/15-sales-order-tablet.png) | [polish-09](docs/screenshots/polish-09-tablet-sales-order.png) |

### Verdict
**Partly. The guidance closed the layout gap, not the polish gap.**
- **What improved.**
  - Headers, spacing rhythm and density now look deliberate, and the record pages are calmer than shadcn's: no card around the fields and no helper text under every field.
  - The sales order and trial balance are now fair matches for `ui/`.
  - The guidance was the reason. Every change above came from a rule in `astryx init` or `astryx docs layout`, not from my taste. The user's hunch holds: with the guidance loaded, the first attempt is much closer.
- **What it didn't close.**
  - **Agent fluency.** The guidance is short (about 15 rules). Beyond it, an agent still has to read the props of each component (`astryx component X --dense`) before using it. With shadcn, agents already know the components, so pages come out right first time.
  - **The grid.** The biggest screens are tables, and the grid is our own skin, not Astryx, so the guidance covers it only loosely. The lines grid reads more like a read-only table than shadcn's always-visible inputs. That is denser, but less obviously editable.
  - **A few rough edges remain:**
    - the checkbox column reads "Choose";
    - a short record (customer) leaves a lot of empty page;
    - Typeahead chips ("C001 · Buyer LLC") read heavier than a plain select value.
- **Recommendation.** Keep AGENTS.md next to the app whichever UI wins. It costs nothing and removes most of the "agent doesn't know Astryx" penalty. The choice now comes down to the bundle and beta churn listed above, not looks.

## Guidance-first re-test
The polish pass fixed pages after they were written. This test asks the other question: does Astryx look better if an agent follows its guidance *from the start*? I rebuilt one screen, the sales order, from nothing, in new files only (`src/pages/fresh/`, route `/c/:company/fresh/sales_order/:id` and `/new`). It reuses only the API client, the `lib/` helpers (meta, decimals, dates, link labels, recent choices) and `useAsync`. It does not use `page.tsx`, `field-grid`, `field-input`, `lookup`, `lines-grid` or `data-table`.

### How it was built
1. Read first, code second: AGENTS.md, `ui/GUIDELINES.md`, `astryx docs layout` and its five sub-guides, `principles` and `is-my-code-good`, then `astryx component <Name> --dense` for every component used (Layout, LayoutHeader, LayoutContent, Section, Grid, FormLayout, Table and its parts, Typeahead, TypeaheadItem, TextInput, DateInput, Selector, MetadataList, StatusDot, Banner, AlertDialog, Button, IconButton, DropdownMenu, Skeleton, Link, Heading, Text, useTableRowStatus).
2. `astryx build "sales order document page: header fields, customer lookup, editable line items table, totals, workflow actions"` proposed `table-page` first and `detail-page` second. I scaffolded **`detail-page`** (`astryx template detail-page --type page src/pages/fresh`), because it is the one with a status header, line items and a totals block. I kept its header (back link, title, bullet-separated facts, actions that drop to a row of their own at 1024 or less, tabs with `paddingBlockEnd={0}`), its `contentWidth={1000}`, its Sections and its MetadataList totals.
3. **I dropped the template's side panel**, and the file says why. With a 320px rail, the lines get about 830px and need about 940. `layout/responsive` says "drop a region rather than let it compete". So the order details sit above the lines, on a reflowing `Grid columns={{minWidth: 240, max: 3}}`.
4. **Lines: Astryx `Table`, not `@erpcraft/grid`.** The guidance says "dense data = rows (Table)" and "compact density with sm controls", and Table's `renderCell` takes any component. So each cell holds a small TextInput or Typeahead. The grid would have been the app convention (AGENTS.md, second half), but the point here was what Astryx's own guidance produces. One Typeahead per line searches items, charges and accounts at once, with results grouped through `auxiliaryData.group`. That replaces the old kind selector plus lookup.
5. Behaviour as in GUIDELINES:
   - totals come live from the server preview;
   - Ctrl+S saves, with one idempotency key per payload;
   - errors appear at their field or line, and what was typed is kept;
   - leaving with unsaved changes asks first (an AlertDialog over `useBlocker`);
   - workflow actions come from the registry and are disabled while there are unsaved changes; Cancel is confirmed in an AlertDialog;
   - recent choices show first in lookups;
   - Enter on the last line adds a line and focuses it.

### The same order, three ways
| Width | shadcn (`ui/`) | Astryx, existing page | Astryx, guidance-first |
|---|---|---|---|
| 1440 | [fresh-shadcn-1440](docs/screenshots/fresh-shadcn-1440.png) | [fresh-astryx-1440](docs/screenshots/fresh-astryx-1440.png) | [fresh-fresh-1440](docs/screenshots/fresh-fresh-1440.png) |
| 820 | [fresh-shadcn-820](docs/screenshots/fresh-shadcn-820.png) | [fresh-astryx-820](docs/screenshots/fresh-astryx-820.png) | [fresh-fresh-820](docs/screenshots/fresh-fresh-820.png) |

(`bun scripts/fresh-screenshots.ts`, draft order `01a1ea99…` in OHIO.)

### Verdict
**Yes for the page around the table, mostly for the lines. Guidance-first closed most of the gap on this screen, and it beats the existing Astryx page.**
- **Better than shadcn:**
  - **The header.** Back link, title, one line of facts ("● Draft · Buyer LLC · Nov 16, 2026 · Created by …") and one primary action, with the rest in "⋯". shadcn shows six buttons in a row, two of them red.
  - **The details.** Six fields and the memo sit in two calm rows. shadcn's card is twice as tall, because every field carries its help text underneath. Here the help is in an ⓘ tooltip, from the guidance's "demote, don't shout".
  - **The totals.** Discounts, Charges and a bold Total (USD) as right-aligned rows.
  - **At 820** the header actions wrap under the facts as the template does, and the details still fit three to a row. shadcn's actions break onto two rows beside the title.
- **Better than the existing Astryx page:**
  - The lines are visibly editable: inputs in every cell, as in shadcn. The existing grid reads like a read-only table.
  - The fields have a heading, and the page has one rhythm: Section, gap and Section.
- **Still worse, or only even:**
  - **Line cells are heavier than shadcn's.** A chosen Typeahead value is a token chip with an ×, so every line shows two or three chips ("GADGET · Gadget ×", "MAIN · Mai ×"). That is busier than shadcn's flat combobox text, and a 132px "Ship from" truncates. Astryx has no plain-value mode for Typeahead.
  - **Right-aligned amounts in inputs need CSS.** Astryx's TextInput has no alignment prop, its input sets `text-align` with high specificity, and `controlStyle` reaches only the box around the input. So the page has one rule in `globals.css` (`.fresh-amount input { text-align: end !important }`), the only departure from "no imported CSS". It is a good candidate for `astryx gap-report TextInput`.
  - **The template frame costs width.** `contentWidth={1000}` leaves the lines about 940px. At 1440 they fit exactly, after I trimmed columns three times. At 820 the table scrolls sideways, and the hidden scrollbar gives no cue. shadcn scrolls there too.
  - **No quick-create** ("New customer…") and no links from a chosen value to its record. Both GUIDELINES rules are met by the old components and were left out of this scope.
  - **A disabled Save is a filled grey button.** It reads as more "off" than shadcn's.
- **What the guidance did and did not do.**
  - **It decided the structure:** template, frame, header, Section and Grid, Table for the lines, StatusDot, MetadataList totals, and the AlertDialog confirmations. All of it was right the first time, with no layout rework.
  - **Every rework was visual and component-level:** column widths, the native input's alignment, and the Typeahead query being "C001 · Buyer LLC" when you edit a chosen value. That last one needed a fix in the search source, so editing starts from recent choices.
  - The guidance can't help with those. You find them only by rendering and looking.
- **How long it took.**
  - The shell clock read 12:46 at the start and about 12:58 at the end, but it jumped backwards once, so I don't trust it for a duration.
  - By the work itself: reading came first and was about a third of it (about 25 doc and component reads). Writing the four files was one pass. Then came four build-and-screenshot rounds.
  - It was noticeably less than the polish pass, because nothing had to be undone.
- **Checks:** `tsc --noEmit` and `bun run build` pass, and `bun scripts/smoke.ts` passes 11/11. The existing routes are unchanged.
