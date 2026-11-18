// The fixed asset register beside the ledger, and one asset's depreciation
// month by month. Column titles are the report schemas' own; statuses and
// methods are labelled as the asset entity labels them.

import { type ReactNode, useMemo } from "react";
import { Link } from "react-router";
import { api } from "@/api/client";
import type { AssetRegisterOutput, AssetScheduleOutput } from "@/api/generated";
import type { FieldMeta } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Amount, Empty } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { PageHeader, Section } from "@/components/page";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Label } from "@/components/ui/label";
import { TableCell, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";
import { formatCalendarDate } from "@/lib/dates";
import { formatDecimal, isZeroDecimal } from "@/lib/decimal";
import { canEntity, canRun, enumLabel, findCommand, findEntity, outputTitle, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

const REGISTER = "asset.register";
const SCHEDULE = "asset.schedule";
const ASSET = "asset";
type RegisterRow = AssetRegisterOutput["assets"][number];
type LedgerAccount = AssetRegisterOutput["accounts"][number];
type ScheduleRow = AssetScheduleOutput["months"][number];

/** A field of the asset entity, for its labels. */
function useAssetField(name: string): FieldMeta | undefined {
  const { meta } = useApp();
  return findEntity(meta, ASSET)?.fields.find((field) => field.name === name);
}

function labelled(field: FieldMeta | undefined, value: string): string {
  return field ? enumLabel(field, value) : value;
}

/** A month as "Jul 2026", from its first day. */
function monthLabel(day: string): string {
  const [year, month] = day.split("-").map(Number);
  if (!year || !month) return day;
  return new Intl.DateTimeFormat(undefined, { month: "short", year: "numeric", timeZone: "UTC" }).format(
    new Date(Date.UTC(year, month - 1, 1)),
  );
}

export function AssetRegisterPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const asOf = url.get("as_of");
  const command = findCommand(meta, REGISTER);
  const allowed = command !== undefined && canRun(meta, REGISTER);
  const statusField = useAssetField("status");

  const report = useAsync(allowed && company ? `${company.id}|${asOf}` : null, (signal) =>
    api.run(REGISTER, { company_id: company?.id ?? "", as_of: asOf || null }, { signal }),
  );
  const data = report.data;
  const decimals = currencyDecimals[data?.currency ?? company?.base_currency ?? ""] ?? 2;
  const canSchedule = canRun(meta, SCHEDULE);

  const columns = useMemo<Column<RegisterRow>[]>(() => {
    if (!command) return [];
    const title = (field: string) => outputTitle(command, field, "RegisterRow");
    const amount = (field: "cost" | "accumulated" | "book_value" | "depreciation_this_year"): Column<RegisterRow> => ({
      id: field,
      header: title(field),
      align: "end",
      cell: (row) => <Amount value={row[field]} decimals={decimals} />,
    });
    return [
      { id: "code", header: title("code"), cell: (row) => <span className="whitespace-nowrap">{row.code}</span> },
      { id: "name", header: title("name"), className: "w-full", cell: (row) => row.name },
      { id: "class", header: title("class_code"), cell: (row) => row.class_code },
      {
        id: "status",
        header: title("status"),
        cell: (row) => <span className="whitespace-nowrap">{labelled(statusField, row.status)}</span>,
      },
      {
        id: "in_service",
        header: title("in_service_date"),
        cell: (row) =>
          row.in_service_date ? (
            <span className="whitespace-nowrap">{formatCalendarDate(row.in_service_date)}</span>
          ) : (
            <Empty />
          ),
      },
      { id: "location", header: title("location"), cell: (row) => row.location ?? <Empty /> },
      amount("cost"),
      amount("accumulated"),
      amount("book_value"),
      amount("depreciation_this_year"),
    ];
  }, [command, decimals, statusField]);

  if (!allowed || !command) return <NotFoundPage />;
  const accountTitle = (field: string) => outputTitle(command, field, "LedgerAccount");

  return (
    <>
      <PageHeader title={reportTitle(command)} />
      <p className="mb-3 max-w-form text-sm text-muted-foreground">{command.description}</p>
      <div className="mb-4 flex flex-wrap items-end gap-3">
        <div className="space-y-1">
          <Label htmlFor="register-as-of">{t("report.as_of")}</Label>
          <DateInput
            id="register-as-of"
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
      {data ? (
        <Alert
          className="mb-4"
          tone={data.ties ? "info" : "destructive"}
          title={data.ties ? t("assets.ties") : t("assets.does_not_tie")}
        >
          {data.ties ? t("assets.ties_detail") : t("assets.does_not_tie_detail")}
        </Alert>
      ) : null}
      <div className="space-y-5">
        {data && data.accounts.length > 0 ? (
          <Section title={t("assets.accounts")}>
            <DataTable<LedgerAccount>
              caption={t("assets.accounts")}
              rows={data.accounts}
              rowKey={(row) => row.account_id}
              rowHref={(row) =>
                canRun(meta, "gl.account_ledger")
                  ? `${paths.report(code, "gl.account_ledger")}?${new URLSearchParams({ account: row.account_id })}`
                  : undefined
              }
              empty={null}
              columns={[
                {
                  id: "account",
                  header: accountTitle("name"),
                  className: "w-full",
                  cell: (row) => (
                    <span className="whitespace-nowrap">{[row.code, row.name].join(t("value.separator"))}</span>
                  ),
                },
                {
                  id: "register",
                  header: accountTitle("register"),
                  align: "end",
                  cell: (row) => <Amount value={row.register} decimals={decimals} />,
                },
                {
                  id: "ledger",
                  header: accountTitle("ledger"),
                  align: "end",
                  cell: (row) => <Amount value={row.ledger} decimals={decimals} />,
                },
                {
                  id: "difference",
                  header: accountTitle("difference"),
                  align: "end",
                  cell: (row) => (
                    <span className={cn(!isZeroDecimal(row.difference) && "font-semibold text-destructive")}>
                      <Amount value={row.difference} decimals={decimals} />
                    </span>
                  ),
                },
                {
                  id: "ties",
                  header: t("assets.account_ties"),
                  cell: (row) =>
                    isZeroDecimal(row.difference) ? (
                      <Badge tone="success">{t("assets.agrees")}</Badge>
                    ) : (
                      <Badge tone="destructive">{t("assets.differs")}</Badge>
                    ),
                },
              ]}
            />
          </Section>
        ) : null}
        <Section title={outputTitle(command, "assets")}>
          <DataTable
            caption={reportTitle(command)}
            columns={columns}
            rows={company ? data?.assets : []}
            rowKey={(row) => row.asset_id}
            rowHref={(row) =>
              canSchedule
                ? `${paths.report(code, SCHEDULE)}?${new URLSearchParams({ asset: row.asset_id })}`
                : canEntity(meta, ASSET, "read")
                  ? paths.entityRecord(code, ASSET, row.asset_id)
                  : undefined
            }
            loading={report.loading}
            error={report.error}
            onRetry={report.reload}
            empty={<p>{company ? t("assets.none") : t("report.needs_company")}</p>}
            footer={
              data && data.assets.length > 0 ? (
                <TableRow>
                  <TableCell colSpan={6}>{t("report.total")}</TableCell>
                  <TableCell className="text-end">
                    <Amount value={data.total_cost} decimals={decimals} />
                  </TableCell>
                  <TableCell className="text-end">
                    <Amount value={data.total_accumulated} decimals={decimals} />
                  </TableCell>
                  <TableCell className="text-end font-semibold">
                    <Amount value={data.total_book_value} decimals={decimals} />
                  </TableCell>
                  <TableCell className="text-end">
                    <Amount value={data.total_depreciation_this_year} decimals={decimals} />
                  </TableCell>
                </TableRow>
              ) : null
            }
          />
        </Section>
      </div>
    </>
  );
}

export function AssetSchedulePage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const asset = url.get("asset");
  const command = findCommand(meta, SCHEDULE);
  const allowed = command !== undefined && canRun(meta, SCHEDULE);
  const statusField = useAssetField("status");
  const methodField = useAssetField("method");
  const decimals = currencyDecimals[company?.base_currency ?? ""] ?? 2;

  const report = useAsync(allowed && asset ? asset : null, (signal) => api.run(SCHEDULE, { id: asset }, { signal }));
  const data = report.data;

  const columns = useMemo<Column<ScheduleRow>[]>(() => {
    if (!command) return [];
    const title = (field: string) => outputTitle(command, field, "ScheduleRow");
    return [
      {
        id: "month",
        header: title("month"),
        cell: (row) => <span className="whitespace-nowrap">{monthLabel(row.month)}</span>,
      },
      {
        id: "posted",
        header: title("posted"),
        className: "w-full",
        cell: (row) =>
          row.posted ? (
            row.document_id ? (
              <span className="whitespace-nowrap">
                {t("assets.posted_by", { number: row.number ?? t("document.unnumbered") })}
              </span>
            ) : (
              t("assets.posted")
            )
          ) : (
            <span className="text-muted-foreground">{t("assets.planned")}</span>
          ),
      },
      {
        id: "amount",
        header: title("amount"),
        align: "end",
        cell: (row) => <Amount value={row.amount} decimals={decimals} />,
      },
      {
        id: "accumulated",
        header: title("accumulated"),
        align: "end",
        cell: (row) => <Amount value={row.accumulated} decimals={decimals} />,
      },
      {
        id: "book_value",
        header: title("book_value"),
        align: "end",
        cell: (row) => <Amount value={row.book_value} decimals={decimals} />,
      },
    ];
  }, [command, decimals]);

  if (!allowed || !command) return <NotFoundPage />;
  const title = (field: string) => outputTitle(command, field);
  const posted = data?.months.filter((row) => row.posted).length ?? 0;
  const facts: [string, ReactNode][] = data
    ? [
        [title("status"), labelled(statusField, data.status)],
        [
          title("method"),
          data.declining_rate
            ? t("assets.method_rate", { method: labelled(methodField, data.method), rate: data.declining_rate })
            : labelled(methodField, data.method),
        ],
        [
          title("useful_life_months"),
          data.useful_life_months === null ? (
            <Empty key="life" />
          ) : (
            t("assets.months", { count: data.useful_life_months })
          ),
        ],
        [title("cost"), <Amount key="cost" value={data.cost} decimals={decimals} />],
        [title("salvage_value"), <Amount key="salvage" value={data.salvage_value} decimals={decimals} />],
        [
          title("in_service_date"),
          data.in_service_date ? formatCalendarDate(data.in_service_date) : <Empty key="in-service" />,
        ],
        ...(data.opening_through
          ? ([
              [
                title("opening_accumulated"),
                t("assets.taken_over", {
                  amount: formatDecimal(data.opening_accumulated, { minFraction: decimals }),
                  date: formatCalendarDate(data.opening_through),
                }),
              ],
            ] as [string, ReactNode][])
          : []),
      ]
    : [];

  return (
    <>
      <PageHeader
        title={
          data
            ? t("assets.schedule_of", {
                title: reportTitle(command),
                asset: [data.code, data.name].join(t("value.separator")),
              })
            : reportTitle(command)
        }
        actions={
          data && canEntity(meta, ASSET, "read") ? (
            <Link className="text-sm underline underline-offset-2" to={paths.entityRecord(code, ASSET, data.asset_id)}>
              {t("assets.open_asset")}
            </Link>
          ) : null
        }
      />
      <p className="mb-3 max-w-form text-sm text-muted-foreground">{command.description}</p>
      <div className="mb-4 space-y-1">
        <Label htmlFor="schedule-asset">{findEntity(meta, ASSET)?.label ?? title("asset_id")}</Label>
        <Lookup
          id="schedule-asset"
          className="w-80 max-w-full"
          entity={ASSET}
          keyField="id"
          value={asset || null}
          companyId={company?.id}
          required
          allowCreate={false}
          onChange={(next) => url.set({ asset: next })}
        />
      </div>
      {!asset ? (
        <p className="text-sm text-muted-foreground">{t("assets.choose_asset")}</p>
      ) : report.error ? (
        <p role="alert" className="text-sm text-destructive">
          {report.error.message}
        </p>
      ) : data ? (
        <div className="space-y-5">
          <dl className="grid max-w-3xl grid-cols-[auto_1fr] gap-x-6 gap-y-1 rounded-md border bg-card p-4 text-sm sm:grid-cols-[auto_1fr_auto_1fr]">
            {facts.map(([label, value]) => (
              <div key={label} className="contents">
                <dt className="text-muted-foreground">{label}</dt>
                <dd>{value}</dd>
              </div>
            ))}
          </dl>
          <Section
            title={title("months")}
            aside={
              <p className="text-xs text-muted-foreground">
                {t("assets.posted_planned", { posted, planned: data.months.length - posted })}
              </p>
            }
          >
            <DataTable
              caption={title("months")}
              columns={columns}
              rows={data.months}
              rowKey={(row) => row.month}
              rowHref={(row) =>
                row.document_id && row.doc_type ? paths.doc(code, row.doc_type, row.document_id) : undefined
              }
              empty={<p>{t("assets.no_months")}</p>}
            />
          </Section>
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">{t("value.loading")}</p>
      )}
    </>
  );
}
