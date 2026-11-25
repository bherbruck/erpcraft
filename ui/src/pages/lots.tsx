// Lots and serial numbers: what is on hand per lot (`stock.lots`), and where a lot went or came
// from (`stock.trace`): the lots reached through production, every movement with its document
// and party, and the customers or vendors involved. Filters are in the URL.

import type { ReactNode } from "react";
import { Link } from "react-router";
import { api } from "@/api/client";
import type { LotRow, TracedLot, TraceMovement, TraceParty } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Amount, Empty } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { PageHeader, Section } from "@/components/page";
import { SearchBox } from "@/components/search-box";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { canDoc, canEntity, canRun, findCommand, findDocType, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

const LOTS = "stock.lots";
const TRACE = "stock.trace";
/** The indent of a lot reached through production, by how many steps away it is. */
const DEPTH = ["", "ps-5", "ps-10", "ps-15", "ps-20"];

function Filter({ id, label, children }: { id: string; label: string; children: ReactNode }) {
  return (
    <div className="space-y-1">
      <Label htmlFor={id}>{label}</Label>
      {children}
    </div>
  );
}

/** Lots on hand, per lot and warehouse, first expiring first; each row traces its lot. */
export function LotsPage() {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const item = url.get("item");
  const warehouse = url.get("warehouse");
  const lot = url.get("lot");
  const expiring = url.get("expiring_by");
  const command = findCommand(meta, LOTS);
  const allowed = canRun(meta, LOTS);
  const report = useAsync(
    allowed && company ? `${company.id}|${item}|${warehouse}|${lot}|${expiring}` : null,
    (signal) =>
      api.run(
        LOTS,
        {
          company_id: company?.id ?? "",
          item_id: item || null,
          warehouse_id: warehouse || null,
          lot: lot || null,
          expiring_by: expiring || null,
        },
        { signal },
      ),
  );
  if (!allowed || !command) return <NotFoundPage />;
  const traceable = canRun(meta, TRACE);

  const columns: Column<LotRow>[] = [
    {
      id: "lot",
      header: t("lots.lot"),
      cell: (row) =>
        traceable ? (
          <Link
            className="font-medium underline-offset-2 hover:underline"
            to={`${paths.report(code, TRACE)}?${new URLSearchParams({ item: row.item_id, lot: row.lot })}`}
          >
            {row.lot}
          </Link>
        ) : (
          <span className="font-medium">{row.lot}</span>
        ),
    },
    { id: "item", header: t("report.item_code"), cell: (row) => row.item_code },
    { id: "warehouse", header: t("report.warehouse"), cell: (row) => row.warehouse_code },
    {
      id: "expiry",
      header: t("lots.expiry"),
      cell: (row) => (row.expiry_date ? formatCalendarDate(row.expiry_date) : <Empty />),
    },
    {
      id: "made",
      header: t("lots.made"),
      className: "w-full",
      cell: (row) => (row.manufactured_date ? formatCalendarDate(row.manufactured_date) : <Empty />),
    },
    { id: "qty", header: t("report.quantity"), align: "end", cell: (row) => <Amount value={row.qty} decimals={0} /> },
  ];

  return (
    <>
      <PageHeader title={reportTitle(command)} />
      <div className="mb-3 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <Filter id="lots-item" label={t("lots.item")}>
          <Lookup
            id="lots-item"
            entity="item"
            keyField="id"
            value={item || null}
            placeholder={t("lots.any_item")}
            allowCreate={false}
            recordLink={false}
            onChange={(next) => url.set({ item: next })}
          />
        </Filter>
        <Filter id="lots-warehouse" label={t("report.warehouse")}>
          <Lookup
            id="lots-warehouse"
            entity="warehouse"
            keyField="id"
            value={warehouse || null}
            companyId={company?.id}
            placeholder={t("lots.any_warehouse")}
            allowCreate={false}
            recordLink={false}
            onChange={(next) => url.set({ warehouse: next })}
          />
        </Filter>
        <Filter id="lots-lot" label={t("lots.lot")}>
          <SearchBox
            value={lot}
            label={t("lots.search")}
            onChange={(next) => url.set({ lot: next }, { replace: true })}
          />
        </Filter>
        <Filter id="lots-expiring" label={t("lots.expiring_by")}>
          <DateInput id="lots-expiring" value={expiring} onChange={(next) => url.set({ expiring_by: next })} />
        </Filter>
      </div>
      <DataTable
        caption={reportTitle(command)}
        columns={columns}
        rows={company ? report.data?.rows : []}
        rowKey={(row) => `${row.lot_id}:${row.warehouse_id}`}
        loading={report.loading}
        error={report.error}
        onRetry={report.reload}
        empty={<p>{company ? t("lots.none_on_hand") : t("report.needs_company")}</p>}
      />
    </>
  );
}

/** Where a lot went (forward) or came from (backward). */
export function LotTracePage() {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const item = url.get("item");
  const lot = url.get("lot");
  const direction = url.get("direction") === "backward" ? "backward" : "forward";
  const command = findCommand(meta, TRACE);
  const allowed = canRun(meta, TRACE);
  const ready = allowed && company !== null && item !== "" && lot !== "";
  const report = useAsync(ready ? `${company?.id}|${item}|${lot}|${direction}` : null, (signal) =>
    api.run(TRACE, { company_id: company?.id ?? "", item_id: item, lot, direction }, { signal }),
  );
  if (!allowed || !command) return <NotFoundPage />;

  const docLink = (movement: TraceMovement) => {
    const docType = findDocType(meta, movement.doc_type);
    const label = `${docType?.label ?? movement.doc_type} ${movement.number ?? ""}`.trim();
    return canDoc(meta, movement.doc_type, "read") ? (
      <Link
        className="whitespace-nowrap underline-offset-2 hover:underline"
        to={paths.doc(code, movement.doc_type, movement.document_id)}
      >
        {label}
      </Link>
    ) : (
      <span className="whitespace-nowrap">{label}</span>
    );
  };
  const partyLink = (party: { party_type: string | null; party_id: string | null; name: string | null }) =>
    party.party_type && party.party_id && canEntity(meta, party.party_type, "read") ? (
      <Link
        className="underline-offset-2 hover:underline"
        to={paths.entityRecord(code, party.party_type, party.party_id)}
      >
        {party.name ?? party.party_id}
      </Link>
    ) : (
      (party.name ?? <Empty />)
    );

  const lotColumns: Column<TracedLot>[] = [
    {
      id: "lot",
      header: t("lots.lot"),
      cell: (row) => (
        // Lots reached through production are indented by how many steps away they are.
        <span className={`font-medium ${DEPTH[Math.min(row.depth, DEPTH.length - 1)] ?? ""}`}>{row.lot}</span>
      ),
    },
    { id: "item", header: t("report.item_code"), cell: (row) => row.item_code },
    {
      id: "expiry",
      header: t("lots.expiry"),
      className: "w-full",
      cell: (row) => (row.expiry_date ? formatCalendarDate(row.expiry_date) : <Empty />),
    },
    { id: "depth", header: t("lots.steps"), align: "end", cell: (row) => row.depth },
  ];
  const movementColumns: Column<TraceMovement>[] = [
    {
      id: "date",
      header: t("document.date"),
      cell: (row) => <span className="whitespace-nowrap">{formatCalendarDate(row.posting_date)}</span>,
    },
    { id: "doc", header: t("lots.document"), cell: docLink },
    { id: "lot", header: t("lots.lot"), cell: (row) => `${row.item_code} ${row.lot}` },
    { id: "warehouse", header: t("report.warehouse"), cell: (row) => row.warehouse_code },
    {
      id: "party",
      header: t("lots.party"),
      className: "w-full",
      cell: (row) => partyLink({ party_type: row.party_type, party_id: row.party_id, name: row.party_name }),
    },
    { id: "qty", header: t("report.quantity"), align: "end", cell: (row) => <Amount value={row.qty} decimals={0} /> },
  ];
  const partyColumns: Column<TraceParty>[] = [
    { id: "name", header: t("lots.party"), className: "w-full", cell: (row) => partyLink(row) },
  ];

  return (
    <>
      <PageHeader title={reportTitle(command)} />
      <div className="mb-4 grid gap-3 sm:grid-cols-3">
        <Filter id="trace-item" label={t("lots.item")}>
          <Lookup
            id="trace-item"
            entity="item"
            keyField="id"
            value={item || null}
            allowCreate={false}
            recordLink={false}
            onChange={(next) => url.set({ item: next })}
          />
        </Filter>
        <Filter id="trace-lot" label={t("lots.lot")}>
          <SearchBox value={lot} label={t("lots.lot")} onChange={(next) => url.set({ lot: next }, { replace: true })} />
        </Filter>
        <Filter id="trace-direction" label={t("lots.direction")}>
          <NativeSelect
            id="trace-direction"
            className="w-full"
            value={direction}
            onChange={(event) => url.set({ direction: event.target.value === "backward" ? "backward" : null })}
          >
            <option value="forward">{t("lots.forward")}</option>
            <option value="backward">{t("lots.backward")}</option>
          </NativeSelect>
        </Filter>
      </div>
      {!ready ? (
        <p className="rounded-lg border bg-card p-4 text-sm text-muted-foreground">{t("lots.trace_needs")}</p>
      ) : (
        <div className="space-y-6">
          <Section title={t("lots.traced_lots")}>
            <DataTable
              caption={t("lots.traced_lots")}
              columns={lotColumns}
              rows={report.data?.lots}
              rowKey={(row) => row.lot_id}
              loading={report.loading}
              error={report.error}
              onRetry={report.reload}
              empty={<p>{t("lots.not_found", { lot })}</p>}
            />
          </Section>
          <Section title={t("lots.movements")}>
            <DataTable
              caption={t("lots.movements")}
              columns={movementColumns}
              rows={report.data?.movements}
              rowKey={(row) => `${row.document_id}:${row.lot_id}:${row.warehouse_code}:${row.qty}`}
              loading={report.loading}
              empty={<p>{t("lots.no_movements")}</p>}
            />
          </Section>
          <Section title={direction === "forward" ? t("lots.customers") : t("lots.vendors")}>
            <DataTable
              caption={direction === "forward" ? t("lots.customers") : t("lots.vendors")}
              columns={partyColumns}
              rows={report.data?.parties}
              rowKey={(row) => `${row.party_type}:${row.party_id}`}
              loading={report.loading}
              empty={<p>{direction === "forward" ? t("lots.no_customers") : t("lots.no_vendors")}</p>}
            />
          </Section>
        </div>
      )}
    </>
  );
}
