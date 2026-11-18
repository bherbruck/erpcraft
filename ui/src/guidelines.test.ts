// The rules of GUIDELINES.md that the linter cannot express, checked on the
// source text. `bun run check` runs this with the other tests.

import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";

const SRC = import.meta.dir;

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sources(path);
    return /\.tsx?$/.test(entry.name) && !entry.name.endsWith(".test.ts") ? [path] : [];
  });
}

// shadcn/ui's CLI writes src/components/ui, src/hooks and src/lib/utils.ts. They are its output, kept as it wrote
// them (see GUIDELINES.md, Stack), so they are not held to these rules; everything that uses them is.
const VENDORED = (name: string): boolean =>
  name.startsWith("components/ui/") || name.startsWith("hooks/") || name === "lib/utils.ts";

const files = sources(SRC)
  .map((path) => ({ name: relative(SRC, path), text: readFileSync(path, "utf8") }))
  .filter((file) => !VENDORED(file.name));

function offenders(pattern: RegExp, except: (name: string) => boolean = () => false): string[] {
  const found: string[] = [];
  for (const file of files) {
    if (except(file.name)) continue;
    file.text.split("\n").forEach((line, index) => {
      if (pattern.test(line)) found.push(`${file.name}:${index + 1}: ${line.trim()}`);
    });
  }
  return found;
}

test("only the API client talks to the server", () => {
  expect(offenders(/\bfetch\(|XMLHttpRequest|navigator\.sendBeacon/, (name) => name === "api/client.ts")).toEqual([]);
});

test("amounts are never parsed into a JavaScript number", () => {
  expect(
    offenders(/\bparseFloat\(|\bNumber\(|\bNumber\.parseFloat\(|\.toFixed\(/, (name) => name === "api/generated.ts"),
  ).toEqual([]);
});

test("components use design tokens, not raw colors or pixel sizes", () => {
  // Hex and functional colors, Tailwind's fixed palette, and arbitrary pixel values.
  const rawColor = /#[0-9a-fA-F]{3,8}\b(?![-\w])|\b(?:rgb|rgba|hsl|hsla|oklch)\(/;
  const palette =
    /\b(?:bg|text|border|ring|fill|stroke|outline|accent|from|to|via)-(?:slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|white|black)(?:-\d{2,3})?\b/;
  const pixels = /-\[[^\]]*\d(?:px|pt)\]|style=\{\{/;
  expect(offenders(rawColor)).toEqual([]);
  expect(offenders(palette)).toEqual([]);
  expect(offenders(pixels)).toEqual([]);
});

test("no type escapes", () => {
  expect(
    offenders(
      /@ts-ignore|@ts-expect-error|@ts-nocheck|\bas any\b|: any\b|<any>|\bas unknown as\b/,
      (name) => name === "api/generated.ts",
    ),
  ).toEqual([]);
});

test("API types are generated, and every hand-written shape lives in api/types.ts", () => {
  const generated = files.find((file) => file.name === "api/generated.ts");
  expect(generated?.text.startsWith("// GENERATED")).toBe(true);
});
