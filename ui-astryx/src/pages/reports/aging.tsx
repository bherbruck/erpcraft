// What a company owes or is owed, by vendor or customer, in buckets of days past due; what
// falls due soon; and each control account's balance beside what its items add up to.

import { Badge } from "@astryxdesign/core/Badge";
import { Banner } from "@astryxdesign/core/Banner";
import { DateInput } from "@astryxdesign/core/DateInput";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Link } from "@astryxdesign/core/Link";
import { MetadataList, MetadataListItem } from "@astryxdesign/core/MetadataList";
import { SegmentedControl, SegmentedControlItem } from "@astryxdesign/core/SegmentedControl";
import { Skeleton } from "@astryxdesign/core/Skeleton";
import { Text } from "@astryxdesign/core/Text";
import type { ISODateString } from "@astryxdesign/core/utils";
import { VStack } from "@astryxdesign/core/VStack";
import { useMemo } from "react";
import { api } from "@/api/client";
import type { OpenItemAgingOutput } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { LinkLabel } from "@/components/field-value";
import { Page } from "@/components/page";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { isZeroDecimal, sameDecimal } from "@/lib/decimal";
import { canRun, commandInputs, findCommand, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "../not-found";
import { Money } from "./statements";

const REPORT = "open_item.aging";
type Party = OpenItemAgingOutput["parties"][number];

function isAging(value: unknown): value is OpenItemAgingOutput {
  return typeof value === "object" && value !== null && "parties" in value && "buckets" in value && "totals" in value;
}

export function AgingPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const command = findCommand(meta, REPORT);
  const allowed = command !== undefined && canRun(meta, REPORT);
  const kinds = useMemo(() => (command ? (commandInputs(command).find((input) => input.name === "kind")?.options ?? []) : []), [command]);
  const kind = url.get("kind") || kinds[0]?.value || "";
  const asOf = url.get("as_of");
  const report = useAsync(allowed && company && kind ? `${company.id}|${kind}|${asOf}` : null, (signal) =>
    api.runNamed(REPORT, { company_id: company?.id ?? "", kind, as_of: asOf || null }, { signal }),
  );
  const data = isAging(report.data) ? report.data : undefined;
  const decimals = currencyDecimals[data?.currency ?? company?.base_currency ?? ""] ?? 2;
  if (!allowed || !command) return <NotFoundPage />;
  const kindLabel = (value: string, fallback: string) =>
    value === "payable" ? t("open_items.payables") : value === "receivable" ? t("open_items.receivables") : fallback;
  const itemsOf = (partyType: string, partyId: string) =>
    `${paths.report(code, "open_item.list")}?${new URLSearchParams({ kind, party_type: partyType, party_id: partyId })}`;

  return (
    <Page
      title={reportTitle(command)}
      toolbar={
        <HStack gap={3} vAlign="end" wrap="wrap">
          <SegmentedControl label={t("open_items.kind")} value={kind} onChange={(next: string) => url.set({ kind: next })}>
            {kinds.map((option) => (
              <SegmentedControlItem key={option.value} value={option.value} label={kindLabel(option.value, option.label)} />
            ))}
          </SegmentedControl>
          <DateInput label={t("report.as_of")} format="date" hasClear value={asOf ? (asOf as ISODateString) : undefined} onChange={(next) => url.set({ as_of: next ?? null }, { replace: true })} />
          {data ? (
            <span className="push-end">
              <Text type="supporting">{t("bank.amounts_in", { currency: data.currency, date: formatCalendarDate(data.as_of) })}</Text>
            </span>
          ) : null}
        </HStack>
      }
    >
      {report.error ? <Banner status="error" title={t("list.load_failed")} description={report.error.message} /> : null}
      {!company ? (
        <Text>{t("report.needs_company")}</Text>
      ) : !data ? (
        <Skeleton height={240} />
      ) : (
        <VStack gap={5}>
          <DataTable<Party>
            id={`aging.${kind}`}
            caption={reportTitle(command)}
            columns={[
              {
                id: "party",
                header: t("aging.party"),
                width: 260,
                value: (party) => party.party_name ?? party.party_id,
                cell: (party) => (party.party_name ? undefined : <LinkLabel entity={party.party_type} keyField="id" value={party.party_id} />),
              },
              ...data.buckets.map(
                (bucket, index): Column<Party> => ({
                  id: `bucket-${index}`,
                  header: bucket.from_days === null ? t("aging.current") : t("aging.days", { label: bucket.label }),
                  type: "money",
                  minFraction: decimals,
                  total: true,
                  width: 130,
                  // An empty bucket shows nothing, as before; the total still adds it as zero.
                  value: (party) => (isZeroDecimal(party.amounts[index] ?? "0") ? null : (party.amounts[index] ?? null)),
                }),
              ),
              { id: "total", header: t("aging.total"), type: "money", minFraction: decimals, total: true, width: 140, value: (party) => party.total },
              { id: "next7", header: t("aging.due_7"), type: "money", minFraction: decimals, total: true, width: 130, value: (party) => party.due_next_7 },
              { id: "next30", header: t("aging.due_30"), type: "money", minFraction: decimals, total: true, width: 130, value: (party) => party.due_next_30 },
            ]}
            rows={data.parties}
            rowKey={(party) => `${party.party_type}|${party.party_id}`}
            rowHref={(party) => itemsOf(party.party_type, party.party_id)}
            empty={{ title: t("aging.empty") }}
          />
          {data.accounts.length > 0 ? (
            <VStack gap={2} maxWidth={720}>
              <Heading level={2}>{t("aging.accounts")}</Heading>
              <MetadataList>
                {data.accounts.map((account) => {
                  const agrees = sameDecimal(account.balance, account.open);
                  return (
                    <MetadataListItem key={account.account_id} label={[account.code, account.name].join(t("value.separator"))}>
                      <HStack gap={3} vAlign="center" wrap="wrap">
                        <Link href={`${paths.report(code, "gl.account_ledger")}?account_id=${account.account_id}`}>
                          {t("aging.balance")}
                        </Link>
                        <Money value={account.balance} decimals={decimals} />
                        <Text type="supporting">{t("aging.items_add_up")}</Text>
                        <Money value={account.open} decimals={decimals} />
                        {/* A status has a word, not only a color. */}
                        {agrees ? null : <Badge variant="error" label={t("aging.differs")} />}
                      </HStack>
                    </MetadataListItem>
                  );
                })}
              </MetadataList>
            </VStack>
          ) : null}
        </VStack>
      )}
    </Page>
  );
}
