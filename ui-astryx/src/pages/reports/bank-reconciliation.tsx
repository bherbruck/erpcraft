// A bank account's ledger balance set against its statements at a date: what the books say,
// what the bank says, postings the bank has not shown yet, statement lines the books do not
// hold yet, and whether that explains the difference to the cent.

import { Badge } from "@astryxdesign/core/Badge";
import { Banner } from "@astryxdesign/core/Banner";
import { Card } from "@astryxdesign/core/Card";
import { DateInput } from "@astryxdesign/core/DateInput";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { MetadataList, MetadataListItem } from "@astryxdesign/core/MetadataList";
import { Skeleton } from "@astryxdesign/core/Skeleton";
import { Text } from "@astryxdesign/core/Text";
import type { ISODateString } from "@astryxdesign/core/utils";
import { VStack } from "@astryxdesign/core/VStack";
import { useMemo } from "react";
import { api } from "@/api/client";
import type { BankReconciliationOutput } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { Empty } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { Page } from "@/components/page";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { isZeroDecimal } from "@/lib/decimal";
import { canDoc, canRun, commandInputs, findCommand, findDocType, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "../not-found";
import { Money } from "./statements";

const REPORT = "bank.reconciliation";
type Outstanding = BankReconciliationOutput["outstanding"][number];
type Unmatched = BankReconciliationOutput["unmatched_lines"][number];

export function BankReconciliationPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const account = url.get("account") || url.get("bank_account_id");
  const asOf = url.get("as_of");
  const command = findCommand(meta, REPORT);
  const allowed = command !== undefined && canRun(meta, REPORT);
  const accountType = command ? commandInputs(command).find((input) => input.name === "bank_account_id")?.fieldType : null;
  const report = useAsync(allowed && account ? `${account}|${asOf}` : null, (signal) =>
    api.run("bank.reconciliation", { bank_account_id: account, as_of: asOf || null }, { signal }),
  );
  const data = report.data;
  const decimals = currencyDecimals[data?.currency ?? company?.base_currency ?? ""] ?? 2;

  const outstandingColumns = useMemo<Column<Outstanding>[]>(
    () => [
      { id: "date", header: t("bank.posting_date"), width: 130, cell: (row) => formatCalendarDate(row.posting_date) },
      {
        id: "document",
        header: t("ledger.document"),
        width: 220,
        cell: (row) => `${findDocType(meta, row.doc_type)?.label ?? row.doc_type} ${row.number ?? t("document.unnumbered")}`,
      },
      { id: "memo", header: t("document.memo"), cell: (row) => row.memo ?? <Empty /> },
      { id: "amount", header: t("report.amount"), align: "end", width: 150, cell: (row) => <Money value={row.amount} decimals={decimals} /> },
    ],
    [meta, decimals],
  );
  const unmatchedColumns = useMemo<Column<Unmatched>[]>(
    () => [
      { id: "date", header: t("bank.booking_date"), width: 130, cell: (row) => formatCalendarDate(row.booking_date) },
      { id: "counterparty", header: t("bank.counterparty"), cell: (row) => row.counterparty_name ?? <Empty /> },
      { id: "reference", header: t("bank.reference"), cell: (row) => row.reference ?? <Empty /> },
      { id: "amount", header: t("report.amount"), align: "end", width: 140, cell: (row) => <Money value={row.amount} decimals={decimals} /> },
      { id: "unmatched", header: t("bank.unmatched"), align: "end", width: 140, cell: (row) => <Money value={row.unmatched} decimals={decimals} /> },
    ],
    [decimals],
  );

  if (!allowed || !command) return <NotFoundPage />;
  const figures: [string, string | undefined][] = data
    ? [
        [t("bank.ledger_balance"), data.ledger_balance],
        [t("bank.statement_balance"), data.statement_balance],
        [t("bank.difference"), data.difference],
        [t("bank.outstanding_total"), data.outstanding_total],
        [t("bank.unmatched_total"), data.unmatched_total],
        [t("bank.first_opening"), data.first_opening_balance],
        [t("bank.explained"), data.explained],
      ]
    : [];
  const agrees = data ? isZeroDecimal(data.unexplained) : false;

  return (
    <Page
      title={reportTitle(command)}
      toolbar={
        <VStack gap={3}>
          {command.description ? <Text type="supporting">{command.description}</Text> : null}
          <HStack gap={3} vAlign="end" wrap="wrap">
            <div className="report-filter">
              <Lookup
                label={t("bank.account")}
                entity={accountType?.kind === "link" ? accountType.entity : "account"}
                keyField="id"
                filters={accountType?.kind === "link" ? accountType.filters : undefined}
                value={account || null}
                companyId={company?.id}
                required
                allowCreate={false}
                onChange={(next) => url.set({ account: next, bank_account_id: null })}
              />
            </div>
            <DateInput label={t("report.as_of")} format="date" hasClear value={asOf ? (asOf as ISODateString) : undefined} onChange={(next) => url.set({ as_of: next ?? null }, { replace: true })} />
            {data ? (
              <span className="push-end">
                <Text type="supporting">{t("bank.amounts_in", { currency: data.currency, date: formatCalendarDate(data.as_of) })}</Text>
              </span>
            ) : null}
          </HStack>
        </VStack>
      }
    >
      {!account ? (
        <Text type="supporting">{t("bank.choose_account")}</Text>
      ) : report.error ? (
        <Banner status="error" title={t("list.load_failed")} description={report.error.message} />
      ) : !data ? (
        <Skeleton height={240} />
      ) : (
        <VStack gap={5}>
          <Card padding={4} maxWidth={620}>
            <VStack gap={3}>
              <MetadataList label={{ position: "start", width: "60%" }}>
                {figures.map(([label, value]) => (
                  <MetadataListItem key={label} label={label}>
                    <span className="num-block">
                      <Money value={value} decimals={decimals} />
                    </span>
                  </MetadataListItem>
                ))}
                <MetadataListItem label={t("bank.unexplained")}>
                  <span className="num-block">
                    <Money value={data.unexplained} decimals={decimals} strong />
                  </span>
                </MetadataListItem>
              </MetadataList>
              <HStack gap={2} vAlign="center">
                <Badge variant={agrees ? "success" : "error"} label={agrees ? t("bank.agrees_short") : t("bank.disagrees_short")} />
                <Text type="supporting">{agrees ? t("bank.agrees") : t("bank.disagrees")}</Text>
              </HStack>
            </VStack>
          </Card>
          <VStack gap={2}>
            <Heading level={2}>{t("bank.outstanding")}</Heading>
            <DataTable
              caption={t("bank.outstanding")}
              columns={outstandingColumns}
              rows={data.outstanding}
              rowKey={(row) => row.posting_id}
              rowHref={(row) => (canDoc(meta, row.doc_type, "read") ? paths.doc(code, row.doc_type, row.document_id) : undefined)}
              empty={{ title: t("bank.none_outstanding") }}
            />
          </VStack>
          <VStack gap={2}>
            <Heading level={2}>{t("bank.unmatched_lines")}</Heading>
            <DataTable
              caption={t("bank.unmatched_lines")}
              columns={unmatchedColumns}
              rows={data.unmatched_lines}
              rowKey={(row) => row.line_id}
              rowHref={(row) => paths.doc(code, "bank_statement", row.statement_id)}
              empty={{ title: t("bank.none_unmatched") }}
            />
          </VStack>
        </VStack>
      )}
    </Page>
  );
}
