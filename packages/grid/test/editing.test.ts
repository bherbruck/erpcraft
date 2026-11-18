import { describe, expect, test } from "bun:test";
import { copyRange, EditHistory, planClear, planEdit, planPaste, type Sheet } from "../src/core/editing";
import { parseTsv, toTsv } from "../src/core/tsv";
import type { GridColumn } from "../src/types";

interface Row {
  id: string;
  name: string;
  qty: string | null;
  status: string;
  locked?: boolean;
}

const columns: GridColumn<Row>[] = [
  { id: "id", header: "Id", type: "text" },
  { id: "name", header: "Name", type: "text", editable: (row) => !row.locked },
  {
    id: "qty",
    header: "Qty",
    type: "number",
    editable: true,
    validate: (v) => (typeof v === "string" && v.startsWith("-") ? "must not be negative" : null),
  },
  {
    id: "status",
    header: "Status",
    type: "state",
    editable: true,
    options: [
      { value: "draft", label: "Draft" },
      { value: "posted", label: "Posted" },
    ],
  },
];

const rows: Row[] = [
  { id: "a", name: "Alpha", qty: "1", status: "draft" },
  { id: "b", name: "Beta", qty: "2", status: "posted", locked: true },
  { id: "c", name: "Gamma\twith tab", qty: null, status: "draft" },
];

const sheet: Sheet<Row> = { rows, columns, rowId: (r) => r.id, locale: "en-US" };

describe("copy", () => {
  test("copies the range as shown, and as TSV that pastes back", () => {
    const cells = copyRange(sheet, { top: 0, bottom: 2, left: 1, right: 3 });
    expect(cells).toEqual([
      ["Alpha", "1", "Draft"],
      ["Beta", "2", "Posted"],
      ["Gamma\twith tab", "", "Draft"],
    ]);
    expect(parseTsv(toTsv(cells))).toEqual(cells);
  });
});

describe("paste", () => {
  test("a block from Excel lands from the top left, cell by cell", () => {
    const plan = planPaste(sheet, { top: 0, bottom: 0, left: 2, right: 2 }, parseTsv("5\tPosted\r\n6\tdraft\r\n"));
    expect(plan.rejected).toEqual([]);
    expect(plan.changes.map((c) => [c.rowId, c.columnId, c.oldValue, c.newValue])).toEqual([
      ["a", "qty", "1", "5"],
      ["a", "status", "draft", "posted"],
      ["b", "qty", "2", "6"],
      ["b", "status", "posted", "draft"],
    ]);
  });

  test("rejected cells are reported with the reason; the others are applied", () => {
    const plan = planPaste(
      sheet,
      { top: 0, bottom: 0, left: 1, right: 1 },
      parseTsv("New A\tabc\tDraft\r\nNew B\t-3\tVoid\r\nNew C\t7\tDraft\r\nNew D\t1\tDraft"),
    );
    expect(plan.changes.map((c) => `${c.rowId}.${c.columnId}=${c.newValue}`)).toEqual([
      "a.name=New A",
      "a.status=draft",
      "c.name=New C",
      "c.qty=7",
      "c.status=draft",
    ]);
    expect(plan.rejected.map((r) => [r.rowIndex, r.columnId, r.reason])).toEqual([
      [0, "qty", '"abc" is not a number'],
      [1, "name", "read-only"],
      [1, "qty", "must not be negative"],
      [1, "status", '"Void" is not one of the choices'],
      [3, "name", "outside the grid"],
      [3, "qty", "outside the grid"],
      [3, "status", "outside the grid"],
    ]);
  });

  test("columns past the last are outside the grid", () => {
    const plan = planPaste(sheet, { top: 0, bottom: 0, left: 3, right: 3 }, [["draft", "extra"]]);
    expect(plan.rejected).toEqual([{ rowIndex: 0, columnId: "#4", text: "extra", reason: "outside the grid" }]);
  });

  test("one copied cell fills the whole selection", () => {
    const plan = planPaste(sheet, { top: 0, bottom: 2, left: 2, right: 2 }, [["9"]]);
    expect(plan.changes.map((c) => [c.rowId, c.newValue])).toEqual([
      ["a", "9"],
      ["b", "9"],
      ["c", "9"],
    ]);
  });

  test("numbers as Excel formats them", () => {
    const plan = planPaste(sheet, { top: 0, bottom: 0, left: 2, right: 2 }, [["1,234.50"]]);
    expect(plan.changes[0]?.newValue).toBe("1234.5");
  });
});

describe("clear and edit", () => {
  test("Delete clears editable cells only", () => {
    const plan = planClear(sheet, { top: 0, bottom: 1, left: 0, right: 2 });
    expect(plan.changes.map((c) => `${c.rowId}.${c.columnId}`)).toEqual(["a.name", "a.qty", "b.qty"]);
  });

  test("a typed edit is parsed and validated", () => {
    expect(planEdit(sheet, 0, 2, "12").changes[0]?.newValue).toBe("12");
    expect(planEdit(sheet, 0, 2, "-1").rejected[0]?.reason).toBe("must not be negative");
    expect(planEdit(sheet, 0, 2, "1").changes).toEqual([]); // unchanged
  });
});

describe("history", () => {
  test("undo gives the inverse of the last batch, redo gives it back", () => {
    const history = new EditHistory();
    const first = planPaste(sheet, { top: 0, bottom: 0, left: 2, right: 2 }, [["5"], ["6"]]).changes;
    history.record("paste", first);
    history.record("edit", planEdit(sheet, 0, 1, "Zed").changes);
    expect(history.undo()).toEqual([{ rowId: "a", columnId: "name", oldValue: "Zed", newValue: "Alpha" }]);
    expect(history.undo()).toEqual([
      { rowId: "b", columnId: "qty", oldValue: "6", newValue: "2" },
      { rowId: "a", columnId: "qty", oldValue: "5", newValue: "1" },
    ]);
    expect(history.undo()).toBeNull();
    expect(history.redo()?.map((c) => c.newValue)).toEqual(["5", "6"]);
    history.record("edit", planEdit(sheet, 0, 1, "Q").changes);
    expect(history.canRedo).toBe(false);
  });
});
