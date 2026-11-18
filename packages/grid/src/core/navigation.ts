// The keyboard of the grid as a pure state machine: a key and the state
// before it give the state after it and what the grid has to do. Nothing
// here touches the DOM, so every rule is unit tested.
//
// The model follows Excel and Google Sheets:
// - the active cell is where typing goes; it stays put while Shift extends
//   the selection, which is the rectangle from `anchor` to `extent`;
// - an edit started by typing ("enter" mode) is committed by an arrow key,
//   which then moves; an edit started with F2, Enter or a double click
//   ("edit" mode) leaves the arrow keys to the text;
// - row -1 is the header row, reached with ArrowUp from the first row.

export interface Cell {
  row: number;
  col: number;
}

export interface Edit {
  row: number;
  col: number;
  mode: "enter" | "edit";
  /** The text being edited, as it is now. */
  draft: string;
  /** The text it started from, for Escape. */
  original: string;
}

export interface NavState {
  active: Cell;
  anchor: Cell;
  extent: Cell;
  editing: Edit | null;
  /** The column a run of Tabs started in: Enter goes back to it on the next row, as in Excel. */
  tabColumn: number | null;
}

export interface NavContext {
  rowCount: number;
  colCount: number;
  /** Rows one PageDown moves. */
  pageRows: number;
  /** Is the cell empty? For Ctrl+arrow, which stops at the edges of the data. */
  isEmpty(row: number, col: number): boolean;
  isEditable(row: number, col: number): boolean;
  /** The text an edit of this cell starts from (F2, Enter). */
  editText(row: number, col: number): string;
  /** Document lines: Enter or ArrowDown on the last row adds one; rows can be inserted and deleted. */
  lineEditing?: boolean;
  /** Can the header row be reached with the keyboard? */
  headerRow?: boolean;
  /** Does Enter on an editable cell start editing (Google Sheets) rather than move down (Excel)? Default true. */
  enterEdits?: boolean;
  /** Is the column sortable or has it a menu? For keys on the header row. */
  sortable?(col: number): boolean;
}

export interface KeyInput {
  key: string;
  shift?: boolean;
  /** Ctrl, or Cmd on a Mac. */
  ctrl?: boolean;
  alt?: boolean;
}

export type Effect =
  | { type: "commit"; edit: Edit }
  /** Ctrl+Enter while editing: the text goes into every cell of the selection, as in Excel. */
  | { type: "fill"; edit: Edit; range: Range }
  | { type: "cancel"; edit: Edit }
  | { type: "beginEdit"; row: number; col: number }
  | { type: "clear"; range: Range }
  | { type: "undo" }
  | { type: "redo" }
  | { type: "appendRow" }
  | { type: "insertRow"; at: number }
  | { type: "deleteRows"; from: number; to: number }
  | { type: "sort"; col: number; multi: boolean }
  /** "filter": the column's filter popover (Alt+ArrowDown, as Excel's filter button); "column": its menu. */
  | { type: "openMenu"; col: number; menu: "filter" | "column" };

export interface KeyResult {
  state: NavState;
  effects: Effect[];
  /** Did the grid act on the key? If not, the browser's default runs (Tab leaves the grid, Ctrl+C copies). */
  handled: boolean;
}

export interface Range {
  top: number;
  left: number;
  bottom: number;
  right: number;
}

export function initialNav(): NavState {
  const origin = { row: 0, col: 0 };
  return { active: origin, anchor: origin, extent: origin, editing: null, tabColumn: null };
}

export function rangeOf(state: NavState): Range {
  return {
    top: Math.min(state.anchor.row, state.extent.row),
    bottom: Math.max(state.anchor.row, state.extent.row),
    left: Math.min(state.anchor.col, state.extent.col),
    right: Math.max(state.anchor.col, state.extent.col),
  };
}

export function inRange(range: Range, row: number, col: number): boolean {
  return row >= range.top && row <= range.bottom && col >= range.left && col <= range.right;
}

export function isSingleCell(range: Range): boolean {
  return range.top === range.bottom && range.left === range.right;
}

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

/** Put a cell inside the data rows; an empty grid keeps (0, 0). */
export function clampCell(cell: Cell, ctx: Pick<NavContext, "rowCount" | "colCount">): Cell {
  return {
    row: clamp(cell.row, 0, Math.max(ctx.rowCount - 1, 0)),
    col: clamp(cell.col, 0, Math.max(ctx.colCount - 1, 0)),
  };
}

/** Move to one cell: the selection becomes that cell. */
export function moveTo(state: NavState, cell: Cell): NavState {
  return { ...state, active: cell, anchor: cell, extent: cell, editing: null };
}

/** Extend the selection to a cell, the active cell staying where it is. */
export function extendTo(state: NavState, cell: Cell): NavState {
  return { ...state, extent: cell, editing: null };
}

export function selectAll(state: NavState, ctx: NavContext): NavState {
  if (ctx.rowCount === 0 || ctx.colCount === 0) return state;
  return {
    ...state,
    anchor: { row: 0, col: 0 },
    extent: { row: ctx.rowCount - 1, col: ctx.colCount - 1 },
    editing: null,
  };
}

type Direction = "up" | "down" | "left" | "right";

const STEP: Record<Direction, [number, number]> = {
  up: [-1, 0],
  down: [1, 0],
  left: [0, -1],
  right: [0, 1],
};

/**
 * Where Ctrl+arrow lands, as in Excel: from a cell with data whose
 * neighbour has data too, the last cell of that block; otherwise the next
 * cell with data; and the edge of the grid if there is none.
 */
export function edgeOfData(from: Cell, direction: Direction, ctx: NavContext): Cell {
  const [dr, dc] = STEP[direction];
  const inside = (c: Cell) => c.row >= 0 && c.row < ctx.rowCount && c.col >= 0 && c.col < ctx.colCount;
  const next = (c: Cell) => ({ row: c.row + dr, col: c.col + dc });
  const filled = (c: Cell) => !ctx.isEmpty(c.row, c.col);
  let cell = from;
  let n = next(cell);
  if (!inside(n)) return cell;
  if (filled(cell) && filled(n)) {
    while (inside(n) && filled(n)) {
      cell = n;
      n = next(cell);
    }
    return cell;
  }
  while (inside(n)) {
    cell = n;
    if (filled(cell)) return cell;
    n = next(cell);
  }
  return cell;
}

function step(cell: Cell, direction: Direction, ctx: NavContext): Cell {
  const [dr, dc] = STEP[direction];
  return clampCell({ row: cell.row + dr, col: cell.col + dc }, ctx);
}

function isPrintable(input: KeyInput): boolean {
  return input.key.length === 1 && !input.ctrl && !input.alt;
}

const ARROWS: Record<string, Direction> = {
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
};

function result(state: NavState, effects: Effect[] = [], handled = true): KeyResult {
  return { state, effects, handled };
}

/** Start editing a cell. `typed` replaces its text; without it the text is kept (F2, Enter). */
export function beginEdit(state: NavState, ctx: NavContext, cell: Cell, typed?: string): KeyResult {
  if (cell.row < 0 || !ctx.isEditable(cell.row, cell.col)) return result(state, [], typed === undefined);
  const original = ctx.editText(cell.row, cell.col);
  const editing: Edit = {
    row: cell.row,
    col: cell.col,
    mode: typed === undefined ? "edit" : "enter",
    draft: typed ?? original,
    original,
  };
  // Typing into a selection keeps it, so Ctrl+Enter can fill it (Excel); elsewhere it collapses.
  const range = rangeOf(state);
  const keep = !isSingleCell(range) && inRange(range, cell.row, cell.col) && cell.row === state.active.row && cell.col === state.active.col;
  const base = keep ? { ...state, editing: null } : moveTo(state, cell);
  return result({ ...base, tabColumn: state.tabColumn, editing }, [
    { type: "beginEdit", row: cell.row, col: cell.col },
  ]);
}

/** Moving down from a cell; in line editing, past the last row adds one. */
function down(state: NavState, ctx: NavContext, from: Cell, effects: Effect[]): KeyResult {
  if (ctx.lineEditing && from.row === ctx.rowCount - 1) {
    const cell = { row: from.row + 1, col: from.col };
    return result({ ...moveTo(state, cell), tabColumn: null }, [...effects, { type: "appendRow" }]);
  }
  return result({ ...moveTo(state, step(from, "down", ctx)), tabColumn: null }, effects);
}

function handleEditing(state: NavState, edit: Edit, input: KeyInput, ctx: NavContext): KeyResult {
  const commit: Effect = { type: "commit", edit };
  const here = { row: edit.row, col: edit.col };
  switch (input.key) {
    case "Escape":
      return result({ ...state, editing: null }, [{ type: "cancel", edit }]);
    case "Enter": {
      if (input.alt) return result(state, [], false); // a line break in the text, if the editor allows one
      if (input.ctrl) {
        // Into every cell of the selection; the selection stays.
        return result({ ...state, editing: null }, [{ type: "fill", edit, range: rangeOf(state) }]);
      }
      if (input.shift) return result({ ...moveTo(state, step(here, "up", ctx)), tabColumn: null }, [commit]);
      const target = { row: edit.row, col: state.tabColumn ?? edit.col };
      return down(state, ctx, target, [commit]);
    }
    case "Tab":
      return tab(state, ctx, here, Boolean(input.shift), [commit]);
    case "F2":
      // Excel: F2 switches between entering and editing.
      return result({ ...state, editing: { ...edit, mode: edit.mode === "edit" ? "enter" : "edit" } });
  }
  const direction = ARROWS[input.key];
  if (direction && edit.mode === "enter" && !input.ctrl && !input.shift) {
    if (direction === "down") return down(state, ctx, here, [commit]);
    return result({ ...moveTo(state, step(here, direction, ctx)), tabColumn: null }, [commit]);
  }
  // Everything else belongs to the text editor.
  return result(state, [], false);
}

function tab(state: NavState, ctx: NavContext, from: Cell, back: boolean, effects: Effect[]): KeyResult {
  const lastCol = ctx.colCount - 1;
  const tabColumn = state.tabColumn ?? from.col;
  if (!back && from.col < lastCol) {
    return result({ ...moveTo(state, { row: from.row, col: from.col + 1 }), tabColumn }, effects);
  }
  if (back && from.col > 0) {
    return result({ ...moveTo(state, { row: from.row, col: from.col - 1 }), tabColumn }, effects);
  }
  if (ctx.lineEditing && from.row >= 0) {
    // Entering lines: Tab past the last column goes on to the next line.
    if (!back) return down(state, ctx, { row: from.row, col: 0 }, effects);
    if (from.row > 0) return result({ ...moveTo(state, { row: from.row - 1, col: lastCol }), tabColumn: null }, effects);
  }
  // At the edge Tab leaves the grid, so the page stays reachable by keyboard.
  return result({ ...state, editing: null, tabColumn: null }, effects, false);
}

function handleHeader(state: NavState, input: KeyInput, ctx: NavContext): KeyResult {
  const col = state.active.col;
  const direction = ARROWS[input.key];
  if (direction === "down" && input.alt) return result(state, [{ type: "openMenu", col, menu: "filter" }]);
  if (direction === "down") return result(moveTo(state, { row: 0, col }));
  if (direction === "left" || direction === "right") {
    const next = clamp(col + (direction === "left" ? -1 : 1), 0, Math.max(ctx.colCount - 1, 0));
    return result(moveTo(state, { row: -1, col: next }));
  }
  if (direction === "up") return result(state);
  switch (input.key) {
    case "Enter":
    case " ":
      if (ctx.sortable && !ctx.sortable(col)) return result(state);
      return result(state, [{ type: "sort", col, multi: Boolean(input.shift) }]);
    case "ContextMenu":
      return result(state, [{ type: "openMenu", col, menu: "column" }]);
    case "F10":
      if (input.shift) return result(state, [{ type: "openMenu", col, menu: "column" }]);
      break;
    case "Home":
      return result(moveTo(state, { row: -1, col: 0 }));
    case "End":
      return result(moveTo(state, { row: -1, col: Math.max(ctx.colCount - 1, 0) }));
    case "Tab":
      return tab(state, ctx, state.active, Boolean(input.shift), []);
  }
  return result(state, [], false);
}

/** The state after a key, and what the grid must do. */
export function handleKey(state: NavState, input: KeyInput, ctx: NavContext): KeyResult {
  if (state.editing) return handleEditing(state, state.editing, input, ctx);
  if (state.active.row === -1) return handleHeader(state, input, ctx);
  if (ctx.colCount === 0) return result(state, [], false);

  const { active } = state;
  const lastRow = Math.max(ctx.rowCount - 1, 0);
  const lastCol = Math.max(ctx.colCount - 1, 0);
  const go = (cell: Cell, extend: boolean): KeyResult =>
    result(extend ? extendTo(state, clampCell(cell, ctx)) : { ...moveTo(state, clampCell(cell, ctx)), tabColumn: null });

  const direction = ARROWS[input.key];
  if (direction) {
    if (input.alt && direction === "down") return result(state, [{ type: "openMenu", col: active.col, menu: "filter" }]);
    const from = input.shift ? state.extent : active;
    if (input.ctrl) return go(edgeOfData(from, direction, ctx), Boolean(input.shift));
    if (direction === "up" && from.row === 0 && !input.shift && ctx.headerRow) {
      return result(moveTo(state, { row: -1, col: from.col }));
    }
    if (direction === "down" && !input.shift) return down(state, ctx, from, []);
    return go(step(from, direction, ctx), Boolean(input.shift));
  }

  const key = input.key;
  if (input.ctrl) {
    switch (key.toLowerCase()) {
      case "a":
        return result(selectAll(state, ctx));
      case "z":
        return result(state, [{ type: input.shift ? "redo" : "undo" }]);
      case "y":
        return result(state, [{ type: "redo" }]);
      case "home":
        return go({ row: 0, col: 0 }, Boolean(input.shift));
      case "end":
        return go({ row: lastRow, col: lastCol }, Boolean(input.shift));
      case " ":
        // The whole column.
        return result({ ...state, anchor: { row: 0, col: active.col }, extent: { row: lastRow, col: active.col } });
      case "+":
      case "=":
        if (ctx.lineEditing && (key === "+" || input.shift)) {
          return result(state, [{ type: "insertRow", at: active.row }]);
        }
        return result(state, [], false);
      case "-": {
        if (!ctx.lineEditing || ctx.rowCount === 0) return result(state, [], false);
        const range = rangeOf(state);
        const next = clampCell({ row: range.top, col: active.col }, { ...ctx, rowCount: ctx.rowCount - (range.bottom - range.top + 1) });
        return result(moveTo(state, next), [{ type: "deleteRows", from: range.top, to: range.bottom }]);
      }
      case "enter":
        if (ctx.lineEditing) return result(state, [{ type: "insertRow", at: active.row + 1 }]);
        return result(state, [], false);
    }
    // Ctrl+C, Ctrl+V, Ctrl+X: the browser fires copy, paste and cut.
    return result(state, [], false);
  }

  switch (key) {
    case "Tab":
      return tab(state, ctx, active, Boolean(input.shift), []);
    case "Enter":
      if (input.shift) return go(step(active, "up", ctx), false);
      if (ctx.enterEdits !== false && ctx.isEditable(active.row, active.col)) {
        return beginEdit(state, ctx, active);
      }
      return down(state, ctx, { row: active.row, col: state.tabColumn ?? active.col }, []);
    case "F2":
      return beginEdit(state, ctx, active);
    case "Home":
      return go({ row: input.shift ? state.extent.row : active.row, col: 0 }, Boolean(input.shift));
    case "End":
      return go({ row: input.shift ? state.extent.row : active.row, col: lastCol }, Boolean(input.shift));
    case "PageDown": {
      const from = input.shift ? state.extent : active;
      return go({ row: from.row + ctx.pageRows, col: from.col }, Boolean(input.shift));
    }
    case "PageUp": {
      const from = input.shift ? state.extent : active;
      return go({ row: from.row - ctx.pageRows, col: from.col }, Boolean(input.shift));
    }
    case "Delete":
      return result(state, [{ type: "clear", range: rangeOf(state) }]);
    case "Backspace":
      // Excel: clears the cell and starts typing into it.
      return beginEdit(state, ctx, active, "");
    case "Escape":
      // Collapse a selection to the active cell.
      return result(moveTo(state, active), [], !isSingleCell(rangeOf(state)));
    case " ":
      if (input.shift) {
        return result({ ...state, anchor: { row: active.row, col: 0 }, extent: { row: active.row, col: lastCol } });
      }
      break;
    case "ContextMenu":
      return result(state, [{ type: "openMenu", col: active.col, menu: "column" }]);
    case "F10":
      if (input.shift) return result(state, [{ type: "openMenu", col: active.col, menu: "column" }]);
      break;
  }
  if (isPrintable(input)) {
    if (!ctx.isEditable(active.row, active.col)) return result(state, [], false);
    return beginEdit(state, ctx, active, key);
  }
  return result(state, [], false);
}
