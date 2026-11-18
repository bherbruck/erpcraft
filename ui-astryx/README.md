# ui-astryx: Astryx spike

A throwaway spike of the erpcraft admin UI built with [Astryx](https://github.com/meta/astryx)
(`@astryxdesign/core` 0.7.16, theme `@astryxdesign/theme-neutral`), to compare with the shadcn UI in `../ui`.
Nothing here is imported from `../ui`; the API client, generated types and helpers were copied.

Screens: sign-in, app shell (side nav, company switcher, user), sales order list
(server search, status filter, sort, paging, all in the URL), sales order page (header fields,
server-searched customer combobox, editable lines, totals from the server's preview), customer list
and customer record.

```sh
bun install
bun run build && bun run preview   # http://127.0.0.1:5174, /api proxied to 127.0.0.1:8600
bun scripts/screenshots.ts         # writes docs/screenshots/*.png against the running preview
```

Component docs come from the locally installed CLI: `./node_modules/.bin/astryx component Table --dense`.
