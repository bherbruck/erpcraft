// A bank account's ledger balance set against its statements at a date: what
// the books say, what the bank says, the postings the bank has not shown yet,
// the statement lines the books do not hold yet, and whether that explains the
// difference to the cent.

import { useMemo } from "react";
import { api } from "@/api/client";
import type { BankReconciliationOutput } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Amount, Empty } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { PageHeader, Section } from "@/components/page";
import { Label } from "@/components/ui/label";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";
import { formatCalendarDate } from "@/lib/dates";
import { isZeroDecimal } from "@/lib/decimal";
import { canDoc, canRun, commandInputs, findCommand, findDocType, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

const REPORT = "bank.reconciliation";
type Outstanding = BankReconciliationOutput["outstanding"][number];
type Unmatched = BankReconciliationOutput["unmatched_lines"][number];

export function BankReconciliationPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const account = url.get("account");
  const asOf = url.get("as_of");
  const command = findCommand(meta, REPORT);
  const allowed = command !== undefined && canRun(meta, REPORT);
  const accountType = command
    ? commandInputs(command).find((input) => input.name === "bank_account_id")?.fieldType
    : null;

  const report = useAsync(allowed && account ? `${account}|${asOf}` : null, (signal) =>
    api.run("bank.reconciliation", { bank_account_id: account, as_of: asOf || null }, { signal }),
  );
  const data = report.data;
  const decimals = currencyDecimals[data?.currency ?? company?.base_currency ?? ""] ?? 2;

  const outstandingColumns = useMemo<Column<Outstanding>[]>(
    () => [
      {
        id: "date",
        header: t("bank.posting_date"),
        cell: (row) => <span className="whitespace-nowrap">{formatCalendarDate(row.posting_date)}</span>,
      },
      {
        id: "document",
        header: t("ledger.document"),
        cell: (row) => (
          <span className="whitespace-nowrap">
            {findDocType(meta, row.doc_type)?.label ?? row.doc_type} {row.number ?? t("document.unnumbered")}
          </span>
        ),
      },
      { id: "memo", header: t("document.memo"), className: "w-full", cell: (row) => row.memo ?? <Empty /> },
      {
        id: "amount",
        header: t("report.amount"),
        align: "end",
        cell: (row) => <Amount value={row.amount} decimals={decimals} />,
      },
    ],
    [meta, decimals],
  );
  const unmatchedColumns = useMemo<Column<Unmatched>[]>(
    () => [
      {
        id: "date",
        header: t("bank.booking_date"),
        cell: (row) => <span className="whitespace-nowrap">{formatCalendarDate(row.booking_date)}</span>,
      },
      {
        id: "counterparty",
        header: t("bank.counterparty"),
        cell: (row) => <span className="whitespace-nowrap">{row.counterparty_name ?? <Empty />}</span>,
      },
      { id: "reference", header: t("bank.reference"), className: "w-full", cell: (row) => row.reference ?? <Empty /> },
      {
        id: "amount",
        header: t("report.amount"),
        align: "end",
        cell: (row) => <Amount value={row.amount} decimals={decimals} />,
      },
      {
        id: "unmatched",
        header: t("bank.unmatched"),
        align: "end",
        cell: (row) => <Amount value={row.unmatched} decimals={decimals} />,
      },
    ],
    [decimals],
  );

  if (!allowed || !command) return <NotFoundPage />;
  const figures: [string, string | undefined, boolean?][] = data
    ? [
        [t("bank.ledger_balance"), data.ledger_balance],
        [t("bank.statement_balance"), data.statement_balance],
        [t("bank.difference"), data.difference, true],
        [t("bank.outstanding_total"), data.outstanding_total],
        [t("bank.unmatched_total"), data.unmatched_total],
        [t("bank.first_opening"), data.first_opening_balance],
        [t("bank.explained"), data.explained],
      ]
    : [];
  const agrees = data ? isZeroDecimal(data.unexplained) : false;

  return (
    <>
      <PageHeader title={reportTitle(command)} />
      <p className="mb-3 max-w-form text-sm text-muted-foreground">{command.description}</p>
      <div className="mb-4 flex flex-wrap items-end gap-3">
        <div className="space-y-1">
          <Label htmlFor="recon-account">{t("bank.account")}</Label>
          <Lookup
            id="recon-account"
            className="w-72 max-w-full"
            entity={accountType?.kind === "link" ? accountType.entity : "account"}
            keyField="id"
            filters={accountType?.kind === "link" ? accountType.filters : undefined}
            value={account || null}
            companyId={company?.id}
            required
            allowCreate={false}
            onChange={(next) => url.set({ account: next })}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="recon-as-of">{t("report.as_of")}</Label>
          <DateInput
            id="recon-as-of"
            className="w-44"
            value={asOf}
            onChange={(next) => url.set({ as_of: next }, { replace: true })}
          />
        </div>
        {data ? (
          <p className="ms-auto text-sm text-muted-foreground">
            {t("bank.amounts_in", { currency: data.currency, date: formatCalendarDate(data.as_of) })}
          </p>
        ) : null}
      </div>
      {!account ? (
        <p className="text-sm text-muted-foreground">{t("bank.choose_account")}</p>
      ) : report.error ? (
        <p role="alert" className="text-sm text-destructive">
          {report.error.message}
        </p>
      ) : data ? (
        <div className="space-y-5">
          <dl className="grid max-w-xl grid-cols-[1fr_auto] gap-x-6 gap-y-1 rounded-md border bg-card p-4 text-sm">
            {figures.map(([label, value, strong]) => (
              <div key={label} className="contents">
                <dt className={cn("text-muted-foreground", strong && "font-medium text-foreground")}>{label}</dt>
                <dd className={cn("text-end", strong && "font-semibold")}>
                  <Amount value={value} decimals={decimals} />
                </dd>
              </div>
            ))}
            <dt className="border-t pt-1 font-medium">{t("bank.unexplained")}</dt>
            <dd className={cn("border-t pt-1 text-end font-semibold", !agrees && "text-destructive")}>
              <Amount value={data.unexplained} decimals={decimals} />
            </dd>
            <dd className="col-span-2 text-xs text-muted-foreground">
              {agrees ? t("bank.agrees") : t("bank.disagrees")}
            </dd>
          </dl>
          <Section title={t("bank.outstanding")}>
            <DataTable
              caption={t("bank.outstanding")}
              columns={outstandingColumns}
              rows={data.outstanding}
              rowKey={(row) => row.posting_id}
              rowHref={(row) =>
                canDoc(meta, row.doc_type, "read") ? paths.doc(code, row.doc_type, row.document_id) : undefined
              }
              empty={<p>{t("bank.none_outstanding")}</p>}
            />
          </Section>
          <Section title={t("bank.unmatched_lines")}>
            <DataTable
              caption={t("bank.unmatched_lines")}
              columns={unmatchedColumns}
              rows={data.unmatched_lines}
              rowKey={(row) => row.line_id}
              rowHref={(row) => paths.doc(code, "bank_statement", row.statement_id)}
              empty={<p>{t("bank.none_unmatched")}</p>}
            />
          </Section>
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">{t("value.loading")}</p>
      )}
    </>
  );
}
