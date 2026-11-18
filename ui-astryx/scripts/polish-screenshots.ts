// The screens ../ui/docs/screenshots shows, taken the same way, for the side-by-side with shadcn.
// bun scripts/polish-screenshots.ts  → docs/screenshots/polish-*.png

import { resolve } from "node:path";
import { launchBrowser } from "./browser";
import { adminCredentials } from "./lib";

const BASE = process.env.SPIKE_URL ?? "http://127.0.0.1:5174";
const OUT = resolve(import.meta.dir, "../docs/screenshots");
const ORDER = "01a1ea99-5a5b-7361-a3ee-ce746505d37e";
const { email, password } = adminCredentials();
const b = await launchBrowser(9353);
const shot = async (name: string, wait = 1500) => {
  await Bun.sleep(wait);
  await b.screenshot(`${OUT}/polish-${name}.png`);
  console.log("wrote", name);
};

try {
  await b.viewport(1440, 900);
  await b.goto(`${BASE}/sign-in`);
  await b.fill("input[type=email]", email);
  await b.fill("input[type=password]", password);
  await b.click("Sign in");
  await b.goto(`${BASE}/c/AWC/d/sales_order`);
  await shot("01-sales-order-list");
  await b.goto(`${BASE}/c/OHIO/d/sales_order/${ORDER}`);
  await shot("02-sales-order");
  await b.goto(`${BASE}/c/AWC/e/customer`);
  await Bun.sleep(1200);
  const customer = await b.evaluate(`document.querySelector("[data-grid-body] a")?.getAttribute("href")`);
  if (typeof customer === "string") {
    await b.goto(`${BASE}${customer}`);
    await shot("03-customer");
  }
  await b.goto(`${BASE}/c/OHIO/r/gl.trial_balance`);
  await shot("04-trial-balance");
  await b.goto(`${BASE}/c/OHIO/approvals?scope=all`);
  await shot("05-approvals");
  await b.goto(`${BASE}/c/OHIO/d/sales_order/${ORDER}`);
  await Bun.sleep(1200);
  await b.key("k", 2);
  await shot("06-palette", 900);
  await b.key("Escape");
  await b.dark(true);
  await b.goto(`${BASE}/c/OHIO/d/sales_order/${ORDER}`);
  await shot("07-dark-sales-order");
  await b.goto(`${BASE}/c/AWC/d/sales_order`);
  await shot("08-dark-sales-order-list");
  await b.dark(false);
  await b.viewport(820, 1180);
  await b.goto(`${BASE}/c/OHIO/d/sales_order/${ORDER}`);
  await shot("09-tablet-sales-order");
} finally {
  await b.close();
}
