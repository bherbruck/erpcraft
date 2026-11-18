import { api } from "@/api/client";
import type { StatementSection } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Amount } from "@/components/field-value";
import { PageHeader, Section } from "@/components/page";
import { Label } from "@/components/ui/label";
import { TableCell, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { todayCalendarDate } from "@/lib/dates";
import { canRun } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

type Row = StatementSection["rows"][number];

/** One section of a statement: its accounts and its total, as the server worked them out. */
function StatementTable({
  title,
  section,
  decimals,
  range,
}: {
  /** The dates of the statement, as the query string the account ledger takes. */
  range: string;
  title: string;
  section: StatementSection | undefined;
  decimals: number;
}) {
  const { meta } = useApp();
  const { code } = useCompany();
  const columns: Column<Row>[] = [
    { id: "code", header: t("report.account_code"), cell: (row) => <span className="tabular-nums">{row.code}</span> },
    { id: "name", header: t("report.account"), cell: (row) => row.name, className: "w-full" },
    {
      id: "balance",
      header: t("report.amount"),
      align: "end",
      cell: (row) => <Amount value={row.balance} decimals={decimals} />,
    },
  ];
  return (
    <Section title={title}>
      <DataTable
        caption={title}
        columns={columns}
        rows={section?.rows}
        rowKey={(row) => row.account_id}
        rowHref={
          canRun(meta, "gl.account_ledger")
            ? (row) => `${paths.report(code, "gl.account_ledger")}?account=${row.account_id}${range}`
            : undefined
        }
        empty={<p>{t("report.section_empty", { section: title })}</p>}
        footer={
          section ? (
            <TableRow>
              <TableCell colSpan={2}>{t("report.section_total", { section: title })}</TableCell>
              <TableCell className="text-end">
                <Amount value={section.total} decimals={decimals} />
              </TableCell>
            </TableRow>
          ) : null
        }
      />
    </Section>
  );
}

/** A figure of the whole statement, under the sections. */
function Figure({ label, value, decimals }: { label: string; value: string | undefined; decimals: number }) {
  return (
    <div className="flex items-baseline justify-between gap-4 rounded-md border bg-card px-3 py-2 text-sm font-semibold">
      <span>{label}</span>
      <Amount value={value} decimals={decimals} />
    </div>
  );
}

function ReportError({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <Alert
      className="mb-3"
      title={t("list.load_failed")}
      actions={
        <Button variant="outline" size="sm" onClick={onRetry}>
          {t("action.retry")}
        </Button>
      }
    >
      {message}
    </Alert>
  );
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
    <>
      <PageHeader title={t("report.balance_sheet")} />
      <div className="mb-3 flex flex-wrap items-end gap-3">
        <div className="space-y-1">
          <Label htmlFor="as-of">{t("report.as_of")}</Label>
          <DateInput
            id="as-of"
            className="w-44"
            value={asOf}
            onChange={(next) => url.set({ as_of: next }, { replace: true })}
          />
        </div>
        {asOf ? (
          <Button variant="ghost" onClick={() => url.set({ as_of: null })}>
            {t("report.all_entries")}
          </Button>
        ) : null}
        <p className="ms-auto text-sm text-muted-foreground">
          {company ? t("report.amounts_in", { company: company.name, currency }) : t("report.needs_company")}
        </p>
      </div>
      {report.error ? <ReportError message={report.error.message} onRetry={report.reload} /> : null}
      {company && !report.error ? (
        <div className="grid items-start gap-5 xl:grid-cols-2">
          <div className="space-y-3">
            <StatementTable range={range} title={t("report.assets")} section={data?.assets} decimals={decimals} />
            <Figure label={t("report.total_assets")} value={data?.total_assets} decimals={decimals} />
          </div>
          <div className="space-y-3">
            <StatementTable
              range={range}
              title={t("report.liabilities")}
              section={data?.liabilities}
              decimals={decimals}
            />
            <StatementTable range={range} title={t("report.equity")} section={data?.equity} decimals={decimals} />
            <Figure label={t("report.net_income_open")} value={data?.net_income} decimals={decimals} />
            <Figure
              label={t("report.total_liabilities_and_equity")}
              value={data?.total_liabilities_and_equity}
              decimals={decimals}
            />
          </div>
        </div>
      ) : null}
    </>
  );
}

export function IncomeStatementPage() {
  const { meta, currencyDecimals } = useApp();
  const { company } = useCompany();
  const url = useUrlState();
  // Without dates in the URL the statement covers the current calendar year.
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
  const range = `&from=${from}&to=${to}`;

  return (
    <>
      <PageHeader title={t("report.income_statement")} />
      <div className="mb-3 flex flex-wrap items-end gap-3">
        <div className="space-y-1">
          <Label htmlFor="from">{t("report.from")}</Label>
          <DateInput
            id="from"
            className="w-44"
            value={from}
            onChange={(next) => url.set({ from: next }, { replace: true })}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="to">{t("report.to")}</Label>
          <DateInput
            id="to"
            className="w-44"
            value={to}
            onChange={(next) => url.set({ to: next }, { replace: true })}
          />
        </div>
        <p className="ms-auto text-sm text-muted-foreground">
          {company ? t("report.amounts_in", { company: company.name, currency }) : t("report.needs_company")}
        </p>
      </div>
      {report.error ? <ReportError message={report.error.message} onRetry={report.reload} /> : null}
      {company && !report.error ? (
        <div className="max-w-form space-y-3">
          <StatementTable range={range} title={t("report.income")} section={data?.income} decimals={decimals} />
          <StatementTable range={range} title={t("report.expenses")} section={data?.expenses} decimals={decimals} />
          <Figure label={t("report.net_income")} value={data?.net_income} decimals={decimals} />
        </div>
      ) : null}
    </>
  );
}
