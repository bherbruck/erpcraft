import { Heading } from "@astryxdesign/core/Heading";
import { Table, type TableColumn, pixel, proportional } from "@astryxdesign/core/Table";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import type { Posting } from "@/api/types";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";

interface Entry extends Record<string, unknown> {
  key: string;
  account: string;
  memo: string;
  debit: string;
  credit: string;
}

function amount(value: string | null | undefined): string {
  return value ? formatDecimal(value, { minFraction: 2 }) : "";
}

/**
 * The general-ledger entries of a document's postings, read-only. The spike shows the
 * GL; the other ledgers (stock, open items, tax) are in ../ui's postings view.
 */
export function Postings({ postings, currency }: { postings: Posting[]; currency: string }) {
  if (postings.length === 0) return null;
  const columns: TableColumn<Entry>[] = [
    { key: "account", header: t("postings.what"), width: proportional(2, { minWidth: 200 }) },
    { key: "memo", header: t("document.memo"), width: proportional(2, { minWidth: 120 }) },
    { key: "debit", header: t("postings.debit", { currency }), align: "end", width: pixel(140) },
    { key: "credit", header: t("postings.credit", { currency }), align: "end", width: pixel(140) },
  ];
  return (
    <VStack gap={3}>
      <Heading level={2}>{t("postings.title")}</Heading>
      {postings.map((posting) => {
        const entries: Entry[] = (posting.entries.gl ?? []).map((entry, index) => ({
          key: entry.id ?? String(index),
          account: [entry.account_code, entry.account_name].filter(Boolean).join(" · "),
          memo: entry.memo ?? "",
          debit: amount(entry.debit),
          credit: amount(entry.credit),
        }));
        return (
          <VStack key={posting.id} gap={1}>
            <Text type="supporting">{t("postings.dated", { date: formatCalendarDate(posting.posting_date) })}</Text>
            <Table<Entry> data={entries} columns={columns} idKey="key" density="compact" dividers="rows" />
          </VStack>
        );
      })}
    </VStack>
  );
}
