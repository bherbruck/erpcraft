// The demo: the grid with the reference renderer, in its three modes.
// #client (100k rows in memory), #server (a simulated list API), #lines.

import { StrictMode, useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  applyChanges,
  type CellChange,
  compareValues,
  type EditReport,
  Grid,
  type GridColumn,
  type GridQueryState,
  isEmptyValue,
  localStorageLayout,
  type Option,
  rowPasses,
  matchesSearch,
  sumDecimals,
  toDecimal,
  toErpcraftQuery,
} from "../src";
import { referenceClassNames, referenceSlots } from "../src/reference";
import "../src/reference/reference.css";
import { CUSTOMERS, ITEMS, makeOrders, type Order, STATES, searchCustomers } from "./data";

const slots = referenceSlots<Order>();
const lineSlots = referenceSlots<Line>();
const storage = localStorageLayout("erpcraft.grid.demo.");

function customerLabel(value: unknown): string {
  return CUSTOMERS.find((c) => c.value === value)?.label ?? String(value ?? "");
}

const orderColumns: GridColumn<Order>[] = [
  { id: "number", header: "Number", type: "text", width: 120, pin: "left", hideable: false },
  {
    id: "customer",
    header: "Customer",
    type: "reference",
    width: 180,
    format: (v) => customerLabel(v),
    options: CUSTOMERS,
    loadOptions: searchCustomers,
    editable: true,
  },
  { id: "status", header: "Status", type: "state", options: STATES, editable: true, width: 120 },
  { id: "doc_date", header: "Date", type: "date", editable: true },
  { id: "qty", header: "Qty", type: "number", editable: true, total: true, validate: (v) => (v !== null && String(v).startsWith("-") ? "must not be negative" : null) },
  {
    id: "amount",
    header: "Amount",
    type: "money",
    minFraction: 2,
    currency: (row) => row.currency,
    width: 160,
    editable: true,
    total: true,
  },
  { id: "memo", header: "Memo", type: "text", editable: true, width: 220 },
  { id: "urgent", header: "Urgent", type: "boolean", editable: true },
];

function Report({ report }: { report: EditReport | null }) {
  if (!report) return <p data-testid="report">No edits yet.</p>;
  return (
    <div data-testid="report">
      <strong>{report.source}</strong>: {report.applied} applied, {report.rejected.length} rejected
      {report.rejected.length ? (
        <ul data-testid="rejected">
          {report.rejected.slice(0, 8).map((r) => (
            <li key={`${r.rowIndex}-${r.columnId}`}>
              row {r.rowIndex + 1}, {r.columnId}: {r.reason}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function ClientDemo() {
  const count = Number(new URLSearchParams(location.search).get("rows") ?? 100_000);
  const [rows, setRows] = useState(() => makeOrders(count));
  const [report, setReport] = useState<EditReport | null>(null);
  const [query, setQuery] = useState<GridQueryState | null>(null);
  const onEdit = useCallback((changes: CellChange<Order>[]) => {
    setRows((current) => applyChanges(current, changes, orderColumns));
  }, []);
  return (
    <>
      <Grid<Order>
        id="demo-orders"
        aria-label="Sales orders"
        columns={orderColumns}
        rows={rows}
        slots={slots}
        classNames={referenceClassNames}
        onEdit={onEdit}
        onReport={setReport}
        onQueryChange={setQuery}
        layoutStorage={storage}
        height={520}
      />
      <Report report={report} />
      <details>
        <summary>Query state</summary>
        <pre data-testid="query">{JSON.stringify(query, null, 2)}</pre>
      </details>
    </>
  );
}

/** A list API over rows in memory, answering a GridQueryState after a delay. */
function serve(all: Order[], query: GridQueryState): { items: Order[]; total: number; totals: Record<string, string> } {
  const filters = query.filters.map((f) => ({ columnId: f.columnId, condition: f.condition }));
  let rows = all.filter((row) => rowPasses(orderColumns, filters, row) && matchesSearch(orderColumns, row, query.search));
  const totals = {
    qty: sumDecimals(rows.map((r) => r.qty)),
    amount: sumDecimals(rows.map((r) => r.amount)),
  };
  if (query.sort.length) {
    rows = [...rows].sort((a, b) => {
      for (const key of query.sort) {
        const column = orderColumns.find((c) => c.id === key.columnId);
        if (!column) continue;
        const va = (a as unknown as Record<string, unknown>)[column.id];
        const vb = (b as unknown as Record<string, unknown>)[column.id];
        if (isEmptyValue(va) !== isEmptyValue(vb)) return isEmptyValue(va) ? 1 : -1;
        const c = compareValues(column, va, vb);
        if (c !== 0) return key.desc ? -c : c;
      }
      return 0;
    });
  }
  const start = query.page.index * query.page.size;
  return { items: rows.slice(start, start + query.page.size), total: rows.length, totals };
}

function ServerDemo() {
  const all = useMemo(() => makeOrders(5_000, 7), []);
  const [query, setQuery] = useState<GridQueryState | null>(null);
  const [page, setPage] = useState<{ items: Order[]; total: number; totals: Record<string, string> }>({
    items: [],
    total: 0,
    totals: {},
  });
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    if (!query) return undefined;
    setLoading(true);
    const timer = setTimeout(() => {
      setPage(serve(all, query));
      setLoading(false);
    }, 150);
    return () => clearTimeout(timer);
  }, [all, query]);
  const erpcraft = query
    ? toErpcraftQuery(query, { kind: "document", docType: "sales_order", companyId: "00000000-0000-0000-0000-000000000001" })
    : null;
  const serverColumns = useMemo<GridColumn<Order>[]>(
    () =>
      orderColumns.map((c) => ({
        ...c,
        editable: false,
        field: c.id === "customer" ? "summary.customer_id" : c.id === "amount" ? "summary.total" : c.id,
      })),
    [],
  );
  return (
    <>
      <Grid<Order>
        id="demo-server"
        aria-label="Sales orders (server)"
        mode="server"
        columns={serverColumns}
        rows={page.items}
        total={page.total}
        totals={page.totals}
        loading={loading}
        pageSize={50}
        slots={slots}
        classNames={referenceClassNames}
        onQueryChange={setQuery}
        height={480}
      />
      <h3>Grid state</h3>
      <pre data-testid="query">{JSON.stringify(query, null, 2)}</pre>
      <h3>toErpcraftQuery</h3>
      <pre data-testid="erpcraft">{JSON.stringify(erpcraft, null, 2)}</pre>
    </>
  );
}

interface Line {
  id: string;
  item: string | null;
  description: string;
  qty: string | null;
  price: string | null;
}

let lineSeq = 3;
const newLine = (): Line => ({ id: `l-${(lineSeq += 1)}`, item: null, description: "", qty: null, price: null });

function lineTotal(line: Line): string | null {
  const q = toDecimal(line.qty);
  const p = toDecimal(line.price);
  if (q === null || p === null) return null;
  // Exact product of two decimals, through integers.
  const scale = (s: string) => (s.split(".")[1] ?? "").length;
  const digits = (s: string) => BigInt(s.replace(".", "").replace("-", "")) * (s.startsWith("-") ? -1n : 1n);
  const places = scale(q) + scale(p);
  const product = (digits(q) * digits(p)).toString();
  const negative = product.startsWith("-");
  const body = (negative ? product.slice(1) : product).padStart(places + 1, "0");
  const text = places ? `${body.slice(0, -places)}.${body.slice(-places)}` : body;
  return toDecimal(`${negative ? "-" : ""}${text}`);
}

const lineColumns: GridColumn<Line>[] = [
  {
    id: "item",
    header: "Item",
    type: "reference",
    options: ITEMS,
    editable: true,
    width: 200,
    format: (v) => ITEMS.find((i) => i.value === v)?.label ?? "",
  },
  { id: "description", header: "Description", type: "text", editable: true, width: 260 },
  { id: "qty", header: "Qty", type: "number", editable: true, total: true },
  { id: "price", header: "Price", type: "money", minFraction: 2, currency: "EUR", editable: true },
  { id: "total", header: "Line total", type: "money", minFraction: 2, currency: "EUR", value: lineTotal, total: true },
];

function LinesDemo() {
  const [lines, setLines] = useState<Line[]>([
    { id: "l-1", item: "i-1", description: "Bolts for frame", qty: "100", price: "0.12" },
    { id: "l-2", item: "i-3", description: "Nuts", qty: "100", price: "0.05" },
    { id: "l-3", item: "i-8", description: "Pallet", qty: "1", price: "12.00" },
  ]);
  const [report, setReport] = useState<EditReport | null>(null);
  return (
    <>
      <Grid<Line>
        id="demo-lines"
        aria-label="Order lines"
        columns={lineColumns}
        rows={lines}
        slots={lineSlots}
        classNames={referenceClassNames}
        toolbar={false}
        height={300}
        onEdit={(changes) => setLines((current) => applyChanges(current, changes, lineColumns))}
        onReport={setReport}
        lineEditing={{
          onAppend: () => setLines((current) => [...current, newLine()]),
          onInsert: (at) => setLines((current) => [...current.slice(0, at), newLine(), ...current.slice(at)]),
          onDelete: (ids) => setLines((current) => current.filter((l) => !ids.includes(l.id))),
        }}
      />
      <Report report={report} />
      <p data-testid="line-count">{lines.length} lines</p>
    </>
  );
}

function App() {
  const [tab, setTab] = useState(() => location.hash.slice(1) || "client");
  useEffect(() => {
    const onHash = () => setTab(location.hash.slice(1) || "client");
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  return (
    <main style={{ padding: 16, fontFamily: "system-ui, sans-serif" }}>
      <h1 style={{ fontSize: 18 }}>@erpcraft/grid: reference renderer</h1>
      <nav style={{ display: "flex", gap: 12, marginBottom: 12 }}>
        <a href="#client">Client (100k rows)</a>
        <a href="#server">Server</a>
        <a href="#lines">Line editing</a>
      </nav>
      <button type="button" data-testid="before-grid">
        Before the grid
      </button>
      {tab === "server" ? <ServerDemo /> : tab === "lines" ? <LinesDemo /> : <ClientDemo key={tab} />}
      <button type="button" data-testid="after-grid">
        After the grid
      </button>
    </main>
  );
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

export type { Option };
