import { Link } from "@astryxdesign/core/Link";
import { Selector } from "@astryxdesign/core/Selector";
import { pixel, proportional, type TableColumn } from "@astryxdesign/core/Table";
import { Text } from "@astryxdesign/core/Text";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { DocumentSummary } from "@/api/types";
import { Amount } from "@/components/amount";
import { ListView } from "@/components/list-view";
import { StatusBadge } from "@/components/status-badge";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { docType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useListState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { useSession } from "@/session";

interface Row extends Record<string, unknown> {
  id: string;
  number: string | null;
  doc_date: string;
  status: string;
  customer: string;
  memo: string;
  total: string | null;
  currency: string | null;
}

function summaryText(summary: unknown, key: string): string | null {
  if (typeof summary !== "object" || summary === null) return null;
  const value = (summary as Record<string, unknown>)[key];
  return typeof value === "string" ? value : null;
}

function toRow(doc: DocumentSummary): Row {
  return {
    id: doc.id,
    number: doc.number,
    doc_date: doc.doc_date,
    status: doc.status,
    customer: summaryText(doc.summary, "customer_name") ?? "",
    memo: doc.memo ?? "",
    total: summaryText(doc.summary, "total_net"),
    currency: summaryText(doc.summary, "currency"),
  };
}

export function SalesOrderListPage() {
  const { company: code = "" } = useParams();
  const { meta, companies } = useSession();
  const company = companies.find((c) => c.code === code);
  const type = docType(meta, "sales_order");
  const state = useListState("-doc_date");

  const result = useAsync(
    async (signal) => {
      const page = await api.run(
        "document.list",
        {
          doc_type: "sales_order",
          company_id: company?.id ?? null,
          q: state.q || null,
          status: state.status || null,
          sort: [state.sort, "-created_at"],
          limit: state.size,
          offset: (state.page - 1) * state.size,
        },
        { signal },
      );
      return { items: page.items.map(toRow), total: page.total };
    },
    [company?.id, state.q, state.status, state.sort, state.page, state.size],
  );

  const columns: TableColumn<Row>[] = [
    {
      key: "number",
      header: "Number",
      width: pixel(130),
      sortable: true,
      renderCell: (row) => (
        <Link href={paths.salesOrder(code, row.id)}>{row.number ?? t("list.draft")}</Link>
      ),
    },
    {
      key: "doc_date",
      header: "Date",
      width: pixel(120),
      sortable: true,
      renderCell: (row) => <Text hasTabularNumbers>{formatCalendarDate(row.doc_date)}</Text>,
    },
    {
      key: "customer",
      header: "Customer",
      width: proportional(2, { minWidth: 160 }),
      sortable: { sortKey: "summary.customer_name" },
    },
    {
      key: "status",
      header: "Status",
      width: pixel(110),
      sortable: true,
      renderCell: (row) => <StatusBadge type={type} status={row.status} />,
    },
    { key: "memo", header: "Memo", width: proportional(3, { minWidth: 160 }) },
    {
      key: "total",
      header: "Net total",
      width: pixel(150),
      align: "end",
      sortable: { sortKey: "summary.total_net" },
      renderCell: (row) => <Amount value={row.total} currency={row.currency} />,
    },
  ];

  const statusFilter = (
    <Selector
      label={t("list.status")}
      isLabelHidden
      size="sm"
      variant="secondary"
      value={state.status}
      hasClear
      placeholder={t("list.allStatuses")}
      onChange={(value: string | null) => state.setStatus(value ?? "")}
      options={(type?.states ?? []).map((s) => ({ value: s.name, label: s.label }))}
    />
  );

  return (
    <ListView<Row>
      title={type?.plural ?? "Sales orders"}
      plural={type?.plural ?? "Sales orders"}
      searchLabel={t("list.searchOrders")}
      state={state}
      result={result}
      columns={columns}
      idKey="id"
      newHref={paths.newSalesOrder(code)}
      newLabel={t("doc.new")}
      filters={statusFilter}
    />
  );
}
