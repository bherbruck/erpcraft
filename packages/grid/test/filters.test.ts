import { describe, expect, test } from "bun:test";
import { describeFilter, isComplete, matches, matchesSearch, rowPasses } from "../src/core/filters";
import type { GridColumn } from "../src/types";

const today = new Date(2026, 10, 18);

describe("conditions", () => {
  test("text", () => {
    expect(matches({ kind: "text", op: "contains", value: "ACME" }, "Acme Corp")).toBe(true);
    expect(matches({ kind: "text", op: "startsWith", value: "corp" }, "Acme Corp")).toBe(false);
    expect(matches({ kind: "text", op: "equals", value: "acme corp" }, "Acme Corp")).toBe(true);
    expect(matches({ kind: "text", op: "empty" }, "")).toBe(true);
    expect(matches({ kind: "text", op: "notEmpty" }, null)).toBe(false);
  });

  test("numbers compare exactly, between in either order", () => {
    expect(matches({ kind: "number", op: "ge", value: "100" }, "100.00")).toBe(true);
    expect(matches({ kind: "number", op: "lt", value: "0.3" }, "0.29999999999999999")).toBe(true);
    expect(matches({ kind: "number", op: "ne", value: "5" }, 5)).toBe(false);
    expect(matches({ kind: "number", op: "between", value: "10", to: "1" }, "5")).toBe(true);
    expect(matches({ kind: "number", op: "eq", value: "5" }, { amount: "5.00", currency: "EUR" })).toBe(true);
    expect(matches({ kind: "number", op: "gt", value: "1" }, null)).toBe(false);
  });

  test("dates, including relative ranges", () => {
    expect(matches({ kind: "date", op: "on", value: "2026-11-18" }, "2026-11-18")).toBe(true);
    expect(matches({ kind: "date", op: "before", value: "2026-11-18" }, "2026-11-18")).toBe(false);
    expect(matches({ kind: "date", op: "after", value: "2026-11-17" }, "2026-11-18")).toBe(true);
    expect(matches({ kind: "date", op: "between", value: "2026-11-01", to: "2026-11-30" }, "2026-11-30")).toBe(true);
    expect(matches({ kind: "date", op: "relative", range: "lastMonth" }, "2026-10-31", today)).toBe(true);
    expect(matches({ kind: "date", op: "relative", range: "thisMonth" }, "2026-10-31", today)).toBe(false);
    expect(matches({ kind: "date", op: "relative", range: "thisQuarter" }, "2026-12-31", today)).toBe(true);
  });

  test("sets, with or without empty", () => {
    expect(matches({ kind: "set", values: ["a", "b"] }, "b")).toBe(true);
    expect(matches({ kind: "set", values: ["a"] }, null)).toBe(false);
    expect(matches({ kind: "set", values: ["a"], includeEmpty: true }, null)).toBe(true);
  });

  test("incomplete conditions filter nothing", () => {
    expect(isComplete({ kind: "number", op: "between", value: "1" })).toBe(false);
    expect(isComplete({ kind: "text", op: "contains", value: "" })).toBe(false);
    expect(isComplete({ kind: "set", values: [] })).toBe(false);
    expect(isComplete({ kind: "date", op: "relative" })).toBe(false);
  });
});

interface Row {
  name: string;
  status: string;
  total: string;
}
const columns: GridColumn<Row>[] = [
  { id: "name", header: "Name", type: "text" },
  {
    id: "status",
    header: "Status",
    type: "state",
    options: [
      { value: "draft", label: "Draft" },
      { value: "posted", label: "Posted" },
    ],
  },
  { id: "total", header: "Total", type: "money" },
];

test("rows pass every filter; search looks at text and labels", () => {
  const row = { name: "Acme", status: "posted", total: "10.00" };
  expect(rowPasses(columns, [{ columnId: "total", condition: { kind: "number", op: "gt", value: "5" } }], row)).toBe(true);
  expect(
    rowPasses(
      columns,
      [
        { columnId: "total", condition: { kind: "number", op: "gt", value: "5" } },
        { columnId: "status", condition: { kind: "set", values: ["draft"] } },
      ],
      row,
    ),
  ).toBe(false);
  expect(matchesSearch(columns, row, "post")).toBe(true); // the label "Posted"
  expect(matchesSearch(columns, row, "10")).toBe(false); // money is not searched
});

test("chips read as sentences", () => {
  const [name, status, total] = columns as [GridColumn<Row>, GridColumn<Row>, GridColumn<Row>];
  expect(describeFilter(total, { kind: "number", op: "between", value: "1000", to: "2000" }, undefined, "en-US")).toBe(
    "Total between 1,000 and 2,000",
  );
  expect(describeFilter(status, { kind: "set", values: ["draft", "posted"], includeEmpty: true })).toBe(
    "Status: Draft, Posted, (empty)",
  );
  expect(describeFilter(name, { kind: "text", op: "startsWith", value: "Ac" })).toBe('Name starts with "Ac"');
});
