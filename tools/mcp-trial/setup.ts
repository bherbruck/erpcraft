#!/usr/bin/env bun
// Puts the month scenario into the state the trial's tasks start from,
// over the HTTP API as an administrator: an approved sales order for more
// widgets than are on hand (SO-2026-00004), and March open again.
//
//   bun run setup.ts [--base-url URL]

const i = process.argv.indexOf("--base-url");
const BASE = i >= 0 ? process.argv[i + 1] : "http://127.0.0.1:8611";
const login = await fetch(`${BASE}/api/auth/login`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ email: "admin@example.com", password: "correct-horse-battery" }),
});
const { token } = (await login.json()) as { token: string };
async function run(name: string, input: unknown) {
  const r = await fetch(`${BASE}/api/commands/${name}`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify(input),
  });
  const body = await r.json();
  if (!r.ok) throw new Error(`${name}: ${r.status} ${JSON.stringify(body)}`);
  return body as any;
}
const first = async (entity: string, filters: Record<string, string> = {}) =>
  (await run("entity.list", { entity, filters, limit: 1 })).items[0];

const company = await first("company");
const customer = await first("customer", { code: "NORTHFIELD" });
const item = await first("item", { code: "WIDGET" });
const warehouse = await first("warehouse", { company_id: company.id });
const existing = (await run("document.list", { doc_type: "sales_order", q: "SO-2026-00004" })).items[0];
const order = existing ?? await run("document.create", {
  doc_type: "sales_order", company_id: company.id, doc_date: "2026-04-01",
  body: { customer_id: customer.id, lines: [{ item_id: item.id, warehouse_id: warehouse.id, qty: "500", unit_price: "12.50" }] },
});
const approved = existing ?? await run("sales_order.approve", { id: order.id });
console.log(`approved ${approved.number}`);
const march = await first("fiscal_period", { company_id: company.id, name: "2026-03" });
if (march.status === "closed") {
  await run("fiscal_period.reopen", { id: march.id });
  console.log("March reopened");
}
