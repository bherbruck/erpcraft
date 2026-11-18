// Column widths that fit the space there is, from the headers, the content and the types.

import type { ColumnType, GridColumn } from "@erpcraft/grid";

const MIN_WIDTH: Record<ColumnType, number> = {
  text: 100,
  number: 84,
  money: 90,
  date: 108,
  datetime: 150,
  boolean: 64,
  enum: 110,
  state: 110,
  reference: 130,
};
const MAX_WIDTH = 320;
let measurer: CanvasRenderingContext2D | null = null;
let font: { size: string; family: string } | null = null;
/** Text width in the grid's own font: the theme's small size (as Astryx's compact Table) and family. */
function measure(text: string, bold = false): number {
  measurer ??= document.createElement("canvas").getContext("2d");
  if (!measurer) return text.length * 7;
  if (!font) {
    // The token is a calc() a canvas cannot read: resolve it on a probe element.
    const probe = document.createElement("span");
    probe.style.fontSize = "var(--font-size-sm)";
    document.body.append(probe);
    font = { size: getComputedStyle(probe).fontSize || "13px", family: getComputedStyle(probe).fontFamily || "sans-serif" };
    probe.remove();
  }
  measurer.font = `${bold ? 600 : 400} ${font.size} ${font.family}`;
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
  for (const column of columns) {
    if (column.hidden) continue;
    const min = MIN_WIDTH[column.type];
    const header = measure(column.header, true) + 28;
    let content = 0;
    for (const row of rows.slice(0, 50)) {
      const value = column.value ? column.value(row) : (row as Record<string, unknown>)[column.id];
      const text = column.format ? column.format(value, row) : value == null ? "" : String(value);
      content = Math.max(content, measure(text) + 20);
    }
    // The header is read too (up to a point): a column called "Quantity" should not read "Quan…".
    const want = Math.max(min, Math.min(header, 200), content);
    widths.set(column.id, Math.round(Math.min(MAX_WIDTH, want)));
  }
  const total = () => [...widths.values()].reduce((a, b) => a + b, 0);
  const flex = columns.filter((c) => !c.hidden && flexible(c));
  if (total() > available) {
    // Shrink the flexible columns toward their minimum, all by the same share.
    const room = flex.reduce((sum, c) => sum + ((widths.get(c.id) ?? 0) - MIN_WIDTH[c.type]), 0);
    const over = total() - available;
    if (room > 0)
      for (const c of flex) {
        const spare = (widths.get(c.id) ?? 0) - MIN_WIDTH[c.type];
        widths.set(c.id, Math.round((widths.get(c.id) ?? 0) - Math.min(spare, (spare / room) * over)));
      }
  } else if (flex.length > 0) {
    const extra = (available - total()) / flex.length;
    for (const c of flex) widths.set(c.id, Math.round((widths.get(c.id) ?? 0) + extra));
  }
  return widths;
}

