// Demo data for the admin UI, created through the API like any other client.
//
//   bun run seed
//
// Safe to run twice: every record is looked up before it is created (entities by
// their code, documents by their memo), and a document is only moved forward
// from the state it is in.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { asArray, asObject, asString, type Json, type JsonObject, type Session, signIn } from "./lib";

const session: Session = await signIn();
let created = 0;
let found = 0;

function log(what: string, state: "created" | "exists" | "done"): void {
  console.log(`${state.padEnd(8)}${what}`);
}

/** Find one record by exact field values, or create it. Returns its id. */
async function ensure(entity: string, match: JsonObject, rest: JsonObject = {}): Promise<string> {
  const filters: JsonObject = {};
  for (const [key, value] of Object.entries(match)) filters[key] = String(value);
  const list = asObject(await session.run("entity.list", { entity, filters, limit: 1 }));
  const existing = asArray(list.items)[0];
  const label = `${entity} ${Object.values(match).join(" ")}`;
  if (existing) {
    found += 1;
    log(label, "exists");
    return asString(asObject(existing).id);
  }
  const record = asObject(await session.run("entity.create", { entity, data: { ...match, ...rest } }));
  created += 1;
  log(label, "created");
  return asString(record.id);
}

interface Doc {
  id: string;
  status: string;
  number: string | null;
  body: JsonObject;
}

function toDoc(value: Json): Doc {
  const doc = asObject(value);
  return {
    id: asString(doc.id),
    status: asString(doc.status),
    number: typeof doc.number === "string" ? doc.number : null,
    body: asObject(doc.body),
  };
}

/** Find a document by its memo, or create it as a draft. */
async function ensureDoc(
  docType: string,
  companyId: string,
  memo: string,
  docDate: string,
  body: () => JsonObject | Promise<JsonObject>,
): Promise<Doc> {
  const list = asObject(
    await session.run("document.list", {
      doc_type: docType,
      company_id: companyId,
      where: `record.memo == '${memo}'`,
      limit: 1,
    }),
  );
  const existing = asArray(list.items)[0];
  if (existing) {
    found += 1;
    log(`${docType} "${memo}"`, "exists");
    return toDoc(await session.run("document.get", { doc_type: docType, id: asString(asObject(existing).id) }));
  }
  const doc = toDoc(
    await session.run("document.create", {
      doc_type: docType,
      company_id: companyId,
      doc_date: docDate,
      memo,
      body: await body(),
    }),
  );
  created += 1;
  log(`${docType} "${memo}"`, "created");
  return doc;
}

/** Run a workflow command on a document only if it is still in `from`. */
async function advance(docType: string, doc: Doc, from: string, action: string): Promise<Doc> {
  if (doc.status !== from) return doc;
  const next = toDoc(await session.run(`${docType}.${action}`, { id: doc.id }));
  log(`${docType}.${action} -> ${next.status}${next.number ? ` ${next.number}` : ""}`, "done");
  return next;
}

/** One line of a document body. */
const line = (values: JsonObject): JsonObject => values;

function lineId(doc: Doc, index: number): string {
  const line = asArray(doc.body.lines)[index];
  return asString(asObject(line).id);
}

// ------------------------------------------------------------ master data

const company = await ensure("company", { code: "OHIO" }, { name: "Ohio Widgets", base_currency: "USD" });
// A rate for the documents in euros, from the start of the demo year.
await ensure("currency", { code: "EUR" }, { name: "Euro", decimals: 2 });
await ensure("exchange_rate", { from_currency: "EUR", to_currency: "USD", rate_date: "2026-01-01" }, { rate: "1.08" });

const accounts: Record<string, string> = {};
for (const [code, name, type] of [
  ["1000", "Bank", "asset"],
  ["1200", "Accounts receivable", "asset"],
  ["1300", "Inventory", "asset"],
  ["1320", "Finished goods", "asset"],
  ["2000", "Accounts payable", "liability"],
  ["2100", "Goods received not invoiced", "liability"],
  ["2300", "Sales tax payable", "liability"],
  ["3000", "Owner's equity", "equity"],
  ["4000", "Sales revenue", "income"],
  ["5000", "Cost of goods sold", "expense"],
  ["5200", "Purchase price variance", "expense"],
  ["5300", "Sales tax expense", "expense"],
  ["6000", "Rent", "expense"],
] as const) {
  // The bank account is marked as one, so payments offer it.
  const data: JsonObject = { name, account_type: type };
  if (code === "1000") data.is_bank = true;
  accounts[code] = await ensure("account", { company_id: company, code }, data);
}
const account = (code: string): string => {
  const id = accounts[code];
  if (!id) throw new Error(`no account ${code}`);
  return id;
};

await ensure(
  "fiscal_period",
  { company_id: company, name: "2026" },
  { start_date: "2026-01-01", end_date: "2026-12-31" },
);

const widget = await ensure("item", { code: "WIDGET" }, { name: "Widget", uom: "ea" });
const gadget = await ensure("item", { code: "GADGET" }, { name: "Gadget", uom: "ea" });
await ensure("item", { code: "BOLT-M6" }, { name: "Bolt M6 x 20", uom: "box" });

const warehouse = await ensure("warehouse", { company_id: company, code: "MAIN" }, { name: "Main warehouse" });
await ensure("warehouse", { company_id: company, code: "DOCK" }, { name: "Receiving dock" });

const vendor = await ensure("vendor", { code: "V001" }, { name: "Parts Inc" });
await ensure("vendor", { code: "V002" }, { name: "Dayton Fasteners" });
const customer = await ensure("customer", { code: "C001" }, { name: "Buyer LLC" });
await ensure("customer", { code: "C002" }, { name: "Cleveland Hardware" });

// Which account stock is valued on. The company default is the rule without a scope.
await ensure("account_rule", { company_id: company, purpose: "inventory" }, { account_id: account("1300") });

await ensure(
  "purchasing_settings",
  { company_id: company },
  {
    grni_account_id: account("2100"),
    payable_account_id: account("2000"),
    price_variance_account_id: account("5200"),
  },
);
await ensure(
  "sales_settings",
  { company_id: company },
  {
    receivable_account_id: account("1200"),
    revenue_account_id: account("4000"),
    cogs_account_id: account("5000"),
  },
);
const taxCode = await ensure("tax_code", { code: "OH-SALES" }, { name: "Ohio sales tax", rate: "5.75" });
await ensure(
  "tax_settings",
  { company_id: company, tax_code_id: taxCode },
  { collected_account_id: account("2300"), paid_account_id: account("5300") },
);

// -------------------------------------------------------------- documents

// A posted journal entry.
const capital = await ensureDoc("journal_entry", company, "Demo: opening capital", "2026-01-02", () => ({
  lines: [
    line({ account_id: account("1000"), debit: "50000.00", memo: "Paid in by the owner" }),
    line({ account_id: account("3000"), credit: "50000.00" }),
  ],
}));
await advance("journal_entry", capital, "draft", "post");

// A purchase order, partly received and partly invoiced.
let purchaseOrder = await ensureDoc("purchase_order", company, "Demo: first stock order", "2026-02-03", () => ({
  vendor_id: vendor,
  lines: [
    { item_id: widget, warehouse_id: warehouse, qty: "100", unit_price: "4.00" },
    { item_id: gadget, warehouse_id: warehouse, qty: "50", unit_price: "9.50" },
  ],
}));
purchaseOrder = await advance("purchase_order", purchaseOrder, "draft", "approve");

const receipt = await ensureDoc(
  "purchase_receipt",
  company,
  "Demo: first delivery from Parts Inc",
  "2026-02-10",
  () => ({
    purchase_order_id: purchaseOrder.id,
    lines: [
      { order_line_id: lineId(purchaseOrder, 0), qty: "60" },
      { order_line_id: lineId(purchaseOrder, 1), qty: "50" },
    ],
  }),
);
await advance("purchase_receipt", receipt, "draft", "post");

const vendorInvoice = await ensureDoc(
  "vendor_invoice",
  company,
  "Demo: invoice for the widgets received",
  "2026-02-12",
  () => ({
    purchase_order_id: purchaseOrder.id,
    vendor_reference: "INV-7731",
    lines: [{ order_line_id: lineId(purchaseOrder, 0), qty: "60", unit_price: "4.00", tax_code_id: taxCode }],
  }),
);
await advance("vendor_invoice", vendorInvoice, "draft", "post");

// A sales order, delivered and invoiced in full.
let salesOrder = await ensureDoc("sales_order", company, "Demo: order from Buyer LLC", "2026-03-02", () => ({
  customer_id: customer,
  lines: [{ item_id: widget, warehouse_id: warehouse, qty: "20", unit_price: "9.00" }],
}));
salesOrder = await advance("sales_order", salesOrder, "draft", "approve");

const delivery = await ensureDoc("sales_delivery", company, "Demo: shipped to Buyer LLC", "2026-03-04", () => ({
  sales_order_id: salesOrder.id,
  lines: [{ order_line_id: lineId(salesOrder, 0), qty: "20" }],
}));
await advance("sales_delivery", delivery, "draft", "post");

const salesInvoice = await ensureDoc("sales_invoice", company, "Demo: invoice to Buyer LLC", "2026-03-05", () => ({
  sales_order_id: salesOrder.id,
  lines: [{ order_line_id: lineId(salesOrder, 0), qty: "20", unit_price: "9.00", tax_code_id: taxCode }],
}));
await advance("sales_invoice", salesInvoice, "draft", "post");

// One payment: the customer pays part of the invoice.
const payment = await ensureDoc("payment", company, "Demo: part payment from Buyer LLC", "2026-03-20", async () => {
  const open = asObject(await session.run("open_item.list", { company_id: company, kind: "receivable" }));
  const item = asArray(open.items)[0];
  if (!item) throw new Error("no open receivable to pay; was the sales invoice posted?");
  return {
    direction: "incoming",
    bank_account_id: account("1000"),
    allocations: [{ open_item_id: asString(asObject(item).id), amount: "100.00" }],
  };
});
await advance("payment", payment, "draft", "post");

// Drafts, to show the editable screens.
await ensureDoc("journal_entry", company, "Demo: March rent (draft)", "2026-03-31", () => ({
  lines: [
    line({ account_id: account("6000"), debit: "1800.00", memo: "Warehouse rent, March" }),
    line({ account_id: account("1000"), credit: "1800.00" }),
  ],
}));
await ensureDoc("purchase_order", company, "Demo: restock gadgets (draft)", "2026-04-01", () => ({
  vendor_id: vendor,
  lines: [{ item_id: gadget, warehouse_id: warehouse, qty: "25", unit_price: "9.75" }],
}));
await ensureDoc("stock_adjustment", company, "Demo: count correction (draft)", "2026-04-02", () => ({
  offset_account_id: account("5000"),
  lines: [{ item_id: widget, warehouse_id: warehouse, qty: "-2" }],
}));

// ------------------------------------------------------------ payment terms

const net30 = await ensure("payment_terms", { code: "NET30" }, { name: "Net 30 days", net_days: 30 });
const early = await ensure(
  "payment_terms",
  { code: "2-10-NET30" },
  { name: "2 % in 10 days, net 30", net_days: 30, discount_percent: "2", discount_days: 10 },
);

/**
 * An order on terms, received and invoiced on the given day, posted. Each step is
 * taken only if the order does not lead to it yet, so a second run changes nothing.
 */
async function invoicedOnTerms(memo: string, terms: string, orderDate: string, invoiceDate: string): Promise<void> {
  let order = await ensureDoc("purchase_order", company, memo, orderDate, () => ({
    vendor_id: vendor,
    payment_terms_id: terms,
    lines: [line({ item_id: widget, warehouse_id: warehouse, qty: "10", unit_price: "4.00" })],
  }));
  order = await advance("purchase_order", order, "draft", "approve");
  const links = (): Promise<JsonObject[]> =>
    session
      .run("document.get", { doc_type: "purchase_order", id: order.id })
      .then((doc) => asArray(asObject(doc).links).map(asObject));
  if (!(await links()).some((link) => link.doc_type === "purchase_receipt")) {
    const receipt = toDoc(await session.run("purchase_order.receive", { id: order.id, doc_date: invoiceDate }));
    await advance("purchase_receipt", receipt, "draft", "post");
  }
  if (!(await links()).some((link) => link.doc_type === "vendor_invoice")) {
    const invoice = toDoc(await session.run("purchase_order.invoice", { id: order.id, doc_date: invoiceDate }));
    await advance("vendor_invoice", invoice, "draft", "post");
  }
}
// Long past due, for the aging buckets; and one whose early-payment discount is still open.
await invoicedOnTerms("Demo: order on net 30, invoiced in August", net30, "2026-07-28", "2026-08-05");
await invoicedOnTerms("Demo: order on 2 % 10 days", early, "2026-11-10", "2026-11-12");

// ------------------------------------------------------------ banking

// A checking account with a June statement to match: a rent payment recorded
// before, a payment of the demo vendor invoice, and a bank fee. Importing the
// same file again returns the same statement, so this stays idempotent.
const checking = await ensure(
  "account",
  { company_id: company, code: "1010" },
  { name: "Checking (demo)", account_type: "asset", is_bank: true },
);
// The number the bank's files name the account by: a statement is imported only into the account that has it.
await ensure(
  "bank_account_number",
  { company_id: company, bank_account_id: checking },
  { iban: "US00EXAMPLE000000001" },
);
const junRent = await ensureDoc("journal_entry", company, "Demo: storage rent from checking", "2026-06-03", () => ({
  lines: [
    line({ account_id: account("6000"), debit: "25.00", credit: "0" }),
    line({ account_id: checking, debit: "0", credit: "25.00" }),
  ],
}));
await advance("journal_entry", junRent, "draft", "post");
const statementFile = readFileSync(resolve(import.meta.dir, "fixtures/demo.camt053.xml"), "utf8");
const statement = asObject(
  await session.run("bank_statement.import", { bank_account_id: checking, format: "camt053", content: statementFile }),
);
log(`bank_statement June ${asString(statement.status)}`, "done");

// ------------------------------------------------------------ discounts and charges

// Freight is a charge: a line that is not stock, earning on an account of its own.
const freightIncome = await ensure(
  "account",
  { company_id: company, code: "4100" },
  { name: "Freight income", account_type: "income" },
);
const freight = await ensure("charge", { code: "FREIGHT" }, { name: "Freight", kind: "freight" });
await ensure(
  "account_rule",
  { company_id: company, purpose: "charge_revenue", scope_type: "charge", scope_id: freight },
  { account_id: freightIncome },
);
// An order at a discount with freight on top, delivered and invoiced.
let discounted = await ensureDoc(
  "sales_order",
  company,
  "Demo: widgets at 10 % off, with freight",
  "2026-11-02",
  () => ({
    customer_id: customer,
    lines: <JsonObject[]>[
      { item_id: widget, warehouse_id: warehouse, qty: "10", unit_price: "9.00", discount_percent: "10" },
      { charge_id: freight, description: "Freight to Columbus", qty: "1", unit_price: "15.00" },
    ],
  }),
);
discounted = await advance("sales_order", discounted, "draft", "approve");
const salesLinks = (): Promise<JsonObject[]> =>
  session
    .run("document.get", { doc_type: "sales_order", id: discounted.id })
    .then((doc) => asArray(asObject(doc).links).map(asObject));
if (!(await salesLinks()).some((link) => link.doc_type === "sales_delivery")) {
  const shipped = toDoc(await session.run("sales_order.deliver", { id: discounted.id, doc_date: "2026-11-03" }));
  await advance("sales_delivery", shipped, "draft", "post");
}
if (!(await salesLinks()).some((link) => link.doc_type === "sales_invoice")) {
  const invoiced = toDoc(await session.run("sales_order.invoice", { id: discounted.id, doc_date: "2026-11-04" }));
  await advance("sales_invoice", invoiced, "draft", "post");
}

// The same kinds of line on an order still being written: stock at a discount, freight, a service on an account.
await ensureDoc("sales_order", company, "Demo: order with a discount and freight (draft)", "2026-11-16", () => ({
  customer_id: customer,
  lines: <JsonObject[]>[
    { item_id: gadget, warehouse_id: warehouse, qty: "12", unit_price: "18.00", discount_percent: "5" },
    { item_id: widget, warehouse_id: warehouse, qty: "40", unit_price: "9.00", discount_amount: "20.00" },
    { charge_id: freight, description: "Freight, two pallets", qty: "2", unit_price: "35.00" },
    { account_id: account("4000"), description: "Assembly, per hour", qty: "3", unit_price: "45.00" },
  ],
}));

// ------------------------------------------------------------ fixed assets

// Equipment, depreciated straight over five years, on accounts of its own.
const assetAccounts: Record<string, string> = {};
for (const [code, name, type] of [
  ["1500", "Machinery and vehicles", "asset"],
  ["1590", "Accumulated depreciation", "asset"],
  ["6100", "Depreciation", "expense"],
  ["4900", "Gain on disposal of assets", "income"],
  ["6900", "Loss on disposal of assets", "expense"],
] as const) {
  assetAccounts[code] = await ensure("account", { company_id: company, code }, { name, account_type: type });
}
const equipment = await ensure(
  "asset_class",
  { company_id: company, code: "EQUIP" },
  { name: "Machinery and vehicles", method: "straight_line", useful_life_months: 60 },
);
for (const [purpose, code] of [
  ["asset_cost", "1500"],
  ["accumulated_depreciation", "1590"],
  ["depreciation_expense", "6100"],
  ["asset_disposal_gain", "4900"],
  ["asset_disposal_loss", "6900"],
] as const) {
  await ensure(
    "account_rule",
    { company_id: company, purpose, scope_type: "asset_class", scope_id: equipment },
    { account_id: assetAccounts[code] ?? "" },
  );
}

/** An asset, made planned; `then` capitalises or takes it over once. */
async function asset(code: string, name: string, then: (id: string) => Promise<unknown>): Promise<string> {
  const id = await ensure("asset", { company_id: company, code }, { class_id: equipment, name });
  const record = asObject(await session.run("entity.get", { entity: "asset", id }));
  if (record.status === "planned" && record.cost == null) {
    await then(id);
    log(`asset ${code} in the books`, "done");
  }
  return id;
}

// Bought this year and booked by a journal entry: capitalised from its line.
const machineBought = await ensureDoc("journal_entry", company, "Demo: packaging machine bought", "2026-01-15", () => ({
  lines: [
    line({ account_id: assetAccounts["1500"] ?? "", debit: "12000.00", credit: "0" }),
    line({ account_id: account("1000"), debit: "0", credit: "12000.00" }),
  ],
}));
const machinePosted = await advance("journal_entry", machineBought, "draft", "post");
await asset("MACH-01", "Packaging machine", (id) =>
  session.run("asset.capitalise", {
    id,
    document_id: machinePosted.id,
    line_id: lineId(machinePosted, 0),
    in_service_date: "2026-02-01",
  }),
);
// A van taken over from the old books, with its opening entry.
const vanOpening = await ensureDoc("journal_entry", company, "Demo: delivery van taken over", "2026-01-01", () => ({
  lines: [
    line({ account_id: assetAccounts["1500"] ?? "", debit: "30000.00", credit: "0" }),
    line({ account_id: assetAccounts["1590"] ?? "", debit: "0", credit: "6000.00" }),
    line({ account_id: account("3000"), debit: "0", credit: "24000.00" }),
  ],
}));
await advance("journal_entry", vanOpening, "draft", "post");
await asset("VAN-01", "Delivery van", (id) =>
  session.run("asset.take_over", {
    id,
    cost: "30000.00",
    accumulated_depreciation: "6000.00",
    acquisition_date: "2024-01-10",
    in_service_date: "2024-01-10",
    depreciated_through: "2025-12-31",
    opening_date: "2026-01-01",
  }),
);
// A forklift taken over too, to be sold.
const forkliftOpening = await ensureDoc("journal_entry", company, "Demo: forklift taken over", "2026-01-01", () => ({
  lines: [
    line({ account_id: assetAccounts["1500"] ?? "", debit: "8000.00", credit: "0" }),
    line({ account_id: assetAccounts["1590"] ?? "", debit: "0", credit: "4000.00" }),
    line({ account_id: account("3000"), debit: "0", credit: "4000.00" }),
  ],
}));
await advance("journal_entry", forkliftOpening, "draft", "post");
const forklift = await asset("LIFT-01", "Forklift", (id) =>
  session.run("asset.take_over", {
    id,
    cost: "8000.00",
    accumulated_depreciation: "4000.00",
    acquisition_date: "2023-07-01",
    in_service_date: "2023-07-01",
    depreciated_through: "2025-12-31",
    opening_date: "2026-01-01",
  }),
);
// Bought and booked, but not capitalised yet: the asset waits, planned, for its line to be chosen.
const attachmentBought = await ensureDoc(
  "journal_entry",
  company,
  "Demo: forklift attachment bought",
  "2026-11-03",
  () => ({
    lines: [
      line({
        account_id: assetAccounts["1500"] ?? "",
        debit: "2400.00",
        credit: "0",
        memo: "Side shifter for the forklift",
      }),
      line({ account_id: account("1000"), debit: "0", credit: "2400.00" }),
    ],
  }),
);
await advance("journal_entry", attachmentBought, "draft", "post");
await ensure("asset", { company_id: company, code: "ATT-01" }, { class_id: equipment, name: "Forklift side shifter" });
// Depreciation posted to the end of June; the run to the end of October waits as a draft.
const june = await ensureDoc("depreciation_run", company, "Demo: depreciation to June", "2026-06-30", () => ({}));
await advance("depreciation_run", june, "draft", "post");
await ensureDoc("depreciation_run", company, "Demo: depreciation to October", "2026-10-31", () => ({}));
// The forklift sold, waiting to be posted.
await ensureDoc("asset_disposal", company, "Demo: forklift sold", "2026-11-14", () => ({
  asset_id: forklift,
  proceeds: "2500.00",
  proceeds_account_id: account("1000"),
}));

// ------------------------------------------------------------ e-invoicing

// What an EN 16931 invoice needs to say about the company, its bank account, a customer and a
// vendor, and the VAT category of the sales tax. Example values: no real company or person.
await ensure(
  "bank_account_number",
  { company_id: company, bank_account_id: account("1000") },
  { iban: "DE02120300000000202051", bic: "BYLADEM1001" },
);
// Party data: the company's, the customer's and the vendor's particulars, addresses, tax
// identifiers, electronic addresses and contacts (`party.set` replaces each part it is given,
// so running this twice changes nothing). Addresses already on record are kept as they are, with
// their ids, since documents may name them.
async function setParty(input: JsonObject): Promise<void> {
  const current = asObject(
    await session.run("party.get", { party_type: input.party_type ?? null, party_id: input.party_id ?? null }),
  );
  const { addresses, ...rest } = input;
  await session.run(
    "party.set",
    asArray(current.addresses).length > 0 ? rest : { ...rest, addresses: addresses ?? null },
  );
}
await setParty({
  party_type: "company",
  party_id: company,
  profile: {
    legal_name: "Ohio Widgets LLC",
    legal_registration: "Example registration 12345",
    payment_account_id: account("1000"),
  },
  addresses: [
    {
      kind: "billing",
      is_default: true,
      street: "100 Example Street",
      city: "Columbus",
      postcode: "43215",
      region: "OH",
      country_code: "US",
    },
  ],
  tax_ids: [{ scheme: "eu_vat", value: "DE123456789", country_code: "DE" }],
  electronic_addresses: [{ scheme: "EM", value: "invoices@ohio-widgets.example", is_default: true }],
  contacts: [
    { name: "Accounts (example)", phone: "+1 614 555 0100", email: "accounts@ohio-widgets.example", is_default: true },
  ],
});
await setParty({
  party_type: "customer",
  party_id: customer,
  profile: { legal_name: "Buyer LLC", invoice_reference: "BUYER-2026" },
  addresses: [
    {
      kind: "billing",
      is_default: true,
      street: "200 Example Avenue",
      city: "Cleveland",
      postcode: "44114",
      region: "OH",
      country_code: "US",
    },
    {
      kind: "shipping",
      is_default: true,
      attention: "Receiving dock",
      street: "210 Example Avenue",
      city: "Cleveland",
      postcode: "44114",
      region: "OH",
      country_code: "US",
    },
  ],
  tax_ids: [{ scheme: "eu_vat", value: "DE987654321", country_code: "DE" }],
  electronic_addresses: [{ scheme: "EM", value: "invoices@buyer.example", is_default: true }],
  contacts: [{ name: "Accounts payable (example)", email: "ap@buyer.example", is_default: true }],
});
await setParty({
  party_type: "vendor",
  party_id: vendor,
  profile: { legal_name: "Parts Inc" },
  addresses: [
    {
      kind: "billing",
      is_default: true,
      street: "300 Example Road",
      city: "Dayton",
      postcode: "45402",
      region: "OH",
      country_code: "US",
    },
  ],
  tax_ids: [{ scheme: "eu_vat", value: "DE111222333", country_code: "DE" }],
  electronic_addresses: [{ scheme: "EM", value: "billing@parts.example", is_default: true }],
});
log("party data of the company, the customer and the vendor", "done");
await ensure("einvoice_tax_category", { tax_code_id: taxCode }, { category: "S" });

console.log(`\n${created} created, ${found} already there.`);
