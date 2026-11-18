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
const COMPANY = "OHIO";
const BIG = "AWC"; // the company with thousands of orders
const { email, password } = adminCredentials();

const browser = await launchBrowser(9344);
const shot = async (name: string) => {
  await Bun.sleep(300);
  await browser.screenshot(`${OUT}/${name}.png`);
  console.log("wrote", name);
};

try {
  await browser.viewport(1440, 900);
  await browser.goto(`${BASE}/sign-in`);
  await browser.fill("input[type=email]", email);
  await browser.evaluate("document.fonts.ready.then(() => true)");
  await Bun.sleep(1000);
  await shot("01-sign-in");
  await browser.fill("input[type=password]", password);
  await browser.click("Sign in");
  await browser.settle();

  await browser.goto(`${BASE}/c/${BIG}/sales-orders`);
  await shot("02-sales-orders");

  await browser.goto(`${BASE}/c/${BIG}/sales-orders?q=SO-2023-012&sort=-summary.total_net&status=closed`);
  await shot("03-sales-orders-search-sort");

  await browser.goto(`${BASE}/c/${BIG}/sales-orders?page=3&size=50&sort=summary.customer_name`);
  await shot("04-sales-orders-page3");

  await browser.goto(`${BASE}/c/${COMPANY}/sales-orders/${ORDER}`);
  await shot("05-sales-order");

  // The customer combobox: server-side search as the user types.
  await browser.evaluate(`(() => { const l = [...document.querySelectorAll("label")].find((x) => x.textContent.startsWith("Customer")); const i = l && document.getElementById(l.htmlFor); i?.focus(); i?.click(); })()`);
  await Bun.sleep(300);
  await browser.insert("adv");
  await Bun.sleep(800);
  await browser.settle();
  await shot("06-sales-order-customer-combobox");
  await browser.key("Escape");
  await browser.key("Escape");
  await browser.evaluate("document.activeElement?.blur()");
  await Bun.sleep(300);
  await shot("06b-sales-order-combobox-escaped");

  // Edit a quantity; the totals come back from the server's preview.
  await browser.evaluate(`(() => { const l = [...document.querySelectorAll("label")].find((x) => x.textContent.startsWith("Quantity, 1")); const e = l && document.getElementById(l.htmlFor); e?.focus(); e?.select(); })()`);
  await browser.insert("15");
  await Bun.sleep(1200);
  await browser.settle();
  await shot("07-sales-order-edited");

  await browser.goto(`${BASE}/c/${COMPANY}/sales-orders/new`);
  await shot("08-sales-order-new");
  // Saving without a customer: the server refuses, the error sits by the field, nothing typed is lost.
  await browser.evaluate(`(() => { const l = [...document.querySelectorAll("label")].find((x) => x.textContent.startsWith("Description, 1")); const e = l && document.getElementById(l.htmlFor); e?.focus(); })()`);
  await browser.insert("Consulting");
  await browser.key("s", 2);
  await Bun.sleep(800);
  await browser.settle();
  await shot("08b-sales-order-save-error");

  await browser.goto(`${BASE}/c/${COMPANY}/customers`);
  await shot("09-customers");

  const firstCustomer = await browser.evaluate(`document.querySelector("table a")?.getAttribute("href")`);
  if (typeof firstCustomer === "string") {
    await browser.goto(`${BASE}${firstCustomer}`);
    await shot("10-customer");
  }

  await browser.dark(true);
  await browser.goto(`${BASE}/c/${COMPANY}/sales-orders/${ORDER}`);
  await shot("11-sales-order-dark");
  await browser.goto(`${BASE}/c/${BIG}/sales-orders`);
  await shot("12-sales-orders-dark");
  await browser.dark(false);

  await browser.viewport(820, 1180);
  await browser.goto(`${BASE}/c/${COMPANY}/sales-orders/${ORDER}`);
  await shot("13-sales-order-tablet");

  if (browser.errors.length) console.log("page errors:\n" + browser.errors.join("\n"));
} finally {
  await browser.close();
}
