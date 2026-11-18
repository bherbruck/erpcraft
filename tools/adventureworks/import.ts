#!/usr/bin/env bun
// Loads Microsoft's AdventureWorks sample company into a running erpcraft
// server, through its public HTTP API only. See README.md.
//
//   bun run import.ts [--limit-orders N | --all] [--concurrency N]
//                     [--company-code CODE] [--close-years]
//                     [--base-url URL] [--env PATH] [--steps a,b,c] [--label NAME]
//
// No dependencies. Safe to run again: everything is looked up before it is
// created (master data by code, documents by memo).

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

// ------------------------------------------------------------------ options

const HERE = dirname(new URL(import.meta.url).pathname);
const DATA = join(HERE, "data");
const SOURCE =
  "https://raw.githubusercontent.com/microsoft/sql-server-samples/master/samples/databases/adventure-works/oltp-install-script/";
const FILES = [
  "Product", "BillOfMaterials", "Vendor", "ProductVendor", "PurchaseOrderHeader",
  "PurchaseOrderDetail", "Customer", "Person", "Store", "SalesOrderHeader", "SalesOrderDetail",
];
const ALL_STEPS = [
  "setup", "items", "vendors", "customers", "boms", "purchases", "opening_stock", "sales", "year_end", "checks",
];

function option(name: string): string | undefined {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : undefined;
}
const ALL = process.argv.includes("--all");
const LIMIT = ALL ? Infinity : Number(option("limit-orders") ?? 300);
const CONCURRENCY = Number(option("concurrency") ?? 4);
const BASE = (option("base-url") ?? "http://127.0.0.1:8600").replace(/\/$/, "");
const ENV_FILE = resolve(option("env") ?? join(HERE, "..", "..", ".env"));
const STEPS = (option("steps") ?? ALL_STEPS.join(",")).split(",").map((s) => s.trim());
const LABEL = option("label") ?? (ALL ? "all" : String(LIMIT));
if (!(LIMIT > 0) || !(CONCURRENCY > 0)) throw new Error("--limit-orders and --concurrency must be positive numbers");
for (const s of STEPS) if (!ALL_STEPS.includes(s)) throw new Error(`unknown step '${s}'; steps are ${ALL_STEPS.join(", ")}`);

const CODE = option("company-code") ?? "AWC";
const CLOSE_YEARS = process.argv.includes("--close-years");
const COMPANY = { code: CODE, name: CODE === "AWC" ? "Adventure Works Cycles" : `Adventure Works Cycles (${CODE})`, base_currency: "USD" };
const WAREHOUSE = { code: "MAIN", name: "Main warehouse" };
const TAX = { code: "AW-ST8", name: "Sales tax 8% (AdventureWorks)", rate: "8" };
/// Days a vendor or a reseller is given to pay. Card sales are paid on the invoice date.
const TERMS_DAYS = 30;
/// Paid in by the owners on the first day, so the bank account can pay for three years of purchases.
const OPENING_CAPITAL = "25000000.00";
const ACCOUNTS: [string, string, string][] = [
  ["1000", "Cash at bank", "asset"],
  ["1200", "Accounts receivable", "asset"],
  ["1300", "Inventory", "asset"],
  ["2000", "Accounts payable", "liability"],
  ["2100", "Goods received not invoiced", "liability"],
  ["2200", "Sales tax payable", "liability"],
  ["3000", "Opening balance equity", "equity"],
  ["3100", "Retained earnings", "equity"],
  ["3200", "Share capital", "equity"],
  ["4000", "Sales revenue", "income"],
  ["5000", "Cost of goods sold", "expense"],
  ["5200", "Purchase price variance", "expense"],
  ["5300", "Sales tax on purchases", "expense"],
];

// --------------------------------------------------------------- source data

async function download(): Promise<void> {
  mkdirSync(DATA, { recursive: true });
  for (const name of FILES) {
    const path = join(DATA, `${name}.csv`);
    if (existsSync(path)) continue;
    const response = await fetch(SOURCE + `${name}.csv`);
    if (!response.ok) {
      throw new Error(`cannot download ${SOURCE}${name}.csv: HTTP ${response.status}. Nothing was invented; stopping.`);
    }
    writeFileSync(path, new Uint8Array(await response.arrayBuffer()));
    console.log(`downloaded ${name}.csv`);
  }
}

/// Decode by byte-order mark (UTF-16 either way, UTF-8), else UTF-8.
function decode(bytes: Uint8Array): string {
  if (bytes[0] === 0xff && bytes[1] === 0xfe) return new TextDecoder("utf-16le").decode(bytes.subarray(2));
  if (bytes[0] === 0xfe && bytes[1] === 0xff) return new TextDecoder("utf-16be").decode(bytes.subarray(2));
  if (bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) return new TextDecoder("utf-8").decode(bytes.subarray(3));
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

/// Rows of one file. Two formats occur: tab-separated with newline rows,
/// and `+|` between fields with `&|` ending each row (used where a field
/// holds XML). The format is recognised from the content.
function rows(name: string, width: number): string[][] {
  const text = decode(new Uint8Array(readFileSync(join(DATA, `${name}.csv`))));
  const piped = text.includes("+|") && text.includes("&|");
  const [fieldSep, rowSep] = piped ? ["+|", "&|"] : ["\t", "\n"];
  const out: string[][] = [];
  for (const raw of text.split(rowSep)) {
    const line = raw.replace(/^[\r\n]+|[\r\n]+$/g, "");
    if (line === "") continue;
    const fields = line.split(fieldSep).map((f) => f.trim());
    if (fields.length !== width) {
      throw new Error(`${name}.csv: a row has ${fields.length} fields, expected ${width}: ${line.slice(0, 120)}`);
    }
    out.push(fields);
  }
  return out;
}

const day = (stamp: string) => stamp.slice(0, 10);
function addDays(date: string, days: number): string {
  const d = new Date(`${date}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + days);
  return d.toISOString().slice(0, 10);
}

/// A decimal string as an integer scaled by 10^places (exact).
function scaled(text: string, places: number): bigint {
  const negative = text.startsWith("-");
  const [whole, frac = ""] = text.replace("-", "").split(".");
  if (frac.length > places && /[1-9]/.test(frac.slice(places))) throw new Error(`'${text}' has more than ${places} decimals`);
  const value = BigInt((whole || "0") + frac.padEnd(places, "0").slice(0, places));
  return negative ? -value : value;
}
function unscaled(value: bigint, places: number): string {
  const negative = value < 0n;
  const digits = (negative ? -value : value).toString().padStart(places + 1, "0");
  const text = `${digits.slice(0, -places)}.${digits.slice(-places)}`.replace(/\.?0+$/, "");
  return (negative ? "-" : "") + text;
}
/// Normalise a source decimal such as `.0000` or `3.00`.
const num = (text: string) => unscaled(scaled(text, 6), 6);
/// The unit price after the line's discount fraction, to 6 decimals, half up.
function netPrice(unitPrice: string, discount: string): string {
  const eight = scaled(unitPrice, 4) * (10000n - scaled(discount, 4)); // 8 decimals
  return unscaled((eight + 50n) / 100n, 6);
}

interface Product { id: string; name: string; number: string; make: boolean; finished: boolean; cost: string }
interface PoLine { product: string; ordered: string; price: string; received: string; rejected: string; stocked: string }
interface Po { id: string; status: string; vendor: string; orderDate: string; shipDate: string; subTotal: string; taxAmt: string; freight: string; lines: PoLine[] }
interface SoLine { product: string; qty: string; price: string; discount: string; lineTotal: string }
interface So { id: string; number: string; orderDate: string; shipDate: string; online: boolean; customer: string; foreign: boolean; subTotal: string; taxAmt: string; freight: string; lines: SoLine[] }

function loadSource() {
  const products = new Map<string, Product>();
  for (const r of rows("Product", 25)) {
    products.set(r[0], { id: r[0], name: r[1], number: r[2], make: r[3] === "1", finished: r[4] === "1", cost: num(r[8]) });
  }
  const vendors = rows("Vendor", 8).map((r) => ({ id: r[0], code: r[1], name: r[2], active: r[5] === "1" }));
  const stores = new Map(rows("Store", 6).map((r) => [r[0], r[1]]));
  const people = new Map(rows("Person", 13).map((r) => [r[0], `${r[4]} ${r[6]}`.trim()]));
  const customers = new Map<string, { id: string; code: string; name: string; store: boolean; person: boolean }>();
  for (const r of rows("Customer", 7)) {
    const name = (r[2] && stores.get(r[2])) || (r[1] && people.get(r[1]));
    if (!name) throw new Error(`customer ${r[0]} has neither a known store nor a known person`);
    customers.set(r[0], { id: r[0], code: r[4], name, store: r[2] !== "", person: r[1] !== "" });
  }
  // Current components only (no end date), one level: the direct
  // components of each assembly. Rows without an assembly are the top of a
  // tree, not a component of anything.
  const boms = new Map<string, { component: string; qty: string }[]>();
  let bomRows = 0, bomEnded = 0, bomTop = 0;
  for (const r of rows("BillOfMaterials", 9)) {
    bomRows++;
    if (r[4] !== "") { bomEnded++; continue; }
    if (r[1] === "") { bomTop++; continue; }
    if (!boms.has(r[1])) boms.set(r[1], []);
    boms.get(r[1])!.push({ component: r[2], qty: num(r[7]) });
  }

  const poLines = new Map<string, PoLine[]>();
  for (const r of rows("PurchaseOrderDetail", 11)) {
    if (!poLines.has(r[0])) poLines.set(r[0], []);
    poLines.get(r[0])!.push({ product: r[4], ordered: num(r[3]), price: num(r[5]), received: num(r[7]), rejected: num(r[8]), stocked: num(r[9]) });
  }
  const pos: Po[] = rows("PurchaseOrderHeader", 13).map((r) => ({
    id: r[0], status: r[2], vendor: r[4], orderDate: day(r[6]), shipDate: day(r[7]),
    subTotal: r[8], taxAmt: r[9], freight: r[10], lines: poLines.get(r[0]) ?? [],
  }));
  pos.sort((a, b) => a.orderDate.localeCompare(b.orderDate) || Number(a.id) - Number(b.id));

  const soLines = new Map<string, SoLine[]>();
  for (const r of rows("SalesOrderDetail", 11)) {
    if (!soLines.has(r[0])) soLines.set(r[0], []);
    soLines.get(r[0])!.push({ product: r[4], qty: num(r[3]), price: r[6], discount: r[7], lineTotal: r[8] });
  }
  const sos: So[] = rows("SalesOrderHeader", 26).map((r) => ({
    id: r[0], number: r[7], orderDate: day(r[2]), shipDate: day(r[4]), online: r[6] === "1",
    customer: r[10], foreign: r[18] !== "", subTotal: r[19], taxAmt: r[20], freight: r[21], lines: soLines.get(r[0]) ?? [],
  }));
  sos.sort((a, b) => a.orderDate.localeCompare(b.orderDate) || Number(a.id) - Number(b.id));

  return { products, vendors, customers, boms, bomStats: { bomRows, bomEnded, bomTop }, pos, sos };
}

// ---------------------------------------------------------------- API client

interface Sample { step: string; label: string; ms: number; status: number; at: number; lines?: number }
/// Lines of the order a document belongs to, by the order's memo.
const linesOf = new Map<string, number>();
interface Failure { step: string; label: string; status: number; message: string; context: string }
const samples: Sample[] = [];
const failures: Failure[] = [];
let currentStep = "start";
let token = "";

function readEnv(): Record<string, string> {
  const env: Record<string, string> = {};
  for (const line of readFileSync(ENV_FILE, "utf8").split("\n")) {
    const match = line.match(/^\s*(?:export\s+)?([A-Z0-9_]+)\s*=\s*(.*)\s*$/);
    if (match) env[match[1]] = match[2].replace(/^(['"])(.*)\1$/, "$2");
  }
  return env;
}

/// A change to an existing record (switching a vendor on, then off) is a
/// new act in every run, unlike a create: its key is tied to this run, or
/// the server would answer the second run from the first one's result.
const RUN = String(Date.now());
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/// One request. Every write carries an idempotency key derived from the
/// request itself, so a retry after a lost answer cannot do the work twice.
/// Transport failures and 502/503/504 are retried; anything the server
/// answers is returned as it is and recorded when it is a refusal.
async function call(method: string, path: string, body?: unknown, context = ""): Promise<{ status: number; body: any }> {
  const label = `${method} ${path.split("?")[0].replace(/\/[0-9a-f]{8}-[0-9a-f-]{27}$/, "/{id}")}`;
  const payload = body === undefined ? undefined : JSON.stringify(body);
  const headers: Record<string, string> = { authorization: `Bearer ${token}` };
  if (payload !== undefined) headers["content-type"] = "application/json";
  if (method !== "GET") {
    headers["idempotency-key"] = "aw-" + createHash("sha256").update(`${method} ${path}\n${payload ?? ""}${method === "PATCH" ? RUN : ""}`).digest("hex").slice(0, 40);
  }
  for (let attempt = 1; ; attempt++) {
    const started = performance.now();
    let status = 0;
    let parsed: any = null;
    let transport = "";
    try {
      const response = await fetch(BASE + path, { method, headers, body: payload, signal: AbortSignal.timeout(120_000) });
      status = response.status;
      const text = await response.text();
      try { parsed = text ? JSON.parse(text) : null; } catch { parsed = text; }
    } catch (error) {
      transport = String((error as Error)?.message ?? error);
    }
    samples.push({
      step: currentStep, label, ms: performance.now() - started, status, at: Date.now(),
      lines: linesOf.get(context.replace(/ (receipt|invoice|delivery|payment)$/, "")),
    });
    const unfinished = status === 409 && String(parsed?.error?.message ?? "").includes("has not finished");
    const retryable = status === 0 || status === 502 || status === 503 || status === 504 || unfinished;
    if (status >= 400 || status === 0) {
      const message = status === 0 ? `transport: ${transport}` : String(parsed?.error?.message ?? JSON.stringify(parsed).slice(0, 300));
      failures.push({ step: currentStep, label, status, message, context: `${context}${attempt > 1 ? ` (attempt ${attempt})` : ""}` });
    }
    if (retryable && attempt < 4) { await sleep(500 * attempt); continue; }
    if (status === 0) throw new Error(`no answer from ${BASE} for ${label}: ${transport}`);
    return { status, body: parsed };
  }
}

class Refused extends Error {}
/// A request that must succeed for the caller to go on.
async function must(method: string, path: string, body?: unknown, context = ""): Promise<any> {
  const answer = await call(method, path, body, context);
  if (answer.status >= 400) throw new Refused(`${method} ${path}: ${answer.status} ${answer.body?.error?.message ?? ""}`);
  return answer.body;
}
const command = (name: string, input: unknown, context = "") => must("POST", `/api/commands/${name}`, input, context);

function query(params: Record<string, string | number>): string {
  return Object.entries(params).map(([k, v]) => `${k}=${encodeURIComponent(String(v))}`).join("&");
}
/// Every record of a list, page by page.
async function listAll(path: string, params: Record<string, string | number> = {}): Promise<any[]> {
  const out: any[] = [];
  for (let offset = 0; ; offset += 500) {
    const page = await must("GET", `${path}?${query({ ...params, sort: "created_at", limit: 500, offset })}`);
    out.push(...page.items);
    if (out.length >= page.total || page.items.length === 0) return out;
  }
}

/// Run `work` over `items`, at most CONCURRENCY at a time.
async function pool<T>(items: T[], work: (item: T) => Promise<void>): Promise<void> {
  let next = 0;
  const runner = async () => {
    while (next < items.length) await work(items[next++]);
  };
  await Promise.all(Array.from({ length: Math.min(CONCURRENCY, items.length) }, runner));
}

// ------------------------------------------------------------------ counters

const counts: Record<string, { created: number; existing: number; failed: number }> = {};
function count(kind: string, what: "created" | "existing" | "failed", n = 1) {
  counts[kind] ??= { created: 0, existing: 0, failed: 0 };
  counts[kind][what] += n;
}
const notes: string[] = [];
const stepTimes: { step: string; seconds: number; requests: number }[] = [];
async function step(name: string, work: () => Promise<void>) {
  if (!STEPS.includes(name)) return;
  currentStep = name;
  const before = samples.length;
  const started = performance.now();
  await work();
  const seconds = (performance.now() - started) / 1000;
  stepTimes.push({ step: name, seconds, requests: samples.length - before });
  console.log(`${name.padEnd(14)} ${seconds.toFixed(1).padStart(8)} s  ${String(samples.length - before).padStart(7)} requests`);
}

/// Create a record unless one with this code is already there. `known`
/// maps codes to ids and is filled in.
async function ensure(entity: string, kind: string, known: Map<string, string>, code: string, data: Record<string, unknown>): Promise<string | undefined> {
  const found = known.get(code);
  if (found) { count(kind, "existing"); return found; }
  const answer = await call("POST", `/api/entities/${entity}`, data, `${entity} ${code}`);
  if (answer.status >= 400) { count(kind, "failed"); return undefined; }
  known.set(code, answer.body.id);
  count(kind, "created");
  return answer.body.id;
}
async function codes(entity: string, params: Record<string, string> = {}): Promise<Map<string, string>> {
  return new Map((await listAll(`/api/entities/${entity}`, params)).map((r) => [r.code, r.id]));
}

// ---------------------------------------------------------------------- main

interface Doc { id: string; status: string; number: string | null }

async function main() {
  const startedAt = new Date();
  const loadStarted = performance.now();
  await download();
  const src = loadSource();
  stepTimes.push({ step: "download and parse", seconds: (performance.now() - loadStarted) / 1000, requests: 0 });
  const pos = src.pos.slice(0, LIMIT);
  const sos = src.sos.slice(0, LIMIT);
  console.log(`source: ${src.products.size} products, ${src.vendors.length} vendors, ${src.customers.size} customers, ` +
    `${src.boms.size} current bills of materials, ${src.pos.length} purchase orders, ${src.sos.length} sales orders`);
  console.log(`loading ${pos.length} purchase orders and ${sos.length} sales orders, ${CONCURRENCY} requests at a time`);

  const env = readEnv();
  if (!env.ERPCRAFT_ADMIN_EMAIL || !env.ERPCRAFT_ADMIN_PASSWORD) throw new Error(`${ENV_FILE} lacks ERPCRAFT_ADMIN_EMAIL or ERPCRAFT_ADMIN_PASSWORD`);
  const login = await fetch(`${BASE}/api/auth/login`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ email: env.ERPCRAFT_ADMIN_EMAIL, password: env.ERPCRAFT_ADMIN_PASSWORD }),
  });
  if (!login.ok) throw new Error(`sign-in refused: HTTP ${login.status}`);
  token = (await login.json()).token;

  const meta = await must("GET", "/api/meta");
  const has = (list: string, name: string) => meta[list].some((x: any) => x.name === name);

  // What the dates of the whole data set span, whatever the limit.
  const allDates = [...src.pos.flatMap((p) => [p.orderDate, p.shipDate]), ...src.sos.flatMap((s) => [s.orderDate, s.shipDate])].sort();
  const firstYear = Number(allDates[0].slice(0, 4));
  const lastYear = Number(allDates[allDates.length - 1].slice(0, 4));
  const firstSale = src.sos[0].orderDate;
  // Invoices are paid after TERMS_DAYS, if that day lies within what is loaded.
  const purchaseHorizon = pos.map((p) => p.shipDate).sort().at(-1)!;
  const salesHorizon = sos.map((s) => s.shipDate).sort().at(-1)!;

  let company = "";
  let warehouse = "";
  let taxCode: string | undefined;
  const account = new Map<string, string>();
  const items = new Map<string, string>(); // ProductNumber -> id
  const vendorIds = new Map<string, string>(); // AccountNumber -> id
  const customerIds = new Map<string, string>(); // AccountNumber -> id
  const itemOf = (productId: string) => items.get(src.products.get(productId)!.number);

  // The company and its ids are needed by every step, so this part runs
  // even when `--steps` leaves `setup` out.
  currentStep = "setup";
  const setupStarted = performance.now();
  const companies = await codes("company");
  const creating = STEPS.includes("setup");
  if (!companies.has(COMPANY.code) && !creating) throw new Error("the company does not exist yet; run the setup step first");
  company = (await ensure("company", "company", companies, COMPANY.code, COMPANY))!;
  if (!company) throw new Error("the company could not be created");
  const scope = { company_id: company };

  for (const [code, id] of await codes("account", scope)) account.set(code, id);
  for (const [code, name, type] of ACCOUNTS) {
    await ensure("account", "account", account, code, { company_id: company, code, name, account_type: type });
  }
  const acc = (code: string) => {
    const id = account.get(code);
    if (!id) throw new Error(`account ${code} is missing`);
    return id;
  };

  const periods = new Map((await listAll("/api/entities/fiscal_period", scope)).map((p) => [p.name, p.id]));
  for (let year = firstYear; year <= lastYear; year++) {
    await ensure("fiscal_period", "fiscal_period", periods, `FY${year}`, {
      company_id: company, name: `FY${year}`, start_date: `${year}-01-01`, end_date: `${year}-12-31`,
    });
  }

  // Opening capital: the company has to pay its vendors from something.
  if (creating) {
    const memo = "AW opening capital";
    const found = (await listAll("/api/documents/journal_entry", { company_id: company, q: memo })).find((d) => d.memo === memo);
    let entry = found;
    if (!entry) {
      const answer = await call("POST", "/api/documents/journal_entry", {
        company_id: company, doc_date: allDates[0], memo,
        body: { lines: [{ account_id: acc("1000"), debit: OPENING_CAPITAL }, { account_id: acc("3200"), credit: OPENING_CAPITAL }] },
      }, memo);
      count("journal_entry", answer.status < 400 ? "created" : "failed");
      entry = answer.status < 400 ? answer.body : undefined;
    } else {
      count("journal_entry", "existing");
    }
    if (entry?.status === "draft") await call("POST", "/api/commands/journal_entry.post", { id: entry.id }, memo);
  }

  const warehouses = await codes("warehouse", scope);
  warehouse = (await ensure("warehouse", "warehouse", warehouses, WAREHOUSE.code, { company_id: company, ...WAREHOUSE }))!;

  /// One settings record per company (or per company and tax code).
  const settings = async (entity: string, data: Record<string, unknown>, match: (r: any) => boolean = () => true) => {
    if (!has("entities", entity)) { notes.push(`The server has no '${entity}' entity; skipped.`); return; }
    const existing = (await listAll(`/api/entities/${entity}`, scope)).filter(match);
    if (existing.length) { count(entity, "existing"); return; }
    const answer = await call("POST", `/api/entities/${entity}`, { company_id: company, ...data }, entity);
    count(entity, answer.status < 400 ? "created" : "failed");
  };
  await settings("purchasing_settings", {
    grni_account_id: acc("2100"),
    payable_account_id: acc("2000"), price_variance_account_id: acc("5200"),
  });
  await settings("sales_settings", {
    receivable_account_id: acc("1200"), revenue_account_id: acc("4000"),
    cogs_account_id: acc("5000"),
  });
  // The company's default inventory account, used by receipts, deliveries,
  // adjustments, production and transfers. One account for everything in
  // stock, so the value on hand can be compared with one balance.
  await settings("account_rule", { purpose: "inventory", account_id: acc("1300") },
    (r) => r.purpose === "inventory" && !r.scope_type);
  if (has("entities", "tax_code")) {
    const taxCodes = await codes("tax_code");
    taxCode = await ensure("tax_code", "tax_code", taxCodes, TAX.code, TAX);
    if (taxCode) {
      await settings("tax_settings", { tax_code_id: taxCode, collected_account_id: acc("2200"), paid_account_id: acc("5300") },
        (r) => r.tax_code_id === taxCode);
    }
  } else {
    notes.push("The server has no tax codes; invoices carry no tax.");
  }
  stepTimes.push({ step: "setup", seconds: (performance.now() - setupStarted) / 1000, requests: samples.length });
  console.log(`setup          ${((performance.now() - setupStarted) / 1000).toFixed(1).padStart(8)} s  ${String(samples.length).padStart(7)} requests`);

  await step("items", async () => {
    for (const [code, id] of await codes("item")) items.set(code, id);
    await pool([...src.products.values()], async (p) => {
      await ensure("item", "item", items, p.number, { code: p.number, name: p.name, uom: "EA" });
    });
  });
  if (!STEPS.includes("items")) for (const [code, id] of await codes("item")) items.set(code, id);

  await step("vendors", async () => {
    for (const [code, id] of await codes("vendor")) vendorIds.set(code, id);
    await pool(src.vendors, async (v) => {
      await ensure("vendor", "vendor", vendorIds, v.code, { code: v.code, name: v.name, active: v.active });
    });
  });
  if (!STEPS.includes("vendors")) for (const [code, id] of await codes("vendor")) vendorIds.set(code, id);

  // Customers: the stores that buy (a store with a contact person), and
  // whoever placed one of the orders being loaded. `--all` loads them all.
  await step("customers", async () => {
    for (const [code, id] of await codes("customer")) customerIds.set(code, id);
    const ordering = new Set(sos.map((s) => s.customer));
    const wanted = [...src.customers.values()].filter((c) => ALL || (c.store && c.person) || ordering.has(c.id));
    await pool(wanted, async (c) => {
      await ensure("customer", "customer", customerIds, c.code, { code: c.code, name: c.name });
    });
  });
  if (!STEPS.includes("customers")) for (const [code, id] of await codes("customer")) customerIds.set(code, id);

  await step("boms", async () => {
    if (!has("entities", "bom")) { notes.push("The server has no 'bom' entity; bills of materials skipped."); return; }
    const bomIds = await codes("bom");
    const existingLines = new Set((await listAll("/api/entities/bom_line")).map((l) => `${l.bom_id}/${l.item_id}`));
    await pool([...src.boms.entries()], async ([assembly, components]) => {
      const product = src.products.get(assembly)!;
      const code = `BOM-${product.number}`;
      const bom = await ensure("bom", "bom", bomIds, code, { item_id: itemOf(assembly), code, name: product.name, output_qty: "1" });
      if (!bom) return;
      for (const line of components) {
        const item = itemOf(line.component);
        if (existingLines.has(`${bom}/${item}`)) { count("bom_line", "existing"); continue; }
        const answer = await call("POST", "/api/entities/bom_line", { bom_id: bom, item_id: item, qty: line.qty },
          `${code} component ${src.products.get(line.component)!.number}`);
        count("bom_line", answer.status < 400 ? "created" : "failed");
      }
    });
  });

  /// What is already there, by memo: this is how a second run knows.
  const snapshot = async (docType: string, prefix: string): Promise<Map<string, Doc>> => {
    const docs = await listAll(`/api/documents/${docType}`, { company_id: company, q: prefix });
    return new Map(docs.filter((d) => d.memo).map((d) => [d.memo as string, { id: d.id, status: d.status, number: d.number }]));
  };
  /// Draft a document unless it exists; returns it with its status.
  const draft = async (known: Map<string, Doc>, docType: string, memo: string, docDate: string, body: unknown): Promise<Doc> => {
    const found = known.get(memo);
    if (found) { count(docType, "existing"); return found; }
    const answer = await call("POST", `/api/documents/${docType}`, { company_id: company, doc_date: docDate, memo, body }, memo);
    if (answer.status >= 400) { count(docType, "failed"); throw new Refused(`${memo}: ${answer.body?.error?.message}`); }
    count(docType, "created");
    const doc = { id: answer.body.id, status: answer.body.status, number: answer.body.number };
    known.set(memo, doc);
    return doc;
  };
  /// Run a workflow command on a document unless it is past `from` already.
  const advance = async (doc: Doc, from: string, name: string, memo: string): Promise<any> => {
    if (doc.status !== from) return undefined;
    const result = await command(name, { id: doc.id }, memo);
    doc.status = result.status;
    doc.number = result.number;
    return result;
  };
  /// Pay one invoice in full, if it is still open.
  const pay = async (payments: Map<string, Doc>, memo: string, date: string, direction: "outgoing" | "incoming",
    partyType: string, partyId: string, invoice: Doc) => {
    let payment = payments.get(memo);
    if (!payment) {
      let open: any;
      for (let offset = 0; !open; offset += 500) {
        const page = await command("open_item.list", {
          company_id: company, kind: direction === "outgoing" ? "payable" : "receivable",
          party_type: partyType, party_id: partyId, limit: 500, offset,
        }, memo);
        open = page.items.find((i: any) => i.document_id === invoice.id);
        if (page.items.length < 500) break;
      }
      if (!open) { count("payment", "existing"); return; } // nothing left to pay
      payment = await draft(payments, "payment", memo, date, {
        direction, bank_account_id: acc("1000"), allocations: [{ open_item_id: open.id, amount: open.open }],
      });
    } else {
      count("payment", "existing");
    }
    await advance(payment, "draft", "payment.post", memo);
  };

  // What the source says the selected orders came to, for comparison with the books.
  const sum = (list: { subTotal: string; taxAmt: string; freight: string }[]) => ({
    net: list.reduce((n, o) => n + Number(o.subTotal), 0), tax: list.reduce((n, o) => n + Number(o.taxAmt), 0),
    freight: list.reduce((n, o) => n + Number(o.freight), 0),
  });
  const sourceTotals = {
    complete_purchase_orders_as_ordered: sum(pos.filter((p) => p.status === "4")),
    complete_purchase_orders_stocked_net: pos.filter((p) => p.status === "4")
      .reduce((n, p) => n + p.lines.reduce((m, l) => m + Number(l.stocked) * Number(l.price), 0), 0),
    sales_orders: sum(sos),
  };
  const orderOutcome: Record<string, number> = {};
  const outcome = (key: string) => { orderOutcome[key] = (orderOutcome[key] ?? 0) + 1; };

  await step("purchases", async () => {
    const [orders, receipts, invoices, payments] = await Promise.all([
      snapshot("purchase_order", "AW PO "), snapshot("purchase_receipt", "AW PO "),
      snapshot("vendor_invoice", "AW PO "), snapshot("payment", "AW PO "),
    ]);
    const vendorOf = new Map(src.vendors.map((v) => [v.id, vendorIds.get(v.code)]));
    // The source has orders of vendors that are inactive today, and the
    // server approves nothing for an inactive vendor. They are active
    // while the orders load and inactive again afterwards.
    const dormant = src.vendors.filter((v) => !v.active && vendorIds.has(v.code));
    const setActive = async (active: boolean) => {
      for (const v of dormant) {
        await call("PATCH", `/api/entities/vendor/${vendorIds.get(v.code)}`, { active }, `vendor ${v.code} active=${active}`);
      }
    };
    await setActive(true);
    try {
    await pool(pos, async (po) => {
      const memo = `AW PO ${po.id}`;
      linesOf.set(memo, po.lines.length);
      try {
        const vendor = vendorOf.get(po.vendor);
        if (!vendor) throw new Refused(`${memo}: its vendor was not loaded`);
        const order = await draft(orders, "purchase_order", memo, po.orderDate, {
          vendor_id: vendor,
          lines: po.lines.map((l) => ({ item_id: itemOf(l.product), warehouse_id: warehouse, qty: l.ordered, unit_price: l.price })),
        });
        // Source status: 1 pending, 2 approved, 3 rejected, 4 complete.
        if (po.status === "1") { outcome("purchase orders left as drafts (source: pending)"); return; }
        if (po.status === "3") {
          await advance(order, "draft", "purchase_order.cancel", memo);
          outcome("purchase orders cancelled (source: rejected)");
          return;
        }
        await advance(order, "draft", "purchase_order.approve", memo);
        if (po.status === "2") { outcome("purchase orders approved and left open (source: approved)"); return; }

        const payDate = addDays(po.shipDate, TERMS_DAYS);
        const settled = payments.get(`${memo} payment`)?.status === "posted";
        const finished = invoices.get(`${memo} invoice`)?.status === "posted" && (settled || payDate > purchaseHorizon) && order.status === "closed";
        if (finished) {
          for (const t of ["purchase_receipt", "vendor_invoice"]) count(t, "existing");
          if (settled) count("payment", "existing");
          outcome("purchase orders complete");
          return;
        }
        // Only what went into stock is received and invoiced.
        const full = await must("GET", `/api/documents/purchase_order/${order.id}`, undefined, memo);
        const stocked = po.lines.map((l, i) => ({ line: full.body.lines[i].id, qty: l.stocked, price: l.price }))
          .filter((l) => Number(l.qty) > 0);
        if (stocked.length) {
          const receipt = await draft(receipts, "purchase_receipt", `${memo} receipt`, po.shipDate, {
            purchase_order_id: order.id, lines: stocked.map((l) => ({ order_line_id: l.line, qty: l.qty })),
          });
          await advance(receipt, "draft", "purchase_receipt.post", `${memo} receipt`);
          const invoice = await draft(invoices, "vendor_invoice", `${memo} invoice`, po.shipDate, {
            purchase_order_id: order.id, vendor_reference: `AW-PO-${po.id}`,
            lines: stocked.map((l) => ({ order_line_id: l.line, qty: l.qty, unit_price: l.price, ...(taxCode ? { tax_code_id: taxCode } : {}) })),
          });
          await advance(invoice, "draft", "vendor_invoice.post", `${memo} invoice`);
          if (payDate <= purchaseHorizon) await pay(payments, `${memo} payment`, payDate, "outgoing", "vendor", vendor, invoice);
        }
        // An order that got less than it asked for does not close by itself.
        if (po.lines.some((l) => l.stocked !== l.ordered)) {
          const now = await must("GET", `/api/documents/purchase_order/${order.id}`, undefined, memo);
          if (now.status === "open") await command("purchase_order.close", { id: order.id }, memo);
        }
        outcome("purchase orders complete");
      } catch (error) {
        if (!(error instanceof Refused)) throw error;
        outcome("purchase orders stopped by a refusal");
      }
    });
    } finally {
      await setActive(false);
    }
  });

  await step("opening_stock", async () => {
    const adjustments = await snapshot("stock_adjustment", "AW opening stock");
    for (const [memo, doc] of adjustments) await advance(doc, "draft", "stock_adjustment.post", memo);
    const deliveries = await snapshot("sales_delivery", "AW SO");
    const needed = new Map<string, number>();
    for (const so of sos) {
      if (deliveries.get(`AW ${so.number} delivery`)?.status === "posted") continue;
      for (const l of so.lines) needed.set(l.product, (needed.get(l.product) ?? 0) + Number(l.qty));
    }
    const onHand = new Map<string, number>();
    for (const row of (await command("stock.on_hand", { company_id: company })).rows) {
      if (row.warehouse_id === warehouse) onHand.set(row.item_id, Number(row.qty));
    }
    const lines = [];
    for (const [product, qty] of needed) {
      const short = qty - (onHand.get(itemOf(product)!) ?? 0);
      if (short > 0) lines.push({ item_id: itemOf(product), warehouse_id: warehouse, qty: String(short), unit_cost: src.products.get(product)!.cost });
    }
    if (!lines.length) return;
    const memo = `AW opening stock ${adjustments.size + 1}`;
    try {
      const doc = await draft(adjustments, "stock_adjustment", memo, addDays(firstSale, -1), {
        offset_account_id: acc("3000"), lines,
      });
      await advance(doc, "draft", "stock_adjustment.post", memo);
      count("opening stock lines", "created", lines.length);
    } catch (error) {
      if (!(error instanceof Refused)) throw error;
    }
  });

  await step("sales", async () => {
    const [orders, deliveries, invoices, payments] = await Promise.all([
      snapshot("sales_order", "AW SO"), snapshot("sales_delivery", "AW SO"),
      snapshot("sales_invoice", "AW SO"), snapshot("payment", "AW SO"),
    ]);
    await pool(sos, async (so) => {
      const memo = `AW ${so.number}`;
      linesOf.set(memo, so.lines.length);
      try {
        const customer = customerIds.get(src.customers.get(so.customer)!.code);
        if (!customer) throw new Refused(`${memo}: its customer was not loaded`);
        // A card sale is paid when it is invoiced; a reseller pays on terms.
        const payDate = so.online ? so.shipDate : addDays(so.shipDate, TERMS_DAYS);
        const settled = payments.get(`${memo} payment`)?.status === "posted";
        if (invoices.get(`${memo} invoice`)?.status === "posted" && (settled || payDate > salesHorizon)) {
          for (const t of ["sales_order", "sales_delivery", "sales_invoice"]) count(t, "existing");
          if (settled) count("payment", "existing");
          outcome("sales orders complete");
          return;
        }
        const prices = so.lines.map((l) => netPrice(l.price, l.discount));
        const order = await draft(orders, "sales_order", memo, so.orderDate, {
          customer_id: customer,
          lines: so.lines.map((l, i) => ({ item_id: itemOf(l.product), warehouse_id: warehouse, qty: l.qty, unit_price: prices[i] })),
        });
        const approved = await advance(order, "draft", "sales_order.approve", memo);
        const full = approved ?? await must("GET", `/api/documents/sales_order/${order.id}`, undefined, memo);
        const lineIds: string[] = full.body.lines.map((l: any) => l.id);
        const delivery = await draft(deliveries, "sales_delivery", `${memo} delivery`, so.shipDate, {
          sales_order_id: order.id, lines: so.lines.map((l, i) => ({ order_line_id: lineIds[i], qty: l.qty })),
        });
        await advance(delivery, "draft", "sales_delivery.post", `${memo} delivery`);
        const invoice = await draft(invoices, "sales_invoice", `${memo} invoice`, so.shipDate, {
          sales_order_id: order.id,
          lines: so.lines.map((l, i) => ({ order_line_id: lineIds[i], qty: l.qty, unit_price: prices[i], ...(taxCode ? { tax_code_id: taxCode } : {}) })),
        });
        await advance(invoice, "draft", "sales_invoice.post", `${memo} invoice`);
        if (payDate <= salesHorizon) await pay(payments, `${memo} payment`, payDate, "incoming", "customer", customer, invoice);
        outcome("sales orders complete");
      } catch (error) {
        if (!(error instanceof Refused)) throw error;
        outcome("sales orders stopped by a refusal");
      }
    });
  });

  // Close every year before the last one the load reaches, oldest first.
  // Only on request: a closed year refuses later postings, so a later run
  // with a larger limit would be refused wherever it reaches back.
  await step("year_end", async () => {
    if (!CLOSE_YEARS) return;
    if (!has("doc_types", "year_end_close")) { notes.push("The server has no year-end close; years were not closed."); return; }
    const lastActive = Number([purchaseHorizon, salesHorizon].sort().at(-1)!.slice(0, 4));
    const closes = await snapshot("year_end_close", "AW year-end");
    for (let year = firstYear; year < lastActive; year++) {
      const memo = `AW year-end FY${year}`;
      try {
        const doc = await draft(closes, "year_end_close", memo, `${year}-12-31`, {
          fiscal_period_id: periods.get(`FY${year}`), retained_earnings_account_id: acc("3100"),
        });
        await advance(doc, "draft", "year_end_close.post", memo);
      } catch (error) {
        if (!(error instanceof Refused)) throw error;
        break; // years close in order
      }
    }
  });

  // ------------------------------------------------------------- the books
  const checks: { check: string; ok: boolean; detail: string }[] = [];
  let books: any = {};
  await step("checks", async () => {
    const tb = await command("gl.trial_balance", { company_id: company });
    const balance = (code: string) => Number(tb.rows.find((r: any) => r.code === code)?.balance ?? 0);
    const near = (a: number, b: number) => Math.abs(a - b) < 0.005;
    checks.push({
      check: "trial balance: total debit equals total credit",
      ok: near(Number(tb.total_debit), Number(tb.total_credit)), detail: `debit ${tb.total_debit}, credit ${tb.total_credit}`,
    });
    const stock = await command("stock.on_hand", { company_id: company });
    checks.push({
      check: "stock on hand value equals the inventory account (1300)",
      ok: near(Number(stock.total_value), balance("1300")), detail: `stock ${stock.total_value}, account ${balance("1300").toFixed(4)}`,
    });
    const payables = await command("open_item.list", { company_id: company, kind: "payable", limit: 1 });
    checks.push({
      check: "open payables equal the payables account (2000)",
      ok: near(Number(payables.total_open), -balance("2000")),
      detail: `${payables.total} open items ${payables.total_open}, account ${(-balance("2000")).toFixed(4)}`,
    });
    const receivables = await command("open_item.list", { company_id: company, kind: "receivable", limit: 1 });
    checks.push({
      check: "open receivables equal the receivables account (1200)",
      ok: near(Number(receivables.total_open), balance("1200")),
      detail: `${receivables.total} open items ${receivables.total_open}, account ${balance("1200").toFixed(4)}`,
    });
    checks.push({
      check: "nothing is left in goods received not invoiced (2100): every receipt was invoiced",
      ok: near(balance("2100"), 0), detail: `account ${balance("2100").toFixed(4)}`,
    });
    const negative = stock.rows.filter((r: any) => Number(r.qty) < 0 || Number(r.value) < 0).length;
    checks.push({ check: "no item has negative stock or value", ok: negative === 0, detail: `${negative} rows` });

    if (has("commands", "gl.balance_sheet") && has("commands", "gl.income_statement")) {
      const sheet = await command("gl.balance_sheet", { company_id: company });
      checks.push({
        check: "balance sheet: assets equal liabilities plus equity plus unclosed result",
        ok: near(Number(sheet.total_assets), Number(sheet.total_liabilities_and_equity)),
        detail: `assets ${sheet.total_assets}, liabilities and equity ${sheet.total_liabilities_and_equity}, of which unclosed result ${sheet.net_income}`,
      });
      const income = await command("gl.income_statement", { company_id: company, from: `${firstYear}-01-01`, to: `${lastYear}-12-31` });
      const years: Record<string, string> = {};
      for (let year = firstYear; year <= lastYear; year++) {
        years[year] = (await command("gl.income_statement", { company_id: company, from: `${year}-01-01`, to: `${year}-12-31` })).net_income;
      }
      const byYear = Object.values(years).reduce((n, v) => n + Number(v), 0);
      checks.push({
        check: "income statement: the years add up to the whole range",
        ok: near(byYear, Number(income.net_income)), detail: `years ${JSON.stringify(years)}, whole range ${income.net_income}`,
      });
      checks.push({
        check: "result of all years = unclosed result on the balance sheet + retained earnings (3100)",
        ok: near(Number(income.net_income), Number(sheet.net_income) - balance("3100")),
        detail: `income statement ${income.net_income}, unclosed ${sheet.net_income}, retained earnings ${(-balance("3100")).toFixed(4)}`,
      });
      books.balance_sheet = sheet;
      books.income_statement = income;
    }

    const documents: Record<string, Record<string, number>> = {};
    for (const type of meta.doc_types as any[]) {
      for (const state of type.states) {
        const page = await must("GET", `/api/documents/${type.name}?${query({ company_id: company, status: state.name, limit: 1 })}`);
        if (page.total) (documents[type.name] ??= {})[state.name] = page.total;
      }
    }
    books = {
      ...books,
      trial_balance: tb.rows.map((r: any) => ({ code: r.code, name: r.name, debit: r.debit, credit: r.credit, balance: r.balance })),
      stock_rows: stock.rows.length, stock_value: stock.total_value, documents,
      payables_open: payables.total_open, receivables_open: receivables.total_open,
    };
    if (has("commands", "tax.report")) {
      books.tax = await command("tax.report", { company_id: company, from: `${firstYear}-01-01`, to: `${lastYear}-12-31` });
    }
  });

  report(startedAt, { src, pos, sos, checks, books, sourceTotals, orderOutcome, purchaseHorizon, salesHorizon });
}

// -------------------------------------------------------------------- report

function percentile(sorted: number[], p: number): number {
  if (!sorted.length) return 0;
  return sorted[Math.min(sorted.length - 1, Math.ceil(p * sorted.length) - 1)];
}

function report(startedAt: Date, extra: any) {
  const byLabel = new Map<string, Sample[]>();
  for (const s of samples) {
    if (!byLabel.has(s.label)) byLabel.set(s.label, []);
    byLabel.get(s.label)!.push(s);
  }
  const endpoints = [...byLabel.entries()].map(([label, list]) => {
    const ms = list.map((s) => s.ms).sort((a, b) => a - b);
    const quarter = Math.max(1, Math.floor(list.length / 4));
    const median = (part: Sample[]) => percentile(part.map((s) => s.ms).sort((a, b) => a - b), 0.5);
    return {
      label, count: list.length, errors: list.filter((s) => s.status >= 400 || s.status === 0).length,
      p50: percentile(ms, 0.5), p95: percentile(ms, 0.95), max: ms[ms.length - 1],
      total_s: ms.reduce((a, b) => a + b, 0) / 1000,
      // Does it get slower as the data grows? Median of the first and of the last quarter of the calls.
      p50_first_quarter: median(list.slice(0, quarter)), p50_last_quarter: median(list.slice(-quarter)),
    };
  }).sort((a, b) => b.p95 - a.p95);

  const buckets: [string, number, number][] = [["1-2", 1, 2], ["3-9", 3, 9], ["10-29", 10, 29], ["30+", 30, Infinity]];
  const byLines = [...byLabel.entries()].filter(([label]) => label.endsWith(".post")).map(([label, list]) => ({
    label,
    buckets: buckets.map(([name, low, high]) => {
      const ms = list.filter((s) => s.lines !== undefined && s.lines >= low && s.lines <= high).map((s) => s.ms).sort((a, b) => a - b);
      return { lines: name, count: ms.length, p50: percentile(ms, 0.5), p95: percentile(ms, 0.95), max: ms[ms.length - 1] ?? 0 };
    }),
  }));

  const groups = new Map<string, { count: number; statuses: Set<number>; labels: Set<string>; example: Failure }>();
  for (const f of failures) {
    // Group by the message with what varies (ids, numbers, codes in quotes) taken out.
    const key = f.message.replace(/[0-9a-f]{8}-[0-9a-f-]{27}/g, "<id>").replace(/'[^']*'/g, "'…'").replace(/\d+(\.\d+)?/g, "N");
    const group = groups.get(key) ?? { count: 0, statuses: new Set(), labels: new Set(), example: f };
    group.count++;
    group.statuses.add(f.status);
    group.labels.add(f.label);
    groups.set(key, group);
  }
  const errors = [...groups.entries()].map(([message, g]) => ({
    message, count: g.count, statuses: [...g.statuses], endpoints: [...g.labels], example: g.example,
  })).sort((a, b) => b.count - a.count);

  const seconds = (Date.now() - startedAt.getTime()) / 1000;
  const result = {
    started_at: startedAt.toISOString(), base_url: BASE, company: CODE, close_years: CLOSE_YEARS, limit_orders: ALL ? "all" : LIMIT, concurrency: CONCURRENCY,
    steps_run: STEPS, total_seconds: seconds, total_requests: samples.length,
    selected: {
      purchase_orders: extra.pos.length, purchase_lines: extra.pos.reduce((n: number, p: Po) => n + p.lines.length, 0),
      purchase_dates: [extra.pos[0]?.orderDate, extra.purchaseHorizon],
      sales_orders: extra.sos.length, sales_lines: extra.sos.reduce((n: number, s: So) => n + s.lines.length, 0),
      sales_dates: [extra.sos[0]?.orderDate, extra.salesHorizon],
      sales_in_foreign_currency: extra.sos.filter((s: So) => s.foreign).length,
      sales_lines_with_discount: extra.sos.reduce((n: number, s: So) => n + s.lines.filter((l) => Number(l.discount) > 0).length, 0),
      purchase_lines_with_rejects: extra.pos.reduce((n: number, p: Po) => n + p.lines.filter((l) => Number(l.rejected) > 0).length, 0),
    },
    bom_source: extra.src.bomStats, counts, order_outcomes: extra.orderOutcome, source_totals: extra.sourceTotals,
    steps: stepTimes.map((s) => ({ ...s, requests_per_second: s.seconds ? s.requests / s.seconds : 0 })),
    endpoints, latency_by_order_lines: byLines, errors, checks: extra.checks, books: extra.books, notes,
  };
  mkdirSync(DATA, { recursive: true });
  const path = join(DATA, `run-${LABEL}-${startedAt.toISOString().replace(/[:.]/g, "-")}.json`);
  writeFileSync(path, JSON.stringify(result, null, 1));

  console.log(`\ntotal ${seconds.toFixed(1)} s, ${samples.length} requests, ${(samples.length / seconds).toFixed(1)} requests/s`);
  console.log("\nloaded (created / already there / failed):");
  for (const [kind, c] of Object.entries(counts)) console.log(`  ${kind.padEnd(22)} ${String(c.created).padStart(7)} ${String(c.existing).padStart(7)} ${String(c.failed).padStart(7)}`);
  for (const [what, n] of Object.entries(extra.orderOutcome)) console.log(`  ${n} ${what}`);
  console.log("\nslowest endpoints by 95th percentile (ms):");
  for (const e of endpoints.slice(0, 12)) {
    console.log(`  ${e.label.padEnd(48)} n=${String(e.count).padStart(6)} p50=${e.p50.toFixed(1).padStart(8)} p95=${e.p95.toFixed(1).padStart(8)} max=${e.max.toFixed(1).padStart(8)}`);
  }
  console.log(`\nerrors: ${failures.length} in ${errors.length} kinds`);
  for (const e of errors) console.log(`  ${String(e.count).padStart(5)} x [${e.statuses.join(",")}] ${e.message}\n          e.g. ${e.example.label} (${e.example.context}): ${e.example.message}`);
  console.log("\nthe books:");
  for (const c of extra.checks) console.log(`  ${c.ok ? "ok  " : "FAIL"} ${c.check}: ${c.detail}`);
  for (const n of notes) console.log(`note: ${n}`);
  console.log(`\nreport written to ${path}`);
}

main().catch((error) => {
  console.error(`\nstopped: ${error?.message ?? error}`);
  if (samples.length) {
    try { report(new Date(samples[0].at), { src: { bomStats: {} }, pos: [], sos: [], checks: [], books: {}, sourceTotals: {}, orderOutcome: {} }); } catch {}
  }
  process.exit(1);
});
