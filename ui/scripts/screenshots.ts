// Takes the screenshots in docs/screenshots with headless Chrome.
//
//   bun run dev          (in another terminal)
//   bun run seed
//   bun run screenshots  [part of a name ...]
//
// The script signs in through the API and hands the token to the page in the
// URL fragment, which the app honours in development only. The token is never
// written to a file. A few pictures need a draft that does not exist in the
// seed (a receipt about to be posted, a payment about to be made); the script
// drafts it through the API and deletes it again afterwards.

import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { type Browser, launchBrowser } from "./browser";
import { sweepLeftovers, undoOnSignal } from "./leftovers";
import { asArray, asObject, asString, type JsonObject, RESTRICTED, signIn, signInRestricted } from "./lib";

const APP = process.env.ERPCRAFT_UI_URL ?? "http://127.0.0.1:5173";
const OUT = resolve(import.meta.dir, "../docs/screenshots");
const only = process.argv.slice(2);

const session = await signIn();
await sweepLeftovers(session);

async function first(command: string, input: JsonObject): Promise<JsonObject> {
  const page = asObject(await session.run(command, input));
  const item = asArray(page.items)[0];
  if (!item) throw new Error(`nothing found for ${command} ${JSON.stringify(input)}; run \`bun run seed\` first`);
  return asObject(item);
}

async function maybe(command: string, input: JsonObject): Promise<JsonObject | null> {
  try {
    return await first(command, input);
  } catch {
    return null;
  }
}

const company = await first("entity.list", { entity: "company", filters: { code: "OHIO" } });
const companyId = asString(company.id);
const account = await first("entity.list", { entity: "account", filters: { company_id: companyId, code: "1000" } });
const docIn = async (company: string, docType: string, status: string): Promise<string> =>
  asString((await first("document.list", { doc_type: docType, company_id: company, status, sort: ["-doc_date"] })).id);
const doc = (docType: string, status: string): Promise<string> => docIn(companyId, docType, status);
const openOrder = await first("document.list", {
  doc_type: "purchase_order",
  company_id: companyId,
  status: "open",
  where: "record.memo == 'Demo: first stock order'",
});

// Drafts made for a picture, deleted again right after it.
const drafts: [string, string][] = [];
async function draft(command: string, input: JsonObject, docType: string): Promise<string | null> {
  try {
    const made = asObject(await session.run(command, input));
    const id = asString(made.id);
    drafts.push([docType, id]);
    return id;
  } catch (cause) {
    console.log(`skip ${command}: ${cause instanceof Error ? cause.message : String(cause)}`);
    return null;
  }
}
// Anything else made for a picture, undone right after it.
const undo: (() => Promise<unknown>)[] = [];
// What several pictures share, undone after the last picture.
const atEnd: (() => Promise<unknown>)[] = [];
async function removeDrafts(): Promise<void> {
  for (const [docType, id] of drafts.splice(0)) {
    await session.run("document.delete", { doc_type: docType, id }).catch(() => null);
  }
  for (const step of undo.splice(0).reverse()) await step().catch(() => null);
}

/** The id of the seed's document with this memo. */
async function byMemo(docType: string, memo: string): Promise<string> {
  return asString(
    (await first("document.list", { doc_type: docType, company_id: companyId, where: `record.memo == '${memo}'` })).id,
  );
}

/** Marks what one run of this script made, so a search finds only that. */
const run = `demo ${Date.now().toString(36).toUpperCase()}`;
const accountOf = async (code: string): Promise<string> =>
  asString((await first("entity.list", { entity: "account", filters: { company_id: companyId, code } })).id);
async function journal(memo: string, credit: string): Promise<string> {
  const doc = asObject(
    await session.run("document.create", {
      doc_type: "journal_entry",
      company_id: companyId,
      doc_date: "2026-06-01",
      memo,
      body: {
        lines: [
          { account_id: await accountOf("6000"), debit: "10", credit: "0" },
          { account_id: await accountOf("1000"), debit: "0", credit },
        ],
      },
    }),
  );
  const id = asString(doc.id);
  undo.push(async () => {
    const now = asObject(await session.run("document.get", { doc_type: "journal_entry", id }));
    return now.status === "posted"
      ? session.run("journal_entry.reverse", { id })
      : session.run("document.delete", { doc_type: "journal_entry", id });
  });
  return id;
}
/** A provider to show, deleted after the picture. Its secret is a random value nobody keeps. */
async function exampleProvider(): Promise<string> {
  const made = asObject(
    await session.run("sso_provider.create", {
      name: "Microsoft (example)",
      slug: "microsoft-example",
      issuer: "https://login.microsoftonline.com/00000000-0000-0000-0000-000000000000/v2.0",
      client_id: "11111111-2222-3333-4444-555555555555",
      client_secret: crypto.randomUUID(),
      allowed_domains: ["example.com"],
    }),
  );
  const id = asString(made.id);
  undo.push(() => session.run("sso_provider.delete", { id }));
  return id;
}

const restricted = await signInRestricted(session, [
  "company:read",
  "currency:read",
  "item:read",
  "item:update",
  "purchase_order:read",
  "vendor:read",
  "screenshots:approve_large",
  { deny: "item:read", fields: ["uom"] },
  { deny: "item:update", fields: ["name"] },
]);

/**
 * A large order waiting for a second person's approval: a rule asking for one,
 * an order over its limit, and the request approving it made. The request is
 * rejected, the order deleted and the rule deleted after the pictures.
 */
async function waitingApproval(): Promise<string | null> {
  const rule = asObject(
    await session.run("entity.create", {
      entity: "policy_rule",
      data: {
        company_id: companyId,
        point: "purchase_order.approve",
        effect: "approve",
        condition: "facts.total_net > 100000",
        message: "An order over 100,000 needs a second person to approve it",
        approver_permission: "screenshots:approve_large",
        approvals_needed: 1,
      },
    }),
  );
  atEnd.push(() => session.run("entity.delete", { entity: "policy_rule", id: asString(rule.id) }));
  const vendor = await first("entity.list", { entity: "vendor", filters: { code: "V001" } });
  const item = await first("entity.list", { entity: "item", filters: { code: "WIDGET" } });
  const warehouse = await first("entity.list", {
    entity: "warehouse",
    filters: { company_id: companyId, code: "MAIN" },
  });
  const order = asObject(
    await session.run("document.create", {
      doc_type: "purchase_order",
      company_id: companyId,
      // Early in the year, so that it is not the latest draft other pictures pick.
      doc_date: "2026-01-05",
      memo: "Demo: a year of widgets in one order",
      body: {
        vendor_id: asString(vendor.id),
        lines: [
          { item_id: asString(item.id), warehouse_id: asString(warehouse.id), qty: "12000", unit_price: "12.50" },
        ],
      },
    }),
  );
  const orderId = asString(order.id);
  // Rejected first (pushed last, undone first), then the draft is deleted.
  atEnd.push(() => session.run("document.delete", { doc_type: "purchase_order", id: orderId }));
  const asked = asObject(await session.run("purchase_order.approve", { id: orderId }));
  const request = asked.approval_request ? asObject(asked.approval_request) : null;
  if (!request) return null;
  const id = asString(request.id);
  atEnd.push(() => restricted.run("approval.reject", { id, reason: "Only a picture for the documentation" }));
  return id;
}
const approvalId = await waitingApproval();
const widget = await first("entity.list", { entity: "item", filters: { code: "WIDGET" } });
const template = await maybe("entity.list", { entity: "print_template", filters: { doc_type: "sales_invoice" } });
const receivable = await maybe("open_item.list", { company_id: companyId, kind: "receivable" });

// A second company with real volume, if the database has one: thousands of
// documents show what paging, search and totals look like in practice.
const big = await maybe("entity.list", { entity: "company", filters: { code: "AWC" } });
const bigId = big ? asString(big.id) : null;

interface Shot {
  name: string;
  /** The page, or how to make what it shows and then name it; null skips the picture. */
  path: string | (() => Promise<string | null>);
  signedIn?: boolean;
  width?: number;
  height?: number;
  dark?: boolean;
  /** What to do on the page before the picture is taken. */
  act?: (browser: Browser) => Promise<void>;
  /** Signs in with this token instead of the administrator's. */
  token?: string;
}

async function pick(browser: Browser, selector: string, search: string): Promise<void> {
  await browser.focus(selector);
  await browser.key(search.charAt(0));
  await browser.insert(search.slice(1));
  await Bun.sleep(500);
  await browser.settle();
  await browser.key("Enter");
  await browser.settle();
}

const base = "/c/OHIO";
const shots: Shot[] = [
  { name: "01-sign-in", path: "/sign-in", signedIn: false },
  { name: "02-entity-list", path: `${base}/e/account?sort=code` },
  { name: "03-entity-record", path: `${base}/e/account/${asString(account.id)}` },
  { name: "04-document-list", path: `${base}/d/purchase_order` },
  {
    name: "05-document-posted",
    // The seed's invoice in euros: amounts in two currencies, and its postings.
    path: async () => {
      const invoice = await maybe("document.list", {
        doc_type: "vendor_invoice",
        company_id: companyId,
        status: "posted",
        summary: { currency: "EUR" },
      });
      return `${base}/d/vendor_invoice/${invoice ? asString(invoice.id) : await doc("vendor_invoice", "posted")}`;
    },
    height: 1250,
  },
  { name: "06-document-draft", path: `${base}/d/purchase_order/${await doc("purchase_order", "draft")}` },
  {
    name: "07-preview-dialog",
    path: async () => {
      const receipt = await draft("purchase_order.receive", { id: asString(openOrder.id) }, "purchase_receipt");
      return receipt ? `${base}/d/purchase_receipt/${receipt}?action=post` : null;
    },
  },
  { name: "08-trial-balance", path: `${base}/r/gl.trial_balance` },
  { name: "09-stock-on-hand", path: `${base}/r/stock.on_hand` },
  { name: "10-document-history", path: `${base}/d/purchase_order/${asString(openOrder.id)}?tab=history` },
  { name: "11-open-order", path: `${base}/d/purchase_order/${asString(openOrder.id)}`, height: 800 },
  {
    name: "12-tablet-document",
    path: `${base}/d/purchase_order/${await doc("purchase_order", "draft")}`,
    width: 820,
    height: 1100,
  },
  { name: "13-home", path: base },
  {
    name: "14-dark-document",
    path: `${base}/d/sales_invoice/${await doc("sales_invoice", "posted")}`,
    dark: true,
    height: 1200,
  },
  { name: "15-dark-draft", path: `${base}/d/journal_entry/${await doc("journal_entry", "draft")}`, dark: true },
  {
    name: "16-open-items-pay",
    path: `${base}/r/open_item.list?kind=receivable`,
    act: async (browser) => {
      await browser.evaluate(`document.querySelector('tbody [role=checkbox]:not([disabled])')?.click()`);
      await pick(browser, "#pay-bank", "1000");
    },
  },
  {
    name: "17-payment-draft",
    path: async () => {
      if (!receivable) return null;
      const input = {
        company_id: companyId,
        open_item_ids: [asString(receivable.id)],
        bank_account_id: asString(account.id),
      };
      const payment = await draft("payment.draft", input, "payment");
      return payment ? `${base}/d/payment/${payment}` : null;
    },
  },
  {
    name: "18-policy-rule",
    path: `${base}/e/policy_rule/new`,
    height: 1000,
    act: async (browser) => {
      await browser.select("#field-point", "purchase_receipt.over_receipt");
      await browser.select("#field-effect", "allow");
      await browser.fill("#field-condition", "facts.over_percent <= 5");
    },
  },
  { name: "19-tax-report", path: `${base}/r/tax.report?from=2026-01-01&to=2026-12-31` },
  {
    name: "20-palette",
    path: `${base}/d/purchase_order`,
    act: async (browser) => {
      await browser.key("k", 2);
      await browser.insert("widget");
      await Bun.sleep(700);
      await browser.settle();
    },
  },
  {
    name: "22-open-items-credit",
    path: `${base}/r/open_item.list?kind=payable`,
    act: async (browser) => {
      // The credit note and the first invoice: same vendor, same currency.
      await browser.evaluate(
        `[...document.querySelectorAll("tbody tr")].filter((row) => /VC-2026-00001|VI-2026-00001/.test(row.textContent)).forEach((row) => row.querySelector("[role=checkbox]")?.click())`,
      );
      await browser.settle();
      await browser.click("Apply credit");
      await Bun.sleep(800);
    },
  },
  {
    name: "23-print-view",
    path: `${base}/d/vendor_invoice/${await doc("vendor_invoice", "posted")}/print`,
    height: 1000,
  },
  ...(template
    ? [
        {
          name: "24-print-template",
          path: `${base}/e/print_template/${asString(template.id)}`,
          height: 1100,
          act: async (browser: Browser) => pick(browser, "#preview-document", "SI-2026"),
        },
      ]
    : []),
  {
    name: "25-sso-providers",
    height: 600,
    path: async () => {
      await exampleProvider();
      return `${base}/e/sso_provider`;
    },
  },
  { name: "26-sso-provider", height: 1100, path: async () => `${base}/e/sso_provider/${await exampleProvider()}` },
  { name: "27-restricted-record", path: `${base}/e/item/${asString(widget.id)}`, token: restricted.token, height: 600 },
  {
    name: "28-bulk-confirm",
    path: async () => {
      await journal(`Rent, June (${run})`, "10");
      await journal(`Rent, June, mistyped (${run})`, "1");
      return `${base}/d/journal_entry?q=${encodeURIComponent(run)}`;
    },
    act: async (browser) => {
      await browser.evaluate("document.querySelectorAll('tbody [role=checkbox]').forEach((box) => box.click())");
      await browser.settle();
      await browser.click("Post");
    },
  },
  {
    name: "29-bulk-result",
    path: async () => {
      await journal(`Rent, July (${run})`, "10");
      await journal(`Rent, July, mistyped (${run})`, "1");
      // Another address than the picture before, so the page loads afresh.
      return `${base}/d/journal_entry?status=draft&q=${encodeURIComponent(run)}`;
    },
    act: async (browser) => {
      await browser.evaluate("document.querySelectorAll('tbody [role=checkbox]').forEach((box) => box.click())");
      await browser.settle();
      await browser.click("Post");
      await browser.click("Post 2", "[role=dialog] button");
      await Bun.sleep(800);
    },
  },
  {
    name: "30-palette-actions",
    path: async () => `${base}/d/journal_entry/${await journal("Rent, August", "10")}`,
    act: async (browser) => {
      await browser.key("k", 2);
      await Bun.sleep(300);
    },
  },
  {
    name: "31-saved-views",
    path: `${base}/d/purchase_order?status=cancelled&sort=-number`,
    act: async (browser) => {
      await browser.evaluate("localStorage.clear()");
      await browser.click("Views");
      await browser.fill('input[aria-label="Name of the view"]', "Cancelled, newest first");
      await browser.click("Save", "[data-radix-popper-content-wrapper] button");
      await browser.fill('input[aria-label="Name of the view"]', "Drafts to approve");
    },
  },
  {
    name: "32-company-setup",
    path: `${base}/setup`,
    height: 1100,
    act: async (browser) => {
      await browser.evaluate(
        `[...document.querySelectorAll("label")].find((label) => label.textContent.includes("US small business"))?.querySelector("[role=radio]")?.click()`,
      );
      await browser.fill("#setup-code", "BUCKEYE");
      await browser.fill("#setup-name", "Buckeye Supply");
      await browser.click("Show what will be created");
      await Bun.sleep(600);
    },
  },
  {
    name: "33-user",
    path: async () => {
      const users = asObject(
        await session.run("entity.list", { entity: "user", filters: { email: RESTRICTED.email } }),
      );
      return `${base}/e/user/${asString(asObject(asArray(users.items)[0] ?? {}).id)}`;
    },
  },
  {
    name: "34-bank-import-refused",
    path: `${base}/run/bank_statement.import`,
    act: async (browser) => {
      await pick(browser, "#import-account", "1010");
      await browser.evaluate(`document.querySelector('[role=radio][value=ofx]').click()`);
      await browser.setFile("#import-file", resolve(import.meta.dir, "fixtures/broken.ofx"));
      await browser.click("Import statement");
    },
  },
  {
    name: "35-bank-matching",
    height: 1000,
    path: async () => {
      const statement = await first("document.list", {
        doc_type: "bank_statement",
        company_id: companyId,
        status: "draft",
        // The seed's demo statement, not one an end-to-end run left.
        summary: { bank_account_id: await accountOf("1010") },
      });
      return `${base}/d/bank_statement/${asString(statement.id)}`;
    },
  },
  {
    name: "36-bank-reconciliation",
    height: 1000,
    path: async () => {
      const checking = await first("entity.list", {
        entity: "account",
        filters: { company_id: companyId, code: "1010" },
      });
      return `${base}/r/bank.reconciliation?account=${asString(checking.id)}&as_of=2026-06-30`;
    },
  },
  { name: "37-new-user", path: `${base}/e/user/new` },
  { name: "38-aging", path: `${base}/r/open_item.aging?kind=payable` },
  {
    name: "39-pay-with-discount",
    path: `${base}/r/open_item.list?kind=payable`,
    act: async (browser) => {
      await browser.evaluate(
        `[...document.querySelectorAll("tbody tr")].find((row) => row.textContent.includes("until"))?.querySelector("[role=checkbox]")?.click()`,
      );
      await browser.settle();
    },
  },
  ...(approvalId
    ? [
        { name: "40-approvals-inbox", path: `${base}/approvals`, token: restricted.token },
        { name: "41-approval-request", path: `${base}/approvals/${approvalId}`, token: restricted.token },
        // Another address than the picture before, so that the page loads again with the other user.
        { name: "42-approval-own-request", path: `${base}/approvals/${approvalId}?as=requester` },
      ]
    : []),
  { name: "43-asset-register", path: `${base}/r/asset.register` },
  {
    name: "44-asset-schedule",
    height: 1100,
    path: async () => {
      const machine = await first("entity.list", { entity: "asset", filters: { code: "MACH-01" } });
      return `${base}/r/asset.schedule?asset=${asString(machine.id)}`;
    },
  },
  {
    name: "45-depreciation-run",
    path: async () => `${base}/d/depreciation_run/${await byMemo("depreciation_run", "Demo: depreciation to October")}`,
  },
  {
    name: "46-asset-disposal-preview",
    height: 1000,
    path: async () => `${base}/d/asset_disposal/${await byMemo("asset_disposal", "Demo: forklift sold")}?action=post`,
  },
  {
    name: "47-asset-take-over",
    height: 1000,
    path: async () => {
      const equipment = await first("entity.list", { entity: "asset_class", filters: { code: "EQUIP" } });
      const made = asObject(
        await session.run("entity.create", {
          entity: "asset",
          data: {
            company_id: companyId,
            class_id: asString(equipment.id),
            code: "LATHE-01",
            name: "Lathe (from the old books)",
          },
        }),
      );
      undo.push(() => session.run("entity.delete", { entity: "asset", id: asString(made.id) }));
      return `${base}/e/asset/${asString(made.id)}?action=take_over`;
    },
    act: async (browser) => {
      await browser.fill("#action-cost", "15000");
      await browser.fill("#action-accumulated_depreciation", "4500");
      await browser.setDate("#action-acquisition_date", "2023-04-01");
      await browser.setDate("#action-in_service_date", "2023-04-01");
      await browser.setDate("#action-depreciated_through", "2025-12-31");
      await Bun.sleep(700);
      await browser.settle();
    },
  },
  {
    name: "50-asset-capitalise-lines",
    height: 700,
    path: async () => {
      const attachment = await first("entity.list", { entity: "asset", filters: { code: "ATT-01" } });
      return `${base}/e/asset/${asString(attachment.id)}?action=capitalise`;
    },
    act: async (browser) => {
      await browser.evaluate(
        `[...document.querySelectorAll("[role=dialog] tbody tr")].find((row) => row.textContent.includes("Side shifter"))?.querySelector("[role=radio]")?.click()`,
      );
      await browser.settle();
    },
  },
  {
    name: "51-asset-capitalise-preview",
    height: 900,
    path: async () => {
      const attachment = await first("entity.list", { entity: "asset", filters: { code: "ATT-01" } });
      return `${base}/e/asset/${asString(attachment.id)}?action=capitalise`;
    },
    act: async (browser) => {
      await browser.evaluate(
        `[...document.querySelectorAll("[role=dialog] tbody tr")].find((row) => row.textContent.includes("Side shifter"))?.querySelector("[role=radio]")?.click()`,
      );
      await browser.click("Continue", "[role=dialog] button");
      await browser.setDate("#action-in_service_date", "2026-11-03");
      await Bun.sleep(700);
      await browser.settle();
    },
  },
  {
    name: "52-depreciation-run-posted",
    path: async () => `${base}/d/depreciation_run/${await byMemo("depreciation_run", "Demo: depreciation to June")}`,
    height: 1100,
  },
  { name: "53-asset-list", path: `${base}/e/asset?sort=code` },
  {
    name: "54-vendor-invoice-without-order",
    path: `${base}/d/vendor_invoice/new?without=purchase_order_id`,
    height: 700,
    act: async (browser) => {
      await pick(browser, "#header-vendor_id", "V001");
      await browser.select('[aria-label="What line 1 is"]', "Account");
      await pick(browser, '[aria-label="Account, line 1"]', "6000");
      await browser.fill('[aria-label="Description, line 1"]', "Consulting, October");
      await browser.fill('[aria-label="Quantity invoiced, line 1"]', "4");
      await browser.fill('[aria-label="Unit price, line 1"]', "120");
    },
  },
  {
    name: "55-einvoice-export",
    height: 700,
    path: async () => `${base}/d/sales_invoice/${await byMemo("sales_invoice", "Demo: invoice to Buyer LLC")}`,
    act: async (browser) => {
      await browser.click("Export e-invoice");
      await browser.click("Export and download", "[role=dialog] button");
    },
  },
  {
    name: "56-einvoice-import",
    height: 1000,
    path: `${base}/run/einvoice.import`,
    act: async (browser) => {
      // The demo invoice as one Parts Inc sent to Ohio Widgets: an incoming XRechnung to read.
      const id = await byMemo("sales_invoice", "Demo: invoice to Buyer LLC");
      const written = asObject(await session.run("einvoice.export", { doc_type: "sales_invoice", id }));
      const xml = asString(written.xml)
        .replaceAll("DE123456789", "DE111222333")
        .replaceAll("DE987654321", "DE123456789")
        .replace(/<cbc:ID>[^<]*<\/cbc:ID>/, "<cbc:ID>PI-2026-0417</cbc:ID>");
      const dir = mkdtempSync(join(tmpdir(), "erpcraft-shots-"));
      const file = join(dir, "PI-2026-0417.xml");
      writeFileSync(file, xml);
      await browser.setFile("#einvoice-file", file);
      await pick(browser, "#einvoice-account", "6000");
      rmSync(dir, { recursive: true, force: true });
    },
  },
  {
    name: "57-customer-einvoice-party",
    height: 600,
    path: async () =>
      `${base}/e/customer/${asString((await first("entity.list", { entity: "customer", filters: { code: "C001" } })).id)}`,
  },
  {
    name: "58-company-logo-and-mail",
    height: 1400,
    path: async () => {
      // The demo's logo, put in for the picture when the company has none.
      const logo = asObject(await session.run("company_logo.get", { company_id: companyId }));
      if (logo.data === null) {
        const png = readFileSync(resolve(import.meta.dir, "fixtures", "logo.png")).toString("base64");
        await session.run("company_logo.set", { company_id: companyId, data: png });
        undo.push(() => session.run("company_logo.remove", { company_id: companyId }));
      }
      return `${base}/e/company/${companyId}`;
    },
  },
  {
    name: "59-send-preview",
    path: async () =>
      `${base}/d/sales_invoice/${asString((await first("document.list", { doc_type: "sales_invoice", company_id: companyId, status: "posted" })).id)}`,
    act: async (browser) => {
      await browser.click("Send by email");
      await browser.fill("#mail-to", "ap@buyer.example");
      await browser.click("Show the message", "[role=dialog] button");
    },
  },
  {
    name: "60-edi-review",
    height: 1300,
    path: async () => {
      const control = String(Date.now()).slice(-9);
      const content = readFileSync(resolve(import.meta.dir, "fixtures", "po850.x12"), "utf8")
        .replaceAll("{{CONTROL}}", control)
        .replaceAll("{{GROUP}}", control.slice(-4))
        .replaceAll("{{ORDER}}", `4500${control.slice(-6)}`);
      const got = asObject(await session.run("edi.receive", { content, company_id: companyId }));
      const message = asObject(asArray(got.messages)[0]);
      const id = asString(message.id);
      undo.push(() => session.run("edi.dismiss", { message_id: id, reason: "Received for a screenshot." }));
      return `${base}/e/edi_message/${id}`;
    },
  },
  {
    name: "48-sales-order-discounts",
    // Stock, charge and account lines side by side need the width.
    width: 1920,
    path: async () =>
      `${base}/d/sales_order/${await byMemo("sales_order", "Demo: order with a discount and freight (draft)")}`,
  },
  {
    name: "49-sales-invoice-charges",
    height: 1250,
    path: async () => {
      const order = await byMemo("sales_order", "Demo: widgets at 10 % off, with freight");
      const got = asObject(await session.run("document.get", { doc_type: "sales_order", id: order }));
      const invoice = asArray(got.links)
        .map(asObject)
        .find((link) => link.doc_type === "sales_invoice");
      return invoice ? `${base}/d/sales_invoice/${asString(invoice.id)}` : null;
    },
  },
  {
    // Last of the company's pictures: it leaves a form with unsaved changes.
    name: "21-line-error",
    path: `${base}/d/purchase_order/${await doc("purchase_order", "draft")}`,
    act: async (browser) => {
      await browser.fill('[aria-label="Quantity, line 1"]', "-4");
      await browser.click("Save");
    },
  },
];

if (bigId) {
  const awc = "/c/AWC";
  const bom = await maybe("entity.list", { entity: "bom", sort: ["code"] });
  shots.push(
    { name: "awc-01-customer-list", path: `${awc}/e/customer?page=3` },
    { name: "awc-02-customer-search", path: `${awc}/e/customer?q=bike&sort=-name` },
    { name: "awc-03-item-list", path: `${awc}/e/item?sort=name` },
    { name: "awc-04-purchase-orders", path: `${awc}/d/purchase_order?page=2` },
    { name: "awc-05-sales-orders-closed", path: `${awc}/d/sales_order?status=closed&page=40` },
    {
      name: "awc-06-purchase-order",
      path: `${awc}/d/purchase_order/${await docIn(bigId, "purchase_order", "closed")}`,
      height: 1100,
    },
    {
      name: "awc-07-sales-invoice",
      path: `${awc}/d/sales_invoice/${await docIn(bigId, "sales_invoice", "posted")}`,
      height: 1300,
    },
    { name: "awc-08-payment", path: `${awc}/d/payment/${await docIn(bigId, "payment", "posted")}`, height: 1000 },
    { name: "awc-09-trial-balance", path: `${awc}/r/gl.trial_balance`, height: 1300 },
    { name: "awc-10-balance-sheet", path: `${awc}/r/gl.balance_sheet?as_of=2024-12-31`, height: 1000 },
    { name: "awc-11-income-statement", path: `${awc}/r/gl.income_statement?from=2022-01-01&to=2025-12-31` },
    { name: "awc-12-stock-on-hand", path: `${awc}/r/stock.on_hand` },
    { name: "awc-13-tablet-orders", path: `${awc}/d/sales_order`, width: 820, height: 1100 },
    { name: "awc-14-open-items", path: `${awc}/r/open_item.list?kind=payable` },
    ...(bom ? [{ name: "awc-15-bill-of-materials", path: `${awc}/e/bom/${asString(bom.id)}` }] : []),
    { name: "awc-16-sales-invoices-by-total", path: `${awc}/d/sales_invoice?sort=-summary.total_gross` },
    { name: "awc-17-purchase-orders-by-vendor", path: `${awc}/d/purchase_order?sort=summary.vendor_name` },
  );
}

mkdirSync(OUT, { recursive: true });

const browser = await launchBrowser(9333);
async function undoAll(): Promise<void> {
  await browser.close();
  await removeDrafts();
  for (const step of atEnd.splice(0).reverse()) await step().catch(() => null);
}
undoOnSignal(undoAll);
try {
  for (const shot of shots) {
    if (only.length > 0 && !only.some((name) => shot.name.includes(name))) continue;
    const path = typeof shot.path === "string" ? shot.path : await shot.path();
    if (path === null) continue;
    await browser.viewport(shot.width ?? 1440, shot.height ?? 900);
    await browser.dark(shot.dark === true);
    if (shot.signedIn === false) {
      await browser.goto(`${APP}/sign-in`);
      await browser.evaluate("sessionStorage.clear()");
    }
    // The token goes in the fragment, which the app reads and removes at once.
    await browser.goto(`${APP}${path}${shot.signedIn === false ? "" : `#token=${shot.token ?? session.token}`}`);
    if (shot.act) await shot.act(browser);
    await browser.settle();
    const file = join(OUT, `${shot.name}.png`);
    await browser.screenshot(file);
    await removeDrafts();
    console.log(`ok   ${file}`);
  }
} finally {
  await undoAll();
}
