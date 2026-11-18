import { describe, expect, test } from "bun:test";
import {
  type Cell,
  edgeOfData,
  handleKey,
  initialNav,
  type KeyInput,
  type NavContext,
  type NavState,
  rangeOf,
} from "../src/core/navigation";

// A 10 x 5 sheet. Cells in `blank` are empty; all cells are editable except column 0.
function sheet(overrides: Partial<NavContext> = {}, blank: Set<string> = new Set()): NavContext {
  return {
    rowCount: 10,
    colCount: 5,
    pageRows: 4,
    headerRow: true,
    isEmpty: (r, c) => blank.has(`${r},${c}`),
    isEditable: (_r, c) => c !== 0,
    editText: (r, c) => `v${r}${c}`,
    ...overrides,
  };
}

function at(row: number, col: number): NavState {
  const cell = { row, col };
  return { ...initialNav(), active: cell, anchor: cell, extent: cell };
}

function press(state: NavState, keys: (string | KeyInput)[], ctx = sheet()) {
  let current = state;
  const effects = [];
  let handled = true;
  for (const key of keys) {
    const input = typeof key === "string" ? { key } : key;
    const result = handleKey(current, input, ctx);
    current = result.state;
    effects.push(...result.effects);
    handled = result.handled;
  }
  return { state: current, effects, handled };
}

const pos = (s: NavState): Cell => s.active;

describe("moving", () => {
  test("arrows move one cell and stop at the edges", () => {
    expect(pos(press(at(0, 0), ["ArrowRight", "ArrowDown", "ArrowDown"]).state)).toEqual({ row: 2, col: 1 });
    expect(pos(press(at(9, 4), ["ArrowDown", "ArrowRight"]).state)).toEqual({ row: 9, col: 4 });
    expect(pos(press(at(3, 0), ["ArrowLeft"]).state)).toEqual({ row: 3, col: 0 });
  });

  test("ArrowUp from the first row reaches the header, ArrowDown comes back", () => {
    const up = press(at(0, 2), ["ArrowUp"]).state;
    expect(pos(up)).toEqual({ row: -1, col: 2 });
    expect(pos(press(up, ["ArrowUp"]).state)).toEqual({ row: -1, col: 2 });
    expect(pos(press(up, ["ArrowRight", "ArrowDown"]).state)).toEqual({ row: 0, col: 3 });
  });

  test("Tab and Shift+Tab move across; at the edge Tab is left to the browser", () => {
    expect(pos(press(at(2, 1), ["Tab", "Tab"]).state)).toEqual({ row: 2, col: 3 });
    expect(pos(press(at(2, 3), [{ key: "Tab", shift: true }]).state)).toEqual({ row: 2, col: 2 });
    const edge = press(at(2, 4), ["Tab"]);
    expect(edge.handled).toBe(false);
    expect(pos(edge.state)).toEqual({ row: 2, col: 4 });
    expect(press(at(2, 0), [{ key: "Tab", shift: true }]).handled).toBe(false);
  });

  test("Enter after a run of Tabs returns to the column the run started in (read-only cell)", () => {
    const ctx = sheet({ isEditable: () => false });
    const s = press(at(1, 1), ["Tab", "Tab", "Enter"], ctx).state;
    expect(pos(s)).toEqual({ row: 2, col: 1 });
  });

  test("Enter moves down and Shift+Enter up when Enter does not edit (Excel)", () => {
    const ctx = sheet({ enterEdits: false });
    expect(pos(press(at(1, 2), ["Enter"], ctx).state)).toEqual({ row: 2, col: 2 });
    expect(pos(press(at(1, 2), [{ key: "Enter", shift: true }], ctx).state)).toEqual({ row: 0, col: 2 });
  });

  test("Home, End, Ctrl+Home, Ctrl+End", () => {
    expect(pos(press(at(4, 2), ["Home"]).state)).toEqual({ row: 4, col: 0 });
    expect(pos(press(at(4, 2), ["End"]).state)).toEqual({ row: 4, col: 4 });
    expect(pos(press(at(4, 2), [{ key: "Home", ctrl: true }]).state)).toEqual({ row: 0, col: 0 });
    expect(pos(press(at(4, 2), [{ key: "End", ctrl: true }]).state)).toEqual({ row: 9, col: 4 });
  });

  test("PageDown and PageUp move by a page and stop at the ends, not at the header", () => {
    expect(pos(press(at(1, 1), ["PageDown"]).state)).toEqual({ row: 5, col: 1 });
    expect(pos(press(at(8, 1), ["PageDown"]).state)).toEqual({ row: 9, col: 1 });
    expect(pos(press(at(2, 1), ["PageUp"]).state)).toEqual({ row: 0, col: 1 });
  });
});

describe("Ctrl+arrow, to the edge of the data", () => {
  // Column 1: rows 0-3 filled, 4-6 empty, 7-9 filled.
  const blank = new Set(["4,1", "5,1", "6,1"]);
  const ctx = sheet({}, blank);

  test("inside a block: to its last cell", () => {
    expect(edgeOfData({ row: 0, col: 1 }, "down", ctx)).toEqual({ row: 3, col: 1 });
  });
  test("at the end of a block: to the start of the next", () => {
    expect(edgeOfData({ row: 3, col: 1 }, "down", ctx)).toEqual({ row: 7, col: 1 });
  });
  test("from an empty cell: to the next filled one", () => {
    expect(edgeOfData({ row: 5, col: 1 }, "up", ctx)).toEqual({ row: 3, col: 1 });
  });
  test("no more data: to the edge of the grid", () => {
    expect(edgeOfData({ row: 9, col: 1 }, "down", ctx)).toEqual({ row: 9, col: 1 });
    const empty = sheet({ isEmpty: (_r, c) => c > 0 });
    expect(edgeOfData({ row: 0, col: 0 }, "right", empty)).toEqual({ row: 0, col: 4 });
  });
  test("through handleKey, with Shift extending", () => {
    const s = press(at(0, 1), [{ key: "ArrowDown", ctrl: true, shift: true }], ctx).state;
    expect(pos(s)).toEqual({ row: 0, col: 1 });
    expect(rangeOf(s)).toEqual({ top: 0, bottom: 3, left: 1, right: 1 });
  });
});

describe("selection", () => {
  test("Shift+arrows extend while the active cell stays", () => {
    const s = press(at(2, 2), [
      { key: "ArrowDown", shift: true },
      { key: "ArrowDown", shift: true },
      { key: "ArrowLeft", shift: true },
    ]).state;
    expect(pos(s)).toEqual({ row: 2, col: 2 });
    expect(rangeOf(s)).toEqual({ top: 2, bottom: 4, left: 1, right: 2 });
  });

  test("Shift+arrow back past the anchor flips the range", () => {
    const s = press(at(2, 2), [
      { key: "ArrowUp", shift: true },
      { key: "ArrowUp", shift: true },
    ]).state;
    expect(rangeOf(s)).toEqual({ top: 0, bottom: 2, left: 2, right: 2 });
  });

  test("a plain arrow collapses the selection", () => {
    const s = press(at(2, 2), [{ key: "ArrowDown", shift: true }, "ArrowRight"]).state;
    expect(rangeOf(s)).toEqual({ top: 2, bottom: 2, left: 3, right: 3 });
  });

  test("Ctrl+A selects everything and keeps the active cell", () => {
    const s = press(at(3, 3), [{ key: "a", ctrl: true }]).state;
    expect(pos(s)).toEqual({ row: 3, col: 3 });
    expect(rangeOf(s)).toEqual({ top: 0, bottom: 9, left: 0, right: 4 });
  });

  test("Shift+Space selects the row, Ctrl+Space the column", () => {
    expect(rangeOf(press(at(3, 2), [{ key: " ", shift: true }]).state)).toEqual({ top: 3, bottom: 3, left: 0, right: 4 });
    expect(rangeOf(press(at(3, 2), [{ key: " ", ctrl: true }]).state)).toEqual({ top: 0, bottom: 9, left: 2, right: 2 });
  });

  test("Escape collapses a selection", () => {
    const s = press(at(1, 1), [{ key: "ArrowDown", shift: true }, "Escape"]).state;
    expect(rangeOf(s)).toEqual({ top: 1, bottom: 1, left: 1, right: 1 });
  });

  test("Delete clears the selection", () => {
    const r = press(at(1, 1), [{ key: "ArrowRight", shift: true }, "Delete"]);
    expect(r.effects.at(-1)).toEqual({ type: "clear", range: { top: 1, bottom: 1, left: 1, right: 2 } });
  });
});

describe("editing", () => {
  test("typing replaces the value and starts an edit in enter mode", () => {
    const r = press(at(1, 1), ["x"]);
    expect(r.state.editing).toMatchObject({ row: 1, col: 1, mode: "enter", draft: "x", original: "v11" });
    expect(r.effects).toEqual([{ type: "beginEdit", row: 1, col: 1 }]);
  });

  test("F2 and Enter edit the current text", () => {
    expect(press(at(1, 2), ["F2"]).state.editing).toMatchObject({ mode: "edit", draft: "v12" });
    expect(press(at(1, 2), ["Enter"]).state.editing).toMatchObject({ mode: "edit", draft: "v12" });
  });

  test("a read-only cell does not edit: typing is ignored, Enter moves down", () => {
    expect(press(at(1, 0), ["x"]).state.editing).toBeNull();
    expect(pos(press(at(1, 0), ["Enter"]).state)).toEqual({ row: 2, col: 0 });
  });

  test("Escape cancels", () => {
    const r = press(at(1, 1), ["x", "Escape"]);
    expect(r.state.editing).toBeNull();
    expect(pos(r.state)).toEqual({ row: 1, col: 1 });
    expect(r.effects.at(-1)).toMatchObject({ type: "cancel" });
  });

  test("Enter commits and moves down; Shift+Enter up; Tab right; Shift+Tab left", () => {
    const enter = press(at(1, 1), ["x", "Enter"]);
    expect(enter.effects.at(-1)).toMatchObject({ type: "commit", edit: { row: 1, col: 1, draft: "x" } });
    expect(pos(enter.state)).toEqual({ row: 2, col: 1 });
    expect(enter.state.editing).toBeNull();
    expect(pos(press(at(1, 1), ["x", { key: "Enter", shift: true }]).state)).toEqual({ row: 0, col: 1 });
    expect(pos(press(at(1, 1), ["x", "Tab"]).state)).toEqual({ row: 1, col: 2 });
    expect(pos(press(at(1, 2), ["x", { key: "Tab", shift: true }]).state)).toEqual({ row: 1, col: 1 });
  });

  test("arrows commit and move in enter mode, but belong to the text in edit mode", () => {
    const enter = press(at(1, 1), ["x", "ArrowRight"]);
    expect(enter.effects.at(-1)).toMatchObject({ type: "commit" });
    expect(pos(enter.state)).toEqual({ row: 1, col: 2 });
    const edit = press(at(1, 1), ["F2", "ArrowRight"]);
    expect(edit.handled).toBe(false);
    expect(edit.state.editing).not.toBeNull();
  });

  test("F2 toggles between enter and edit modes", () => {
    expect(press(at(1, 1), ["x", "F2"]).state.editing?.mode).toBe("edit");
  });

  test("Backspace clears into an edit", () => {
    expect(press(at(1, 1), ["Backspace"]).state.editing).toMatchObject({ draft: "", mode: "enter" });
  });

  test("other keys while editing go to the editor", () => {
    const r = press(at(1, 1), ["x", "y"]);
    expect(r.handled).toBe(false);
  });

  test("Tab after typing, then Enter, returns to the first column on the next row", () => {
    const s = press(at(1, 1), ["a", "Tab", "b", "Tab", "c", "Enter"]).state;
    expect(pos(s)).toEqual({ row: 2, col: 1 });
  });

  test("undo and redo", () => {
    expect(press(at(0, 1), [{ key: "z", ctrl: true }]).effects).toEqual([{ type: "undo" }]);
    expect(press(at(0, 1), [{ key: "y", ctrl: true }]).effects).toEqual([{ type: "redo" }]);
    expect(press(at(0, 1), [{ key: "Z", ctrl: true, shift: true }]).effects).toEqual([{ type: "redo" }]);
  });

  test("Ctrl+C and Ctrl+V are left to the browser's copy and paste events", () => {
    expect(press(at(0, 1), [{ key: "c", ctrl: true }]).handled).toBe(false);
    expect(press(at(0, 1), [{ key: "v", ctrl: true }]).handled).toBe(false);
  });
});

describe("header row", () => {
  test("Enter sorts, Shift+Enter adds to the sort, Alt+ArrowDown opens the filter, Shift+F10 the menu", () => {
    const header = at(-1, 2);
    expect(press(header, ["Enter"]).effects).toEqual([{ type: "sort", col: 2, multi: false }]);
    expect(press(header, [{ key: "Enter", shift: true }]).effects).toEqual([{ type: "sort", col: 2, multi: true }]);
    expect(press(header, [{ key: "ArrowDown", alt: true }]).effects).toEqual([{ type: "openMenu", col: 2, menu: "filter" }]);
    expect(press(header, [{ key: "F10", shift: true }]).effects).toEqual([{ type: "openMenu", col: 2, menu: "column" }]);
    expect(press(header, ["ContextMenu"]).effects).toEqual([{ type: "openMenu", col: 2, menu: "column" }]);
  });

  test("Alt+ArrowDown on a cell opens its column's filter, as in Excel; Shift+F10 its menu", () => {
    expect(press(at(3, 1), [{ key: "ArrowDown", alt: true }]).effects).toEqual([{ type: "openMenu", col: 1, menu: "filter" }]);
    expect(press(at(3, 1), [{ key: "F10", shift: true }]).effects).toEqual([{ type: "openMenu", col: 1, menu: "column" }]);
  });
});

describe("line editing", () => {
  const lines = sheet({ lineEditing: true, rowCount: 3 });

  test("ArrowDown on the last row adds a row and moves into it", () => {
    const r = press(at(2, 1), ["ArrowDown"], lines);
    expect(r.effects).toEqual([{ type: "appendRow" }]);
    expect(pos(r.state)).toEqual({ row: 3, col: 1 });
  });

  test("Enter committing on the last row adds a row", () => {
    const r = press(at(2, 1), ["x", "Enter"], lines);
    expect(r.effects.map((e) => e.type)).toEqual(["beginEdit", "commit", "appendRow"]);
  });

  test("Tab past the last column goes on to the next line", () => {
    expect(pos(press(at(1, 4), ["Tab"], lines).state)).toEqual({ row: 2, col: 0 });
    const last = press(at(2, 4), ["Tab"], lines);
    expect(last.effects).toEqual([{ type: "appendRow" }]);
  });

  test("Ctrl++ inserts above, Ctrl+Enter below, Ctrl+- deletes the selected rows", () => {
    expect(press(at(1, 1), [{ key: "+", ctrl: true, shift: true }], lines).effects).toEqual([{ type: "insertRow", at: 1 }]);
    expect(press(at(1, 1), [{ key: "=", ctrl: true, shift: true }], lines).effects).toEqual([{ type: "insertRow", at: 1 }]);
    expect(press(at(1, 1), [{ key: "Enter", ctrl: true }], lines).effects).toEqual([{ type: "insertRow", at: 2 }]);
    const del = press(at(0, 1), [{ key: "ArrowDown", shift: true }, { key: "-", ctrl: true }], lines);
    expect(del.effects).toEqual([{ type: "deleteRows", from: 0, to: 1 }]);
  });

  test("without line editing, the last row does not grow", () => {
    expect(press(at(9, 1), ["ArrowDown"]).effects).toEqual([]);
  });
});
