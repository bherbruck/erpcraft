// The balance sheet and the income statement, laid out as statements: sections of accounts
// with their totals, then the figures of the whole statement. Each account leads to its ledger.

import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { DateInput } from "@astryxdesign/core/DateInput";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Text } from "@astryxdesign/core/Text";
import type { ISODateString } from "@astryxdesign/core/utils";
import { VStack } from "@astryxdesign/core/VStack";
import { api } from "@/api/client";
import type { StatementSection } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { Page } from "@/components/page";
import { t } from "@/i18n/t";
import { todayCalendarDate } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { canRun } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "../not-found";

export function Money({ value, decimals, strong }: { value: string | null | undefined; decimals: number; strong?: boolean }) {
  if (value == null || value === "") return <Text color="secondary">{t("value.none")}</Text>;
  return (
    <Text hasTabularNumbers weight={strong ? "semibold" : undefined}>
      <span className="nowrap">{formatDecimal(value, { minFraction: decimals })}</span>
    </Text>
  );
}

interface StatementRow {
  account_id: string;
  code: string;
  name: string;
  balance: string;
}

function StatementTable({ title, section, decimals, range }: { title: string; section: StatementSection | undefined; decimals: number; range: string }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const ledger = canRun(meta, "gl.account_ledger");
  const columns: Column<StatementRow>[] = [
    { id: "code", header: t("report.account_code"), width: 120, value: (row) => row.code },
    { id: "name", header: t("report.account"), width: 300, value: (row) => row.name },
    // The section's total is the grid's exact sum of its rows, under the column.
    { id: "balance", header: t("report.amount"), type: "money", minFraction: decimals, total: true, width: 160, value: (row) => row.balance },
  ];
  return (
    <VStack gap={2}>
      <Heading level={2}>{title}</Heading>
      <DataTable
        id={`statement.${title}`}
        caption={title}
        columns={columns}
        rows={section?.rows}
        rowKey={(row) => row.account_id}
        rowHref={ledger ? (row) => `${paths.report(code, "gl.account_ledger")}?account_id=${row.account_id}${range}` : undefined}
        empty={{ title: t("report.section_empty", { section: title }) }}
      />
      {section ? (
        <HStack hAlign="between">
          <Text type="supporting">{t("report.section_total", { section: title })}</Text>
          <Money value={section.total} decimals={decimals} strong />
        </HStack>
      ) : null}
    </VStack>
  );
}

function Figure({ label, value, decimals }: { label: string; value: string | undefined; decimals: number }) {
  return (
    <Card padding={3} variant="muted">
      <HStack hAlign="between" vAlign="center">
        <Text weight="semibold">{label}</Text>
        <Money value={value} decimals={decimals} strong />
      </HStack>
    </Card>
  );
}

function Error({ message, onRetry }: { message: string; onRetry: () => void }) {
  return <Banner status="error" title={t("list.load_failed")} description={message} endContent={<Button label={t("action.retry")} size="sm" onClick={onRetry} />} />;
}

export function BalanceSheetPage() {
  const { meta, currencyDecimals } = useApp();
  const { company } = useCompany();
  const url = useUrlState();
  const asOf = url.get("as_of");
  const allowed = canRun(meta, "gl.balance_sheet");
  const report = useAsync(allowed && company ? `${company.id}|${asOf}` : null, (signal) =>
    api.run("gl.balance_sheet", { company_id: company?.id ?? "", as_of: asOf || null }, { signal }),
  );
  if (!allowed) return <NotFoundPage />;
  const currency = report.data?.currency ?? company?.base_currency ?? "";
  const decimals = currencyDecimals[currency] ?? 2;
  const data = report.data;
  const range = asOf ? `&to=${asOf}` : "";
  return (
    <Page
      title={t("report.balance_sheet")}
      toolbar={
        <HStack gap={3} vAlign="end" wrap="wrap">
          <DateInput label={t("report.as_of")} format="date" hasClear value={asOf ? (asOf as ISODateString) : undefined} onChange={(next) => url.set({ as_of: next ?? null }, { replace: true })} />
          <span className="push-end">
            <Text type="supporting">{company ? t("report.amounts_in", { company: company.name, currency }) : t("report.needs_company")}</Text>
          </span>
        </HStack>
      }
    >
      {report.error ? <Error message={report.error.message} onRetry={report.reload} /> : null}
      {company && !report.error ? (
        <div className="two-columns">
          <VStack gap={3}>
            <StatementTable range={range} title={t("report.assets")} section={data?.assets} decimals={decimals} />
            <Figure label={t("report.total_assets")} value={data?.total_assets} decimals={decimals} />
          </VStack>
          <VStack gap={3}>
            <StatementTable range={range} title={t("report.liabilities")} section={data?.liabilities} decimals={decimals} />
            <StatementTable range={range} title={t("report.equity")} section={data?.equity} decimals={decimals} />
            <Figure label={t("report.net_income_open")} value={data?.net_income} decimals={decimals} />
            <Figure label={t("report.total_liabilities_and_equity")} value={data?.total_liabilities_and_equity} decimals={decimals} />
          </VStack>
        </div>
      ) : null}
    </Page>
  );
}

export function IncomeStatementPage() {
  const { meta, currencyDecimals } = useApp();
  const { company } = useCompany();
  const url = useUrlState();
  const year = todayCalendarDate().slice(0, 4);
  const from = url.get("from") || `${year}-01-01`;
  const to = url.get("to") || `${year}-12-31`;
  const allowed = canRun(meta, "gl.income_statement");
  const report = useAsync(allowed && company ? `${company.id}|${from}|${to}` : null, (signal) =>
    api.run("gl.income_statement", { company_id: company?.id ?? "", from, to }, { signal }),
  );
  if (!allowed) return <NotFoundPage />;
  const currency = report.data?.currency ?? company?.base_currency ?? "";
  const decimals = currencyDecimals[currency] ?? 2;
  const data = report.data;
  return (
    <Page
      title={t("report.income_statement")}
      toolbar={
        <HStack gap={3} vAlign="end" wrap="wrap">
          <DateInput label={t("report.from")} format="date" value={from as ISODateString} onChange={(next) => url.set({ from: next ?? null }, { replace: true })} />
          <DateInput label={t("report.to")} format="date" value={to as ISODateString} onChange={(next) => url.set({ to: next ?? null }, { replace: true })} />
          <span className="push-end">
            <Text type="supporting">{company ? t("report.amounts_in", { company: company.name, currency }) : t("report.needs_company")}</Text>
          </span>
        </HStack>
      }
    >
      {report.error ? <Error message={report.error.message} onRetry={report.reload} /> : null}
      {company && !report.error ? (
        <VStack gap={3} maxWidth={760}>
          <StatementTable range={`&from=${from}&to=${to}`} title={t("report.income")} section={data?.income} decimals={decimals} />
          <StatementTable range={`&from=${from}&to=${to}`} title={t("report.expenses")} section={data?.expenses} decimals={decimals} />
          <Figure label={t("report.net_income")} value={data?.net_income} decimals={decimals} />
        </VStack>
      ) : null}
    </Page>
  );
}
