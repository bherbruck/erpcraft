// Checks that the production build in dist/ runs under the policy the server
// sends with index.html (apps/server/src/ui.rs):
//
//   default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline';
//   object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'
//
//   bun run build && bun run check:dist
//
// No inline script, no eval, nothing loaded from another host, no data: URL.

import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const DIST = resolve(import.meta.dir, "../dist");
const problems: string[] = [];

function files(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? files(join(dir, entry.name)) : [join(dir, entry.name)],
  );
}

const all = files(DIST);
const html = readFileSync(join(DIST, "index.html"), "utf8");

for (const match of html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/g)) {
  const [, attributes = "", body = ""] = match;
  if (!/\bsrc=/.test(attributes) || body.trim() !== "") problems.push("index.html: an inline script");
}
for (const match of html.matchAll(/\b(?:src|href)="([^"]+)"/g)) {
  const url = match[1] ?? "";
  if (!url.startsWith("/") || url.startsWith("//")) problems.push(`index.html: loads ${url} from elsewhere`);
}
if (/\bon[a-z]+=/i.test(html)) problems.push("index.html: an inline event handler");

for (const file of all) {
  const name = file.slice(DIST.length + 1);
  const text = readFileSync(file, "utf8");
  if (file.endsWith(".js")) {
    if (/(?<![.\w$])eval\(|new Function\(/.test(text)) problems.push(`${name}: eval or new Function`);
    if (/["'`]data:[a-z]+\/[\w+.-]+[;,]/.test(text)) problems.push(`${name}: a data: URL`);
    if (/import\(\s*["'`]https?:/.test(text)) problems.push(`${name}: imports from another host`);
  }
  if (file.endsWith(".css")) {
    if (/url\(\s*["']?data:/.test(text)) problems.push(`${name}: a data: URL`);
    if (/url\(\s*["']?(?:https?:)?\/\//.test(text)) problems.push(`${name}: loads from another host`);
    if (/@import\s+(?:url\()?["']?(?:https?:)?\/\//.test(text)) problems.push(`${name}: imports from another host`);
  }
}

if (problems.length > 0) {
  console.log(problems.join("\n"));
  process.exit(1);
}
console.log(`dist/ is fit for the server's content security policy (${all.length} files)`);
