#!/usr/bin/env bun
// An AI agent's day with erpcraft, through /mcp only, scripted: no model.
// Each task is done the way a careful agent would do it from what the
// server tells it: start from erpcraft_modules, load the tools of a
// module or resource with erpcraft_tools rather than the whole list, read
// errors and act on them. Where the server did not tell it what it needed
// next, the script notes that it was stuck, and goes on the way an agent
// would have to (a bigger read, or knowledge it should not have needed).
//
//   bun run trial.ts [--base-url URL] [--label NAME]
//
// Needs a server on the month scenario (see README.md). Writes
// results/<label>.json and prints a table.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { Client, Step, tokens } from "./mcp.ts";

const HERE = dirname(new URL(import.meta.url).pathname);
const option = (name: string) => {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : undefined;
};
const BASE = option("base-url") ?? "http://127.0.0.1:8611";
const LABEL = option("label") ?? "run";
const EMAIL = "admin@example.com";
const PASSWORD = "correct-horse-battery";

// ------------------------------------------------------------ agent helpers

/// What an agent knows of the tools: the definitions it has loaded.
class Agent {
  known = new Map<string, any>();
  constructor(readonly c: Client) {}

  private sawModules = false;

  /// erpcraft_modules; the first time, also the schemas of the core tools
  /// it names, which any task needs (when the server names them).
  async modules(): Promise<any[]> {
    const got = (await this.c.call("erpcraft_modules")).value;
    if (!this.sawModules) {
      this.sawModules = true;
      const core = (got.core_tools ?? []).map((t: any) => t.name);
      if (core.length) await this.load({ names: core });
    }
    return got.modules;
  }

  async load(args: { module?: string; resource?: string; names?: string[] }): Promise<any> {
    const got = (await this.c.call("erpcraft_tools", args)).value;
    for (const t of got.tools ?? []) this.known.set(t.name, t);
    return got;
  }

  has(name: string) {
    return this.known.has(name);
  }

  async call(name: string, args: Record<string, unknown> = {}) {
    if (!this.known.has(name) && !name.startsWith("erpcraft_")) {
      this.c.stuckOn(`called ${name} without having its definition`);
    }
    return this.c.call(name, args);
  }

  /// The id of the best match for a text, of one kind.
  async find(q: string, kind: string): Promise<string | undefined> {
    const got = await this.call("search_records", { q });
    const group = got.value.groups?.find((g: any) => g.name === kind);
    return group?.items?.[0]?.id;
  }

  /// The fields of a resource: from erpcraft_tools when it says them,
  /// else from meta_describe of its module (a large read).
  async fields(resource: string, module: string, loaded: any): Promise<any> {
    if (loaded?.resource?.fields) return loaded.resource;
    this.c.stuckOn(`erpcraft_tools(resource: ${resource}) did not say its fields; read meta_describe(${module})`);
    const d = (await this.call("meta_describe", { module })).value;
    return [...(d.entities ?? []), ...(d.doc_types ?? [])].find((x: any) => x.name === resource);
  }
}

/// What a tool error tells an agent: its message, and whether it says
/// what to do next (a field, a hint, or the name of a tool).
function readError(value: any): { message: string; actionable: boolean } {
  const e = value?.error ?? value;
  const message = String(e?.message ?? JSON.stringify(e));
  const actionable = !!(e?.field || e?.hint || e?.next || /\b[a-z]+_[a-z_]+\b/.test(message));
  return { message, actionable };
}

// ------------------------------------------------------------------ tasks

type Task = { name: string; run: (a: Agent) => Promise<string> };

const company = async (a: Agent) => {
  const got = await a.call("entity_list", { entity: "company", limit: 5 });
  return got.value.items?.[0]?.id as string;
};

const tasks: Task[] = [
  {
    name: "create a customer and a sales order for 3 widgets",
    async run(a) {
      const mods = await a.modules();
      const sales = mods.find((m: any) => m.name === "sales");
      if (!sales) a.c.stuckOn("no sales module in erpcraft_modules");
      const customer = await a.load({ resource: "customer" });
      if (!a.has("entity_create")) {
        a.c.stuckOn("no tool to create a customer for resource customer; loaded the kernel's tools");
        await a.load({ module: "kernel" });
      }
      const customerMeta = await a.fields("customer", "sales", customer);
      const required = (customerMeta?.fields ?? []).filter((f: any) => f.required).map((f: any) => f.name);
      const made = await a.call("entity_create", {
        entity: "customer",
        data: { code: "GLOBEX", name: "Globex Corporation" },
      });
      if (!made.ok) return `customer refused: ${readError(made.value).message}`;
      const order = await a.load({ resource: "sales_order" });
      if (!a.has("document_create")) a.c.stuckOn("no tool to create a sales order for resource sales_order");
      const orderMeta = await a.fields("sales_order", "sales", order);
      const lineFields = (orderMeta?.collections?.[0]?.fields ?? []).map((f: any) => f.name);
      if (!lineFields.includes("warehouse_id")) a.c.stuckOn("the sales order's line fields were not found");
      const item = await a.find("widget", "item");
      const companyId = await company(a);
      const wh = await a.call("entity_list", { entity: "warehouse", filters: { company_id: companyId }, limit: 5 });
      const warehouse = wh.value.items?.[0]?.id;
      let input: any = {
        doc_type: "sales_order",
        company_id: companyId,
        doc_date: "2026-04-02",
        body: { customer_id: made.value.id, lines: [{ item_id: item, warehouse_id: warehouse, qty: "3" }] },
        view: "header",
      };
      let so = await a.call("document_create", input);
      if (!so.ok && /unit price/i.test(readError(so.value).message)) {
        // The refusal says what is missing; the agent asks its user for the
        // price and is told 12.50.
        input.body.lines[0].unit_price = "12.50";
        so = await a.call("document_create", input);
      }
      if (!so.ok) {
        const e = readError(so.value);
        if (!e.actionable) a.c.stuckOn(`sales order refused without a way forward: ${e.message}`);
        return `order refused: ${e.message}`;
      }
      return `customer ${made.value.code} (${required.join(", ")} required), order ${so.value.id} for ${so.value.summary?.total_net ?? "?"}`;
    },
  },
  {
    name: "what's overdue for Acme",
    async run(a) {
      const mods = await a.modules();
      const holder = mods.find((m: any) => (m.reports ?? []).some((r: any) => /aging|open item/i.test(r.title ?? r)));
      if (!holder) a.c.stuckOn("no module says it has the receivables or aging; guessed open_items");
      await a.load({ module: holder?.name ?? "open_items" });
      const acme = await a.find("acme", "customer");
      if (!acme) return "Acme not found";
      const companyId = await company(a);
      const got = await a.call("open_item_list", {
        company_id: companyId, kind: "receivable", party_type: "customer", party_id: acme, overdue_min: 1,
      });
      if (!got.ok) return `refused: ${readError(got.value).message}`;
      const items = got.value.items ?? [];
      return `${items.length} overdue item(s), ${items.map((i: any) => `${i.document?.number ?? i.document_number ?? "?"} ${i.open ?? i.amount_open ?? ""} due ${i.due_date}`).join("; ")}`;
    },
  },
  {
    name: "post this vendor invoice from an e-invoice file",
    async run(a) {
      const file = readFileSync(join(HERE, "fixtures", "boltwerk-cii.xml"), "utf8");
      const mods = await a.modules();
      const einv = mods.find((m: any) => m.name === "einvoice");
      if (!einv) a.c.stuckOn("no einvoice module listed");
      await a.load({ module: "einvoice" });
      const companyId = await company(a);
      const read = await a.call("einvoice_read", { company_id: companyId, content: file });
      if (!read.ok) return `read refused: ${readError(read.value).message}`;
      const vendor = read.value.vendor?.id ?? read.value.vendor_id;
      if (!vendor) a.c.stuckOn("einvoice_read named no vendor");
      if ((read.value.problems ?? []).length) a.c.stuckOn(`read reports problems: ${read.value.problems.join("; ")}`);
      let args: any = { company_id: companyId, content: file, view: "header" };
      let imported = await a.call("einvoice_import", args);
      for (let tries = 0; !imported.ok && tries < 3; tries++) {
        const e = readError(imported.value);
        const field = imported.value?.error?.field;
        if (/group account/.test(e.message)) {
          // Which field says an account is a group? Its fields tell.
          const meta = await a.load({ resource: "account" });
          const flag = meta?.resource?.fields?.find((f: any) => /group/.test(f.name))?.name;
          if (!flag) {
            a.c.stuckOn("could not tell which accounts may be posted to");
            return `import refused: ${e.message}`;
          }
          const acc = await a.call("entity_list", {
            entity: "account", filters: { company_id: companyId, account_type: "expense", [flag]: "false" }, limit: 50,
          });
          const items = acc.value.items ?? [];
          args.account_id = (items.find((x: any) => /maint|repair|service/i.test(x.name)) ?? items[0])?.id;
        } else if (field === "account_id" || /account_id/.test(e.message)) {
          const acc = await a.call("entity_list", {
            entity: "account", filters: { company_id: companyId, account_type: "expense" }, limit: 50,
          });
          if (!acc.ok) a.c.stuckOn(`could not list expense accounts: ${readError(acc.value).message}`);
          const items = acc.value.items ?? [];
          args.account_id = (items.find((x: any) => /maint|repair|service/i.test(x.name)) ?? items[0])?.id;
          if (!args.account_id) return "no expense account found";
        } else {
          if (!e.actionable) a.c.stuckOn(`import refused without a way forward: ${e.message}`);
          return `import refused: ${e.message}`;
        }
        imported = await a.call("einvoice_import", args);
      }
      if (!imported.ok) return `import refused: ${readError(imported.value).message}`;
      // The next step: named by the import's description, or found by the
      // resource it made.
      const next = (a.known.get("einvoice_import")?.description ?? "").match(/\bvendor_invoice_post\b/);
      if (next && !a.has("vendor_invoice_post")) await a.load({ names: ["vendor_invoice_post"] });
      if (!next) {
        a.c.stuckOn("the import did not say how its draft is posted; loaded the vendor invoice's tools");
        await a.load({ resource: "vendor_invoice" });
      }
      const post = await a.call("vendor_invoice_post", { id: imported.value.id, view: "header" });
      if (!post.ok) {
        const e = readError(post.value);
        if (!e.actionable) a.c.stuckOn(`post refused without a way forward: ${e.message}`);
        return `drafted ${imported.value.id}, post refused: ${e.message}`;
      }
      return `posted ${post.value.number}`;
    },
  },
  {
    name: "why didn't order SO-2026-00004 ship",
    async run(a) {
      await a.modules();
      const found = await a.call("search_records", { q: "SO-2026-00004" });
      const id = found.value.groups?.find((g: any) => g.name === "sales_order")?.items?.[0]?.id;
      if (!id) return "order not found";
      await a.load({ resource: "sales_order" });
      const doc = await a.call("document_get", { doc_type: "sales_order", id });
      if (!a.has("document_get")) a.c.stuckOn("document_get is not among the sales order's tools");
      const status = doc.value.status;
      const deliveries = (doc.value.links ?? []).filter((l: any) => l.doc_type === "sales_delivery");
      const line = doc.value.body?.lines?.[0];
      // Is there stock? The agent looks for a report on stock.
      const mods = await a.modules();
      const stock = mods.find((m: any) => (m.reports ?? []).some((r: any) => /stock on hand/i.test(r.title ?? r)));
      if (!stock) a.c.stuckOn("no module says it reports stock on hand");
      await a.load({ module: stock?.name ?? "stock" });
      const onHand = await a.call("stock_on_hand", { company_id: doc.value.company_id, item_id: line?.item_id });
      if (!onHand.ok) a.c.stuckOn(`stock_on_hand refused: ${readError(onHand.value).message}`);
      const qty = (onHand.value.items ?? onHand.value.rows ?? []).reduce((n: number, r: any) => n + Number(r.qty ?? r.quantity ?? 0), 0);
      return `status ${status}; ${deliveries.length} delivery(ies); ordered ${line?.qty}, delivered ${line?.qty_delivered}, on hand ${qty}`;
    },
  },
  {
    name: "close March",
    async run(a) {
      const prompts = await a.c.rpc("prompts/list");
      a.c.steps.push({ method: "prompts/list", isError: false, descriptionTokens: tokens(prompts.prompts), resultTokens: 0 });
      const close = prompts.prompts?.find((p: any) => p.name === "close_month");
      if (!close) a.c.stuckOn("no prompt for closing a month");
      const text = await a.c.prompt("close_month", { company: "OWI", period: "2026-03" });
      const named = [...new Set(text.match(/\b[a-z]+(?:_[a-z]+)+\b/g) ?? [])].filter((n) => n.includes("_"));
      const wanted = named.filter((n) => !a.has(n));
      const byName = await a.load({ names: wanted } as any);
      if (byName?.error || !(byName.tools ?? []).length) {
        a.c.stuckOn("erpcraft_tools cannot fetch tools by name; loaded them resource by resource");
        for (const tool of wanted.filter((n) => !a.has(n))) {
          await a.load({ resource: tool.split("_").slice(0, -1).join("_") });
        }
      }
      const companyId = await company(a);
      const periods = await a.call("entity_list", { entity: "fiscal_period", filters: { company_id: companyId, name: "2026-03" } });
      const period = periods.value.items?.[0];
      if (!period) return "March not found";
      const tb = await a.call("gl_trial_balance", { company_id: companyId, as_of: "2026-03-31" });
      if (!tb.ok) a.c.stuckOn(`trial balance refused: ${readError(tb.value).message}`);
      if (/`drafts`/.test(text)) {
        const preview = await a.call("fiscal_period_close", { id: period.id, preview: true });
        const tb2 = await a.call("gl_trial_balance", { company_id: companyId, as_of: "2026-03-31" });
        if (!tb2.ok) a.c.stuckOn(`trial balance refused: ${readError(tb2.value).message}`);
        const list = preview.value.result?.drafts ?? [];
        return `${list.length} draft(s) in March (${list.map((d: any) => d.doc_type).join(", ")}); the close would go through`;
      }
      const types: string[] = (await a.modules()).flatMap((m: any) => (m.documents ?? []).map((d: any) => d.name ?? d));
      if (types.some((t) => /\s|[A-Z]/.test(t))) a.c.stuckOn("erpcraft_modules gives document types as labels, not names");
      let draftCount = 0;
      for (const t of types) {
        const docType = /\s|[A-Z]/.test(t) ? t.toLowerCase().replace(/ /g, "_") : t;
        const got = await a.call("document_list", {
          doc_type: docType, company_id: companyId, status: "draft", limit: 1,
          where: "record.doc_date >= '2026-03-01' && record.doc_date <= '2026-03-31'",
        });
        if (!got.ok) continue;
        draftCount += got.value.total ?? 0;
      }
      const drafts = { value: { total: draftCount } };
      const closed = await a.call("fiscal_period_close", { id: period.id, preview: true });
      if (!closed.ok) {
        const e = readError(closed.value);
        if (!e.actionable) a.c.stuckOn(`close refused without a way forward: ${e.message}`);
        return `${drafts.value.total ?? "?"} draft(s) in March; close refused: ${e.message}`;
      }
      return `${drafts.value.total ?? "?"} draft(s) in March; the close would go through`;
    },
  },
];

// ------------------------------------------------------------------ run

const once = await Client.login(BASE, EMAIL, PASSWORD);
const all = await once.listTools();
const fullList = { tools: all.length, tokens: tokens(all) };

const results = [];
for (const task of tasks) {
  const c = await Client.login(BASE, EMAIL, PASSWORD);
  const agent = new Agent(c);
  let outcome: string;
  try {
    outcome = await task.run(agent);
  } catch (e) {
    outcome = `crashed: ${(e as Error).message}`;
  }
  const sum = (f: (s: Step) => number) => c.steps.reduce((n, s) => n + f(s), 0);
  results.push({
    task: task.name,
    outcome,
    requests: c.steps.length,
    toolCalls: c.steps.filter((s) => s.method === "tools/call").length,
    errors: c.steps.filter((s) => s.isError).length,
    descriptionTokens: sum((s) => s.descriptionTokens),
    resultTokens: sum((s) => s.resultTokens),
    stuck: c.stuck,
    steps: c.steps.map((s) => `${s.method}${s.name ? ` ${s.name}` : ""}${s.isError ? " (error)" : ""} d=${s.descriptionTokens} r=${s.resultTokens}`),
  });
}

mkdirSync(join(HERE, "results"), { recursive: true });
writeFileSync(join(HERE, "results", `${LABEL}.json`), JSON.stringify({ base: BASE, fullList, results }, null, 2));
console.log(`tools/list in full: ${fullList.tools} tools, ${fullList.tokens} tokens`);
console.log("| task | requests | tool calls | errors | description tokens | result tokens | stuck |");
console.log("|---|---|---|---|---|---|---|");
for (const r of results) {
  console.log(`| ${r.task} | ${r.requests} | ${r.toolCalls} | ${r.errors} | ${r.descriptionTokens} | ${r.resultTokens} | ${r.stuck.length} |`);
}
for (const r of results) {
  console.log(`\n## ${r.task}\n${r.outcome}`);
  for (const s of r.stuck) console.log(`  stuck: ${s}`);
}
