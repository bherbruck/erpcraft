// Saving how the user arranged the columns, and applying it to columns
// that may have changed since: unknown ids are dropped, new columns kept.

import type { ColumnLayout, GridColumn, LayoutStorage } from "../types";

export function defaultLayout<Row>(columns: readonly GridColumn<Row>[]): ColumnLayout {
  return {
    order: columns.map((c) => c.id),
    sizes: {},
    pinning: {
      left: columns.filter((c) => c.pin === "left").map((c) => c.id),
      right: columns.filter((c) => c.pin === "right").map((c) => c.id),
    },
    hidden: columns.filter((c) => c.hidden).map((c) => c.id),
  };
}

/** A saved layout fitted to the columns there are now. */
export function reconcileLayout<Row>(columns: readonly GridColumn<Row>[], saved: ColumnLayout | null): ColumnLayout {
  const base = defaultLayout(columns);
  if (!saved) return base;
  const ids = new Set(columns.map((c) => c.id));
  const known = (id: string) => ids.has(id);
  const order = saved.order.filter(known);
  for (const id of base.order) {
    if (!order.includes(id)) {
      // A new column goes after the one it follows in the definition.
      const before = base.order[base.order.indexOf(id) - 1];
      const at = before ? order.indexOf(before) + 1 : 0;
      order.splice(at, 0, id);
    }
  }
  const sizes: Record<string, number> = {};
  for (const [id, size] of Object.entries(saved.sizes ?? {})) {
    if (known(id) && Number.isFinite(size) && size > 0) sizes[id] = size;
  }
  const savedIds = new Set(saved.order);
  const fresh = (id: string) => !savedIds.has(id);
  return {
    order,
    sizes,
    pinning: {
      left: [...(saved.pinning?.left ?? []).filter(known), ...base.pinning.left.filter(fresh)],
      right: [...(saved.pinning?.right ?? []).filter(known), ...base.pinning.right.filter(fresh)],
    },
    hidden: [
      ...(saved.hidden ?? []).filter((id) => known(id) && columns.find((c) => c.id === id)?.hideable !== false),
      ...base.hidden.filter(fresh),
    ],
  };
}

/** Layouts in the browser's localStorage, under `prefix + gridId`. */
export function localStorageLayout(prefix = "erpcraft.grid."): LayoutStorage {
  return {
    load(gridId) {
      try {
        const text = globalThis.localStorage?.getItem(prefix + gridId);
        return text ? (JSON.parse(text) as ColumnLayout) : null;
      } catch {
        return null;
      }
    },
    save(gridId, layout) {
      try {
        globalThis.localStorage?.setItem(prefix + gridId, JSON.stringify(layout));
      } catch {
        // Storage full or blocked: the layout lasts as long as the page.
      }
    },
  };
}

/** Move a column before or after another, as a drag across the header does. */
export function moveColumn(order: readonly string[], id: string, targetId: string, after: boolean): string[] {
  if (id === targetId) return [...order];
  const without = order.filter((x) => x !== id);
  let at = without.indexOf(targetId);
  if (at === -1) return [...order];
  if (after) at += 1;
  without.splice(at, 0, id);
  return without;
}
