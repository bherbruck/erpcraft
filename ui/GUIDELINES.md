# erpcraft UI guidelines

These rules apply to the admin UI in this directory and to every screen a module adds. Each rule has its reason, so it can be argued with instead of worked around. Where a rule can be enforced by a tool or by a shared component, it is; the last section says how.

Status: agreed before the UI was written. Nothing here is built yet.

## Stack

- Bun for packages, scripts and tests. Vite for the dev server and the build.
- React and TypeScript.
- shadcn/ui (CLI-generated, in `src/components/ui`), on Radix and Tailwind, with shadcn's standard neutral theme (CSS variables, light and dark) in `src/index.css`. The repository owns the files, so a module can restyle without fighting a dependency.
- New primitives come from `bunx shadcn@latest add <component>`, never written by hand. The files the CLI writes stay as it wrote them, so `shadcn add <component> --diff` shows upstream changes cleanly; a rule of this document that needs different behaviour is implemented in a shared component in `src/components/` that wraps the primitive (the Dialog that does not close on an outside click wraps shadcn's Dialog), not by editing the primitive.
- Biome for linting and formatting.
- The build produces static files that the `erpcraft` binary serves. There is no separate UI server to deploy.

## Principles

1. **The server decides.** The UI holds no business logic: no totals, no validation rules, no permission decisions. It asks the server, which has a preview mode for exactly this. Otherwise the UI and an agent calling MCP could disagree about the same record.
2. **The registry describes, the UI renders.** Lists, forms, menus and the command palette are built from `/api/meta`. A new module gets its screens without writing UI code, and writes custom screens only where the generated one is not good enough.
3. **Anything the UI can do is a command.** No endpoint exists only for the UI. What a person can click, an agent can call.
4. **Never lose the user's work.** No rule below outranks this one.

## Code rules

1. TypeScript strict mode. No `any`, no `@ts-ignore`, no non-null `!` assertions.
2. No hand-written API types. Types are generated from the command JSON Schemas, so a server change breaks the UI build instead of a user's screen.
3. One API client. Nothing else calls `fetch`. Every write goes through a command.
4. Design tokens only. No raw colors, pixel sizes or font names in components; use the theme variables. This is what makes dark mode and a module's own branding possible.
5. No user-facing text in components. Field and command labels come from the registry; the UI's own text goes through the translation function.
6. Money and quantities are decimal strings from the server to the screen. They are never parsed into a JavaScript number.

## Navigation

1. **Back always goes back**, one step, to what the user saw before. This requires that view state lives in the URL: filters, sort, page, selected tab, current company. A side effect is that every screen is a link that can be shared.
2. **Pages, not modals.** A record opens as a page with its own URL. A modal is allowed for two things only: confirming an action, and creating a record from inside a lookup.
3. **A modal closes only through its own buttons.** Clicking outside does nothing. Escape closes it only if nothing has been typed.
4. **The current company is always visible** and is part of the URL. Posting to the wrong company is the most expensive mistake this UI can cause.
5. **Command palette on Ctrl+K**: go to any screen, find any record, run any action the user is allowed to run. It is built from the same registry as the menus.
6. Opening a link in a new tab works everywhere a record or a screen is referenced.

## Forms

1. A failed save keeps everything the user typed.
2. Leaving a form with unsaved changes asks first.
3. An error appears next to the field it concerns, in the server's words. A toast alone is never the only place an error is shown.
4. Keyboard first. A complete document can be entered without the mouse: Tab moves in reading order, Enter on the last line adds a line, Ctrl+S saves.
5. A submit button is disabled and shows progress while its command runs. Each form load carries one idempotency key, so a double click or a retry after a timeout cannot create two records.
6. Required fields are marked before the user submits, not discovered after.
7. If someone else changed the record meanwhile, say so and show what changed. Never silently overwrite, never silently discard.

## Lookups

1. A field whose values come from the database is a searchable combobox. A plain select is only for values defined in code.
2. Search runs on the server. A list of 40,000 items must work the same as a list of 4.
3. Before the user types, show their recent choices, then the most used.
4. The last entry is "New …", which creates the record in a modal and selects it, without losing the form underneath.
5. A selected value shows its name and its code, and links to the record.
6. Records the user may not see never appear, and a count never reveals that they exist.

## Tables

1. One table component for every list.
2. Paging, sorting and filtering always run on the server. Row permissions are applied in the database query, so a table that filtered in the browser would show wrong counts.
3. Filters, sort and page are in the URL.
4. Each user can choose columns and save filters as named views.
5. A row is a link to its record.
6. An empty table says why it is empty and offers the next action: "No accounts yet. Create one." A table emptied by a filter says so and offers to clear it.
7. Bulk actions report per row what succeeded and what did not.

## Ledger actions

1. **Preview before anything irreversible.** Post shows the ledger entries it will create, then asks for confirmation.
2. **No optimistic updates.** A document shows as posted only after the server has said so.
3. **Posted things are reversed, not deleted.** The button says Reverse, and there is no Delete.
4. A document's status and number stay visible at the top of its page while scrolling.
5. A document page shows what it posted and what it was created from or led to, each as a link.
6. Every record has a history view: who ran which command, and when.

## Display

1. Amounts are right-aligned in tabular digits, with the number of decimals of their currency, and the currency is always shown where more than one can occur.
2. A document date is a calendar date and shows the same in every time zone. A timestamp, such as when something was recorded, is shown in the user's time zone.
3. While data loads, show the shape of what is coming. Content must not jump when it arrives.
4. An action the user may not perform is not shown. This is a courtesy, not security: the server refuses it regardless.
5. A destructive action looks different from a normal one and is never the default button.
6. Screens work at tablet width, because receiving and stock counts happen away from a desk.

## Accessibility and language

1. Every control is reachable and usable by keyboard, with a visible focus indicator.
2. Every input has a label. Color is never the only carrier of meaning: a status has a word, not just a color.
3. Contrast meets WCAG 2.1 AA in both light and dark themes.
4. Dates and numbers are formatted for the user's locale. Layout must not assume left-to-right text or short English words.

## How the rules are enforced

Rules that depend on review get broken. So:

- **By shared components.** `Dialog` (outside click disabled), `Button` (`type="button"` unless told otherwise, progress while busy), `Lookup` and `Picker` (shadcn's combobox: Popover and Command, searching on the server), `DataTable`, `MoneyInput`, `DateInput` and `Form` (unsaved-changes guard, idempotency key, field errors) implement the rules above once, in `src/components/`. Screens use these, and lint forbids importing Radix, cmdk, or shadcn's raw `dialog`, `alert-dialog` and `button` outside `src/components/`.
- **By lint.** No `fetch` outside the API client. No raw colors or sizes. No string literals as JSX text. No `Number()` or `parseFloat` on amount fields.
- **By the type generator.** API types cannot drift from the server.
- **By one command.** `bun run check` runs types, lint, format and tests. A change that fails it is not merged.

A rule that turns out to be wrong is changed here first, with the reason, and then in the code.
