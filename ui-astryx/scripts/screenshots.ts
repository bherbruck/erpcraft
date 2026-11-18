// Screenshots of the Astryx spike, against the running preview on 5174.
// bun scripts/screenshots.ts

import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { launchBrowser } from "./browser";
import { adminCredentials } from "./lib";

const BASE = process.env.SPIKE_URL ?? "http://127.0.0.1:5174";
const OUT = resolve(import.meta.dir, "../docs/screenshots");
mkdirSync(OUT, { recursive: true });

const ORDER = process.env.ORDER_ID ?? "01a1ea99-5a5b-7361-a3ee-ce746505d37e";
const SMALL = "OHIO"; // the demo company, with the draft order
const BIG = "AWC"; // the company with thousands of orders
const { email, password } = adminCredentials();

const browser = await launchBrowser(9344);
const shot = async (name: string) => {
  await Bun.sleep(400);
  await browser.screenshot(`${OUT}/${name}.png`);
  console.log("wrote", name);
};
/** Makes a grid cell the active one, by its column header, as a click does. */
const activateCell = async (row: number, header: string) => {
  const col = Number(await browser.evaluate(`[...document.querySelectorAll("[data-grid-header] [role=columnheader]")].findIndex((h) => h.textContent.trim().startsWith(${JSON.stringify(header)}))`));
  await browser.evaluate(`(() => { const c = document.querySelector('[data-grid-body] [data-row="${row}"][data-col="${col}"]'); c?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })); c?.focus(); })()`);
};
/** Focuses the input an Astryx label names. */
const focusLabelled = (prefix: string) =>
  browser.evaluate(
    `(() => { const l = [...document.querySelectorAll("label")].find((x) => x.textContent.startsWith(${JSON.stringify(prefix)})); const e = l && document.getElementById(l.htmlFor); e?.focus(); e?.select?.(); e?.click?.(); return Boolean(e); })()`,
  );

try {
  await browser.viewport(1440, 900);
  await browser.goto(`${BASE}/sign-in`);
  await browser.fill("input[type=email]", email);
  await Bun.sleep(800);
  await shot("01-sign-in");
  await browser.fill("input[type=password]", password);
  await browser.click("Sign in");
  await browser.settle();

  // Generated lists: the same component for every document type and entity.
  await browser.goto(`${BASE}/c/${BIG}/d/sales_order`);
  await shot("02-sales-orders");
  const chips = JSON.stringify([
    { field: "q", operator: "contains", value: { type: "string", value: "SO-2023-012" } },
    { field: "status", operator: "any", value: { type: "enum_list", value: ["closed"] } },
    { field: "doc_date", operator: ">=", value: { type: "date_absolute", unixSeconds: Date.UTC(2023, 4, 1) / 1000 } },
  ]);
  await browser.goto(`${BASE}/c/${BIG}/d/sales_order?f=${encodeURIComponent(chips)}&sort=-summary.total_net`);
  await shot("03-sales-orders-filter-chips-sort");
  await browser.evaluate(`(() => { const i = document.querySelector(".filter-bar input"); i?.focus(); i?.click(); })()`);
  await browser.insert("cust");
  await Bun.sleep(800);
  await shot("03b-filter-field-suggestions");
  await browser.key("Escape");
  await browser.goto(`${BASE}/c/${BIG}/d/purchase_order`);
  await shot("04-purchase-orders");
  await browser.goto(`${BASE}/c/${BIG}/d/journal_entry`);
  await shot("04b-journal-entries");

  // A generated document page.
  await browser.goto(`${BASE}/c/${SMALL}/d/sales_order/${ORDER}`);
  await shot("05-sales-order");
  await focusLabelled("Customer");
  await Bun.sleep(300);
  await browser.insert("adv");
  await Bun.sleep(1000);
  await browser.settle();
  await shot("06-sales-order-customer-combobox");
  await browser.key("Escape");
  await browser.key("Escape");
  await browser.evaluate("document.activeElement?.blur()");
  await activateCell(0, "Quantity");
  await browser.key("1");
  await browser.insert("5");
  await browser.key("Enter");
  await Bun.sleep(1500);
  await browser.settle();
  await shot("07-sales-order-edited");
  await browser.evaluate(`[...document.querySelectorAll("button")].find((b) => b.textContent.trim() === "Approve")?.click()`);
  await Bun.sleep(1200);
  await shot("07b-sales-order-action-blocked-while-dirty");

  await browser.goto(`${BASE}/c/${SMALL}/d/sales_order/new`);
  await shot("08-sales-order-new");
  // "New Customer…" from inside the lookup: created in a dialog, the form underneath kept.
  await focusLabelled("Customer");
  await browser.insert("Zephyr Cycles");
  await Bun.sleep(1000);
  await browser.key("Enter");
  await Bun.sleep(600);
  await shot("08a-quick-create-customer");
  await browser.evaluate(`(() => { const d = document.querySelector("dialog[open]"); const b = d && [...d.querySelectorAll("button")].find((x) => x.textContent.trim() === "Cancel"); b?.click(); })()`);
  await Bun.sleep(300);
  await activateCell(0, "Description");
  await browser.key("C");
  await browser.insert("onsulting");
  await browser.key("Tab");
  await browser.key("s", 2);
  await Bun.sleep(1000);
  await browser.settle();
  await shot("08b-sales-order-save-error");

  // A posted document with its postings.
  const posted = await browser.evaluate(
    `fetch("/api/commands/document.list", { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer " + sessionStorage.getItem("erpcraft.token") }, body: JSON.stringify({ doc_type: "journal_entry", status: "posted", limit: 1 }) }).then((r) => r.json()).then((p) => p.items[0] ? p.items[0].company_id + "|" + p.items[0].id : "")`,
  );
  if (typeof posted === "string" && posted) {
    const [companyId, id] = posted.split("|");
    const companyCode = await browser.evaluate(
      `fetch("/api/commands/entity.get", { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer " + sessionStorage.getItem("erpcraft.token") }, body: JSON.stringify({ entity: "company", id: ${JSON.stringify(companyId)} }) }).then((r) => r.json()).then((c) => c.code)`,
    );
    await browser.goto(`${BASE}/c/${String(companyCode)}/d/journal_entry/${id}`);
    await shot("09-journal-entry-posted");
    // Reverse: the dialog shows the server's preview of the postings before anything happens.
    await browser.evaluate(`[...document.querySelectorAll("button")].find((b) => b.textContent.trim() === "Reverse")?.click()`);
    await Bun.sleep(1500);
    await shot("09b-reverse-preview-dialog");
    await browser.key("Escape");
  }

  // Generated entity screens.
  await browser.goto(`${BASE}/c/${BIG}/e/customer`);
  await shot("10-customers");
  const firstCustomer = await browser.evaluate(`document.querySelector("[data-grid-body] a")?.getAttribute("href")`);
  if (typeof firstCustomer === "string") {
    await browser.goto(`${BASE}${firstCustomer}`);
    await shot("11-customer");
    await browser.goto(`${BASE}${firstCustomer}?tab=history`);
    await shot("11b-customer-history");
  }
  await browser.goto(`${BASE}/c/${BIG}/e/item`);
  await shot("12-items");
  await browser.goto(`${BASE}/c/${BIG}/e/account`);
  await shot("12b-accounts");

  // The rest of the app: reports, approvals, printing, commands, saved views, the palette.
  await browser.goto(`${BASE}/c/${SMALL}/r/gl.trial_balance`);
  await shot("16-report-trial-balance");
  await browser.goto(`${BASE}/c/${BIG}/r/stock.on_hand`);
  await shot("16b-report-stock-on-hand");
  await browser.goto(`${BASE}/c/${SMALL}/approvals?scope=all`);
  await shot("17-approvals");
  const approval = await browser.evaluate(`document.querySelector("table a")?.getAttribute("href")`);
  if (typeof approval === "string") {
    await browser.goto(`${BASE}${approval}`);
    await shot("17b-approval");
  }
  await browser.goto(`${BASE}/c/${SMALL}/d/sales_order/${ORDER}/print`);
  await Bun.sleep(800);
  await shot("18-print");
  await browser.goto(`${BASE}/c/${SMALL}/run/bank_statement.import`);
  await shot("19-command-bank-import");
  await browser.goto(`${BASE}/c/${BIG}/d/sales_order`);
  await browser.evaluate(`[...document.querySelectorAll("button")].find((b) => b.textContent.trim() === "Views")?.click()`);
  await Bun.sleep(500);
  await shot("20-saved-views");
  await browser.key("Escape");
  await browser.key("k", 2);
  await Bun.sleep(600);
  await browser.insert("bike");
  await Bun.sleep(1500);
  await shot("21-command-palette");
  await browser.key("Escape");

  // Parity with ../ui: statements, aging, bank reconciliation and matching, bulk, setup.
  await browser.goto(`${BASE}/c/${SMALL}/r/gl.balance_sheet`);
  await shot("22-balance-sheet");
  await browser.goto(`${BASE}/c/${SMALL}/r/gl.income_statement`);
  await shot("22b-income-statement");
  await browser.goto(`${BASE}/c/${BIG}/r/open_item.aging`);
  await shot("23-aging");
  const statement = "01a1eadc-14a4-7697-8e02-3918b29c6235";
  await browser.goto(`${BASE}/c/${SMALL}/d/bank_statement/${statement}`);
  await shot("24-bank-statement-matching");
  const account = await browser.evaluate(
    `fetch("/api/commands/document.get", { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer " + sessionStorage.getItem("erpcraft.token") }, body: JSON.stringify({ doc_type: "bank_statement", id: "${statement}" }) }).then((r) => r.json()).then((d) => d.body.bank_account_id)`,
  );
  await browser.goto(`${BASE}/c/${SMALL}/r/bank.reconciliation?account=${String(account)}`);
  await shot("24b-bank-reconciliation");
  await browser.goto(`${BASE}/c/${SMALL}/d/journal_entry`);
  await browser.evaluate(`[...document.querySelectorAll(".ag-check input[type=checkbox]")].slice(0, 3).forEach((c) => c.click())`);
  await Bun.sleep(600);
  await shot("25-bulk-selection");
  await browser.goto(`${BASE}/c/${SMALL}/d/sales_order/${ORDER}`);
  await browser.key("k", 2);
  await Bun.sleep(900);
  await shot("26-palette-record-actions");
  await browser.key("Escape");
  await browser.goto(`${BASE}/c/${SMALL}/setup`);
  await browser.evaluate(`document.querySelector("input[type=radio]")?.click()`);
  await Bun.sleep(400);
  await shot("27-company-setup");

  await browser.dark(true);
  await browser.goto(`${BASE}/c/${SMALL}/d/sales_order/${ORDER}`);
  await shot("13-sales-order-dark");
  await browser.goto(`${BASE}/c/${BIG}/d/sales_order`);
  await shot("14-sales-orders-dark");
  await browser.dark(false);

  await browser.viewport(820, 1180);
  await browser.goto(`${BASE}/c/${SMALL}/d/sales_order/${ORDER}`);
  await shot("15-sales-order-tablet");

  if (browser.errors.length) console.log(`page errors:\n${browser.errors.join("\n")}`);
} finally {
  await browser.close();
}
