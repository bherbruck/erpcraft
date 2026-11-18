// A vendor invoice against a purchase order, its lines picked from the order's lines in the grid
// (a line_link field, with the Astryx Typeahead as the cell editor). Saved, checked on the server,
// then deleted again. bun scripts/invoice-smoke.ts

import { launchBrowser } from "./browser";
import { adminCredentials, signIn } from "./lib";

const BASE = process.env.SPIKE_URL ?? "http://127.0.0.1:5174";
const SHOTS = process.env.SHOTS ?? "";
const { email, password } = adminCredentials();
const session = await signIn();
const orders = (await session.run("document.list", { doc_type: "purchase_order", status: "open", limit: 1 })) as {
  items: { id: string; number: string; company_id: string }[];
};
const order = orders.items[0];
if (!order) throw new Error("no open purchase order to invoice");
const company = (await session.run("entity.get", { entity: "company", id: order.company_id })) as { code: string };

const b = await launchBrowser(9352);
const check = (ok: boolean, what: string) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) process.exitCode = 1;
};
const focusLabelled = (prefix: string) =>
  b.evaluate(`(() => { const l = [...document.querySelectorAll("label")].find((x) => x.textContent.startsWith(${JSON.stringify(prefix)})); const e = l && document.getElementById(l.htmlFor); e?.focus(); e?.click?.(); return Boolean(e); })()`);
const waitFor = async (expression: string) => {
  for (let i = 0; i < 40 && (await b.evaluate(expression)) !== true; i += 1) await Bun.sleep(100);
};
const col = async (header: string) =>
  Number(await b.evaluate(`[...document.querySelectorAll("[data-grid-header] [role=columnheader]")].findIndex((h) => h.textContent.trim().startsWith(${JSON.stringify(header)}))`));
const cellText = async (row: number, c: number) => String(await b.evaluate(`document.querySelector('[data-grid-body] [data-row="${row}"][data-col="${c}"]')?.textContent ?? ""`));
let created = "";

try {
  await b.viewport(1280, 900);
  await b.goto(`${BASE}/sign-in`);
  await b.fill("input[type=email]", email);
  await b.fill("input[type=password]", password);
  await b.click("Sign in");
  await b.goto(`${BASE}/c/${company.code}/d/vendor_invoice/new`);
  await Bun.sleep(800);

  // The order, from the header's picker, by its number.
  check((await focusLabelled("Purchase order")) === true, "purchase order picker found");
  await b.insert(order.number);
  await waitFor(`[...document.querySelectorAll("[role=option]")].some((o) => o.textContent.includes(${JSON.stringify(order.number)}))`);
  await b.key("Enter");
  await Bun.sleep(600);
  check((await b.text()).includes(order.number), `order ${order.number} chosen`);

  // Line 1: Enter on its order-line cell opens Astryx's Typeahead with the order's lines.
  const orderLine = await col("Order line");
  check(orderLine >= 0, "the order line column is in the grid");
  await b.evaluate(`(() => { const c = document.querySelector('[data-grid-body] [data-row="0"][data-col="${orderLine}"]'); c?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })); c?.focus(); })()`);
  await b.key("Enter");
  await waitFor(`[...document.querySelectorAll("[role=option]")].some((o) => o.textContent.includes("Line 1") && o.getBoundingClientRect().height > 0)`);
  if (SHOTS) await b.screenshot(`${SHOTS}/31-invoice-order-line-picker.png`);
  await b.key("Enter");
  await Bun.sleep(600);
  check((await cellText(0, orderLine)).startsWith("Line 1"), `order line chosen ("${await cellText(0, orderLine)}")`);

  // Quantity and price, typed.
  await b.key("ArrowUp");
  const [qty, price] = [await col("Quantity"), await col("Unit price")];
  await b.evaluate(`(() => { const c = document.querySelector('[data-grid-body] [data-row="0"][data-col="${qty}"]'); c?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })); c?.focus(); })()`);
  await b.key("1");
  await b.key("Tab");
  if (price !== qty + 1) for (let i = qty + 1; i < price; i += 1) await b.key("Tab");
  await b.key("1");
  await b.insert("0");
  await b.key("Tab");
  await Bun.sleep(1500);
  // The net and total are the server's (preview), in view without scrolling at 1280px.
  const net = await col("Net");
  const visible = await b.evaluate(`(() => { const h = [...document.querySelectorAll("[data-grid-header] [role=columnheader]")][${net}]; if (!h) return false; const r = h.getBoundingClientRect(); const s = h.closest(".ag-scroller").getBoundingClientRect(); return r.right <= s.right + 1; })()`);
  check(net >= 0 && visible === true, "the net column is in view at 1280px without scrolling sideways");
  if (SHOTS) await b.screenshot(`${SHOTS}/32-invoice-lines.png`);

  await b.key("s", 2);
  await Bun.sleep(2000);
  await b.settle();
  const at = await b.location();
  created = /\/d\/vendor_invoice\/([0-9a-f-]{36})$/.exec(at)?.[1] ?? "";
  if (!created) console.log((await b.text()).slice(0, 600));
  check(created !== "", `saved (${at})`);
  if (created) {
    const doc = (await session.run("document.get", { doc_type: "vendor_invoice", id: created })) as { body: { lines: { order_line_id: string | null }[] } };
    check(typeof doc.body.lines[0]?.order_line_id === "string", "the server holds the line's link to the order line");
    check((await cellText(0, orderLine)).startsWith("Line 1"), "the saved line still shows its order line by name");
  }
  // A list with 8 columns fits 1280px as well: no column past the right edge.
  const fits = async () =>
    b.evaluate(`(() => { const s = document.querySelector(".ag-scroller"); if (!s) return "no grid"; const right = s.getBoundingClientRect().right; const over = [...document.querySelectorAll("[data-grid-header] [role=columnheader]")].filter((h) => h.getBoundingClientRect().right > right + 1).map((h) => h.textContent.trim()); return over.length ? over.join(", ") : true; })()`);
  await b.goto(`${BASE}/c/${company.code}/d/sales_order`);
  await Bun.sleep(1200);
  const listFits = await fits();
  check(listFits === true, `the sales order list fits 1280px (${String(listFits)})`);
  if (SHOTS) await b.screenshot(`${SHOTS}/33-list-1280.png`);

} finally {
  if (created) await session.run("document.delete", { doc_type: "vendor_invoice", id: created }).catch(() => undefined);
  await b.close();
}
