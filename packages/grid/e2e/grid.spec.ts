import { expect, type Page, test } from "@playwright/test";

// The demo's client tab with fewer rows, so each test starts fast.
async function open(page: Page, hash = "client", rows = 1000) {
  await page.goto(`/?rows=${rows}#${hash}`);
  await page.evaluate(() => localStorage.clear());
  await page.reload();
  await expect(page.locator('[role="gridcell"]').first()).toBeVisible();
}

/** Row and column of the focused cell, or what has focus instead. */
async function focused(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el) return "none";
    if (el.dataset.row !== undefined) return `${el.dataset.row},${el.dataset.col}`;
    if (el.closest("[data-grid-cell]")) return `editor:${(el as HTMLInputElement).value}`;
    return el.dataset.testid ?? el.tagName;
  });
}

const chips = (page: Page) => page.getByRole("list", { name: "Filters" }).getByRole("listitem");
const count = (page: Page) => page.locator("[data-grid-toolbar] output");
const footer = (page: Page) => page.locator("[data-grid-footer] [role='gridcell']");
const pager = (page: Page) => page.getByRole("navigation", { name: "Page" });
/** The filter popover trigger of a column, by its header text. */
const filterButton = (page: Page, name: string) => page.getByRole("button", { name: `Filter: ${name}`, exact: true });
const menuButton = (page: Page, name: string) => page.getByRole("button", { name: `Column menu: ${name}`, exact: true });

function cell(page: Page, row: number, col: number) {
  return page.locator(`[role="grid"] [data-row="${row}"][data-col="${col}"]`);
}

const COL = { number: 0, customer: 1, status: 2, date: 3, qty: 4, amount: 5, memo: 6, urgent: 7 };

test("arrows, Tab, Home/End, Ctrl+Home/End and PageDown move the active cell", async ({ page }) => {
  await open(page);
  await cell(page, 0, 0).click();
  expect(await focused(page)).toBe("0,0");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowDown");
  expect(await focused(page)).toBe("1,1");
  await page.keyboard.press("Tab");
  expect(await focused(page)).toBe("1,2");
  await page.keyboard.press("Shift+Tab");
  expect(await focused(page)).toBe("1,1");
  await page.keyboard.press("End");
  expect(await focused(page)).toBe("1,7");
  await page.keyboard.press("Home");
  expect(await focused(page)).toBe("1,0");
  await page.keyboard.press("Control+End");
  expect(await focused(page)).toBe("999,7");
  await expect(cell(page, 999, 0)).toHaveText("SO-001000");
  await page.keyboard.press("Control+Home");
  expect(await focused(page)).toBe("0,0");
  await page.keyboard.press("PageDown");
  const [row] = (await focused(page)).split(",").map(Number);
  expect(row).toBeGreaterThan(5);
  // ArrowUp from the first row reaches the header.
  await page.keyboard.press("Control+Home");
  await page.keyboard.press("ArrowUp");
  expect(await focused(page)).toBe("-1,0");
  await expect(page.locator('[role="columnheader"][data-col="0"]')).toHaveAttribute("tabindex", "0");
});

test("roving tabindex: one tab stop, and Tab at the edge leaves the grid", async ({ page }) => {
  await open(page);
  expect(await page.locator('[role="grid"] [tabindex="0"]').count()).toBe(1);
  await page.getByTestId("before-grid").focus();
  // Search box, chip area buttons, Columns button, then the grid's one tab stop.
  for (let i = 0; i < 5 && !(await focused(page)).includes(","); i += 1) await page.keyboard.press("Tab");
  expect(await focused(page)).toBe("0,0");
  await page.keyboard.press("End");
  await page.keyboard.press("Tab");
  // The next thing on the page after the grid: the demo's "Query state" details.
  expect(await focused(page)).toBe("SUMMARY");
});

test("typing replaces, Enter commits and moves down, Escape cancels, F2 edits", async ({ page }) => {
  await open(page);
  await cell(page, 0, COL.memo).click();
  await page.keyboard.type("Hello");
  expect(await focused(page)).toBe("editor:Hello");
  await page.keyboard.press("Enter");
  await expect(cell(page, 0, COL.memo)).toHaveText("Hello");
  expect(await focused(page)).toBe(`1,${COL.memo}`);

  await page.keyboard.type("Nope");
  await page.keyboard.press("Escape");
  expect(await focused(page)).toBe(`1,${COL.memo}`);
  await expect(cell(page, 1, COL.memo)).not.toHaveText("Nope");

  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("F2");
  await page.keyboard.type(" world");
  await page.keyboard.press("Tab");
  await expect(cell(page, 0, COL.memo)).toHaveText("Hello world");
  expect(await focused(page)).toBe(`0,${COL.urgent}`);

  // Enter on an editable cell edits it, as in Google Sheets.
  await cell(page, 2, COL.qty).click();
  await page.keyboard.press("Enter");
  expect((await focused(page)).startsWith("editor:")).toBe(true);
  await page.keyboard.press("Escape");
});

test("a refused value keeps the edit open with the reason", async ({ page }) => {
  await open(page);
  await cell(page, 0, COL.qty).click();
  await page.keyboard.type("-5");
  await page.keyboard.press("Enter");
  expect(await focused(page)).toBe("editor:-5");
  await expect(page.locator('[role="alert"]')).toHaveText("must not be negative");
  await expect(page.getByTestId("report")).toContainText("1 rejected");
  await page.keyboard.press("Escape");
  expect(await focused(page)).toBe(`0,${COL.qty}`);
});

test("Shift+arrows select a range; Ctrl+C copies it as TSV", async ({ page }) => {
  await open(page);
  await cell(page, 0, COL.number).click();
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowDown");
  expect(await focused(page)).toBe("0,0");
  expect(await page.locator('[role="gridcell"][aria-selected="true"]').count()).toBe(6);
  await page.keyboard.press("Control+c");
  const text = await page.evaluate(() => navigator.clipboard.readText());
  const lines = text.split("\r\n").map((l) => l.split("\t"));
  expect(lines).toHaveLength(2);
  expect(lines[0]?.[0]).toBe("SO-000001");
  expect(lines[1]?.[0]).toBe("SO-000002");
  expect(lines[0]).toHaveLength(3);
});

test("Shift+click and Ctrl+A select", async ({ page }) => {
  await open(page);
  await cell(page, 1, 1).click();
  await cell(page, 3, 3).click({ modifiers: ["Shift"] });
  expect(await page.locator('[role="gridcell"][aria-selected="true"]').count()).toBe(9);
  expect(await focused(page)).toBe("1,1");
  await page.keyboard.press("Control+a");
  const selected = await page.locator('[role="gridcell"][data-row][aria-selected="true"]').count();
  const all = await page.locator('[role="gridcell"][data-row]').count();
  expect(selected).toBe(all);
});

test("Ctrl+V pastes TSV from Excel, reports rejected cells; Ctrl+Z undoes it", async ({ page }) => {
  await open(page);
  await page.evaluate(() => navigator.clipboard.writeText("7\t12.50\tPasted\r\nabc\t1,000.00\t\"two\nlines\"\r\n"));
  await cell(page, 0, COL.qty).click();
  const before = await cell(page, 1, COL.qty).textContent();
  await page.keyboard.press("Control+v");
  await expect(cell(page, 0, COL.qty)).toHaveText("7");
  await expect(cell(page, 0, COL.amount)).toContainText("12.50");
  await expect(cell(page, 0, COL.memo)).toHaveText("Pasted");
  await expect(cell(page, 1, COL.amount)).toContainText("1,000.00");
  await expect(cell(page, 1, COL.memo)).toHaveText("two\nlines");
  await expect(cell(page, 1, COL.qty)).toHaveText(before ?? "");
  await expect(page.getByTestId("report")).toContainText("paste: 5 applied, 1 rejected");
  await expect(page.getByTestId("rejected")).toContainText('row 2, qty: "abc" is not a number');
  await expect(page.locator("[data-grid-live]")).toContainText("Pasted 5 cells");
  // The pasted block is selected.
  expect(await page.locator('[role="gridcell"][aria-selected="true"]').count()).toBe(6);

  await page.keyboard.press("Control+z");
  await expect(cell(page, 0, COL.memo)).not.toHaveText("Pasted");
  await expect(cell(page, 1, COL.amount)).not.toContainText("1,000.00");
  await page.keyboard.press("Control+y");
  await expect(cell(page, 0, COL.memo)).toHaveText("Pasted");
});

test("Delete clears editable cells and leaves read-only ones", async ({ page }) => {
  await open(page);
  await cell(page, 0, COL.number).click();
  await page.keyboard.press("Shift+End");
  await page.keyboard.press("Delete");
  await expect(cell(page, 0, COL.number)).toHaveText("SO-000001");
  await expect(cell(page, 0, COL.qty)).toHaveText("");
  await expect(cell(page, 0, COL.memo)).toHaveText("");
  await page.keyboard.press("Control+z");
  await expect(cell(page, 0, COL.qty)).not.toHaveText("");
});

test("Ctrl+arrow jumps to the edge of the data", async ({ page }) => {
  await open(page);
  // Memo has gaps: from a filled cell Ctrl+Down stops at the end of its block.
  await cell(page, 0, COL.number).click();
  await page.keyboard.press("Control+ArrowDown");
  expect(await focused(page)).toBe("999,0");
  await page.keyboard.press("Control+ArrowRight");
  expect(await focused(page)).toBe("999,7");
});

test("click sorts, Shift+click adds a sort with order badges; focus stays", async ({ page }) => {
  await open(page);
  const header = (col: number) => page.locator(`[role="columnheader"][data-col="${col}"]`);
  await header(COL.qty).locator("button").first().click();
  await expect(header(COL.qty)).toHaveAttribute("aria-sort", "ascending");
  // The header's own button handed focus to the header cell: one tab stop, and its keys work.
  expect(await focused(page)).toBe(`-1,${COL.qty}`);
  const first = Number(await cell(page, 0, COL.qty).textContent());
  const second = Number(await cell(page, 1, COL.qty).textContent());
  expect(first).toBeLessThanOrEqual(second);
  await header(COL.qty).locator("button").first().click();
  await expect(header(COL.qty)).toHaveAttribute("aria-sort", "descending");

  await header(COL.status).locator("button").first().click({ modifiers: ["Shift"] });
  await expect(header(COL.qty).locator("[data-sort-order]")).toHaveText("1");
  await expect(header(COL.status).locator("[data-sort-order]")).toHaveText("2");

  // By keyboard: Enter on a header sorts, and the header keeps focus.
  await cell(page, 0, COL.amount).click();
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Enter");
  await expect(header(COL.amount)).toHaveAttribute("aria-sort", "ascending");
  expect(await focused(page)).toBe(`-1,${COL.amount}`);
  // Money sorts by amount, not as text.
  const amounts = await Promise.all(
    [0, 1, 2, 3].map(async (r) => Number((await cell(page, r, COL.amount).innerText()).replace(/[^\d.]/g, ""))),
  );
  expect([...amounts].sort((a, b) => a - b)).toEqual(amounts);
});

test("typed filters show as chips; chips remove; clear all; quick search", async ({ page }) => {
  await open(page);
  // Number filter through the column menu, opened from the keyboard.
  await cell(page, 0, COL.qty).click();
  await page.keyboard.press("Alt+ArrowDown");
  const menu = page.locator("[data-grid-menu]");
  await expect(menu).toBeVisible();
  await menu.getByLabel("Condition").selectOption("ge");
  await menu.getByLabel("Value").fill("490");
  await menu.getByLabel("Value").press("Enter");
  await expect(menu).toBeHidden();
  // Focus came back to the cell the menu was opened from.
  expect(await focused(page)).toBe(`0,${COL.qty}`);
  await expect(chips(page)).toHaveCount(1);
  await expect(chips(page).first().getByRole("button").first()).toHaveText("Qty ≥ 490");
  const shown = await count(page).textContent();
  expect(shown).toMatch(/^\d+ of 1000 rows$/);
  for (let r = 0; r < 3; r += 1) expect(Number(await cell(page, r, COL.qty).textContent())).toBeGreaterThanOrEqual(490);

  // A checklist on the state column.
  await filterButton(page, "Status").click();
  await menu.getByLabel("Draft").uncheck();
  await menu.getByLabel("Cancelled").uncheck();
  await menu.getByRole("button", { name: "Done" }).click();
  await expect(chips(page)).toHaveCount(2);
  await expect(chips(page).nth(1).getByRole("button").first()).toContainText("Status: Submitted, Posted");
  for (let r = 0; r < 3; r += 1) await expect(cell(page, r, COL.status)).toHaveText(/Submitted|Posted/);

  await chips(page).first().getByRole("button").nth(1).click();
  await expect(chips(page)).toHaveCount(1);

  await page.getByRole("searchbox", { name: "Search" }).first().fill("Gift wrap");
  await expect(cell(page, 0, COL.memo)).toHaveText("Gift wrap");
  await page.getByRole("button", { name: "Clear all" }).click();
  await expect(chips(page)).toHaveCount(0);
  await expect(count(page)).toHaveText("1000 rows");
});

test("relative date filter and reference checklist loaded asynchronously", async ({ page }) => {
  await open(page);
  await filterButton(page, "Date").click();
  const menu = page.locator("[data-grid-menu]");
  await menu.getByLabel("Condition").selectOption("relative");
  await menu.getByLabel("Range").selectOption("thisYear");
  await menu.getByRole("button", { name: "Apply" }).click();
  await expect(chips(page).first().getByRole("button").first()).toHaveText("Date: this year");

  await filterButton(page, "Customer").click();
  await menu.getByRole("searchbox").fill("glob");
  await expect(menu.getByLabel("Globex")).toBeVisible();
  await menu.getByLabel("Globex").check();
  await menu.getByRole("button", { name: "Done" }).click();
  await expect(chips(page).nth(1).getByRole("button").first()).toHaveText("Customer: Globex");
  await expect(cell(page, 0, COL.customer)).toHaveText("Globex");
});

test("focus is kept when a filter leaves no rows, and when rows re-order under it", async ({ page }) => {
  await open(page);
  await cell(page, 3, COL.qty).click();
  await page.keyboard.press("Alt+ArrowDown");
  const menu = page.locator("[data-grid-menu]");
  await menu.getByLabel("Condition").selectOption("gt");
  await menu.getByLabel("Value").fill("1000000");
  await menu.getByLabel("Value").press("Enter");
  await expect(page.getByText("No rows match the filters")).toBeVisible();
  // No cell is left, so the column's header holds focus and the keyboard still works.
  expect(await focused(page)).toBe(`-1,${COL.qty}`);
  await page.keyboard.press("Alt+ArrowDown");
  await menu.getByRole("button", { name: "Clear filter" }).click();
  await menu.getByRole("button", { name: "Apply" }).press("Escape");
  expect(await focused(page)).toBe(`-1,${COL.qty}`);
  await page.keyboard.press("ArrowDown");
  expect(await focused(page)).toBe(`0,${COL.qty}`);

  // Sorting re-orders the rows under the focused cell; it keeps focus.
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Shift+F10");
  await menu.getByRole("menuitem", { name: "Sort descending" }).click();
  expect(await focused(page)).toBe(`1,${COL.qty}`);
  // And an edit that changes the rows keeps it too.
  await page.keyboard.type("1");
  await page.keyboard.press("Enter");
  expect(await focused(page)).toBe(`2,${COL.qty}`);
});

test("columns: resize, auto-fit, hide, pin, reorder; the layout is saved", async ({ page }) => {
  await open(page);
  const header = (id: string) => page.locator(`[role="columnheader"][id="demo-orders-h-${id}"]`);
  const width = async (id: string) => (await header(id).boundingBox())?.width ?? 0;

  // Resize the memo column by dragging its edge.
  const before = await width("memo");
  const handle = header("memo").locator("[data-grid-resize]");
  const box = await handle.boundingBox();
  if (!box) throw new Error("no resize handle");
  await page.mouse.move(box.x + 3, box.y + 10);
  await page.mouse.down();
  await page.mouse.move(box.x + 83, box.y + 10, { steps: 5 });
  await page.mouse.up();
  expect(Math.round((await width("memo")) - before)).toBe(80);

  // Double-click fits it to its content.
  await handle.dblclick();
  expect(await width("memo")).toBeLessThan(before + 80);

  // Hide from the column menu.
  await menuButton(page, "Urgent").click();
  await page.locator("[data-grid-menu]").getByRole("menuitem", { name: "Hide column" }).click();
  await expect(header("urgent")).toHaveCount(0);

  // Pin the amount column right, then reorder status before customer by dragging.
  await menuButton(page, "Amount").click();
  await page.locator("[data-grid-menu]").getByRole("menuitem", { name: "Pin right" }).click();
  await expect(header("amount")).toHaveAttribute("data-pinned", "right");
  // Drag the status header onto the left half of customer's: it moves before it.
  const from = await header("status").boundingBox();
  const to = await header("customer").boundingBox();
  if (!from || !to) throw new Error("no headers");
  await page.mouse.move(from.x + 30, from.y + 10);
  await page.mouse.down();
  await page.mouse.move(to.x + 5, to.y + 10, { steps: 5 });
  await expect(header("customer")).toHaveAttribute("data-drop", "before");
  await page.mouse.up();
  // The click that ends the drag did not sort.
  await expect(header("status")).toHaveAttribute("aria-sort", "none");
  const order = await page.locator('[role="columnheader"]').evaluateAll((els) => els.map((e) => e.id.replace("demo-orders-h-", "")));
  expect(order).toEqual(["number", "status", "customer", "doc_date", "qty", "memo", "amount"]);

  // Saved per grid id: it survives a reload.
  await page.waitForTimeout(400);
  await page.reload();
  await expect(page.locator('[role="gridcell"]').first()).toBeVisible();
  const reloaded = await page.locator('[role="columnheader"]').evaluateAll((els) => els.map((e) => e.id.replace("demo-orders-h-", "")));
  expect(reloaded).toEqual(order);
  await expect(header("amount")).toHaveAttribute("data-pinned", "right");
});

test("100k rows: virtual, fast to the end, footer totals", async ({ page }) => {
  await page.goto("/?rows=100000#client");
  await expect(page.locator('[role="gridcell"]').first()).toBeVisible();
  await expect(page.locator('[role="grid"]')).toHaveAttribute("aria-rowcount", "100002");
  const rendered = await page.locator('[role="row"]').count();
  expect(rendered).toBeLessThan(80);
  await cell(page, 0, 0).click();
  const t0 = Date.now();
  await page.keyboard.press("Control+End");
  await expect(cell(page, 99999, 0)).toHaveText("SO-100000");
  expect(await focused(page)).toBe("99999,7");
  expect(Date.now() - t0).toBeLessThan(1500);
  // Sorting 100k rows stays interactive.
  const t1 = Date.now();
  await page.locator('[role="columnheader"][data-col="5"] button').first().click();
  await expect(page.locator('[role="columnheader"][data-col="5"]')).toHaveAttribute("aria-sort", "ascending");
  expect(Date.now() - t1).toBeLessThan(3000);
  await expect(footer(page).first()).toHaveText("Total");
  await expect(footer(page).and(page.locator("[data-type='money']"))).toContainText(/\d{3},\d{3}\.\d{2}/);
});

test("server mode: the query state and the erpcraft query follow sort, filter and page", async ({ page }) => {
  await page.goto("/#server");
  await expect(page.locator('[role="gridcell"]').first()).toBeVisible();
  await page.locator('[role="columnheader"][data-col="5"] button').first().click();
  await filterButton(page, "Status").click();
  const menu = page.locator("[data-grid-menu]");
  await menu.getByLabel("Draft").uncheck();
  await menu.getByLabel("Submitted").uncheck();
  await menu.getByLabel("Cancelled").uncheck();
  await menu.getByRole("button", { name: "Done" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  const erpcraft = JSON.parse((await page.getByTestId("erpcraft").textContent()) ?? "null");
  expect(erpcraft.command).toBe("document.list");
  expect(erpcraft.input.sort).toEqual(["summary.total nulls last"]);
  // Unticking from "everything" keeps blanks, as Excel's checklist does; the values go as params.
  expect(erpcraft.input.where).toBe("(record.status in params.g0 || record.status == null)");
  expect(erpcraft.input.params).toEqual({ g0: ["posted"] });
  expect(typeof erpcraft.input.time_zone).toBe("string");
  expect(erpcraft.input.offset).toBe(50);
  await expect(pager(page)).toContainText("Page 2 of");
  for (let r = 0; r < 3; r += 1) await expect(cell(page, r, 2)).toHaveText("Posted");
  // A filter starts from the first page again.
  await page.getByRole("searchbox", { name: "Search" }).first().fill("Rush");
  await expect(pager(page)).toContainText("Page 1 of");
});

test("line editing: ArrowDown on the last row adds one; Ctrl+- deletes; Ctrl++ inserts", async ({ page }) => {
  await page.goto("/#lines");
  await expect(page.locator('[role="gridcell"]').first()).toBeVisible();
  await expect(page.getByTestId("line-count")).toHaveText("3 lines");
  await cell(page, 2, 1).click();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByTestId("line-count")).toHaveText("4 lines");
  expect(await focused(page)).toBe("3,1");
  await page.keyboard.type("New line");
  await page.keyboard.press("Tab");
  await page.keyboard.type("3");
  await page.keyboard.press("Tab");
  await page.keyboard.type("2.5");
  await page.keyboard.press("Enter");
  await expect(cell(page, 3, 4)).toContainText("7.50");
  await expect(page.getByTestId("line-count")).toHaveText("5 lines");
  // Enter after the Tabs returned to the first column of the run.
  expect(await focused(page)).toBe("4,1");
  await page.keyboard.press("Control+-");
  await expect(page.getByTestId("line-count")).toHaveText("4 lines");
  await cell(page, 0, 1).click();
  await page.keyboard.press("Control+Shift+Equal");
  await expect(page.getByTestId("line-count")).toHaveText("5 lines");
  await expect(cell(page, 1, 1)).toHaveText("Bolts for frame");
  // Totals follow the lines.
  await expect(footer(page).and(page.locator("[data-type='money']")).last()).toContainText("36.50");
});

test("Ctrl+X cuts: copies, then clears the editable cells", async ({ page }) => {
  await open(page);
  await cell(page, 0, COL.qty).click();
  const qty = (await cell(page, 0, COL.qty).textContent()) ?? "";
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Control+x");
  const text = await page.evaluate(() => navigator.clipboard.readText());
  expect(text.split("\t")[0]).toBe(qty);
  await expect(cell(page, 0, COL.qty)).toHaveText("");
  await expect(cell(page, 0, COL.amount)).toHaveText("");
  await page.keyboard.press("Control+z");
  await expect(cell(page, 0, COL.qty)).toHaveText(qty);
});

test("leaving the grid while editing commits the edit", async ({ page }) => {
  await open(page);
  await cell(page, 0, COL.memo).click();
  await page.keyboard.type("Typed then left");
  await page.getByTestId("before-grid").click();
  await expect(cell(page, 0, COL.memo)).toHaveText("Typed then left");
});

test("the column menu (Shift+F10) and its Filter… item; focus stays in the grid", async ({ page }) => {
  await open(page);
  await cell(page, 2, COL.memo).click();
  await page.keyboard.press("Shift+F10");
  const menu = page.locator("[data-grid-menu]");
  await expect(menu.getByRole("menu")).toBeVisible();
  await menu.getByRole("menuitem", { name: "Filter…" }).click();
  await expect(menu.getByLabel("Condition")).toBeVisible();
  await menu.getByLabel("Condition").selectOption("notEmpty");
  await menu.getByRole("button", { name: "Apply" }).click();
  await expect(menu).toHaveCount(0);
  await expect(chips(page).first()).toContainText("Memo is not empty");
  // Opened by pointer from the menu, the header of the column keeps focus.
  expect(await focused(page)).toBe(`-1,${COL.memo}`);
  // A chip opens the filter popover again; Escape closes it.
  await chips(page).first().getByRole("button").first().click();
  await expect(menu.getByLabel("Condition")).toHaveValue("notEmpty");
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  expect(await focused(page)).toBe(`-1,${COL.memo}`);
});

test("wide grids render the columns in view; the keyboard reaches every one", async ({ page }) => {
  // Many keys in a row; a loaded machine needs more than the default.
  test.setTimeout(90_000);
  await page.goto("/#wide");
  await expect(page.locator('[role="gridcell"]').first()).toBeVisible();
  await expect(page.locator('[role="grid"]')).toHaveAttribute("aria-colcount", "60");
  // Far fewer than 60 cells a row are in the page.
  const rendered = await page.locator('[role="row"][aria-rowindex="2"] [role="gridcell"]').count();
  expect(rendered).toBeLessThan(25);
  await cell(page, 0, 0).click();
  await page.keyboard.press("End");
  expect(await focused(page)).toBe("0,59");
  await expect(cell(page, 0, 59)).toHaveText("r1c59");
  await expect(cell(page, 0, 59)).toBeInViewport();
  // The pinned key column stays rendered and in view.
  await expect(cell(page, 0, 0)).toBeInViewport();
  await page.keyboard.press("Home");
  expect(await focused(page)).toBe("0,0");
  await expect(cell(page, 0, 1)).toBeInViewport();
  // Arrow keys walk across, one column at a time, past what was rendered.
  for (let i = 0; i < 30; i += 1) await page.keyboard.press("ArrowRight");
  expect(await focused(page)).toBe("0,30");
  await expect(cell(page, 0, 30)).toBeInViewport();
  // Copying reads the data, not the page: every column of the row comes along.
  await page.keyboard.press("Home");
  await page.keyboard.press("Shift+End");
  await page.keyboard.press("Control+c");
  const text = await page.evaluate(() => navigator.clipboard.readText());
  expect(text.split("\t")).toHaveLength(60);
  // Editing a cell out at the right edge.
  await page.keyboard.press("End");
  await page.keyboard.type("edited");
  await page.keyboard.press("Enter");
  await expect(cell(page, 0, 59)).toHaveText("edited");
  expect(await focused(page)).toBe("1,59");
});

test("Ctrl+Enter puts what was typed into every selected cell; Ctrl+Z takes it back", async ({ page }) => {
  await open(page);
  const before = await cell(page, 0, COL.qty).textContent();
  await cell(page, 0, COL.qty).click();
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.type("7");
  await page.keyboard.press("Control+Enter");
  for (let r = 0; r < 4; r += 1) await expect(cell(page, r, COL.qty)).toHaveText("7");
  await expect(cell(page, 4, COL.qty)).not.toHaveText("7");
  await expect(page.locator("[data-grid-live]")).toContainText("Filled 4 cells");
  // The selection stays, and the focus with it.
  expect(await focused(page)).toBe(`0,${COL.qty}`);
  expect(await page.locator('[role="gridcell"][data-row][aria-selected="true"]').count()).toBe(4);
  // A value a column refuses is reported, cell by cell.
  await page.keyboard.type("-1");
  await page.keyboard.press("Control+Enter");
  await expect(page.getByTestId("report")).toContainText("fill: 0 applied, 4 rejected");
  await expect(cell(page, 0, COL.qty)).toHaveText("7");
  // One undo takes back the whole fill.
  await page.keyboard.press("Control+z");
  await expect(cell(page, 0, COL.qty)).toHaveText(before ?? "");
});
