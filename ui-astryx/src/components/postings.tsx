import { Heading } from "@astryxdesign/core/Heading";
import { type Column, DataTable } from "./data-table";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import type { Posting } from "@/api/types";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";

interface Entry {
  key: string;
  account: string;
  memo: string;
  debit: string | null;
  credit: string | null;
}

/**
 * The general-ledger entries of a document's postings, read-only. The spike shows the
 * GL; the other ledgers (stock, open items, tax) are in ../ui's postings view.
 */
export function Postings({ postings, currency }: { postings: Posting[]; currency: string }) {
  if (postings.length === 0) return null;
  const columns: Column<Entry>[] = [
    { id: "account", header: t("postings.what"), width: 320, value: (e) => e.account },
    { id: "memo", header: t("document.memo"), width: 260, value: (e) => e.memo },
    { id: "debit", header: t("postings.debit", { currency }), type: "money", minFraction: 2, total: true, width: 150, value: (e) => e.debit },
    { id: "credit", header: t("postings.credit", { currency }), type: "money", minFraction: 2, total: true, width: 150, value: (e) => e.credit },
  ];
  return (
    <VStack gap={3}>
      <Heading level={2}>{t("postings.title")}</Heading>
      {postings.map((posting) => {
        const entries: Entry[] = (posting.entries.gl ?? []).map((entry, index) => ({
          key: entry.id ?? String(index),
          account: [entry.account_code, entry.account_name].filter(Boolean).join(" · "),
          memo: entry.memo ?? "",
          debit: entry.debit ?? null,
          credit: entry.credit ?? null,
        }));
        return (
          <VStack key={posting.id} gap={1}>
            <Text type="supporting">{t("postings.dated", { date: formatCalendarDate(posting.posting_date) })}</Text>
            <DataTable id="postings" caption={t("postings.title")} columns={columns} rows={entries} rowKey={(e) => e.key} empty={{ title: t("postings.no_entries") }} />
          </VStack>
        );
      })}
    </VStack>
  );
}
