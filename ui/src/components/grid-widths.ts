// Column widths that fit the space there is, from the headers, the content and the types.
// Ported from ui-astryx; measured in this app's own table font (shadcn's `text-sm`).

import type { ColumnType, GridColumn } from "@erpcraft/grid";

const MIN_WIDTH: Record<ColumnType, number> = {
  text: 100,
  number: 64,
  money: 72,
  date: 108,
  datetime: 150,
  boolean: 64,
  enum: 110,
  state: 110,
  reference: 110,
};
const MAX_WIDTH = 320;
let measurer: CanvasRenderingContext2D | null = null;
let font: { size: string; family: string } | null = null;

/** Text width in the grid's font: the theme's small size and family, as shadcn's Table uses. */
function measure(text: string, bold = false): number {
  measurer ??= document.createElement("canvas").getContext("2d");
  if (!measurer) return text.length * 7;
  if (!font) {
    // Read from a probe with the same class the cells have, so the theme decides.
    const probe = document.createElement("span");
    probe.className = "text-sm";
    document.body.append(probe);
    const style = getComputedStyle(probe);
    font = { size: style.fontSize || "14px", family: style.fontFamily || "sans-serif" };
    probe.remove();
  }
  measurer.font = `${bold ? 500 : 400} ${font.size} ${font.family}`;
  return measurer.measureText(text).width;
}

/**
 * Widths that fit the space there is: each column as wide as its header and content want
 * (within a minimum by type and a maximum), then text and reference columns give up width
 * down to their minimum when the sum is too wide, or take what is left over. A width the
 * user dragged is kept by the grid's saved layout.
 */
export function fitWidths<R>(columns: GridColumn<R>[], rows: readonly R[], available: number): Map<string, number> {
  const widths = new Map<string, number>();
  const flexible = (c: GridColumn<R>) => c.type === "text" || c.type === "reference";
  // The least a column gives up to: its type's minimum, or more where the column asks (a document's number).
  const floor = (c: GridColumn<R>) => Math.max(MIN_WIDTH[c.type], c.minWidth ?? 0);
  for (const column of columns) {
    if (column.hidden) continue;
    const min = floor(column);
    // Room for the label, the cell's padding and, where the column sorts, the arrow. The column's
    // tools (filter, menu) show over the label on hover, so they take no room of their own.
    const header = measure(column.header, true) + 24 + (column.sortable === false ? 0 : 18);
    let content = 0;
    for (const row of rows.slice(0, 50)) {
      const value = column.value ? column.value(row) : (row as Record<string, unknown>)[column.id];
      const text = column.format ? column.format(value, row) : value == null ? "" : String(value);
      content = Math.max(content, measure(text) + 20);
    }
    // The header is read too (up to a point): a column called "Quantity" should not read "Quan…".
    const want = Math.max(min, Math.min(header, 220), content);
    widths.set(column.id, Math.round(Math.min(MAX_WIDTH, want)));
  }
  const total = () => [...widths.values()].reduce((a, b) => a + b, 0);
  const flex = columns.filter((c) => !c.hidden && flexible(c));
  if (total() > available) {
    // Shrink the flexible columns toward their minimum, all by the same share.
    const room = flex.reduce((sum, c) => sum + Math.max(0, (widths.get(c.id) ?? 0) - floor(c)), 0);
    const over = total() - available;
    if (room > 0)
      for (const c of flex) {
        const spare = Math.max(0, (widths.get(c.id) ?? 0) - floor(c));
        widths.set(c.id, Math.round((widths.get(c.id) ?? 0) - Math.min(spare, (spare / room) * over)));
      }
  } else if (flex.length > 0) {
    const extra = (available - total()) / flex.length;
    for (const c of flex) widths.set(c.id, Math.round((widths.get(c.id) ?? 0) + extra));
  }
  return widths;
}
