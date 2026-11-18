// The UI exercised end to end in a headless Chrome, against the real server.
//
//   bun run dev      (in another terminal; it proxies to the API server)
//   bun run seed     (once: the checks use the company OHIO and its records)
//   bun run e2e
//
// The credentials come from ../.env at run time. Everything the run creates it
// removes again, through the UI where that is part of the check and through the
// API otherwise. It exits with 1 if a check fails.

import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { type Browser, CTRL, launchBrowser } from "./browser";
import {
  asArray,
  asObject,
  asString,
  type JsonObject,
  RESTRICTED,
  type Session,
  signIn,
  signInRestricted,
} from "./lib";

const APP = process.env.ERPCRAFT_UI_URL ?? "http://127.0.0.1:5173";
const FAILURES = resolve(import.meta.dir, "../e2e-failures");

const session: Session = await signIn();
const browser: Browser = await launchBrowser(9334);
const stamp = Date.now().toString(36).toUpperCase();
let passed = 0;
let failed = 0;

function check(name: string, ok: boolean, detail = ""): void {
  if (ok) passed += 1;
  else failed += 1;
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${ok || !detail ? "" : `\n      ${detail.slice(0, 500)}`}`);
  if (!ok) {
    // What the page was like when the check failed: where, what was open, what had focus, what was kept.
    const at = new Date().toISOString();
    void browser
      .evaluate(
        `JSON.stringify({ href: location.href, dialogs: [...document.querySelectorAll("[role=dialog]")].map((d) => (d.textContent ?? "").slice(0, 80)), focus: document.activeElement ? document.activeElement.tagName + "#" + document.activeElement.id : null, local: Object.keys(localStorage), session: Object.keys(sessionStorage), title: document.querySelector("h1")?.textContent })`,
      )
      .then((state) => console.log(`      state at ${at}: ${String(state)}`));
  }
}

/** Opens a page of the app, signed in. */
async function open(path: string): Promise<void> {
  await browser.goto(`${APP}${path}#token=${session.token}`);
}

async function value(selector: string): Promise<unknown> {
  return browser.evaluate(`document.querySelector(${JSON.stringify(selector)})?.value`);
}

async function textOf(selector: string): Promise<string> {
  return String(await browser.evaluate(`document.querySelector(${JSON.stringify(selector)})?.textContent ?? ""`));
}

/** Clicks a button inside the open dialog. */
async function dialogClick(text: string): Promise<void> {
  await browser.click(text, "[role=dialog] button");
}

/** Types into a closed lookup, which opens it, and takes the first match. */
async function pickInLookup(selector: string, search: string): Promise<void> {
  await browser.focus(selector);
  await browser.key(search.charAt(0));
  await browser.insert(search.slice(1));
  await Bun.sleep(500);
  await browser.settle();
  await browser.key("Enter");
  await browser.settle();
}

async function first(entity: string, filters: Record<string, string>): Promise<JsonObject | undefined> {
  const page = asObject(await session.run("entity.list", { entity, filters, limit: 1 }));
  const item = asArray(page.items)[0];
  return item ? asObject(item) : undefined;
}

const cleanup: (() => Promise<unknown>)[] = [];

async function signInChecks(): Promise<void> {
  await browser.goto(`${APP}/sign-in`);
  await browser.evaluate("sessionStorage.clear()");
  await browser.goto(`${APP}/c/OHIO/e/item?q=wid`);
  check("a page opened without a session leads to sign-in", (await browser.location()).startsWith("/sign-in"));
  await browser.fill("#email", "nobody@example.com");
  await browser.fill("#password", "wrong");
  await browser.click("Sign in");
  check("a wrong password is refused in words", (await browser.text()).includes("Could not sign in"));
  check("the email is kept after a failed sign-in", (await value("#email")) === "nobody@example.com");

  // Single sign-on: a refusal comes back as a word, and a login code that is no good leads back here.
  await browser.goto(`${APP}/sign-in?sso_error=domain_not_allowed`);
  check(
    "a refused single sign-on is explained in one sentence",
    (await browser.text()).includes("not of a domain that may sign in"),
  );
  await browser.goto(`${APP}/c/OHIO/e/item?q=wid&sso_code=not-a-real-code`);
  const after = await browser.location();
  check(
    "a login code that is no good leads to the sign-in page, and leaves the address",
    after === "/sign-in?sso_error=failed" && (await browser.text()).includes("did not work"),
    after,
  );
}

async function recordChecks(): Promise<void> {
  await open("/c/OHIO/e/vendor/new");
  await browser.click("Create");
  check("a missing required field is reported next to the field", (await textOf("#field-code-error")).includes("code"));
  await browser.fill("#field-code", `T${stamp}`);
  await browser.fill("#field-name", "E2E vendor");
  await browser.click("Create");
  const path = await browser.location();
  check("creating a record opens it", /\/e\/vendor\/[0-9a-f-]{36}$/.test(path), path);
  const id = path.split("/").pop() ?? "";
  cleanup.push(() => session.run("entity.delete", { entity: "vendor", id }));

  await browser.fill("#field-name", "E2E vendor (edited)");
  check("an edited form says it has unsaved changes", (await browser.text()).includes("Unsaved changes"));
  await browser.key("s", CTRL);
  await browser.settle();
  const saved = asObject(await session.run("entity.get", { entity: "vendor", id }));
  check("Ctrl+S saves", saved.name === "E2E vendor (edited)" && (await browser.text()).includes("Saved"));

  // Someone else changes the record behind the form's back.
  await session.run("entity.update", {
    entity: "vendor",
    id,
    data: { name: "Changed by someone else" },
    version: asString(saved._version),
  });
  await browser.fill("#field-code", `U${stamp}`);
  await browser.click("Save");
  const conflict = await browser.text();
  check("a stale save is refused and announced", conflict.includes("Someone else changed this"));
  check("the conflict says what the other person changed", conflict.includes("is now Changed by someone else"));
  await browser.click("Keep my changes");
  check(
    "keeping my changes takes their fields and keeps mine",
    (await value("#field-name")) === "Changed by someone else" && (await value("#field-code")) === `U${stamp}`,
  );
  await browser.click("Save");
  const merged = asObject(await session.run("entity.get", { entity: "vendor", id }));
  check("the save after a conflict succeeds", merged.code === `U${stamp}` && merged.name === "Changed by someone else");

  await browser.fill("#field-name", "typed but not saved");
  await browser.click("Items", "nav a");
  check("leaving with unsaved changes asks first", (await browser.text()).includes("Leave without saving?"));
  await browser.click("Stay and keep editing");
  check(
    "staying keeps what was typed",
    (await value("#field-name")) === "typed but not saved" && (await browser.location()) === path,
  );
  await browser.click("Items", "nav a");
  await browser.click("Leave and discard");
  check("leaving goes where the user wanted", (await browser.location()) === "/c/OHIO/e/item");

  await open(path);
  await browser.click("Delete");
  await dialogClick("Delete");
  check("deleting returns to the list", (await browser.location()) === "/c/OHIO/e/vendor");
  cleanup.pop();
}

async function listChecks(): Promise<void> {
  await open("/c/OHIO/e/item");
  await browser.fill("input[type=search]", "wid");
  await Bun.sleep(500);
  await browser.settle();
  check("the search text is in the URL", (await browser.location()).includes("q=wid"));
  const found = await textOf("tbody");
  check("the search runs and filters the rows", found.includes("WIDGET") && !found.includes("GADGET"), found);
  await browser.click("Name", "th button");
  check("the sort is in the URL", (await browser.location()).includes("sort=name"));
  await browser.evaluate("history.back()");
  await browser.settle();
  check("Back undoes the sort and keeps the search", (await browser.location()) === "/c/OHIO/e/item?q=wid");

  check("the Created column is hidden by default", !(await textOf("thead")).includes("Created"));
  await browser.click("Columns");
  await browser.evaluate(
    `[...document.querySelectorAll("label")].find((l) => l.textContent.trim() === "Created").querySelector("input").click()`,
  );
  await browser.settle();
  check("the column chooser adds a column", (await textOf("thead")).includes("Created"));
  await open("/c/OHIO/e/item");
  check("the chosen columns are remembered", (await textOf("thead")).includes("Created"));
  await browser.click("Columns");
  await browser.click("Back to the default columns");
  check("the default columns can be restored", !(await textOf("thead")).includes("Created"));
  await browser.key("Escape");

  check("modules have readable names in the menu", (await textOf("nav")).includes("General ledger"));
}

async function documentChecks(): Promise<void> {
  await open("/c/OHIO/d/purchase_order/new");
  await browser.fill("#doc-memo", `E2E ${stamp}`);

  // A vendor that does not exist yet is created from inside the lookup.
  await browser.focus("#header-vendor_id");
  await browser.key("e");
  await browser.insert(`2e supplier ${stamp}`);
  await Bun.sleep(500);
  await browser.settle();
  check(
    "every typed letter reaches the lookup's search",
    (await value("[role=combobox]:not(button)")) === `e2e supplier ${stamp}`,
  );
  await browser.click(`New Vendor “e2e supplier ${stamp}”…`, "[role=option]");
  check(
    "New … opens a dialog with the typed name",
    (await value("#quick-create-vendor-name")) === `e2e supplier ${stamp}`,
  );
  await browser.fill("#quick-create-vendor-code", `Q${stamp}`);
  await dialogClick("Create and select");
  const quick = await first("vendor", { code: `Q${stamp}` });
  if (quick) cleanup.push(() => session.run("entity.delete", { entity: "vendor", id: asString(quick.id) }));
  check("the new record is created and selected", (await textOf("#header-vendor_id")).includes(`Q${stamp}`));
  check("the form underneath kept what was typed", (await value("#doc-memo")) === `E2E ${stamp}`);

  await pickInLookup("#header-vendor_id", "parts");
  check("a lookup is filled by typing", (await textOf("#header-vendor_id")).includes("Parts Inc"));
  await browser.focus("#header-vendor_id");
  await browser.key("ArrowDown");
  await Bun.sleep(400);
  await browser.settle();
  const offered = await textOf("[role=listbox]");
  check(
    "recent choices come first in a lookup",
    offered.startsWith("Recently chosen") && offered.includes("Parts Inc"),
    offered,
  );
  await browser.key("Escape");

  const cell = (label: string, line: number): string => `[aria-label="${label}, line ${line}"]`;
  const lines: [string, string, string][] = [
    ["widget", "3", "4.25"],
    ["gadget", "2", "9.5"],
  ];
  for (const [index, [item, qty, price]] of lines.entries()) {
    const line = index + 1;
    await pickInLookup(cell("Item", line), item);
    await pickInLookup(cell("Deliver to", line), "main");
    await browser.fill(cell("Quantity", line), qty);
    await browser.fill(cell("Unit price", line), price);
    if (line === 1) {
      await browser.key("Enter");
      await browser.settle();
      const focused = await browser.evaluate("document.activeElement?.getAttribute('aria-label')");
      check("Enter on the last line adds a line and moves to it", focused === "Item, line 2", String(focused));
    }
  }
  await browser.key("s", CTRL);
  await browser.settle();
  const path = await browser.location();
  check("Ctrl+S creates the document", /\/d\/purchase_order\/[0-9a-f-]{36}$/.test(path), path);
  const id = path.split("/").pop() ?? "";
  cleanup.push(() => session.run("document.delete", { doc_type: "purchase_order", id }).catch(() => null));
  const created = asObject(await session.run("document.get", { doc_type: "purchase_order", id }));
  const createdLines = asArray(asObject(created.body).lines).map(asObject);
  check(
    "the server has the lines as typed, as decimal strings",
    createdLines.length === 2 && createdLines[0]?.qty === "3.000000" && createdLines[1]?.unit_price === "9.500000",
    JSON.stringify(createdLines),
  );

  // The server names the line and the field of a line it refuses.
  await browser.fill(cell("Quantity", 2), "-1");
  await browser.click("Save");
  const lineError = await textOf("#line-lines-2-qty-error");
  check("a line the server refuses is marked on the line, by the field", lineError.length > 0, await browser.text());
  check(
    "a refused save keeps what was typed",
    (await value(cell("Quantity", 2))) === "-1" && (await browser.text()).includes("Not saved"),
  );
  await browser.fill(cell("Quantity", 2), "7");
  await browser.click("Save");
  const updated = asObject(await session.run("document.get", { doc_type: "purchase_order", id }));
  check(
    "a draft is changed and its version moves on",
    updated.version === 2 && asObject(asArray(asObject(updated.body).lines)[1] ?? {}).qty === "7.000000",
  );

  await browser.click("Approve");
  check("an action is part of the URL", (await browser.location()).includes("action=approve"));
  const preview = await textOf("[role=dialog]");
  check(
    "the action is previewed before it runs",
    preview.includes("What will happen") && preview.includes("Open"),
    preview,
  );
  await dialogClick("Cancel");
  const still = asObject(await session.run("document.get", { doc_type: "purchase_order", id }));
  check(
    "closing the preview changes nothing",
    still.status === "draft" && !(await browser.location()).includes("action="),
  );

  await browser.click("Delete");
  await dialogClick("Delete");
  check("a draft can be deleted", (await browser.location()) === "/c/OHIO/d/purchase_order");
  cleanup.pop();

  const posted = asObject(
    await session.run("document.list", { doc_type: "vendor_invoice", status: "posted", limit: 1 }),
  );
  const invoice = asArray(posted.items)[0];
  if (invoice) {
    await open(`/c/OHIO/d/vendor_invoice/${asString(asObject(invoice).id)}`);
    const page = await browser.text();
    check(
      "a posted document shows its ledger entries and where it came from",
      page.includes("Postings") && page.includes("Accounts payable") && page.includes("Created from"),
    );
    check(
      "a posted document offers Reverse and not Post",
      page.includes("Reverse") && !(await textOf("h1 ~ div")).includes("Post"),
    );
  }
}

async function actionChecks(): Promise<void> {
  const company = await first("company", { code: "OHIO" });
  if (!company) throw new Error("no company OHIO; run `bun run seed` first");
  const period = asObject(
    await session.run("entity.create", {
      entity: "fiscal_period",
      data: {
        company_id: asString(company.id),
        name: `E2E ${stamp}`,
        start_date: "2099-01-01",
        end_date: "2099-12-31",
      },
    }),
  );
  const id = asString(period.id);
  cleanup.push(async () => {
    await session.run("fiscal_period.reopen", { id }).catch(() => null);
    return session.run("entity.delete", { entity: "fiscal_period", id });
  });
  await open(`/c/OHIO/e/fiscal_period/${id}`);
  const offered = await textOf("h1 ~ div");
  check(
    "only the actions the state allows are offered",
    offered.includes("Close") && !offered.includes("Reopen"),
    offered,
  );
  await browser.click("Close");
  check("the preview shows the change of status", (await textOf("[role=dialog]")).includes("Closed"));
  await dialogClick("Close");
  await browser.settle();
  const closed = asObject(await session.run("entity.get", { entity: "fiscal_period", id }));
  check("a confirmed action runs on the server", closed.status === "closed", String(closed.status));
  check(
    "the page shows the result and the action left the URL",
    (await textOf("h1 ~ div")).includes("Reopen") && !(await browser.location()).includes("action="),
  );
  await browser.click("Reopen");
  await dialogClick("Reopen");
  await browser.settle();
  await browser.click("History", "[role=tab]");
  const history = await textOf("tbody");
  check(
    "the history shows who ran which command",
    history.includes("fiscal_period.close") && history.includes("fiscal_period.reopen"),
    history,
  );
}

/** Purchase to pay: receive and invoice an order through the drafts it offers, then pay from the open items. */
async function flowChecks(): Promise<void> {
  const company = await first("company", { code: "OHIO" });
  if (!company) throw new Error("no company OHIO; run `bun run seed` first");
  const companyId = asString(company.id);
  const vendor = await first("vendor", { code: "V001" });
  const item = await first("item", { code: "WIDGET" });
  const warehouse = await first("warehouse", { company_id: companyId, code: "MAIN" });
  if (!vendor || !item || !warehouse) throw new Error("the seed data is missing; run `bun run seed` first");
  const order = asObject(
    await session.run("document.create", {
      doc_type: "purchase_order",
      company_id: companyId,
      doc_date: "2026-05-04",
      memo: `E2E flow ${stamp}`,
      body: {
        vendor_id: asString(vendor.id),
        lines: [{ item_id: asString(item.id), warehouse_id: asString(warehouse.id), qty: "5", unit_price: "4.1" }],
      },
    }),
  );
  const orderId = asString(order.id);
  await session.run("purchase_order.approve", { id: orderId });
  const made: [string, string][] = [];
  cleanup.push(async () => {
    for (const [docType, id] of made.reverse()) {
      const doc = asObject(await session.run("document.get", { doc_type: docType, id }));
      if (doc.status === "posted") await session.run(`${docType}.reverse`, { id }).catch(() => null);
      else await session.run("document.delete", { doc_type: docType, id }).catch(() => null);
    }
    return session.run("purchase_order.cancel", { id: orderId }).catch(() => null);
  });

  // Receive: the order drafts the receipt, which opens.
  await open(`/c/OHIO/d/purchase_order/${orderId}`);
  await browser.click("Receive");
  const receiptPath = await browser.location();
  check(
    "Receive drafts a receipt and opens it",
    /\/d\/purchase_receipt\/[0-9a-f-]{36}$/.test(receiptPath),
    receiptPath,
  );
  const receiptId = receiptPath.split("/").pop() ?? "";
  made.push(["purchase_receipt", receiptId]);
  check("the draft receipt names its order", (await browser.text()).includes("PO-2026"));
  await browser.click("Post");
  const receiptPreview = await textOf("[role=dialog]");
  check(
    "the preview shows the ledger entries the receipt will make",
    receiptPreview.includes("General ledger") &&
      receiptPreview.includes("Stock") &&
      receiptPreview.includes("Inventory") &&
      receiptPreview.includes("5 ea"),
    receiptPreview,
  );
  await dialogClick("Post");
  const receipt = asObject(await session.run("document.get", { doc_type: "purchase_receipt", id: receiptId }));
  check("the receipt is posted after confirming", receipt.status === "posted", String(receipt.status));
  check("the posted receipt shows its entries", (await textOf("main")).includes("Postings"));

  // Invoice: the same order drafts the vendor's invoice.
  await open(`/c/OHIO/d/purchase_order/${orderId}`);
  await browser.click("Invoice");
  const invoicePath = await browser.location();
  check(
    "Invoice drafts a vendor invoice and opens it",
    /\/d\/vendor_invoice\/[0-9a-f-]{36}$/.test(invoicePath),
    invoicePath,
  );
  const invoiceId = invoicePath.split("/").pop() ?? "";
  made.push(["vendor_invoice", invoiceId]);
  await browser.click("Post");
  const invoicePreview = await textOf("[role=dialog]");
  check(
    "the invoice preview shows what becomes owed",
    invoicePreview.includes("Open items") && invoicePreview.includes("Becomes payable"),
    invoicePreview,
  );
  await dialogClick("Post");
  const invoice = asObject(await session.run("document.get", { doc_type: "vendor_invoice", id: invoiceId }));
  check("the invoice is posted", invoice.status === "posted", String(invoice.status));

  // Pay: from the open items, the invoice's item, from the bank account.
  await open(`/c/OHIO/r/open_item.list?kind=payable&party=${asString(vendor.id)}`);
  const number = asString(invoice.number);
  const ticked = await browser.evaluate(
    `(() => { const box = document.querySelector('input[aria-label="Choose ${number} to pay"]'); if (!box) return false; box.click(); return true; })()`,
  );
  check("an open item can be chosen to pay", ticked === true, await textOf("tbody"));
  await browser.settle();
  await pickInLookup("#pay-bank", "1000");
  await browser.click("Pay");
  const paymentPath = await browser.location();
  check("Pay drafts the payment and opens it", /\/d\/payment\/[0-9a-f-]{36}$/.test(paymentPath), paymentPath);
  const paymentId = paymentPath.split("/").pop() ?? "";
  made.push(["payment", paymentId]);
  check("the draft payment settles the invoice", (await textOf("main")).includes(number), await textOf("main"));
  await browser.click("Post");
  const paymentPreview = await textOf("[role=dialog]");
  check("the payment preview shows the settlement", paymentPreview.includes("Settles Payable"), paymentPreview);
  await dialogClick("Post");
  const payment = asObject(await session.run("document.get", { doc_type: "payment", id: paymentId }));
  check("the payment is posted", payment.status === "posted", String(payment.status));
}

async function accountId(companyId: string, code: string): Promise<string> {
  const account = await first("account", { company_id: companyId, code });
  if (!account) throw new Error(`no account ${code}; run \`bun run seed\` first`);
  return asString(account.id);
}

async function draftJournal(companyId: string, memo: string, credit: string): Promise<string> {
  const doc = asObject(
    await session.run("document.create", {
      doc_type: "journal_entry",
      company_id: companyId,
      doc_date: "2026-06-01",
      memo,
      body: {
        lines: [
          { account_id: await accountId(companyId, "6000"), debit: "10", credit: "0" },
          { account_id: await accountId(companyId, "1000"), debit: "0", credit },
        ],
      },
    }),
  );
  const id = asString(doc.id);
  cleanup.push(async () => {
    const now = asObject(await session.run("document.get", { doc_type: "journal_entry", id }));
    if (now.status === "posted") return session.run("journal_entry.reverse", { id });
    return session.run("document.delete", { doc_type: "journal_entry", id });
  });
  return id;
}

/** Print view, palette actions, bulk actions and saved views. */
async function moreChecks(): Promise<void> {
  const company = await first("company", { code: "OHIO" });
  if (!company) throw new Error("no company OHIO; run `bun run seed` first");
  const companyId = asString(company.id);

  // Print view: the page in a frame that can run nothing, and a window to print from.
  const posted = asObject(
    await session.run("document.list", {
      doc_type: "vendor_invoice",
      company_id: companyId,
      status: "posted",
      limit: 1,
    }),
  );
  const invoice = asObject(asArray(posted.items)[0] ?? {});
  await open(`/c/OHIO/d/vendor_invoice/${asString(invoice.id)}`);
  await browser.click("Print", "a");
  check("a document opens its print view", (await browser.location()).endsWith("/print"), await browser.location());
  const frame = asObject(
    (await browser.evaluate(
      "(() => { const f = document.querySelector('iframe'); return f ? { sandbox: f.getAttribute('sandbox'), html: f.getAttribute('srcdoc') ?? '' } : {}; })()",
    )) as JsonObject,
  );
  check(
    "the print view shows the page in a sandbox without scripts",
    frame.sandbox === "" && typeof frame.html === "string" && frame.html.includes(asString(invoice.number)),
    JSON.stringify(frame.sandbox),
  );
  const before = asArray((await browser.send("Target.getTargets")).targetInfos).length;
  await browser.click("Print");
  await Bun.sleep(800);
  const targets = asArray((await browser.send("Target.getTargets")).targetInfos).map(asObject);
  const printWindow = targets.find((target) => asString(target.url).startsWith("blob:"));
  check("Print opens the page in a window of its own to print", targets.length > before && printWindow !== undefined);
  if (printWindow) await browser.send("Target.closeTarget", { targetId: asString(printWindow.targetId) });

  // The palette runs the actions of the document on screen, through the same preview.
  const draft = await draftJournal(companyId, `E2E palette ${stamp}`, "10");
  await open(`/c/OHIO/d/journal_entry/${draft}`);
  await browser.key("k", CTRL);
  await browser.insert("post");
  await browser.settle();
  check(
    "the palette offers the document's actions",
    (await textOf("#palette-list")).includes("Actions on Journal entry"),
  );
  await browser.key("Enter");
  await browser.settle();
  check(
    "an action chosen in the palette opens its preview",
    (await browser.location()).includes("action=post") && (await textOf("[role=dialog]")).includes("General ledger"),
  );
  await dialogClick("Cancel");

  // Bulk: one action on several drafts, the result per document.
  const tag = `E2E bulk ${stamp}`;
  await draftJournal(companyId, `${tag} a`, "10");
  await draftJournal(companyId, `${tag} b`, "9");
  await open(`/c/OHIO/d/journal_entry?q=${encodeURIComponent(tag)}`);
  await browser.evaluate("document.querySelectorAll('tbody input[type=checkbox]').forEach((box) => box.click())");
  await browser.settle();
  await browser.click("Post");
  check(
    "a bulk action says what it will run on how many",
    (await textOf("[role=dialog]")).includes("Will run Post on 2"),
  );
  await dialogClick("Post 2");
  await Bun.sleep(800);
  await browser.settle();
  const report = await textOf("main");
  check(
    "a bulk action reports per document what was done and what was refused, and why",
    report.includes("1 done, 1 refused") && report.includes("do not balance"),
    report.slice(0, 400),
  );

  // Saved views: the list's state under a name, applied again later.
  await open("/c/OHIO/d/journal_entry?status=posted&sort=-number");
  await browser.click("Views");
  await browser.fill('input[aria-label="Name of the view"]', `Posted ${stamp}`);
  await browser.click("Save", "[data-radix-popper-content-wrapper] button");
  await open("/c/OHIO/d/journal_entry");
  await browser.click("Views");
  await browser.click(`Posted ${stamp}`, "[data-radix-popper-content-wrapper] button");
  check("a saved view brings back its filters and sort", (await browser.location()).includes("status=posted"));
  await browser.key("Escape");
}

/** A user whose role withholds fields sees what it may, and changes only what it may. */
async function accessChecks(): Promise<void> {
  const restricted = await signInRestricted(session, RESTRICTED_PERMISSIONS);
  const item = await first("item", { code: "WIDGET" });
  if (!item) throw new Error("no item WIDGET; run `bun run seed` first");
  await browser.goto(`${APP}/c/OHIO/e/item#token=${restricted.token}`);
  check("a field the user may not read is not a column", !(await textOf("thead")).includes("Unit of measure"));
  await browser.goto(`${APP}/c/OHIO/e/item/${asString(item.id)}#token=${restricted.token}`);
  const page = await browser.text();
  check(
    "a field the user may not read is not on the form, and one they may not change is read-only",
    !page.includes("Unit of measure") &&
      (await browser.evaluate("!!document.querySelector('#field-name')")) === false &&
      (await browser.evaluate("!!document.querySelector('#field-code')")) === true,
    `uom shown: ${page.includes("Unit of measure")}; page: ${page.slice(0, 300)}`,
  );
}

/** Users: edited through their own command, and roles granted and taken away. */
async function userChecks(): Promise<void> {
  const users = asObject(await session.run("entity.list", { entity: "user", filters: { email: RESTRICTED.email } }));
  const user = asObject(asArray(users.items)[0] ?? {});
  if (typeof user.id !== "string")
    throw new Error("the restricted test user does not exist yet; run the field access group");
  await open(`/c/OHIO/e/user/${user.id}`);
  await browser.click("Edit user");
  check(
    "the edit dialog starts from what the user holds",
    (await value("#action-name")) === RESTRICTED.name,
    String(await value("#action-name")),
  );
  await dialogClick("Cancel");
  await pickInLookup("#grant-role", "warehouse");
  await pickInLookup("#grant-company", "ohio");
  await browser.click("Assign role");
  check("a role is granted for a company", (await textOf("main")).includes("Warehouse clerk"));
  await browser.evaluate(
    `[...document.querySelectorAll("tbody tr")].find((row) => row.textContent.includes("Warehouse clerk"))?.querySelector("button")?.click()`,
  );
  await browser.settle();
  await dialogClick("Revoke role");
  check("a role is taken away again", !(await textOf("main")).includes("Warehouse clerk"));

  // A user is created through its own command; users cannot be deleted, so the check
  // stops at the server's answer for an address that is taken.
  await open("/c/OHIO/e/user");
  await browser.click("New User", "a");
  check(
    "New user opens the create command's form, with the password masked",
    (await browser.location()) === "/c/OHIO/e/user/new" &&
      (await browser.evaluate("document.querySelector('#field-password')?.type")) === "password",
  );
  await browser.fill("#field-email", RESTRICTED.email);
  await browser.fill("#field-name", "Somebody else");
  await browser.click("Create");
  check(
    "a refused new user keeps the form and says why",
    (await browser.location()) === "/c/OHIO/e/user/new" &&
      (await textOf("main")).length > 0 &&
      (await value("#field-name")) === "Somebody else",
  );
}

/** Company setup: the templates, and what a setup would create, before anything is. */
async function setupChecks(): Promise<void> {
  await open("/c/OHIO/e/company");
  await browser.click("Set up a new company", "a");
  check("the companies list leads to setting one up", (await browser.location()) === "/c/OHIO/setup");
  check("the templates are listed with their descriptions", (await textOf("main")).includes("US small business"));
  await browser.evaluate(
    `[...document.querySelectorAll("label")].find((label) => label.textContent.includes("US small business"))?.querySelector("input")?.click()`,
  );
  await browser.fill("#setup-code", `E2E${stamp}`);
  await browser.fill("#setup-name", "E2E preview only");
  await browser.click("Show what will be created");
  check("the preview says what will be created", (await textOf("aside")).includes("Accounts"));
  const made = asObject(await session.run("entity.list", { entity: "company", filters: { code: `E2E${stamp}` } }));
  check("nothing is created by the preview", made.total === 0);
}

/** Single sign-on providers: created, given a secret, changed with a warning, deleted. */
async function ssoChecks(): Promise<void> {
  await open("/c/OHIO/e/sso_provider/new");
  await browser.fill("#sso-name", `E2E provider ${stamp}`);
  await browser.fill("#sso-slug", `e2e-${stamp.toLowerCase()}`);
  await browser.fill("#sso-issuer", "https://login.example.test");
  await browser.fill("#sso-client_id", "erpcraft-e2e");
  await browser.fill("#sso-client_secret", `secret-${stamp}`);
  await browser.fill("#sso-allowed_domains", "example.test");
  await browser.click("Create");
  const path = await browser.location();
  check("a provider is created from its form", /\/e\/sso_provider\/[0-9a-f-]{36}$/.test(path), path);
  const id = path.split("/").pop() ?? "";
  cleanup.push(() => session.run("sso_provider.delete", { id }).catch(() => null));
  const page = await browser.text();
  check(
    "the secret is never shown, only that it is set",
    page.includes("Secret set") && !page.includes(`secret-${stamp}`),
  );
  await browser.fill("#sso-issuer", "https://elsewhere.example.test");
  check("changing the issuer warns before saving", (await browser.text()).includes("switches the provider off"));
  await browser.fill("#sso-issuer", "https://login.example.test");
  await browser.click("Set client secret");
  await browser.fill("#sso-new-secret", `other-${stamp}`);
  await browser.click("Set client secret", "[role=dialog] button");
  check("a new secret is set from its dialog", !(await browser.evaluate("!!document.querySelector('[role=dialog]')")));
  // A user is bound to an identity at the provider, chosen from the users.
  await pickInLookup("#bind-user", "e2e-restricted");
  await browser.fill("#bind-subject", `subject-${stamp}`);
  await browser.click("Bind to identity");
  check("a user chosen from the users is bound to an identity", (await textOf("main")).includes(`subject-${stamp}`));
  await browser.click("Delete");
  await dialogClick("Delete");
  check("a provider is deleted", (await browser.location()) === "/c/OHIO/e/sso_provider");
  cleanup.pop();
}

/** Fill a hand-made sample file from scripts/fixtures and write it where the browser can choose it. */
function fixture(name: string, values: Record<string, string>): string {
  let text = readFileSync(resolve(import.meta.dir, "fixtures", name), "utf8");
  for (const [key, value] of Object.entries(values)) text = text.replaceAll(`{{${key}}}`, value);
  const dir = mkdtempSync(join(tmpdir(), "erpcraft-e2e-"));
  const path = join(dir, name);
  writeFileSync(path, text);
  cleanup.push(async () => rmSync(dir, { recursive: true, force: true }));
  return path;
}

/** Banking: import a statement, match its lines by keyboard, post the drafted payment, reconcile. */
async function bankingChecks(): Promise<void> {
  const company = await first("company", { code: "OHIO" });
  if (!company) throw new Error("no company OHIO; run `bun run seed` first");
  const companyId = asString(company.id);
  // Statements of an account follow each other and are never reversed, so each run has an account of its own.
  const bank = asObject(
    await session.run("entity.create", {
      entity: "account",
      data: {
        company_id: companyId,
        code: `B${stamp}`,
        name: `E2E bank ${stamp}`,
        account_type: "asset",
        is_bank: true,
      },
    }),
  );
  const bankId = asString(bank.id);
  cleanup.push(() => session.run("entity.update", { entity: "account", id: bankId, data: { active: false } }));
  // The number the statement files name the account by; without it the server refuses to import them.
  await session.run("entity.create", {
    entity: "bank_account_number",
    data: { company_id: companyId, bank_account_id: bankId, iban: "US00EXAMPLE000000001" },
  });

  // In the books before the statement: rent paid from the account, and an invoice to pay.
  const rent = asObject(
    await session.run("document.create", {
      doc_type: "journal_entry",
      company_id: companyId,
      doc_date: "2026-06-03",
      memo: `E2E rent ${stamp}`,
      body: {
        lines: [
          { account_id: await accountId(companyId, "6000"), debit: "25", credit: "0" },
          { account_id: bankId, debit: "0", credit: "25" },
        ],
      },
    }),
  );
  await session.run("journal_entry.post", { id: asString(rent.id) });
  const vendor = await first("vendor", { code: "V001" });
  const item = await first("item", { code: "WIDGET" });
  const warehouse = await first("warehouse", { company_id: companyId, code: "MAIN" });
  if (!vendor || !item || !warehouse) throw new Error("the seed data is missing; run `bun run seed` first");
  const order = asObject(
    await session.run("document.create", {
      doc_type: "purchase_order",
      company_id: companyId,
      doc_date: "2026-05-20",
      memo: `E2E bank ${stamp}`,
      body: {
        vendor_id: asString(vendor.id),
        lines: [{ item_id: asString(item.id), warehouse_id: asString(warehouse.id), qty: "5", unit_price: "4.1" }],
      },
    }),
  );
  await session.run("purchase_order.approve", { id: asString(order.id) });
  const receipt = asObject(await session.run("purchase_order.receive", { id: asString(order.id) }));
  await session.run("purchase_receipt.post", { id: asString(receipt.id) });
  const invoice = asObject(await session.run("purchase_order.invoice", { id: asString(order.id) }));
  const posted = asObject(await session.run("vendor_invoice.post", { id: asString(invoice.id) }));
  const number = asString(posted.number);

  // Import the statement through the page that imports one.
  const camt = fixture("statement.camt053.xml", {
    RUN: stamp,
    INVOICE_AMOUNT: "20.50",
    INVOICE_NUMBER: number,
    CLOSING: "49.00",
    CLOSING_SIDE: "DBIT",
  });
  await open("/c/OHIO/d/bank_statement");
  await browser.click("Import statement", "a");
  check(
    "the statement list leads to importing one",
    (await browser.location()) === "/c/OHIO/run/bank_statement.import",
  );
  await pickInLookup("#import-account", `B${stamp}`);
  await browser.evaluate(`document.querySelector('input[name=import-format][value=camt053]').click()`);
  await browser.setFile("#import-file", camt);
  await browser.click("Import statement");
  const statementPath = await browser.location();
  check("an imported statement opens", /\/d\/bank_statement\/[0-9a-f-]{36}$/.test(statementPath), statementPath);
  const statementId = statementPath.split("/").pop() ?? "";
  const workspace = await textOf("main");
  check(
    "each line comes with what it probably is",
    workspace.includes("3 of 3 still to match") &&
      workspace.includes(`Pays ${number}`) &&
      workspace.includes("Recorded as JE-"),
    workspace.slice(0, 600),
  );

  // Match by keyboard: Enter takes the best proposal and moves on.
  await browser.key("Enter");
  await browser.settle();
  check("Enter matches a line to the recorded posting", (await textOf("main")).includes("2 of 3 still to match"));
  await browser.key("Enter");
  await browser.settle();
  check(
    "Enter on the next line pays the open item with a drafted payment",
    (await textOf("main")).includes("1 of 3 still to match"),
  );
  await browser.click("Post the payment");
  check(
    "the drafted payment is posted through its preview",
    (await textOf("[role=dialog]")).includes("General ledger"),
  );
  await dialogClick("Post");
  await browser.settle();
  await browser.key("a");
  await browser.key("6");
  await browser.insert("000");
  await Bun.sleep(500);
  await browser.settle();
  await browser.key("Enter");
  await browser.settle();
  check("a fee is posted to the account chosen with a", (await textOf("main")).includes("0 of 3 still to match"));

  await browser.click("Reconcile");
  await dialogClick("Reconcile");
  await browser.settle();
  const statement = asObject(await session.run("document.get", { doc_type: "bank_statement", id: statementId }));
  check("the statement is reconciled", statement.status === "posted", String(statement.status));

  await open(`/c/OHIO/r/bank.reconciliation?account=${bankId}&as_of=2026-06-30`);
  check(
    "the reconciliation agrees",
    (await textOf("main")).includes("The books and the statements agree"),
    await textOf("main"),
  );

  // A CSV file is read with the account's columns, saved first.
  await open("/c/OHIO/run/bank_statement.import");
  await pickInLookup("#import-account", `B${stamp}`);
  await browser.evaluate(`document.querySelector('input[name=import-format][value=csv]').click()`);
  await browser.settle();
  await browser.click("Say which column holds what", "a");
  await browser.fill("#field-delimiter", ";");
  await browser.fill("#field-date_format", "%d.%m.%Y");
  await browser.evaluate("document.querySelector('#field-decimal_comma')?.click()");
  await browser.fill("#field-booking_date_column", "Date");
  await browser.fill("#field-amount_column", "Amount");
  await browser.fill("#field-counterparty_name_column", "Payee");
  await browser.fill("#field-reference_column", "Text");
  await browser.fill("#field-transaction_id_column", "Id");
  await browser.click("Create");
  check(
    "the columns are saved for the account",
    /\/e\/bank_csv_mapping\/[0-9a-f-]{36}$/.test(await browser.location()),
  );
  await open("/c/OHIO/run/bank_statement.import");
  await pickInLookup("#import-account", `B${stamp}`);
  await browser.evaluate(`document.querySelector('input[name=import-format][value=csv]').click()`);
  await browser.setFile("#import-file", fixture("statement.csv", { RUN: stamp }));
  await browser.click("Import statement");
  check("a CSV statement is imported with the saved columns", (await textOf("main")).includes("Interest June"));

  // A malformed file is refused with the line it is about.
  await open("/c/OHIO/run/bank_statement.import");
  await pickInLookup("#import-account", `B${stamp}`);
  await browser.evaluate(`document.querySelector('input[name=import-format][value=ofx]').click()`);
  await browser.setFile("#import-file", fixture("broken.ofx", {}));
  await browser.click("Import statement");
  const refused = await textOf("main");
  check(
    "a malformed file is refused in the server's words",
    refused.includes("was not imported"),
    refused.slice(0, 500),
  );
}

/** Aging, with the way down to a party's items, and the early-payment discount offered when paying. */
async function agingChecks(): Promise<void> {
  await open("/c/OHIO/r/open_item.aging?kind=payable");
  const aging = await textOf("main");
  check(
    "the aging report has buckets of days past due",
    aging.includes("61-90 days past due") && aging.includes("Parts Inc"),
    aging.slice(0, 400),
  );
  await browser.click("Parts Inc", "tbody a");
  const location = await browser.location();
  check(
    "a party's row leads to its open items",
    location.startsWith("/c/OHIO/r/open_item.list?kind=payable&party="),
    location,
  );
  // The seed's invoice on 2 % in 10 days: chosen to pay, its discount is offered.
  const ticked = await browser.evaluate(
    `(() => { const row = [...document.querySelectorAll("tbody tr")].find((r) => r.textContent.includes("until")); const box = row?.querySelector("input[type=checkbox]"); if (!box) return false; box.click(); return true; })()`,
  );
  await browser.settle();
  const bar = await textOf("main");
  check(
    "an item with an early-payment discount says so when chosen to pay",
    ticked === true && bar.includes("may be kept back if paid by"),
    bar.slice(0, 500),
  );
  // Paid in time, the draft payment keeps the discount back on the item's line.
  await pickInLookup("#pay-bank", "1000");
  await browser.click("Pay");
  const draftPath = await browser.location();
  check("Pay drafts the payment", /\/d\/payment\/[0-9a-f-]{36}$/.test(draftPath), draftPath);
  const draftId = draftPath.split("/").pop() ?? "";
  cleanup.push(() => session.run("document.delete", { doc_type: "payment", id: draftId }));
  const discount = await browser.evaluate(`document.querySelector('[aria-label="Discount, line 1"]')?.value ?? null`);
  check("the draft payment takes the early-payment discount", discount === "0.80", String(discount));
}

// The restricted test user: some fields of items withheld, and the approver of large orders.
const RESTRICTED_PERMISSIONS = [
  "company:read",
  "currency:read",
  "item:read",
  "item:update",
  "purchase_order:read",
  "vendor:read",
  "e2e:approve_large",
  { deny: "item:read", fields: ["uom"] },
  { deny: "item:update", fields: ["name"] },
];

/** Approvals: an action that needs one waits, its requester cannot decide it, an approver does. */
async function approvalChecks(): Promise<void> {
  const company = await first("company", { code: "OHIO" });
  if (!company) throw new Error("no company OHIO; run `bun run seed` first");
  const companyId = asString(company.id);
  const rule = asObject(
    await session.run("entity.create", {
      entity: "policy_rule",
      data: {
        company_id: companyId,
        point: "purchase_order.approve",
        effect: "approve",
        condition: "facts.total_net > 100000",
        message: `E2E ${stamp}: a large order needs a second person`,
        approver_permission: "e2e:approve_large",
        approvals_needed: 1,
      },
    }),
  );
  cleanup.push(() => session.run("entity.delete", { entity: "policy_rule", id: asString(rule.id) }));
  const vendor = await first("vendor", { code: "V001" });
  const item = await first("item", { code: "WIDGET" });
  const warehouse = await first("warehouse", { company_id: companyId, code: "MAIN" });
  if (!vendor || !item || !warehouse) throw new Error("the seed data is missing; run `bun run seed` first");
  const order = asObject(
    await session.run("document.create", {
      doc_type: "purchase_order",
      company_id: companyId,
      doc_date: "2026-06-10",
      memo: `E2E large ${stamp}`,
      body: {
        vendor_id: asString(vendor.id),
        lines: [{ item_id: asString(item.id), warehouse_id: asString(warehouse.id), qty: "1", unit_price: "150000" }],
      },
    }),
  );
  const orderId = asString(order.id);
  cleanup.push(() => session.run("purchase_order.cancel", { id: orderId }).catch(() => null));

  await open(`/c/OHIO/d/purchase_order/${orderId}`);
  await browser.click("Approve");
  const preview = await textOf("[role=dialog]");
  check(
    "the preview says the action needs approval, and by whom",
    preview.includes("needs approval") && preview.includes("e2e:approve_large"),
    preview,
  );
  await dialogClick("Ask for approval to Approve");
  check(
    "asking leaves a request to open, and nothing has run",
    (await textOf("[role=dialog]")).includes("Open the request"),
  );
  const still = asObject(await session.run("document.get", { doc_type: "purchase_order", id: orderId }));
  check("the order is not approved while the request waits", still.status === "draft", String(still.status));
  await browser.click("Open the request", "[role=dialog] a");
  const requestPath = await browser.location();
  check("the request opens", /\/approvals\/[0-9a-f-]{36}$/.test(requestPath), requestPath);
  const own = await textOf("main");
  check(
    "the requester cannot decide their own request",
    own.includes("You asked for this") && !(await textOf("h1 ~ div")).includes("Approve"),
    own.slice(0, 300),
  );

  const approver = await signInRestricted(session, RESTRICTED_PERMISSIONS);
  await browser.goto(`${APP}/c/OHIO/approvals#token=${approver.token}`);
  check("the approver finds it in the inbox", (await textOf("tbody")).includes(`E2E ${stamp}`), await textOf("main"));
  await browser.goto(`${APP}${requestPath}#token=${approver.token}`);
  const readable = await textOf("main");
  check(
    "the request names the order by its party and total, and its facts as the policy declares them",
    readable.includes("Parts Inc") && /Net total\s*150,000\.00\s*USD/.test(readable),
    readable.slice(0, 600),
  );
  check(
    "the raw input is kept behind Details",
    (await browser.evaluate(`document.querySelector("main details")?.open`)) === false,
  );
  check(
    "rejecting needs a reason",
    (await browser.evaluate(
      `[...document.querySelectorAll("button")].find((b) => b.textContent.trim() === "Reject")?.disabled`,
    )) === true,
  );
  await browser.click("Approve");
  await browser.settle();
  check(
    "an approved request runs the command",
    (await textOf("main")).includes("Approved and done"),
    await textOf("main"),
  );
  const approved = asObject(await session.run("document.get", { doc_type: "purchase_order", id: orderId }));
  check("the order is approved once approved", approved.status === "open", String(approved.status));

  // A second large order, asked through the API and rejected with a reason in the UI.
  const second = asObject(
    await session.run("document.create", {
      doc_type: "purchase_order",
      company_id: companyId,
      doc_date: "2026-06-11",
      memo: `E2E large, rejected ${stamp}`,
      body: {
        vendor_id: asString(vendor.id),
        lines: [{ item_id: asString(item.id), warehouse_id: asString(warehouse.id), qty: "1", unit_price: "200000" }],
      },
    }),
  );
  const secondId = asString(second.id);
  cleanup.push(() => session.run("purchase_order.cancel", { id: secondId }).catch(() => null));
  const asked = asObject(await session.run("purchase_order.approve", { id: secondId }));
  const requestId = asString(asObject(asked.approval_request).id);
  await browser.goto(`${APP}/c/OHIO/approvals/${requestId}#token=${approver.token}`);
  await browser.fill("#approval-reason", "Too many widgets at once");
  await browser.click("Reject");
  await browser.settle();
  const rejected = await textOf("main");
  check(
    "a rejected request shows its status and reason",
    rejected.includes("Rejected") && rejected.includes("Too many widgets at once"),
    rejected.slice(0, 400),
  );
  const unchanged = asObject(await session.run("document.get", { doc_type: "purchase_order", id: secondId }));
  check("a rejected request runs nothing", unchanged.status === "draft", String(unchanged.status));
}

/** The seed's document with this memo, or an error naming what to run. */
async function seeded(docType: string, companyId: string, memo: string): Promise<string> {
  const page = asObject(
    await session.run("document.list", {
      doc_type: docType,
      company_id: companyId,
      where: `record.memo == '${memo}'`,
      limit: 1,
    }),
  );
  const found = asArray(page.items)[0];
  if (!found) throw new Error(`no ${docType} "${memo}"; run \`bun run seed\` first`);
  return asString(asObject(found).id);
}

/** Fixed assets: the register beside the ledger, a schedule, the actions on an asset, a run and a disposal. */
async function assetChecks(): Promise<void> {
  const company = await first("company", { code: "OHIO" });
  if (!company) throw new Error("no company OHIO; run `bun run seed` first");
  const companyId = asString(company.id);
  const machine = await first("asset", { code: "MACH-01" });
  const forklift = await first("asset", { code: "LIFT-01" });
  const equipment = await first("asset_class", { code: "EQUIP" });
  if (!machine || !forklift || !equipment) throw new Error("no demo assets; run `bun run seed` first");

  await open("/c/OHIO/r/asset.register");
  const register = await textOf("main");
  check(
    "the register lists the assets with their book value, and totals",
    register.includes("MACH-01") && register.includes("11,000.00") && (await textOf("tfoot")).includes("Total"),
    register.slice(0, 600),
  );
  // The seed books a purchase on 1500 that no asset holds yet: that account differs, 1590 agrees.
  const accountRow = (code: string) =>
    browser.evaluate(
      `[...document.querySelectorAll("tbody tr")].find((row) => row.textContent.startsWith(${JSON.stringify(code)}))?.textContent ?? ""`,
    );
  check(
    "the register says, per account, whether it agrees with the ledger",
    register.includes("The register and the ledger differ") &&
      String(await accountRow("1500")).includes("Differ") &&
      String(await accountRow("1590")).includes("Agree"),
    register.slice(0, 600),
  );
  await browser.click("MACH-01", "tbody td span");
  const schedulePath = await browser.location();
  check(
    "an asset's row opens its depreciation schedule",
    schedulePath.includes("/r/asset.schedule?asset="),
    schedulePath,
  );
  const schedule = await textOf("main");
  check(
    "the schedule shows posted months and planned ones, with the book value",
    schedule.includes("Posted, DEP-") && schedule.includes("Planned") && schedule.includes("11,000.00"),
    schedule.slice(0, 600),
  );

  // An asset not in the books yet: taking it over asks for what the command needs and previews it.
  const planned = asObject(
    await session.run("entity.create", {
      entity: "asset",
      data: {
        company_id: companyId,
        class_id: asString(equipment.id),
        code: `E2E-${stamp}`,
        name: `E2E planned ${stamp}`,
      },
    }),
  );
  cleanup.push(() => session.run("entity.delete", { entity: "asset", id: asString(planned.id) }));
  await open(`/c/OHIO/e/asset/${asString(planned.id)}`);
  check("a planned asset offers to be taken over", (await textOf("main")).includes("Take over"));
  await browser.click("Take over");
  check(
    "the dialog asks for the cost as a number",
    (await browser.evaluate(`document.querySelector("#action-cost")?.inputMode`)) === "decimal",
  );
  await browser.fill("#action-cost", "1000");
  await browser.fill("#action-accumulated_depreciation", "0");
  await browser.setDate("#action-acquisition_date", "2025-11-01");
  await browser.setDate("#action-in_service_date", "2025-11-01");
  await browser.setDate("#action-depreciated_through", "2025-12-31");
  await Bun.sleep(600);
  await browser.settle();
  const preview = await textOf("[role=dialog]");
  check(
    "the take-over is previewed before it is done",
    !preview.includes("would refuse") && preview.includes("In service"),
    preview.slice(0, 600),
  );
  await dialogClick("Cancel");

  // Capitalising asks for the posted line that bought the asset, then previews the cost it takes from it.
  const attachment = await first("asset", { code: "ATT-01" });
  if (!attachment) throw new Error("no asset ATT-01; run `bun run seed` first");
  await open(`/c/OHIO/e/asset/${asString(attachment.id)}`);
  await browser.click("Capitalise");
  const picker = await textOf("[role=dialog]");
  check(
    "capitalising offers the purchase lines on the class's cost account",
    picker.includes("Side shifter for the forklift"),
    picker.slice(0, 600),
  );
  await browser.evaluate(
    `[...document.querySelectorAll("[role=dialog] tbody tr")].find((row) => row.textContent.includes("Side shifter"))?.querySelector("input")?.click()`,
  );
  await dialogClick("Continue");
  await browser.setDate("#action-in_service_date", "2026-11-03");
  await Bun.sleep(600);
  await browser.settle();
  const capitalise = await textOf("[role=dialog]");
  check(
    "the chosen line's amount becomes the cost, in the preview",
    capitalise.includes("2,400.00") && !capitalise.includes("would refuse"),
    capitalise.slice(0, 600),
  );
  await dialogClick("Cancel");

  // A disposal is started from the asset, with the asset filled in.
  await open(`/c/OHIO/e/asset/${asString(forklift.id)}`);
  await browser.click("New Asset disposal", "a");
  const disposalPath = await browser.location();
  check("a disposal is started from its asset", disposalPath.includes("/d/asset_disposal/new?asset_id="), disposalPath);
  check("the new disposal names the asset", (await textOf("main")).includes("LIFT-01"));

  // The seed's draft disposal previews the gain or loss it posts.
  await open(`/c/OHIO/d/asset_disposal/${await seeded("asset_disposal", companyId, "Demo: forklift sold")}`);
  await browser.click("Post");
  const disposal = await textOf("[role=dialog]");
  check(
    "posting a disposal previews what leaves the books",
    disposal.includes("1500") && disposal.includes("Accumulated depreciation"),
    disposal.slice(0, 600),
  );
  await dialogClick("Cancel");

  // The seed's draft run: its lines are the server's proposal, not typed in.
  await open(
    `/c/OHIO/d/depreciation_run/${await seeded("depreciation_run", companyId, "Demo: depreciation to October")}`,
  );
  const run = await textOf("main");
  check(
    "a depreciation run lists the assets it depreciates",
    run.includes("MACH-01") && run.includes("800.00"),
    run.slice(0, 600),
  );
  check("its lines cannot be added to or removed", !run.includes("Add line"));
  await open("/c/OHIO/d/depreciation_run/new");
  check(
    "a new run says its lines are worked out when it is saved",
    (await textOf("main")).includes("worked out when the document is created"),
  );
}

/** Direct invoices, line kinds and e-invoicing: an invoice without an order, an export, an import. */
async function invoiceChecks(): Promise<void> {
  const company = await first("company", { code: "OHIO" });
  if (!company) throw new Error("no company OHIO; run `bun run seed` first");
  const companyId = asString(company.id);

  // A vendor invoice without an order: no order to choose, the vendor named instead, a line on an account.
  await open("/c/OHIO/d/vendor_invoice");
  await browser.click("New Vendor invoice without Purchase order", "a");
  const page = await textOf("main");
  check(
    "an invoice without an order does not ask for one",
    !page.includes("Purchase order") && page.includes("Vendor"),
    page.slice(0, 400),
  );
  check(
    "its lines start as a kind that needs no order",
    (await browser.evaluate(`document.querySelector('[aria-label="What line 1 is"]')?.value`)) === "Charge",
  );
  await pickInLookup("#header-vendor_id", "V001");
  await browser.select('[aria-label="What line 1 is"]', "Account");
  await pickInLookup('[aria-label="Account, line 1"]', "6000");
  await browser.fill('[aria-label="Description, line 1"]', `E2E direct ${stamp}`);
  await browser.fill('[aria-label="Quantity invoiced, line 1"]', "1");
  await browser.fill('[aria-label="Unit price, line 1"]', "50");
  await browser.click("Create");
  const path = await browser.location();
  check("a direct vendor invoice is created", /\/d\/vendor_invoice\/[0-9a-f-]{36}$/.test(path), path);
  const directId = path.split("/").pop() ?? "";
  cleanup.push(() => session.run("document.delete", { doc_type: "vendor_invoice", id: directId }).catch(() => null));
  const direct = asObject(await session.run("document.get", { doc_type: "vendor_invoice", id: directId }));
  const body = asObject(direct.body);
  const line = asObject(asArray(body.lines)[0]);
  check(
    "it names its vendor, has no order, and its line goes to the account",
    typeof body.vendor_id === "string" &&
      body.purchase_order_id == null &&
      typeof line.account_id === "string" &&
      line.charge_id == null,
    JSON.stringify(body).slice(0, 400),
  );

  // Export: a posted sales invoice writes itself as an XRechnung and downloads it.
  const listed = asObject(
    await session.run("document.list", {
      doc_type: "sales_invoice",
      company_id: companyId,
      where: "record.memo == 'Demo: invoice to Buyer LLC'",
    }),
  );
  const invoiceId = asString(asObject(asArray(listed.items)[0]).id);
  await open(`/c/OHIO/d/sales_invoice/${invoiceId}`);
  await browser.click("Export e-invoice");
  await dialogClick("Export and download");
  const exported = await textOf("[role=dialog]");
  check(
    "a posted invoice exports as an e-invoice that keeps the rules",
    exported.includes("keeps every rule"),
    exported.slice(0, 400),
  );
  await dialogClick("Close");

  // Import: the same invoice as one Parts Inc sent to this company, read first, then drafted.
  const written = asObject(await session.run("einvoice.export", { doc_type: "sales_invoice", id: invoiceId }));
  const xml = asString(written.xml)
    .replaceAll("DE123456789", "DE111222333")
    .replaceAll("DE987654321", "DE123456789")
    .replace(/<cbc:ID>[^<]*<\/cbc:ID>/, `<cbc:ID>PI-${stamp}</cbc:ID>`);
  const dir = mkdtempSync(join(tmpdir(), "erpcraft-e2e-"));
  const file = join(dir, `PI-${stamp}.xml`);
  writeFileSync(file, xml);
  cleanup.push(async () => rmSync(dir, { recursive: true, force: true }));
  await open("/c/OHIO/d/vendor_invoice");
  await browser.click("Import e-invoice", "a");
  await browser.setFile("#einvoice-file", file);
  const proposal = await textOf("main");
  check(
    "an incoming invoice is read before anything is created",
    proposal.includes(`PI-${stamp}`) && proposal.includes("Parts Inc") && proposal.includes("a direct invoice"),
    proposal.slice(0, 600),
  );
  check("a line that invoices no order line goes to an account to choose", proposal.includes("the account"));
  await pickInLookup("#einvoice-account", "6000");
  await browser.click(`Create the draft Vendor invoice`);
  const imported = await browser.location();
  check(
    "importing drafts the vendor invoice, which opens",
    /\/d\/vendor_invoice\/[0-9a-f-]{36}$/.test(imported),
    imported,
  );
  const importedId = imported.split("/").pop() ?? "";
  cleanup.push(() => session.run("document.delete", { doc_type: "vendor_invoice", id: importedId }).catch(() => null));
}

async function policyChecks(): Promise<void> {
  await open("/c/OHIO/e/policy_rule/new");
  const options = await textOf("#field-point");
  check("the policy points are offered as a choice", options.includes("purchase_receipt.over_receipt"), options);
  await browser.select("#field-point", "purchase_receipt.over_receipt");
  const panel = await textOf("main aside");
  check(
    "the chosen point shows its facts and its default",
    panel.includes("facts.over_percent") && panel.includes("Without any rule"),
    panel,
  );
  const has = (selector: string) => browser.evaluate(`document.querySelector(${JSON.stringify(selector)}) !== null`);
  await browser.select("#field-effect", "approve");
  await browser.settle();
  check(
    "a rule that asks for approval names who approves, and how many",
    (await has("#field-approver_role_id")) === true &&
      (await has("#field-approver_permission")) === true &&
      (await has("#field-approvals_needed")) === true &&
      (await has("#field-approval_days")) === true,
  );
  await browser.select("#field-effect", "allow");
  await browser.settle();
  check("a rule that only allows has no approvers to name", (await has("#field-approvals_needed")) === false);
  await browser.fill("#field-condition", "facts.over_percent <=");
  await browser.click("Create");
  const refused = await textOf("#field-condition-error");
  check("a refused condition is reported next to the condition", refused.length > 0, await browser.text());
  await browser.fill("#field-condition", "facts.over_percent <= 5");
  await browser.click("Create");
  const path = await browser.location();
  check("a policy rule is created", /\/e\/policy_rule\/[0-9a-f-]{36}$/.test(path), path);
  const id = path.split("/").pop() ?? "";
  cleanup.push(() => session.run("entity.delete", { entity: "policy_rule", id }));
  await browser.fill("#field-condition", "facts.over_percent <= 10");
  await browser.click("Save");
  const rule = asObject(await session.run("entity.get", { entity: "policy_rule", id }));
  check("changing only the condition is accepted", rule.condition === "facts.over_percent <= 10", JSON.stringify(rule));
}

async function reportChecks(): Promise<void> {
  await open("/c/OHIO/r/gl.trial_balance");
  check(
    "the trial balance has rows and a total",
    (await textOf("tbody")).includes("Bank") && (await textOf("tfoot")).includes("Total"),
  );
  await browser.click("Bank", "tbody a");
  const ledger = await browser.location();
  check("an account row opens its ledger", ledger.includes("/r/gl.account_ledger?account="), ledger);
  const entries = await browser.text();
  check(
    "the ledger shows entries with a running balance",
    entries.includes("Opening balance") && entries.includes("JE-2026-00001"),
  );
  const bank = await first("account", { code: "1000" });
  if (bank) {
    await open(`/c/OHIO/e/account/${asString(bank.id)}`);
    await browser.click("Account ledger", "main a");
    check(
      "an account's page leads to its ledger",
      (await browser.location()).startsWith("/c/OHIO/r/gl.account_ledger?account="),
      await browser.location(),
    );
  }
  await open("/c/OHIO/r/gl.balance_sheet");
  check("the balance sheet shows both sides", (await browser.text()).includes("Total liabilities and equity"));
  await open("/c/OHIO/r/gl.income_statement?from=2026-01-01&to=2026-12-31");
  check("the income statement shows net income", (await browser.text()).includes("Net income"));
  await open("/c/OHIO/r/open_item.list?kind=payable");
  const open_ = await browser.text();
  check("open items list what is owed, with a total", open_.includes("VI-2026-00001") && open_.includes("Total open"));
  await open("/c/OHIO/r/stock.on_hand");
  check("stock on hand lists the items", (await textOf("tbody")).includes("WIDGET"));
  await open("/c/OHIO/r/audit.list");
  check("a report without a page of its own gets the generic one", (await textOf("tbody")).length > 0);
}

async function shellChecks(): Promise<void> {
  await open("/c/OHIO/d/journal_entry?status=posted");
  await browser.key("k", CTRL);
  await browser.insert("PO-2026-00001");
  await Bun.sleep(600);
  await browser.settle();
  check("Ctrl+K finds a document by its number", (await textOf("#palette-list")).includes("PO-2026-00001"));
  await browser.key("Escape");
  await browser.key("Escape");

  const companies = asObject(await session.run("entity.list", { entity: "company", limit: 2, sort: ["code"] }));
  const other = asArray(companies.items)
    .map(asObject)
    .find((company) => company.code !== "OHIO");
  if (other) {
    await pickInLookup("#company-switcher", asString(other.code).toLowerCase());
    check(
      "switching company keeps the screen",
      (await browser.location()) === `/c/${encodeURIComponent(asString(other.code))}/d/journal_entry`,
      await browser.location(),
    );
  }
}

/** Party data: shown under a customer, edited on a page of its own through party.get and party.set. */
async function partyChecks(): Promise<void> {
  const customer = await first("customer", { code: "C001" });
  if (!customer) throw new Error("the seed data is missing; run `bun run seed` first");
  const id = asString(customer.id);
  const before = asObject(await session.run("party.get", { party_type: "customer", party_id: id }));
  cleanup.push(() =>
    session.run("party.set", {
      party_type: "customer",
      party_id: id,
      contacts: asArray(before.contacts),
    }),
  );
  await open(`/c/OHIO/e/customer/${id}`);
  check("a customer shows its party details", (await textOf("main")).includes("Party details"));
  await browser.click("Edit party details", "a");
  check(
    "party details are edited on a page of their own",
    (await browser.location()).endsWith(`/e/customer/${id}/party`),
  );
  await browser.click("Add contact");
  const last = asArray(before.contacts).length;
  await browser.fill(`#party-contact-${last}-name`, `E2E contact ${stamp}`);
  await browser.key("s", CTRL);
  await browser.settle();
  const after = asObject(await session.run("party.get", { party_type: "customer", party_id: id }));
  check(
    "Ctrl+S saves the party details and returns to the record",
    asArray(after.contacts).some((contact) => asObject(contact).name === `E2E contact ${stamp}`) &&
      (await browser.location()).endsWith(`/e/customer/${id}`) &&
      (await textOf("main")).includes(`E2E contact ${stamp}`),
  );
  check(
    "addresses keep their ids when the party details are saved",
    JSON.stringify(asArray(after.addresses).map((address) => asObject(address).id)) ===
      JSON.stringify(asArray(before.addresses).map((address) => asObject(address).id)),
  );
}

const groups: [string, () => Promise<void>][] = [
  ["sign-in", signInChecks],
  ["records", recordChecks],
  ["party", partyChecks],
  ["lists", listChecks],
  ["documents", documentChecks],
  ["actions", actionChecks],
  ["purchase to pay", flowChecks],
  ["print, palette, bulk, views", moreChecks],
  ["field access", accessChecks],
  ["single sign-on", ssoChecks],
  ["users", userChecks],
  ["company setup", setupChecks],
  ["banking", bankingChecks],
  ["aging", agingChecks],
  ["approvals", approvalChecks],
  ["fixed assets", assetChecks],
  ["invoices", invoiceChecks],
  ["policy rules", policyChecks],
  ["reports", reportChecks],
  ["shell", shellChecks],
];
const only = process.argv.slice(2);

try {
  await browser.viewport(1440, 900);
  for (const [name, run] of groups) {
    if (only.length > 0 && !only.includes(name)) continue;
    console.log(`\n${name}`);
    try {
      await run();
    } catch (cause) {
      // A step that could not be carried out fails its group; the others still run.
      failed += 1;
      mkdirSync(FAILURES, { recursive: true });
      const file = resolve(FAILURES, `${name.replaceAll(" ", "-")}.png`);
      await browser.screenshot(file);
      console.log(
        `FAIL  ${name} stopped: ${cause instanceof Error ? cause.message : String(cause)}\n      screenshot: ${file}`,
      );
    }
  }
  check(
    "nothing was logged as an error in the browser console",
    browser.errors.length === 0,
    browser.errors.join("\n"),
  );
} finally {
  for (const undo of cleanup.reverse()) await undo().catch(() => null);
  await browser.close();
}

console.log(`\n${passed} passed, ${failed} failed`);
process.exit(failed === 0 ? 0 : 1);
