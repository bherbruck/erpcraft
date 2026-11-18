// Commands that take a line of a posted document (`document_id` and `line_id`)
// whose schema does not say which lines qualify. Each has a picker here that
// finds the candidates from what the command's description names, and hands
// the chosen line to the command's dialog. A UI decision, until the server
// describes such inputs.

import { type ReactNode, useState } from "react";
import { api } from "@/api/client";
import type { Json, JsonObject } from "@/api/types";
import { useApp } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { Dialog } from "@/components/dialog";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { isZeroDecimal } from "@/lib/decimal";
import { findDocType } from "@/lib/meta";
import { useAsync } from "@/lib/use-async";
import { Amount, Empty } from "./field-value";

export interface LinePickerProps {
  record: JsonObject;
  title: string;
  onPick: (input: JsonObject) => void;
  onClose: () => void;
}

interface Candidate {
  entryId: string;
  documentId: string;
  docType: string | null;
  number: string | null;
  date: string;
  memo: string | null;
  amount: string;
  lineId: string;
}

const text = (value: Json | undefined): string | null => (typeof value === "string" && value !== "" ? value : null);

/**
 * `asset.capitalise`: a debit to the asset's class's `asset_cost` account, on
 * a posted document line, that no asset was capitalised from yet.
 */
function CapitalisePicker({ record, title, onPick, onClose }: LinePickerProps) {
  const { meta, currencyDecimals } = useApp();
  const companyId = text(record.company_id);
  const classId = text(record.class_id);
  const [chosen, setChosen] = useState<string | null>(null);

  const candidates = useAsync(companyId && classId ? `capitalise|${companyId}|${classId}` : null, async (signal) => {
    const rules = await api.run(
      "entity.list",
      {
        entity: "account_rule",
        filters: {
          company_id: companyId ?? "",
          purpose: "asset_cost",
          scope_type: "asset_class",
          scope_id: classId ?? "",
        },
        limit: 1,
      },
      { signal },
    );
    const account = text(rules.items[0]?.account_id);
    if (!account) return { account: null, rows: [] as Candidate[] };
    const [ledger, entries, assets] = await Promise.all([
      api.run("gl.account_ledger", { company_id: companyId ?? "", account_id: account, limit: 500 }, { signal }),
      api.run("entity.list", { entity: "gl_entry", filters: { account_id: account }, limit: 500 }, { signal }),
      api.run("entity.list", { entity: "asset", filters: { company_id: companyId ?? "" }, limit: 500 }, { signal }),
    ]);
    const sourceLine = new Map(entries.items.map((entry) => [text(entry.id), text(entry.source_line_id)]));
    const used = new Set(assets.items.map((asset) => text(asset.source_entry_id)).filter(Boolean));
    const rows: Candidate[] = [];
    for (const entry of ledger.entries) {
      const lineId = sourceLine.get(entry.id);
      if (!entry.document_id || !lineId || used.has(entry.id) || isZeroDecimal(entry.debit)) continue;
      rows.push({
        entryId: entry.id,
        documentId: entry.document_id,
        docType: entry.document_type,
        number: entry.document_number,
        date: entry.posting_date,
        memo: entry.memo ?? entry.posting_memo,
        amount: entry.debit,
        lineId,
      });
    }
    return { account, rows, currency: ledger.currency };
  });
  const data = candidates.data;
  const decimals = currencyDecimals[data && "currency" in data ? (data.currency ?? "") : ""] ?? 2;
  const pick = data?.rows.find((row) => row.entryId === chosen);

  let body: ReactNode;
  if (candidates.error) body = <Alert title={t("picker.failed")}>{candidates.error.message}</Alert>;
  else if (!data) body = <p className="text-sm text-muted-foreground">{t("value.loading")}</p>;
  else if (!data.account) body = <p className="text-sm text-muted-foreground">{t("picker.capitalise_no_account")}</p>;
  else if (data.rows.length === 0)
    body = <p className="text-sm text-muted-foreground">{t("picker.capitalise_none")}</p>;
  else
    body = (
      <RadioGroup
        value={chosen ?? ""}
        onValueChange={setChosen}
        aria-label={t("picker.capitalise_lines")}
        className="block rounded-md border"
      >
        <Table aria-label={t("picker.capitalise_lines")}>
          <TableHeader>
            <TableRow>
              <TableHead className="w-8" />
              <TableHead>{t("bank.posting_date")}</TableHead>
              <TableHead>{t("ledger.document")}</TableHead>
              <TableHead className="w-full">{t("document.memo")}</TableHead>
              <TableHead className="text-end">{t("report.amount")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data.rows.map((row) => (
              <TableRow key={row.entryId} className="cursor-pointer" onClick={() => setChosen(row.entryId)}>
                <TableCell>
                  <RadioGroupItem
                    value={row.entryId}
                    aria-label={t("picker.choose", { number: row.number ?? row.entryId })}
                  />
                </TableCell>
                <TableCell className="whitespace-nowrap">{formatCalendarDate(row.date)}</TableCell>
                <TableCell className="whitespace-nowrap">
                  {[findDocType(meta, row.docType)?.label ?? row.docType, row.number].filter(Boolean).join(" ")}
                </TableCell>
                <TableCell>{row.memo ?? <Empty />}</TableCell>
                <TableCell className="text-end">
                  <Amount value={row.amount} decimals={decimals} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </RadioGroup>
    );

  return (
    <Dialog
      open
      wide
      onClose={onClose}
      title={title}
      description={t("picker.capitalise_help")}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {t("action.cancel")}
          </Button>
          <Button
            disabled={!pick}
            onClick={() => pick && onPick({ document_id: pick.documentId, line_id: pick.lineId })}
          >
            {t("picker.continue")}
          </Button>
        </>
      }
    >
      {body}
    </Dialog>
  );
}

/** Pickers by command name. */
export const LINE_PICKERS: Record<string, (props: LinePickerProps) => ReactNode> = {
  "asset.capitalise": CapitalisePicker,
};
