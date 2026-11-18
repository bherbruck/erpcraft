import { api } from "@/api/client";
import type { TrialBalance } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Button } from "@/components/button";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Amount } from "@/components/field-value";
import { PageHeader } from "@/components/page";
import { Label } from "@/components/ui/label";
import { TableCell, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { isZeroDecimal } from "@/lib/decimal";
import { ACCOUNT, canRun, enumLabel, findEntity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

type Row = TrialBalance["rows"][number];

export function TrialBalancePage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const asOf = url.get("as_of");
  const allowed = canRun(meta, "gl.trial_balance");

  const report = useAsync(allowed && company ? `${company.id}|${asOf}` : null, (signal) =>
    api.run("gl.trial_balance", { company_id: company?.id ?? "", as_of: asOf || null }, { signal }),
  );
  if (!allowed) return <NotFoundPage />;

  const currency = report.data?.currency ?? company?.base_currency ?? "";
  const decimals = currencyDecimals[currency] ?? 2;
  const accountType = findEntity(meta, ACCOUNT)?.fields.find((field) => field.name === "account_type");
  const side = (value: string) => (isZeroDecimal(value) ? null : <Amount value={value} decimals={decimals} />);
  const columns: Column<Row>[] = [
    { id: "code", header: t("report.account_code"), cell: (row) => <span className="tabular-nums">{row.code}</span> },
    { id: "name", header: t("report.account"), cell: (row) => row.name, className: "w-full" },
    {
      id: "type",
      header: t("report.account_type"),
      cell: (row) => (
        <span className="whitespace-nowrap">
          {accountType ? enumLabel(accountType, row.account_type) : row.account_type}
        </span>
      ),
    },
    { id: "debit", header: t("report.debit"), align: "end", cell: (row) => side(row.debit) },
    { id: "credit", header: t("report.credit"), align: "end", cell: (row) => side(row.credit) },
    {
      id: "balance",
      header: t("report.balance"),
      align: "end",
      cell: (row) => <Amount value={row.balance} decimals={decimals} />,
    },
  ];

  return (
    <>
      <PageHeader title={t("report.trial_balance")} />
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
          {company ? t("report.amounts_in", { company: company.name, currency }) : null}
        </p>
      </div>
      <DataTable
        caption={t("report.trial_balance")}
        columns={columns}
        rows={company ? report.data?.rows : []}
        rowKey={(row) => row.account_id}
        rowHref={
          canRun(meta, "gl.account_ledger")
            ? (row) =>
                `${paths.report(code, "gl.account_ledger")}?account=${row.account_id}${asOf ? `&to=${asOf}` : ""}`
            : undefined
        }
        loading={report.loading}
        error={report.error}
        onRetry={report.reload}
        empty={
          <p>{company ? t("report.trial_balance_empty", { company: company.name }) : t("report.needs_company")}</p>
        }
        footer={
          report.data ? (
            <TableRow>
              <TableCell colSpan={3}>{t("report.total")}</TableCell>
              <TableCell className="text-end">
                <Amount value={report.data.total_debit} decimals={decimals} />
              </TableCell>
              <TableCell className="text-end">
                <Amount value={report.data.total_credit} decimals={decimals} />
              </TableCell>
              <TableCell />
            </TableRow>
          ) : null
        }
      />
    </>
  );
}
