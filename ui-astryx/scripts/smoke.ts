// A round trip through the generated document page: create a draft sales order with the
// keyboard and the lookups, save it with Ctrl+S, then delete it again. Leaves nothing behind.
// bun scripts/smoke.ts

import { launchBrowser } from "./browser";
import { adminCredentials } from "./lib";

const BASE = process.env.SPIKE_URL ?? "http://127.0.0.1:5174";
const { email, password } = adminCredentials();
const browser = await launchBrowser(9347);
const check = (ok: boolean, what: string) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) process.exitCode = 1;
};
const focusLabelled = (prefix: string) =>
  browser.evaluate(
    `(() => { const l = [...document.querySelectorAll("label")].find((x) => x.textContent.startsWith(${JSON.stringify(prefix)})); const e = l && document.getElementById(l.htmlFor); e?.focus(); e?.click?.(); return Boolean(e); })()`,
  );
const pickFirstOption = async () => {
  await Bun.sleep(900);
  // Astryx highlights the first result; Enter takes it.
  await browser.key("Enter");
  await Bun.sleep(300);
};

try {
  await browser.viewport(1440, 900);
  await browser.goto(`${BASE}/sign-in`);
  await browser.fill("input[type=email]", email);
  await browser.fill("input[type=password]", password);
  await browser.click("Sign in");
  await browser.goto(`${BASE}/c/OHIO/d/sales_order/new`);

  check((await focusLabelled("Customer")) === true, "customer lookup found by its label");
  await browser.insert("Buyer");
  await pickFirstOption();
  check(/Buyer/.test(await browser.text()), "customer chosen from the server search");

  // The lines are @erpcraft/grid: everything by keyboard, as in a spreadsheet.
  const col = async (header: string) =>
    Number(await browser.evaluate(`[...document.querySelectorAll("[data-grid-header] [role=columnheader]")].findIndex((h) => h.textContent.trim().startsWith(${JSON.stringify(header)}))`));
  const cellText = (row: number, c: number) =>
    browser.evaluate(`document.querySelector('[data-grid-body] [data-row="${row}"][data-col="${c}"]')?.textContent ?? ""`);
  const [account, description] = [await col("Account"), await col("Description")];
  await browser.evaluate(`(() => { const c = document.querySelector('[data-grid-body] [data-row="0"][data-col="${account}"]'); c?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })); c?.focus(); })()`);
  await browser.key("Enter");
  await Bun.sleep(300);
  await browser.insert("4000");
  await Bun.sleep(1200);
  await browser.key("Enter");
  await Bun.sleep(500);
  check(String(await cellText(0, account)).includes("4000"), `account chosen in the grid (${String(await cellText(0, account))})`);
  // Enter took the choice and, on the last line, added a line and moved into it.
  check(Number(await browser.evaluate(`document.querySelectorAll("[data-grid-body] [role=row]").length`)) === 2, "Enter on the last line added line 2");
  await browser.key("ArrowUp");
  await browser.key("Home");
  for (let i = 0; i < description; i += 1) await browser.key("ArrowRight");
  // A key press starts the edit (as in Excel); the rest is typed into the cell's input.
  const typeIn = async (text: string) => {
    await browser.key(text.charAt(0));
    if (text.length > 1) await browser.insert(text.slice(1));
  };
  await typeIn("Smoke test consulting");
  await browser.key("Tab");
  await typeIn("2");
  await browser.key("Tab");
  await typeIn("12.5");
  await browser.key("Tab");
  await Bun.sleep(1500);
  check((await browser.text()).includes("25.00"), "server preview worked out the total (25.00)");

  await browser.key("s", 2);
  await Bun.sleep(1500);
  await browser.settle();
  const at = await browser.location();
  check(/\/d\/sales_order\/[0-9a-f-]{36}$/.test(at), `Ctrl+S saved and moved to the record (${at})`);
  check(!(await browser.text()).includes("Unsaved"), "nothing unsaved after the save");

  await browser.click("Delete");
  await Bun.sleep(500);
  await browser.evaluate(`(() => { const d = document.querySelector("dialog[open]"); const b = d && [...d.querySelectorAll("button")].find((x) => x.textContent.trim() === "Delete"); b?.click(); })()`);
  await Bun.sleep(1500);
  await browser.settle();
  check((await browser.location()).endsWith("/d/sales_order"), "deleted, back on the list");
  // Nothing leaves the origin (no Google Fonts), the theme font is the bundled one, and the
  // server's CSP (served by the preview too) blocked nothing.
  const foreign = await browser.evaluate(
    `performance.getEntriesByType("resource").map((e) => e.name).filter((u) => !u.startsWith(location.origin)).join(" ")`,
  );
  check(foreign === "", `no request to another origin${foreign ? `: ${String(foreign)}` : ""}`);
  check((await browser.evaluate(`document.fonts.check("16px Figtree") && [...document.fonts].some((f) => f.family.replaceAll('"', "") === "Figtree" && f.status === "loaded")`)) === true, "Figtree loaded from the bundle");
  const blocked = browser.errors.filter((e) => /Content Security Policy|Refused to/i.test(e));
  check(blocked.length === 0, `no CSP violations${blocked.length ? `: ${blocked.join("; ")}` : ""}`);
  if (browser.errors.length) console.log(`page errors:\n${browser.errors.join("\n")}`);
} finally {
  await browser.close();
}
