import { describe, expect, test } from "bun:test";
import { celString, joinAll, toErpcraftQuery } from "../src/core/erpcraft";
import type { ColumnFilter, GridQueryState } from "../src/types";

const zone = { timeZone: "Europe/Berlin" };

function state(over: Partial<GridQueryState> = {}): GridQueryState {
  return { sort: [], filters: [], search: "", page: { index: 0, size: 50 }, ...over };
}

function filter(field: string, type: ColumnFilter["type"], condition: ColumnFilter["condition"]): ColumnFilter {
  return { columnId: field, field, type, condition };
}

/** The where and params one filter makes on an entity list. */
function one(f: ColumnFilter) {
  const q = toErpcraftQuery(state({ filters: [f] }), { kind: "entity", entity: "x" }, zone);
  expect(q.unsupported).toEqual([]);
  return { where: q.input.where, params: q.input.params };
}

describe("entity.list", () => {
  test("the empty state is the plain first page, in the reader's zone", () => {
    expect(toErpcraftQuery(state(), { kind: "entity", entity: "item" }, zone)).toEqual({
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
        params: {},
        time_zone: "Europe/Berlin",
      },
      unsupported: [],
    });
    // Without a zone given, the browser's; null sends none.
    expect(typeof toErpcraftQuery(state(), { kind: "entity", entity: "item" }).input.time_zone).toBe("string");
    expect(toErpcraftQuery(state(), { kind: "entity", entity: "item" }, { timeZone: null }).input.time_zone).toBeNull();
  });

  test("sort with empty values last, search, page and the consumer's own filters", () => {
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
      zone,
    );
    expect(query.input).toMatchObject({
      sort: ["name nulls last", "-price nulls last"],
      q: "bolt",
      limit: 100,
      offset: 200,
      filters: { company_id: "c1" },
      active_only: true,
    });
  });

  test("text filters ignore case, as the grid does in memory", () => {
    expect(one(filter("name", "text", { kind: "text", op: "equals", value: "O'Brien" })).where).toBe(
      "record.name.equalsIgnoreCase('O\\'Brien')",
    );
    expect(one(filter("name", "text", { kind: "text", op: "contains", value: "50%" })).where).toBe(
      "record.name.containsIgnoreCase('50%')",
    );
    expect(one(filter("sku", "text", { kind: "text", op: "startsWith", value: "B-" })).where).toBe(
      "record.sku.startsWithIgnoreCase('B-')",
    );
    expect(one(filter("sku", "text", { kind: "text", op: "endsWith", value: "-X" })).where).toBe(
      "record.sku.endsWithIgnoreCase('-X')",
    );
    expect(one(filter("name", "text", { kind: "text", op: "empty" })).where).toBe(
      "(record.name == null || record.name == '')",
    );
    expect(one(filter("name", "text", { kind: "text", op: "notEmpty" })).where).toBe(
      "(record.name != null && record.name != '')",
    );
  });

  test("numbers compare as exact decimals", () => {
    expect(one(filter("price", "money", { kind: "number", op: "ge", value: "1000.50" })).where).toBe(
      "record.price >= '1000.5'",
    );
    expect(one(filter("qty", "number", { kind: "number", op: "ne", value: "0" })).where).toBe("record.qty != '0'");
    expect(one(filter("qty", "number", { kind: "number", op: "between", value: "9", to: "1" })).where).toBe(
      "(record.qty >= '1' && record.qty <= '9')",
    );
    expect(one(filter("qty", "number", { kind: "number", op: "empty" })).where).toBe("(record.qty == null)");
    // "1,000" is not a decimal: the filter is not complete and filters nothing.
    const q = toErpcraftQuery(
      state({ filters: [filter("price", "money", { kind: "number", op: "ge", value: "1,000" })] }),
      { kind: "entity", entity: "x" },
      zone,
    );
    expect(q.input.where).toBeNull();
  });

  test("dates: fixed days, and relative ranges the server works out from today()", () => {
    const d = (condition: ColumnFilter["condition"], type: ColumnFilter["type"] = "date") =>
      one(filter(type === "date" ? "doc_date" : "created_at", type, condition)).where;
    expect(d({ kind: "date", op: "on", value: "2026-11-18" })).toBe("record.doc_date == '2026-11-18'");
    expect(d({ kind: "date", op: "before", value: "2026-11-18" })).toBe("record.doc_date < '2026-11-18'");
    expect(d({ kind: "date", op: "after", value: "2026-11-18" })).toBe("record.doc_date >= '2026-11-19'");
    expect(d({ kind: "date", op: "between", value: "2026-11-30", to: "2026-11-01" })).toBe(
      "(record.doc_date >= '2026-11-01' && record.doc_date <= '2026-11-30')",
    );
    expect(d({ kind: "date", op: "relative", range: "today" })).toBe("record.doc_date == today()");
    expect(d({ kind: "date", op: "relative", range: "lastMonth" })).toBe(
      "(record.doc_date >= startOfMonth(addMonths(today(), -1)) && record.doc_date <= endOfMonth(addMonths(today(), -1)))",
    );
    expect(d({ kind: "date", op: "relative", range: "thisQuarter" })).toBe(
      "(record.doc_date >= startOfQuarter(today()) && record.doc_date <= endOfQuarter(today()))",
    );
    // A timestamp is on a day from its midnight to the next, in the reader's zone (sent as time_zone).
    expect(d({ kind: "date", op: "on", value: "2026-11-18" }, "datetime")).toBe(
      "(record.created_at >= '2026-11-18' && record.created_at < '2026-11-19')",
    );
    expect(d({ kind: "date", op: "relative", range: "thisYear" }, "datetime")).toBe(
      "(record.created_at >= startOfYear(today()) && record.created_at < addDays(endOfYear(today()), 1))",
    );
    expect(d({ kind: "date", op: "relative", range: "today" }, "datetime")).toBe(
      "(record.created_at >= today() && record.created_at < addDays(today(), 1))",
    );
  });

  test("checklists travel as bound params, however long", () => {
    const ids = Array.from({ length: 500 }, (_, i) => `00000000-0000-0000-0000-${String(i).padStart(12, "0")}`);
    const r = one(filter("customer_id", "reference", { kind: "set", values: ids }));
    expect(r.where).toBe("record.customer_id in params.g0");
    expect(r.params).toEqual({ g0: ids });
    expect(one(filter("vendor_id", "reference", { kind: "set", values: ["v1"], includeEmpty: true }))).toEqual({
      where: "(record.vendor_id in params.g0 || record.vendor_id == null)",
      params: { g0: ["v1"] },
    });
    expect(one(filter("vendor_id", "reference", { kind: "set", values: [], includeEmpty: true })).where).toBe(
      "(record.vendor_id == null)",
    );
    expect(one(filter("is_group", "boolean", { kind: "boolean", value: false })).where).toBe("record.is_group == false");
  });

  test("filters join with && in a balanced tree after the consumer's condition; params merge", () => {
    const query = toErpcraftQuery(
      state({
        filters: [
          filter("a", "text", { kind: "text", op: "equals", value: "1" }),
          filter("b", "state", { kind: "set", values: ["x"] }),
          filter("c", "state", { kind: "set", values: ["y"] }),
        ],
      }),
      { kind: "entity", entity: "x", where: "record.owner in params.mine", params: { mine: ["u1"] } },
      zone,
    );
    expect(query.input.where).toBe(
      "(((record.owner in params.mine) && record.a.equalsIgnoreCase('1')) && (record.b in params.g0 && record.c in params.g1))",
    );
    expect(query.input.params).toEqual({ mine: ["u1"], g0: ["x"], g1: ["y"] });
  });

  test("what the API cannot express is left out and reported", () => {
    const query = toErpcraftQuery(
      state({
        filters: [filter("a) || (true", "text", { kind: "text", op: "equals", value: "x" })],
        page: { index: 0, size: 1000 },
      }),
      { kind: "entity", entity: "item" },
      zone,
    );
    expect(query.input.where).toBeNull();
    expect(query.input.limit).toBe(500);
    expect(query.unsupported.map((u) => [u.what, u.columnId])).toEqual([
      ["filter", "a) || (true"],
      ["page", null],
    ]);
  });
});

describe("document.list", () => {
  test("header and summary fields in where; sort by summary; other fields reported", () => {
    const query = toErpcraftQuery(
      state({
        filters: [
          filter("status", "state", { kind: "set", values: ["draft", "open"] }),
          filter("doc_date", "date", { kind: "date", op: "relative", range: "thisYear" }),
          { columnId: "vendor", field: "summary.vendor_id", type: "reference", condition: { kind: "set", values: ["v9", "v8"] } },
          { columnId: "total", field: "summary.total", type: "money", condition: { kind: "number", op: "gt", value: "5" } },
          { columnId: "warehouse", field: "warehouse_id", type: "reference", condition: { kind: "set", values: ["w1"] } },
        ],
        sort: [
          { columnId: "total", field: "summary.total", desc: true },
          { columnId: "memo", field: "memo", desc: false },
          { columnId: "status", field: "status", desc: false },
        ],
        search: "PO-1",
      }),
      { kind: "document", docType: "purchase_order", companyId: "c1" },
      zone,
    );
    expect(query).toEqual({
      command: "document.list",
      input: {
        doc_type: "purchase_order",
        company_id: "c1",
        status: null,
        q: "PO-1",
        where:
          "((record.status in params.g0 && (record.doc_date >= startOfYear(today()) && record.doc_date <= endOfYear(today()))) && " +
          "(record.summary.vendor_id in params.g1 && record.summary.total > '5'))",
        sort: ["-summary.total nulls last", "status nulls last"],
        limit: 50,
        offset: 0,
        summary: {},
        params: { g0: ["draft", "open"], g1: ["v9", "v8"] },
        time_zone: "Europe/Berlin",
      },
      unsupported: [
        {
          columnId: "warehouse",
          what: "filter",
          reason: 'a document list filters by its header and summary fields, not "warehouse_id"',
        },
        { columnId: "memo", what: "sort", reason: 'a list cannot sort by "memo"' },
      ],
    });
  });
});

test("string literals escape what the rule language needs", () => {
  expect(celString("a'b\\c\nd")).toBe("'a\\'b\\\\c\\nd'");
  expect(joinAll(["a"], "&&")).toBe("a");
  expect(joinAll([], "&&")).toBe("");
});
