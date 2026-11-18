// The document lines in @erpcraft/grid, driven by the keyboard as a person would: edit a quantity
// by typing, add a line with ArrowDown on the last row, pick an item in the new line by typing its
// name, undo with Ctrl+Z. Works on a new order and never saves, so it leaves nothing behind.
// bun scripts/grid-smoke.ts

import { launchBrowser } from "./browser";
import { adminCredentials } from "./lib";

const BASE = process.env.SPIKE_URL ?? "http://127.0.0.1:5174";
const SHOTS = process.env.SHOTS ?? "";
const { email, password } = adminCredentials();
const b = await launchBrowser(9351);
const check = (ok: boolean, what: string) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) process.exitCode = 1;
};
const cell = (row: number, column: string) =>
  `[data-grid-body] [data-row="${row}"][data-col="${column}"]`;
const colIndex = async (header: string) =>
  Number(await b.evaluate(`[...document.querySelectorAll("[data-grid-header] [role=columnheader]")].findIndex((h) => h.textContent.trim().startsWith(${JSON.stringify(header)}))`));
const text = async (row: number, col: number) => String(await b.evaluate(`document.querySelector(${JSON.stringify(cell(row, String(col)))})?.textContent ?? "(none)"`));
const rows = async () => Number(await b.evaluate(`document.querySelectorAll("[data-grid-body] [role=row]").length`));
const type = async (keys: string) => {
  for (const ch of keys) await b.insert(ch);
};

try {
  await b.viewport(1440, 900);
  await b.goto(`${BASE}/sign-in`);
  await b.fill("input[type=email]", email);
  await b.fill("input[type=password]", password);
  await b.click("Sign in");
  await b.goto(`${BASE}/c/OHIO/d/sales_order/new`);
  await Bun.sleep(800);

  const desc = await colIndex("Description");
  const qty = await colIndex("Quantity");
  const item = await colIndex("Item");
  check(desc >= 0 && qty >= 0 && item >= 0, `columns found (item ${item}, description ${desc}, quantity ${qty})`);

  // Click the description cell of line 1 and type: typing replaces and starts editing.
  await b.evaluate(`document.querySelector(${JSON.stringify(cell(0, String(desc)))})?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }))`);
  await b.evaluate(`document.querySelector(${JSON.stringify(cell(0, String(desc)))})?.focus()`);
  await b.key("C");
  await Bun.sleep(300);
  await type("onsulting");
  await b.key("Tab");
  await Bun.sleep(200);
  check((await text(0, desc)) === "Consulting", `typing into a cell and Tab commits ("${await text(0, desc)}")`);

  // Tab moved to quantity: type a decimal, Enter commits.
  await b.key("2");
  await type(".5");
  await b.key("Enter");
  await Bun.sleep(200);
  check((await text(0, qty)).startsWith("2.5"), `quantity typed as a decimal ("${await text(0, qty)}")`);

  // Enter on the last row moved down into a new line (line editing).
  check((await rows()) === 2, `Enter on the last line added a line (${await rows()} rows)`);

  // In the new line, Home goes to its first column (the item); Enter edits it with Astryx's
  // Typeahead, which searches the server as the name is typed; Enter takes the first match.
  await b.key("Home");
  await b.key("Enter");
  await Bun.sleep(400);
  await type("Gadget");
  // Wait for the server's answer to show in Astryx's list.
  for (let i = 0; i < 30 && !(await b.evaluate(`[...document.querySelectorAll("[role=option]")].some((o) => o.textContent.includes("Gadget") && o.getBoundingClientRect().height > 0)`)); i += 1) await Bun.sleep(100);
  if (SHOTS) await b.screenshot(`${SHOTS}/28-grid-item-typeahead.png`);
  await b.key("Enter");
  await Bun.sleep(600);
  check((await text(1, item)).includes("Gadget"), `item chosen from the server search ("${await text(1, item)}")`);

  // Undo the item choice.
  await b.key("z", 2);
  await Bun.sleep(300);
  check(!(await text(1, item)).includes("Gadget"), "Ctrl+Z undid it");

  // Choosing with Enter on the last line moved down into a new one, as Excel's Enter does.
  const before = await rows();
  check(before === 3, `Enter after choosing on the last line added another (${before} rows)`);
  // Ctrl+- deletes the line of the active cell.
  await b.key("-", 2);
  await Bun.sleep(300);
  check((await rows()) === before - 1, `Ctrl+- deleted the line (${await rows()} rows)`);
  check((await b.text()).includes("Unsaved"), "the page knows it has unsaved changes");

  // Paste a block from Excel (TSV) at quantity of line 1: two lines, quantity and unit price.
  const price = await colIndex("Unit price");
  await b.key("Home", 2);
  for (let i = 0; i < qty; i += 1) await b.key("ArrowRight");
  const pasted = "3\t12.50\n4\t1,234.5\n";
  await b.evaluate(`(() => { const dt = new DataTransfer(); dt.setData("text/plain", ${JSON.stringify(pasted)}); document.activeElement.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true })); })()`);
  await Bun.sleep(400);
  check((await text(0, qty)) === "3" && (await text(0, price)).startsWith("12.50"), `pasted line 1 from Excel (${await text(0, qty)} × ${await text(0, price)})`);
  check((await text(1, qty)) === "4" && (await text(1, price)).startsWith("1,234.50"), `pasted line 2, "1,234.5" read as a decimal (${await text(1, qty)} × ${await text(1, price)})`);

  // Copy the same block back out: TSV, cell by cell, as Excel takes it.
  await b.key("ArrowRight", 8);
  await b.key("ArrowDown", 8);
  const copied = await b.evaluate(`(() => { const dt = new DataTransfer(); document.activeElement.dispatchEvent(new ClipboardEvent("copy", { clipboardData: dt, bubbles: true, cancelable: true })); return dt.getData("text/plain"); })()`);
  check(typeof copied === "string" && /^3\t12\.50/.test(copied) && /\n4\t1,234\.50/.test(copied), `the selection copied as TSV for Excel (${JSON.stringify(copied)})`);
  if (SHOTS) await b.screenshot(`${SHOTS}/29-grid-lines-edited.png`);
  // ------------------------------------------------------------ a list on the grid, server mode
  await b.goto(`${BASE}/c/AWC/d/sales_order`);
  await Bun.sleep(1000);
  const header = async (label: string) => Number(await colIndex(label));
  // Sort by the total: the header's button, then once more for descending.
  const total = await header("Total");
  const sortButton = `[data-grid-header] [role=columnheader]:nth-child(${total + 1}) button`;
  await b.evaluate(`document.querySelector(${JSON.stringify(sortButton)})?.click()`);
  await Bun.sleep(600);
  await b.evaluate(`document.querySelector(${JSON.stringify(sortButton)})?.click()`);
  await Bun.sleep(1200);
  check((await b.location()).includes("sort=-summary.total"), `sorting on the server, kept in the URL (${await b.location()})`);
  // The next page, from the pager.
  await b.evaluate(`[...document.querySelectorAll("nav button")].find((x) => x.textContent.trim() === "2")?.click()`);
  await Bun.sleep(1200);
  check((await b.location()).includes("page=2"), `paging on the server, kept in the URL (${await b.location()})`);
  const listUrl = await b.location();
  // Choose two orders by typing into the choice column, as into any cell: y, ArrowDown, y.
  const chosenCol = await header("Choose");
  await b.evaluate(`(() => { const c = document.querySelector('[data-grid-body] [data-row="0"][data-col="${chosenCol}"]'); c?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })); c?.focus(); })()`);
  await b.key("y");
  await b.key("Enter");
  await b.key("y");
  await b.key("Enter");
  await Bun.sleep(500);
  check((await b.text()).includes("2 chosen"), "two orders chosen by typing y into the choice column");
  if (SHOTS) await b.screenshot(`${SHOTS}/30-list-grid.png`);
  // Copy a block of the list for Excel.
  await b.key("ArrowRight");
  await b.key("ArrowDown", 8);
  await b.key("ArrowRight", 8);
  const block = await b.evaluate(`(() => { const dt = new DataTransfer(); document.activeElement.dispatchEvent(new ClipboardEvent("copy", { clipboardData: dt, bubbles: true, cancelable: true })); return dt.getData("text/plain"); })()`);
  check(typeof block === "string" && /^SO-\d{4}-\d+\t/.test(block) && block.split("\n").filter(Boolean).length === 2, `rows copied as TSV (${JSON.stringify(block).slice(0, 80)})`);
  // Enter on the number opens the order; Back returns to the same sorted page.
  await b.key("Escape");
  await b.key("Enter");
  await Bun.sleep(1500);
  check(/\/d\/sales_order\/[0-9a-f-]{36}/.test(await b.location()), `Enter on the number opened the order (${await b.location()})`);
  await b.evaluate("history.back()");
  await Bun.sleep(1500);
  check((await b.location()) === listUrl, `Back returned to the same sorted page (${await b.location()})`);

  // ------------------------------------------------------------ a report on the grid, client mode
  await b.goto(`${BASE}/c/OHIO/r/gl.trial_balance`);
  await Bun.sleep(1200);
  await b.evaluate(`(() => { const c = document.querySelector('[data-grid-body] [data-row="0"][data-col="0"]'); c?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })); c?.focus(); })()`);
  await b.key("ArrowDown", 8);
  await b.key("End", 8);
  const report = await b.evaluate(`(() => { const dt = new DataTransfer(); document.activeElement.dispatchEvent(new ClipboardEvent("copy", { clipboardData: dt, bubbles: true, cancelable: true })); return dt.getData("text/plain"); })()`);
  check(typeof report === "string" && /^1000\t/.test(report) && report.split("\n").filter(Boolean).length === 2, `trial balance rows copied as TSV (${JSON.stringify(report).slice(0, 90)})`);
  // Sorting in client mode: the balance, largest first.
  const balance = await colIndex("Balance");
  const sortBalance = `[data-grid-header] [role=columnheader]:nth-child(${balance + 1}) button`;
  await b.evaluate(`document.querySelector(${JSON.stringify(sortBalance)})?.click()`);
  await b.evaluate(`document.querySelector(${JSON.stringify(sortBalance)})?.click()`);
  await Bun.sleep(400);
  const top = await text(0, 0);
  check(top === "1500", `sorted by balance in the browser, largest first (${top})`);

  if (b.errors.length) console.log(`page errors:\n${b.errors.join("\n")}`);
} finally {
  await b.close();
}
