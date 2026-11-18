// What a company owes or is owed, by vendor or customer, in buckets of days past
// due (`open_item.aging`); what falls due soon; and each control account's balance
// beside what its items add up to. A party's row leads to its open items.

import { useMemo } from "react";
import { Link } from "react-router";
import { api } from "@/api/client";
import type { OpenItemAgingOutput } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Amount, LinkLabel } from "@/components/field-value";
import { PageHeader, Section } from "@/components/page";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { TableCell, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";
import { formatCalendarDate } from "@/lib/dates";
import { isZeroDecimal, sameDecimal } from "@/lib/decimal";
import { canRun, commandInputs, findCommand, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

const REPORT = "open_item.aging";
type Party = OpenItemAgingOutput["parties"][number];

export function AgingPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const command = findCommand(meta, REPORT);
  const allowed = command !== undefined && canRun(meta, REPORT);
  const kinds = useMemo(
    () => (command ? (commandInputs(command).find((input) => input.name === "kind")?.options ?? []) : []),
    [command],
  );
  const kind = url.get("kind") || kinds[0]?.value || "";
  const asOf = url.get("as_of");

  const report = useAsync(allowed && company && kind ? `${company.id}|${kind}|${asOf}` : null, (signal) =>
    api.runNamed(REPORT, { company_id: company?.id ?? "", kind, as_of: asOf || null }, { signal }),
  );
  const data = isAging(report.data) ? report.data : undefined;
  const decimals = currencyDecimals[data?.currency ?? company?.base_currency ?? ""] ?? 2;
  // A party's open items, on the page that lists them and pays them.
  const itemsOf = (party: Party): string =>
    `${paths.report(code, "open_item.list")}?${new URLSearchParams({ kind, party: party.party_id })}`;

  const columns = useMemo<Column<Party>[]>(() => {
    if (!data) return [];
    return [
      {
        id: "party",
        header: t("aging.party"),
        className: "w-full",
        cell: (party) =>
          party.party_name ?? <LinkLabel entity={party.party_type} keyField="id" value={party.party_id} />,
      },
      ...data.buckets.map(
        (bucket, index): Column<Party> => ({
          id: `bucket-${index}`,
          header: bucket.from_days === null ? t("aging.current") : t("aging.days", { label: bucket.label }),
          align: "end",
          cell: (party) => {
            const value = party.amounts[index] ?? "0";
            return isZeroDecimal(value) ? null : <Amount value={value} decimals={decimals} />;
          },
        }),
      ),
      {
        id: "total",
        header: t("aging.total"),
        align: "end",
        cell: (party) => (
          <span className="font-medium">
            <Amount value={party.total} decimals={decimals} />
          </span>
        ),
      },
      {
        id: "next7",
        header: t("aging.due_7"),
        align: "end",
        cell: (party) => <Amount value={party.due_next_7} decimals={decimals} />,
      },
      {
        id: "next30",
        header: t("aging.due_30"),
        align: "end",
        cell: (party) => <Amount value={party.due_next_30} decimals={decimals} />,
      },
    ];
  }, [data, decimals]);

  if (!allowed || !command) return <NotFoundPage />;
  return (
    <>
      <PageHeader title={reportTitle(command)} />
      <div className="mb-3 flex flex-wrap items-end gap-3">
        <div className="space-y-1">
          <Label htmlFor="aging-kind">{t("open_items.kind")}</Label>
          <Select
            id="aging-kind"
            className="w-60"
            value={kind}
            onChange={(event) => url.set({ kind: event.target.value })}
          >
            {kinds.map((option) => (
              <option key={option.value} value={option.value}>
                {option.value === "payable"
                  ? t("open_items.payables")
                  : option.value === "receivable"
                    ? t("open_items.receivables")
                    : option.label}
              </option>
            ))}
          </Select>
        </div>
        <div className="space-y-1">
          <Label htmlFor="aging-as-of">{t("report.as_of")}</Label>
          <DateInput
            id="aging-as-of"
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
      <DataTable
        caption={reportTitle(command)}
        columns={columns}
        rows={company ? data?.parties : []}
        rowKey={(party) => `${party.party_type}|${party.party_id}`}
        rowHref={itemsOf}
        loading={report.loading}
        error={report.error}
        onRetry={report.reload}
        empty={<p>{company ? t("aging.empty") : t("report.needs_company")}</p>}
        footer={
          data ? (
            <TableRow>
              <TableCell>{t("report.total")}</TableCell>
              {data.totals.map((value, index) => (
                <TableCell key={data.buckets[index]?.label ?? index} className="text-end">
                  <Amount value={value} decimals={decimals} />
                </TableCell>
              ))}
              <TableCell className="text-end font-semibold">
                <Amount value={data.total} decimals={decimals} />
              </TableCell>
              <TableCell className="text-end">
                <Amount value={data.due_next_7} decimals={decimals} />
              </TableCell>
              <TableCell className="text-end">
                <Amount value={data.due_next_30} decimals={decimals} />
              </TableCell>
            </TableRow>
          ) : null
        }
      />
      {data && data.accounts.length > 0 ? (
        <div className="mt-5">
          <Section title={t("aging.accounts")}>
            <ul className="max-w-xl divide-y rounded-md border bg-card text-sm">
              {data.accounts.map((account) => {
                const agrees = sameDecimal(account.balance, account.open);
                return (
                  <li
                    key={account.account_id}
                    className="grid grid-cols-[1fr_auto_auto] items-center gap-x-4 px-3 py-2"
                  >
                    <Link
                      to={`${paths.report(code, "gl.account_ledger")}?account=${account.account_id}`}
                      className="underline underline-offset-2 hover:no-underline"
                    >
                      {[account.code, account.name].join(t("value.separator"))}
                    </Link>
                    <span className="text-end">
                      <span className="block text-xs text-muted-foreground">{t("aging.balance")}</span>
                      <Amount value={account.balance} decimals={decimals} />
                    </span>
                    <span className={cn("text-end", !agrees && "text-destructive")}>
                      <span className="block text-xs text-muted-foreground">{t("aging.items_add_up")}</span>
                      <Amount value={account.open} decimals={decimals} />
                    </span>
                  </li>
                );
              })}
            </ul>
          </Section>
        </div>
      ) : null}
    </>
  );
}

function isAging(value: unknown): value is OpenItemAgingOutput {
  return typeof value === "object" && value !== null && "parties" in value && "buckets" in value && "totals" in value;
}
