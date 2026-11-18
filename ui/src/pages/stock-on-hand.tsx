import { api } from "@/api/client";
import type { EntityName } from "@/api/generated";
import type { StockOnHand } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { Amount, Empty } from "@/components/field-value";
import { PageHeader } from "@/components/page";
import { TableCell, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { canEntity, canRun } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

type Row = StockOnHand["rows"][number];

const ITEM: EntityName = "item";

export function StockOnHandPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const allowed = canRun(meta, "stock.on_hand");

  const report = useAsync(allowed && company ? company.id : null, (signal) =>
    api.run("stock.on_hand", { company_id: company?.id ?? "" }, { signal }),
  );
  if (!allowed) return <NotFoundPage />;

  const currency = report.data?.currency ?? company?.base_currency ?? "";
  const decimals = currencyDecimals[currency] ?? 2;
  const columns: Column<Row>[] = [
    {
      id: "item_code",
      header: t("report.item_code"),
      cell: (row) => <span className="whitespace-nowrap">{row.item_code}</span>,
    },
    { id: "item_name", header: t("report.item"), cell: (row) => row.item_name, className: "w-full" },
    {
      id: "warehouse",
      header: t("report.warehouse"),
      cell: (row) => <span className="whitespace-nowrap">{row.warehouse_code}</span>,
    },
    { id: "qty", header: t("report.quantity"), align: "end", cell: (row) => <Amount value={row.qty} decimals={0} /> },
    { id: "uom", header: t("report.unit"), cell: (row) => row.uom ?? <Empty /> },
    {
      id: "average_cost",
      header: t("report.average_cost"),
      align: "end",
      cell: (row) => <Amount value={row.average_cost} decimals={decimals} />,
    },
    {
      id: "value",
      header: t("report.value"),
      align: "end",
      cell: (row) => <Amount value={row.value} decimals={decimals} />,
    },
  ];

  return (
    <>
      <PageHeader title={t("report.stock_on_hand")} />
      <p className="mb-3 text-end text-sm text-muted-foreground">
        {company ? t("report.amounts_in", { company: company.name, currency }) : null}
      </p>
      <DataTable
        caption={t("report.stock_on_hand")}
        columns={columns}
        rows={company ? report.data?.rows : []}
        rowKey={(row) => `${row.item_id}:${row.warehouse_id}`}
        rowHref={canEntity(meta, ITEM, "read") ? (row) => paths.entityRecord(code, ITEM, row.item_id) : undefined}
        loading={report.loading}
        error={report.error}
        onRetry={report.reload}
        empty={<p>{company ? t("report.stock_empty", { company: company.name }) : t("report.needs_company")}</p>}
        footer={
          report.data ? (
            <TableRow>
              <TableCell colSpan={6}>{t("report.total")}</TableCell>
              <TableCell className="text-end">
                <Amount value={report.data.total_value} decimals={decimals} />
              </TableCell>
            </TableRow>
          ) : null
        }
      />
    </>
  );
}
