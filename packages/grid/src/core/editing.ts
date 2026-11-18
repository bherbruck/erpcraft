// Copying, pasting, clearing and undoing, as plain functions over rows and
// columns. The React layer calls these and hands the changes to the consumer.

import type { CellChange, EditSource, GridColumn, RejectedCell } from "../types";
import { cellValue, formatCell, isEditable, parseCell } from "./columns";
import type { Range } from "./navigation";

export interface Sheet<Row> {
  rows: readonly Row[];
  /** Visible columns in their shown order. */
  columns: readonly GridColumn<Row>[];
  rowId(row: Row, index: number): string;
  locale?: string;
}

/** The cells of a range as text, as they are shown. */
export function copyRange<Row>(sheet: Sheet<Row>, range: Range): string[][] {
  const out: string[][] = [];
  for (let r = range.top; r <= range.bottom; r += 1) {
    const row = sheet.rows[r];
    if (row === undefined) continue;
    const line: string[] = [];
    for (let c = range.left; c <= range.right; c += 1) {
      const column = sheet.columns[c];
      line.push(column ? formatCell(column, row, sheet.locale) : "");
    }
    out.push(line);
  }
  return out;
}

export interface PlannedEdit<Row> {
  changes: CellChange<Row>[];
  rejected: RejectedCell[];
}

function change<Row>(sheet: Sheet<Row>, r: number, column: GridColumn<Row>, newValue: unknown): CellChange<Row> | null {
  const row = sheet.rows[r];
  if (row === undefined) return null;
  return {
    rowIndex: r,
    rowId: sheet.rowId(row, r),
    row,
    columnId: column.id,
    oldValue: cellValue(column, row),
    newValue,
  };
}

/**
 * Where pasted cells go and what they become. The block starts at the
 * selection's top left; a single copied cell fills the whole selection, as
 * in Excel. A cell that is read-only, outside the grid or not valid for
 * its column is rejected with the reason; the others are applied.
 */
export function planPaste<Row>(sheet: Sheet<Row>, range: Range, cells: string[][]): PlannedEdit<Row> {
  const changes: CellChange<Row>[] = [];
  const rejected: RejectedCell[] = [];
  const single = cells.length === 1 && cells[0]?.length === 1;
  const height = single ? range.bottom - range.top + 1 : cells.length;
  for (let i = 0; i < height; i += 1) {
    const line = single ? cells[0] ?? [] : cells[i] ?? [];
    const width = single ? range.right - range.left + 1 : line.length;
    for (let j = 0; j < width; j += 1) {
      const text = single ? line[0] ?? "" : line[j] ?? "";
      const r = range.top + i;
      const c = range.left + j;
      const column = sheet.columns[c];
      const row = sheet.rows[r];
      if (!column || row === undefined) {
        rejected.push({ rowIndex: r, columnId: column?.id ?? `#${c}`, text, reason: "outside the grid" });
        continue;
      }
      if (!isEditable(column, row)) {
        rejected.push({ rowIndex: r, columnId: column.id, text, reason: "read-only" });
        continue;
      }
      const parsed = parseCell(column, text, row, sheet.locale);
      if (!parsed.ok) {
        rejected.push({ rowIndex: r, columnId: column.id, text, reason: parsed.error });
        continue;
      }
      const planned = change(sheet, r, column, parsed.value);
      if (planned) changes.push(planned);
    }
  }
  return { changes, rejected };
}

/** Delete: every editable cell of the range becomes empty, if its column accepts an empty value. */
export function planClear<Row>(sheet: Sheet<Row>, range: Range): PlannedEdit<Row> {
  const changes: CellChange<Row>[] = [];
  const rejected: RejectedCell[] = [];
  for (let r = range.top; r <= range.bottom; r += 1) {
    const row = sheet.rows[r];
    if (row === undefined) continue;
    for (let c = range.left; c <= range.right; c += 1) {
      const column = sheet.columns[c];
      if (!column || !isEditable(column, row)) continue;
      const parsed = parseCell(column, "", row, sheet.locale);
      if (!parsed.ok) {
        rejected.push({ rowIndex: r, columnId: column.id, text: "", reason: parsed.error });
        continue;
      }
      if (cellValue(column, row) === parsed.value) continue;
      const planned = change(sheet, r, column, parsed.value);
      if (planned) changes.push(planned);
    }
  }
  return { changes, rejected };
}

/** One typed edit, or why it is refused. */
export function planEdit<Row>(
  sheet: Sheet<Row>,
  rowIndex: number,
  colIndex: number,
  text: string,
): PlannedEdit<Row> {
  const column = sheet.columns[colIndex];
  const row = sheet.rows[rowIndex];
  if (!column || row === undefined) return { changes: [], rejected: [] };
  const parsed = parseCell(column, text, row, sheet.locale);
  if (!parsed.ok) {
    return { changes: [], rejected: [{ rowIndex, columnId: column.id, text, reason: parsed.error }] };
  }
  if (Object.is(cellValue(column, row), parsed.value)) return { changes: [], rejected: [] };
  const planned = change(sheet, rowIndex, column, parsed.value);
  return { changes: planned ? [planned] : [], rejected: [] };
}

// ----------------------------------------------------------------- history

/** A change as history keeps it: by row id, so sorting or filtering in between does not matter. */
export interface HistoryEntry {
  rowId: string;
  columnId: string;
  oldValue: unknown;
  newValue: unknown;
}

export interface HistoryBatch {
  source: EditSource;
  changes: HistoryEntry[];
}

/** Edit batches to undo and redo. Each paste, clear or typed edit is one batch. */
export class EditHistory {
  private done: HistoryBatch[] = [];
  private undone: HistoryBatch[] = [];

  constructor(private readonly limit = 100) {}

  record(source: EditSource, changes: readonly CellChange<unknown>[] | readonly HistoryEntry[]): void {
    if (changes.length === 0) return;
    this.done.push({
      source,
      changes: changes.map(({ rowId, columnId, oldValue, newValue }) => ({ rowId, columnId, oldValue, newValue })),
    });
    if (this.done.length > this.limit) this.done.shift();
    this.undone = [];
  }

  get canUndo(): boolean {
    return this.done.length > 0;
  }

  get canRedo(): boolean {
    return this.undone.length > 0;
  }

  /** The changes that undo the last batch: each cell back to its old value. */
  undo(): HistoryEntry[] | null {
    const batch = this.done.pop();
    if (!batch) return null;
    this.undone.push(batch);
    return batch.changes
      .map((c) => ({ rowId: c.rowId, columnId: c.columnId, oldValue: c.newValue, newValue: c.oldValue }))
      .reverse();
  }

  redo(): HistoryEntry[] | null {
    const batch = this.undone.pop();
    if (!batch) return null;
    this.done.push(batch);
    return batch.changes.map((c) => ({ ...c }));
  }

  clear(): void {
    this.done = [];
    this.undone = [];
  }
}
