import { describe, expect, test } from "bun:test";
import { parseTsv, toHtmlTable, toTsv } from "../src/core/tsv";

describe("parseTsv", () => {
  test("cells and rows", () => {
    expect(parseTsv("a\tb\r\nc\td")).toEqual([
      ["a", "b"],
      ["c", "d"],
    ]);
  });

  test("Excel's trailing line break starts no row", () => {
    expect(parseTsv("a\tb\r\nc\td\r\n")).toEqual([
      ["a", "b"],
      ["c", "d"],
    ]);
    expect(parseTsv("x\n")).toEqual([["x"]]);
  });

  test("LF and lone CR end rows too", () => {
    expect(parseTsv("a\nb\rc")).toEqual([["a"], ["b"], ["c"]]);
  });

  test("a quoted cell keeps tabs, line breaks and doubled quotes", () => {
    expect(parseTsv('"line 1\nline 2"\t"a\tb"\t"say ""hi"""\r\nnext')).toEqual([
      ["line 1\nline 2", "a\tb", 'say "hi"'],
      ["next"],
    ]);
  });

  test("quotes inside an unquoted cell are text", () => {
    expect(parseTsv('5" pipe\t3\'')).toEqual([['5" pipe', "3'"]]);
  });

  test("an unclosed quote is text, as Excel reads it", () => {
    expect(parseTsv('"open')).toEqual([['"open']]);
  });

  test("empty cells, leading and trailing", () => {
    expect(parseTsv("\ta\t\r\n\t\t")).toEqual([
      ["", "a", ""],
      ["", "", ""],
    ]);
  });

  test("a blank row in the middle is kept", () => {
    expect(parseTsv("a\r\n\r\nb\r\n")).toEqual([["a"], [""], ["b"]]);
  });

  test("nothing is no rows", () => {
    expect(parseTsv("")).toEqual([]);
  });
});

describe("toTsv", () => {
  test("quotes only what needs it, and round-trips", () => {
    const rows = [
      ["plain", "with\ttab", "two\nlines", 'a "quote"'],
      ["", "1,234.50", "x", ""],
    ];
    const text = toTsv(rows);
    expect(text).toBe('plain\t"with\ttab"\t"two\nlines"\t"a ""quote"""\r\n\t1,234.50\tx\t');
    expect(parseTsv(text)).toEqual(rows);
    expect(parseTsv(`${text}\r\n`)).toEqual(rows);
  });

  test("html table escapes", () => {
    expect(toHtmlTable([["<b>", "a&b"]])).toBe("<table><tr><td>&lt;b&gt;</td><td>a&amp;b</td></tr></table>");
  });
});
