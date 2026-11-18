import { Badge } from "@astryxdesign/core/Badge";
import { Link } from "@astryxdesign/core/Link";
import { pixel, proportional, type TableColumn } from "@astryxdesign/core/Table";
import { useParams } from "react-router";
import { api } from "@/api/client";
import { ListView } from "@/components/list-view";
import { t } from "@/i18n/t";
import { entity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useListState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { useSession } from "@/session";

interface Row extends Record<string, unknown> {
  id: string;
  code: string;
  name: string;
  active: boolean;
}

export function CustomerListPage() {
  const { company: code = "" } = useParams();
  const { meta } = useSession();
  const customer = entity(meta, "customer");
  const label = (name: string) => customer?.fields.find((f) => f.name === name)?.label ?? name;
  const state = useListState("code");

  const result = useAsync(
    async (signal) => {
      const page = await api.run(
        "entity.list",
        {
          entity: "customer",
          q: state.q || null,
          sort: [state.sort],
          limit: state.size,
          offset: (state.page - 1) * state.size,
        },
        { signal },
      );
      return {
        items: page.items.map((r) => ({
          id: r.id,
          code: String(r.code ?? ""),
          name: String(r.name ?? ""),
          active: r.active !== false,
        })),
        total: page.total,
      };
    },
    [state.q, state.sort, state.page, state.size],
  );

  const columns: TableColumn<Row>[] = [
    {
      key: "code",
      header: label("code"),
      width: pixel(160),
      sortable: true,
      renderCell: (row) => <Link href={paths.customer(code, row.id)}>{row.code}</Link>,
    },
    { key: "name", header: label("name"), width: proportional(3, { minWidth: 200 }), sortable: true },
    {
      key: "active",
      header: label("active"),
      width: pixel(110),
      sortable: true,
      renderCell: (row) => (
        <Badge variant={row.active ? "success" : "neutral"} label={row.active ? t("common.active") : t("common.inactive")} />
      ),
    },
  ];

  return (
    <ListView<Row>
      title={customer?.plural ?? "Customers"}
      plural={customer?.plural ?? "Customers"}
      searchLabel={t("list.searchCustomers")}
      state={state}
      result={result}
      columns={columns}
      idKey="id"
      newHref={paths.newCustomer(code)}
      newLabel={t("customer.new")}
    />
  );
}
