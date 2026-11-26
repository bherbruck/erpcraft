// The lots of a stock line (`lots`, a list of { lot, qty } with, for goods coming in under a new
// lot, its expiry and manufacturing dates and attributes). The registry gives the field only as
// Json, so its shape is the UI's, from the field's help: a cell shows the lots as text
// ("A-1 ×3, A-2 ×2", which is also what is copied and what may be typed or pasted), and Enter
// opens a dialog to name them, with the server's proposal (`stock.lot_proposal`) for goods going
// out. Whether the lots fit the item and add up to the quantity is the server's to say.

import type { EditorSlotProps } from "@erpcraft/grid";
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { ApiError, api } from "@/api/client";
import type { Json, JsonObject } from "@/api/types";
import { useApp } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { DateInput } from "@/components/date-input";
import { Dialog } from "@/components/dialog";
import { MoneyInput } from "@/components/money-input";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { formatDecimal, isDecimal, parseLocalDecimal, trimDecimal } from "@/lib/decimal";
import { canRun } from "@/lib/meta";

/** The field of a stock line that names its lots. */
export const LOTS = "lots";

export interface LineLot {
  lot: string;
  qty: string;
  expiry_date?: string | null;
  manufactured_date?: string | null;
  attributes?: Json;
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The lots a line holds, as the server sent them (anything else is none). */
export function lineLots(value: unknown): LineLot[] {
  if (!Array.isArray(value)) return [];
  return value.filter(isObject).flatMap((entry) =>
    typeof entry.lot === "string"
      ? [
          {
            ...entry,
            lot: entry.lot,
            qty: typeof entry.qty === "string" ? entry.qty : typeof entry.qty === "number" ? String(entry.qty) : "",
          } as LineLot,
        ]
      : [],
  );
}

/** "A-1 ×3, A-2 ×2": the lots as a cell shows and copies them. */
export function formatLots(value: unknown): string {
  return lineLots(value)
    .map((entry) => `${entry.lot} ${t("lots.times")}${formatDecimal(entry.qty)}`)
    .join(", ");
}

/**
 * Typed or pasted lots: as a cell shows them ("A-1 ×3, A-2 ×2", also with x or a space before
 * the quantity), one lot per line, or the JSON the server takes. Lots already on the line keep
 * their dates and attributes.
 */
export function parseLots(text: string, current: unknown): { ok: true; value: Json } | { ok: false; error: string } {
  const trimmed = text.trim();
  if (trimmed === "") return { ok: true, value: [] };
  if (trimmed.startsWith("[")) {
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (Array.isArray(parsed)) return { ok: true, value: parsed as Json };
    } catch {
      // Not JSON after all: read it as text below.
    }
    return { ok: false, error: t("lots.unreadable") };
  }
  const known = new Map(lineLots(current).map((entry) => [entry.lot, entry]));
  const out: JsonObject[] = [];
  // Lots are parted by a comma and a space, a semicolon or a line: a comma alone may be a decimal comma.
  for (const part of trimmed.split(/;|\n|,\s+/)) {
    const piece = part.trim();
    if (piece === "") continue;
    const match = /^(.*?)\s*(?:[×x*]\s*|\s+)(-?[\d.,]+)$/i.exec(piece);
    const lot = (match?.[1] ?? piece).trim();
    const qty = match?.[2] ? parseLocalDecimal(match[2]) : "1";
    if (lot === "" || qty === null) return { ok: false, error: t("lots.unreadable") };
    const before = known.get(lot);
    out.push({ ...(before ?? {}), lot, qty } as JsonObject);
  }
  return { ok: true, value: out };
}

/** What the dialog needs of the line beyond its lots: to propose them. */
export interface LotContext {
  companyId: string | null;
  itemId: string | null;
  itemLabel: string;
  warehouseId: string | null;
  qty: string | null;
  date: string | null;
  line: number;
}

/** The grid's editor of a `lots` cell: a dialog over the grid. */
export function LotEditor<R>({ p, context }: { p: EditorSlotProps<R>; context: LotContext }) {
  const { meta } = useApp();
  const [rows, setRows] = useState<LineLot[]>(() => {
    const now = lineLots(p.value);
    return now.length > 0
      ? now
      : [{ lot: "", qty: context.qty && isDecimal(context.qty) ? trimDecimal(context.qty.replace(/^-/, "")) : "" }];
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [short, setShort] = useState<string | null>(null);
  const canPropose =
    canRun(meta, "stock.lot_proposal") &&
    context.companyId !== null &&
    context.itemId !== null &&
    context.warehouseId !== null &&
    context.qty !== null &&
    isDecimal(context.qty);

  const change = (index: number, patch: Partial<LineLot>): void =>
    setRows((previous) => previous.map((row, at) => (at === index ? { ...row, ...patch } : row)));

  async function propose(): Promise<void> {
    if (!canPropose) return;
    setBusy(true);
    setError(null);
    try {
      const answer = await api.runNamed("stock.lot_proposal", {
        company_id: context.companyId,
        item_id: context.itemId,
        warehouse_id: context.warehouseId,
        // A line taking goods out may carry its quantity as negative (an adjustment).
        qty: (context.qty ?? "0").replace(/^-/, ""),
        not_expired_on: context.date,
      });
      if (isObject(answer)) {
        setRows(lineLots(answer.lots));
        setShort(typeof answer.short === "string" && !/^0*(\.0*)?$/.test(answer.short) ? answer.short : null);
      }
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  function save(): void {
    // Rows left empty are dropped; the rest go as typed, for the server to check.
    const kept = rows
      .filter((row) => row.lot.trim() !== "" || row.qty.trim() !== "")
      .map((row) => {
        const out: JsonObject = { lot: row.lot.trim(), qty: row.qty.trim() };
        if (row.expiry_date) out.expiry_date = row.expiry_date;
        if (row.manufactured_date) out.manufactured_date = row.manufactured_date;
        if (row.attributes !== undefined && row.attributes !== null) out.attributes = row.attributes;
        return out;
      });
    p.commit(JSON.stringify(kept));
  }

  return (
    <Dialog
      open
      inGrid
      wide
      dirty
      onClose={p.cancel}
      title={t("lots.title", { line: context.line, item: context.itemLabel })}
      description={context.qty ? t("lots.description", { qty: formatDecimal(context.qty) }) : undefined}
      footer={
        <>
          <Button variant="outline" onClick={p.cancel}>
            {t("action.cancel")}
          </Button>
          <Button onClick={save}>{t("lots.apply")}</Button>
        </>
      }
    >
      <div className="space-y-3">
        <Table aria-label={t("lots.table")}>
          <TableHeader>
            <TableRow>
              <TableHead>{t("lots.lot")}</TableHead>
              <TableHead className="w-32 text-end">{t("lots.qty")}</TableHead>
              <TableHead className="w-44">{t("lots.expiry")}</TableHead>
              <TableHead className="w-44">{t("lots.made")}</TableHead>
              <TableHead className="w-10">
                <span className="sr-only">{t("lots.remove_column")}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row, index) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: rows are positions in a list being typed
              <TableRow key={index}>
                <TableCell className="px-1 py-1">
                  <Input
                    aria-label={t("lots.lot_n", { n: index + 1 })}
                    value={row.lot}
                    autoFocus={index === 0}
                    onChange={(event) => change(index, { lot: event.target.value })}
                  />
                </TableCell>
                <TableCell className="px-1 py-1">
                  <MoneyInput
                    aria-label={t("lots.qty_n", { n: index + 1 })}
                    value={row.qty}
                    onChange={(next) => change(index, { qty: next })}
                  />
                </TableCell>
                <TableCell className="px-1 py-1">
                  <DateInput
                    aria-label={t("lots.expiry_n", { n: index + 1 })}
                    value={row.expiry_date ?? ""}
                    onChange={(next) => change(index, { expiry_date: next || null })}
                  />
                </TableCell>
                <TableCell className="px-1 py-1">
                  <DateInput
                    aria-label={t("lots.made_n", { n: index + 1 })}
                    value={row.manufactured_date ?? ""}
                    onChange={(next) => change(index, { manufactured_date: next || null })}
                  />
                </TableCell>
                <TableCell className="px-1 py-1">
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={t("lots.remove", { n: index + 1 })}
                    onClick={() => setRows((previous) => previous.filter((_, at) => at !== index))}
                  >
                    <Trash2 aria-hidden="true" />
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setRows((previous) => [...previous, { lot: "", qty: "" }])}
          >
            <Plus aria-hidden="true" />
            {t("lots.add")}
          </Button>
          {canPropose ? (
            <Button variant="outline" size="sm" busy={busy} onClick={() => void propose()}>
              {t("lots.propose")}
            </Button>
          ) : null}
          <span className="text-sm text-muted-foreground">{t("lots.hint")}</span>
        </div>
        {short ? <Alert tone="warning">{t("lots.short", { qty: formatDecimal(short) })}</Alert> : null}
        {error ? <Alert title={t("lots.propose_failed")}>{error.message}</Alert> : null}
      </div>
    </Dialog>
  );
}
