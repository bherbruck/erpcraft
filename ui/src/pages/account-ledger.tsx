import { useMemo } from "react";
import { api } from "@/api/client";
import type { AccountLedger } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Amount, Empty } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { PageHeader } from "@/components/page";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { isZeroDecimal } from "@/lib/decimal";
import { ACCOUNT, canDoc, canRun, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { pageFromUrl, useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

type Entry = AccountLedger["entries"][number];
const PAGE_SIZE = 50;

/** The entries of one account, oldest first, with the balance after each. */
export function AccountLedgerPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const account = url.get("account");
  const from = url.get("from");
  const to = url.get("to");
  const page = pageFromUrl(url.get("page"));
  const allowed = canRun(meta, "gl.account_ledger");

  const ledger = useAsync(
    allowed && company && account ? `${company.id}|${account}|${from}|${to}|${page}` : null,
    (signal) =>
      api.run(
        "gl.account_ledger",
        {
          company_id: company?.id ?? "",
          account_id: account,
          from: from || null,
          to: to || null,
          limit: PAGE_SIZE,
          offset: (page - 1) * PAGE_SIZE,
        },
        { signal },
      ),
  );

  const currency = ledger.data?.currency ?? company?.base_currency ?? "";
  const decimals = currencyDecimals[currency] ?? 2;
  const columns = useMemo<Column<Entry>[]>(() => {
    const side = (value: string) => (isZeroDecimal(value) ? null : <Amount value={value} decimals={decimals} />);
    return [
      {
        id: "date",
        header: t("ledger.date"),
        cell: (entry) => <span className="whitespace-nowrap">{formatCalendarDate(entry.posting_date)}</span>,
      },
      {
        id: "document",
        header: t("ledger.document"),
        cell: (entry) => (
          <span className="whitespace-nowrap">
            {findDocType(meta, entry.document_type)?.label ?? entry.document_type}{" "}
            <span className="font-medium tabular-nums">{entry.document_number ?? t("document.unnumbered")}</span>
          </span>
        ),
      },
      {
        id: "memo",
        header: t("document.memo"),
        className: "w-full",
        cell: (entry) => entry.memo ?? entry.posting_memo ?? <Empty />,
      },
      { id: "debit", header: t("report.debit"), align: "end", cell: (entry) => side(entry.debit) },
      { id: "credit", header: t("report.credit"), align: "end", cell: (entry) => side(entry.credit) },
      {
        id: "balance",
        header: t("report.balance"),
        align: "end",
        cell: (entry) => <Amount value={entry.balance} decimals={decimals} />,
      },
    ];
  }, [meta, decimals]);

  if (!allowed) return <NotFoundPage />;
  const data = ledger.data;

  return (
    <>
      <PageHeader
        crumbs={[{ label: t("report.trial_balance"), to: paths.report(code, "gl.trial_balance") }]}
        title={data ? t("ledger.title_for", { account: `${data.code} · ${data.name}` }) : t("report.account_ledger")}
      />
      <div className="mb-3 flex flex-wrap items-end gap-3">
        <div className="space-y-1">
          <Label htmlFor="ledger-account">{t("report.account")}</Label>
          <Lookup
            id="ledger-account"
            className="w-72 max-w-full"
            entity={ACCOUNT}
            keyField="id"
            value={account || null}
            companyId={company?.id}
            required
            allowCreate={false}
            onChange={(next) => url.set({ account: next, page: null })}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="ledger-from">{t("report.from")}</Label>
          <DateInput
            id="ledger-from"
            className="w-44"
            value={from}
            onChange={(next) => url.set({ from: next, page: null }, { replace: true })}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="ledger-to">{t("report.to")}</Label>
          <DateInput
            id="ledger-to"
            className="w-44"
            value={to}
            onChange={(next) => url.set({ to: next, page: null }, { replace: true })}
          />
        </div>
        <dl className="ms-auto grid grid-cols-[auto_auto] gap-x-4 text-sm">
          <dt className="text-muted-foreground">{t("ledger.opening", { currency })}</dt>
          <dd className="text-end font-medium">
            {account && !data ? (
              <Skeleton className="ms-auto w-20" />
            ) : (
              <Amount value={data?.opening_balance} decimals={decimals} />
            )}
          </dd>
          <dt className="text-muted-foreground">{t("ledger.closing", { currency })}</dt>
          <dd className="text-end font-medium">
            {account && !data ? (
              <Skeleton className="ms-auto w-20" />
            ) : (
              <Amount value={data?.closing_balance} decimals={decimals} />
            )}
          </dd>
        </dl>
      </div>
      <DataTable
        caption={t("report.account_ledger")}
        columns={columns}
        rows={account && company ? data?.entries : []}
        rowKey={(entry) => entry.id}
        rowHref={(entry) =>
          entry.document_type && entry.document_id && canDoc(meta, entry.document_type, "read")
            ? paths.doc(code, entry.document_type, entry.document_id)
            : undefined
        }
        loading={ledger.loading}
        error={ledger.error}
        onRetry={ledger.reload}
        page={page}
        pageSize={PAGE_SIZE}
        total={data?.total ?? 0}
        onPage={(next) => url.set({ page: next === 1 ? null : String(next) })}
        empty={
          <p>
            {!company
              ? t("report.needs_company")
              : !account
                ? t("ledger.choose_account")
                : from || to
                  ? t("ledger.empty_range")
                  : t("ledger.empty")}
          </p>
        }
      />
    </>
  );
}
