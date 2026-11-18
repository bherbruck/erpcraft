import { describe, expect, test } from "bun:test";
import { celString, joinAll, toErpcraftQuery } from "../src/core/erpcraft";
import type { ColumnFilter, GridQueryState } from "../src/types";

const today = new Date(2026, 10, 18);

function state(over: Partial<GridQueryState> = {}): GridQueryState {
  return { sort: [], filters: [], search: "", page: { index: 0, size: 50 }, ...over };
}

function filter(field: string, type: ColumnFilter["type"], condition: ColumnFilter["condition"]): ColumnFilter {
  return { columnId: field, field, type, condition };
}

describe("entity.list", () => {
  test("the empty state is the plain first page", () => {
    expect(toErpcraftQuery(state(), { kind: "entity", entity: "item" })).toEqual({
      command: "entity.list",
      input: {
        entity: "item",
        filters: {},
        q: null,
        where: null,
        sort: [],
        limit: 50,
        offset: 0,
        active_only: false,
      },
      unsupported: [],
    });
  });

  test("sort, search, page and the consumer's own filters", () => {
    const query = toErpcraftQuery(
      state({
        sort: [
          { columnId: "name", field: "name", desc: false },
          { columnId: "price", field: "price", desc: true },
        ],
        search: "  bolt ",
        page: { index: 2, size: 100 },
      }),
      { kind: "entity", entity: "item", filters: { company_id: "c1" }, activeOnly: true },
    );
    expect(query.input).toMatchObject({
      sort: ["name", "-price"],
      q: "bolt",
      limit: 100,
      offset: 200,
      filters: { company_id: "c1" },
      active_only: true,
    });
  });

  test("each filter type becomes a condition over record", () => {
    const where = (f: ColumnFilter) =>
      toErpcraftQuery(state({ filters: [f] }), { kind: "entity", entity: "x" }, { today }).input.where;
    expect(where(filter("name", "text", { kind: "text", op: "equals", value: "O'Brien" }))).toBe(
      "record.name == 'O\\'Brien'",
    );
    expect(where(filter("name", "text", { kind: "text", op: "empty" }))).toBe("(record.name == null || record.name == '')");
    expect(where(filter("name", "text", { kind: "text", op: "notEmpty" }))).toBe(
      "(record.name != null && record.name != '')",
    );
    expect(where(filter("price", "money", { kind: "number", op: "ge", value: "1,000" }))).toBeNull(); // not a decimal: incomplete
    expect(where(filter("price", "money", { kind: "number", op: "ge", value: "1000.50" }))).toBe(
      "record.price >= '1000.5'",
    );
    expect(where(filter("qty", "number", { kind: "number", op: "ne", value: "0" }))).toBe("record.qty != '0'");
    expect(where(filter("qty", "number", { kind: "number", op: "between", value: "1", to: "9" }))).toBe(
      "(record.qty >= '1' && record.qty <= '9')",
    );
    expect(where(filter("qty", "number", { kind: "number", op: "empty" }))).toBe("(record.qty == null)");
    expect(where(filter("doc_date", "date", { kind: "date", op: "on", value: "2026-11-18" }))).toBe(
      "record.doc_date == '2026-11-18'",
    );
    expect(where(filter("doc_date", "date", { kind: "date", op: "before", value: "2026-11-18" }))).toBe(
      "record.doc_date <= '2026-11-17'",
    );
    expect(where(filter("doc_date", "date", { kind: "date", op: "after", value: "2026-11-18" }))).toBe(
      "record.doc_date >= '2026-11-19'",
    );
    expect(where(filter("doc_date", "date", { kind: "date", op: "relative", range: "lastMonth" }))).toBe(
      "(record.doc_date >= '2026-10-01' && record.doc_date <= '2026-10-31')",
    );
    expect(where(filter("created_at", "datetime", { kind: "date", op: "on", value: "2026-11-18" }))).toBe(
      "(record.created_at >= '2026-11-18' && record.created_at < '2026-11-19')",
    );
    expect(where(filter("status", "state", { kind: "set", values: ["draft", "posted"] }))).toBe(
      "record.status in ['draft', 'posted']",
    );
    expect(where(filter("vendor_id", "reference", { kind: "set", values: ["v1"], includeEmpty: true }))).toBe(
      "(record.vendor_id in ['v1'] || record.vendor_id == null)",
    );
    expect(where(filter("vendor_id", "reference", { kind: "set", values: [], includeEmpty: true }))).toBe(
      "(record.vendor_id == null)",
    );
    expect(where(filter("is_group", "boolean", { kind: "boolean", value: false }))).toBe("record.is_group == false");
  });

  test("filters join with && in a balanced tree, after the consumer's condition", () => {
    const query = toErpcraftQuery(
      state({
        filters: [
          filter("a", "text", { kind: "text", op: "equals", value: "1" }),
          filter("b", "text", { kind: "text", op: "equals", value: "2" }),
          filter("c", "text", { kind: "text", op: "equals", value: "3" }),
        ],
      }),
      { kind: "entity", entity: "x", where: "record.active == true" },
    );
    expect(query.input.where).toBe(
      "(((record.active == true) && record.a == '1') && (record.b == '2' && record.c == '3'))",
    );
  });

  test("what the API cannot express is left out and reported", () => {
    const query = toErpcraftQuery(
      state({
        filters: [
          filter("name", "text", { kind: "text", op: "contains", value: "bolt" }),
          filter("sku", "text", { kind: "text", op: "startsWith", value: "B-" }),
          filter("qty", "number", { kind: "number", op: "gt", value: "1" }),
        ],
        page: { index: 0, size: 1000 },
      }),
      { kind: "entity", entity: "item" },
    );
    expect(query.input.where).toBe("record.qty > '1'");
    expect(query.input.limit).toBe(500);
    expect(query.unsupported.map((u) => [u.what, u.columnId])).toEqual([
      ["filter", "name"],
      ["filter", "sku"],
      ["page", null],
    ]);
  });

  test("a condition longer than the server parses is refused, not truncated", () => {
    const values = Array.from({ length: 400 }, (_, i) => `00000000-0000-0000-0000-${String(i).padStart(12, "0")}`);
    const query = toErpcraftQuery(state({ filters: [filter("customer_id", "reference", { kind: "set", values })] }), {
      kind: "entity",
      entity: "x",
    });
    expect(query.input.where).toBeNull();
    expect(query.unsupported[0]?.reason).toContain("4000");
  });

  test("field names that are not plain identifiers are refused", () => {
    const query = toErpcraftQuery(
      state({ filters: [filter("a) || (true", "text", { kind: "text", op: "equals", value: "x" })] }),
      { kind: "entity", entity: "x" },
    );
    expect(query.input.where).toBeNull();
    expect(query.unsupported).toHaveLength(1);
  });
});

describe("document.list", () => {
  test("header fields in where, summary fields as exact values, sort by summary", () => {
    const query = toErpcraftQuery(
      state({
        filters: [
          filter("status", "state", { kind: "set", values: ["draft", "open"] }),
          filter("doc_date", "date", { kind: "date", op: "relative", range: "thisYear" }),
          { columnId: "vendor", field: "summary.vendor_id", type: "reference", condition: { kind: "set", values: ["v9"] } },
          { columnId: "total", field: "summary.total", type: "money", condition: { kind: "number", op: "gt", value: "5" } },
          { columnId: "warehouse", field: "warehouse_id", type: "reference", condition: { kind: "set", values: ["w1"] } },
        ],
        sort: [
          { columnId: "total", field: "summary.total", desc: true },
          { columnId: "memo", field: "memo", desc: false },
        ],
        search: "PO-1",
      }),
      { kind: "document", docType: "purchase_order", companyId: "c1" },
      { today },
    );
    expect(query).toEqual({
      command: "document.list",
      input: {
        doc_type: "purchase_order",
        company_id: "c1",
        status: null,
        q: "PO-1",
        where: "(record.status in ['draft', 'open'] && (record.doc_date >= '2026-01-01' && record.doc_date <= '2026-12-31'))",
        sort: ["-summary.total"],
        limit: 50,
        offset: 0,
        summary: { vendor_id: "v9" },
      },
      unsupported: [
        { columnId: "total", what: "filter", reason: "a document summary field can only be filtered to one exact value" },
        { columnId: "warehouse", what: "filter", reason: 'a document list can filter only by its header fields, not "warehouse_id"' },
        { columnId: "memo", what: "sort", reason: 'a document list cannot sort by "memo"' },
      ],
    });
  });
});

test("string literals escape what the rule language needs", () => {
  expect(celString("a'b\\c\nd")).toBe("'a\\'b\\\\c\\nd'");
  expect(joinAll(["a"], "&&")).toBe("a");
  expect(joinAll([], "&&")).toBe("");
});
