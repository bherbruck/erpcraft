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
  const first = Number(await cell(page, 0, COL.qty).textContent());
  const second = Number(await cell(page, 1, COL.qty).textContent());
  expect(first).toBeLessThanOrEqual(second);
  await header(COL.qty).locator("button").first().click();
  await expect(header(COL.qty)).toHaveAttribute("aria-sort", "descending");

  await header(COL.status).locator("button").first().click({ modifiers: ["Shift"] });
  await expect(header(COL.qty).locator(".rg-sort-order")).toHaveText("1");
  await expect(header(COL.status).locator(".rg-sort-order")).toHaveText("2");

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
  await expect(page.locator(".rg-chip")).toHaveCount(1);
  await expect(page.locator(".rg-chip-label")).toHaveText("Qty ≥ 490");
  const shown = await page.locator(".rg-count").textContent();
  expect(shown).toMatch(/^\d+ of 1000 rows$/);
  for (let r = 0; r < 3; r += 1) expect(Number(await cell(page, r, COL.qty).textContent())).toBeGreaterThanOrEqual(490);

  // A checklist on the state column.
  await page.locator(`[role="columnheader"][data-col="${COL.status}"] .rg-menu-button`).click();
  await menu.getByLabel("Draft").uncheck();
  await menu.getByLabel("Cancelled").uncheck();
  await menu.getByRole("button", { name: "Done" }).click();
  await expect(page.locator(".rg-chip")).toHaveCount(2);
  await expect(page.locator(".rg-chip-label").nth(1)).toContainText("Status: Submitted, Posted");
  for (let r = 0; r < 3; r += 1) await expect(cell(page, r, COL.status)).toHaveText(/Submitted|Posted/);

  await page.locator(".rg-chip-remove").first().click();
  await expect(page.locator(".rg-chip")).toHaveCount(1);

  await page.getByRole("searchbox", { name: "Search" }).first().fill("Gift wrap");
  await expect(cell(page, 0, COL.memo)).toHaveText("Gift wrap");
  await page.getByRole("button", { name: "Clear all" }).click();
  await expect(page.locator(".rg-chip")).toHaveCount(0);
  await expect(page.locator(".rg-count")).toHaveText("1000 rows");
});

test("relative date filter and reference checklist loaded asynchronously", async ({ page }) => {
  await open(page);
  await page.locator(`[role="columnheader"][data-col="${COL.date}"] .rg-menu-button`).click();
  const menu = page.locator("[data-grid-menu]");
  await menu.getByLabel("Condition").first().selectOption("relative");
  await menu.locator("select").nth(1).selectOption("thisYear");
  await menu.getByRole("button", { name: "Apply" }).click();
  await expect(page.locator(".rg-chip-label")).toHaveText("Date: this year");

  await page.locator(`[role="columnheader"][data-col="${COL.customer}"] .rg-menu-button`).click();
  await menu.getByRole("searchbox").fill("glob");
  await expect(menu.getByLabel("Globex")).toBeVisible();
  await menu.getByLabel("Globex").check();
  await menu.getByRole("button", { name: "Done" }).click();
  await expect(page.locator(".rg-chip-label").nth(1)).toHaveText("Customer: Globex");
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
  await page.keyboard.press("Escape");
  expect(await focused(page)).toBe(`-1,${COL.qty}`);
  await page.keyboard.press("ArrowDown");
  expect(await focused(page)).toBe(`0,${COL.qty}`);

  // Sorting re-orders the rows under the focused cell; it keeps focus.
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Alt+ArrowDown");
  await menu.getByRole("button", { name: "Sort descending" }).click();
  await page.keyboard.press("Escape");
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
  await header("urgent").locator(".rg-menu-button").click();
  await page.locator("[data-grid-menu]").getByRole("button", { name: "Hide column" }).click();
  await expect(header("urgent")).toHaveCount(0);

  // Pin the amount column right, then reorder status before customer by dragging.
  await header("amount").locator(".rg-menu-button").click();
  await page.locator("[data-grid-menu]").getByRole("button", { name: "Pin right" }).click();
  await page.keyboard.press("Escape");
  await expect(header("amount")).toHaveAttribute("data-pinned", "right");
  await header("status").dragTo(header("customer"), { targetPosition: { x: 5, y: 10 } });
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
  await expect(page.locator(".rg-tf").first()).toHaveText("Total");
  await expect(page.locator(".rg-tf[data-type='money']")).toContainText(/\d{3},\d{3}\.\d{2}/);
});

test("server mode: the query state and the erpcraft query follow sort, filter and page", async ({ page }) => {
  await page.goto("/#server");
  await expect(page.locator('[role="gridcell"]').first()).toBeVisible();
  await page.locator('[role="columnheader"][data-col="5"] button').first().click();
  await page.locator('[role="columnheader"][data-col="2"] .rg-menu-button').click();
  const menu = page.locator("[data-grid-menu]");
  await menu.getByLabel("Draft").uncheck();
  await menu.getByLabel("Submitted").uncheck();
  await menu.getByLabel("Cancelled").uncheck();
  await menu.getByRole("button", { name: "Done" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  const erpcraft = JSON.parse((await page.getByTestId("erpcraft").textContent()) ?? "null");
  expect(erpcraft.command).toBe("document.list");
  expect(erpcraft.input.sort).toEqual(["summary.total"]);
  // Unticking from "everything" keeps blanks, as Excel's checklist does.
  expect(erpcraft.input.where).toBe("(record.status in ['posted'] || record.status == null)");
  expect(erpcraft.input.offset).toBe(50);
  await expect(page.locator(".rg-pager")).toContainText("Page 2 of");
  for (let r = 0; r < 3; r += 1) await expect(cell(page, r, 2)).toHaveText("Posted");
  // A filter starts from the first page again.
  await page.getByRole("searchbox", { name: "Search" }).first().fill("Rush");
  await expect(page.locator(".rg-pager")).toContainText("Page 1 of");
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
  await expect(page.locator(".rg-tf[data-type='money']").last()).toContainText("36.50");
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
