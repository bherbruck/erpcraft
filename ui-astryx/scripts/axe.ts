// An axe-core audit (WCAG 2.1 A and AA) of the main screens, light and dark, against the
// running preview. bun scripts/axe.ts  → a summary here and docs/axe.json in full.

import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { launchBrowser } from "./browser";
import { adminCredentials } from "./lib";

const BASE = process.env.SPIKE_URL ?? "http://127.0.0.1:5174";
const ORDER = process.env.ORDER_ID ?? "01a1ea99-5a5b-7361-a3ee-ce746505d37e";
const AXE = readFileSync(resolve(import.meta.dir, "../node_modules/axe-core/axe.min.js"), "utf8");
const PAGES: [string, string][] = [
  ["sign-in", "/sign-in"],
  ["document list", "/c/AWC/d/sales_order"],
  ["document page", `/c/OHIO/d/sales_order/${ORDER}`],
  ["new document", "/c/OHIO/d/sales_order/new"],
  ["entity list", "/c/AWC/e/customer"],
  ["entity record", "/c/AWC/e/account/new"],
  ["report", "/c/OHIO/r/gl.trial_balance"],
  ["approvals", "/c/OHIO/approvals?scope=all"],
];

interface Violation {
  id: string;
  impact: string | null;
  help: string;
  nodes: { target: string[]; failureSummary?: string }[];
}

const { email, password } = adminCredentials();
const browser = await launchBrowser(9349);
// The audit script is injected by the test, so the page's own CSP does not apply to it.
await browser.send("Page.setBypassCSP", { enabled: true });
const report: Record<string, Violation[]> = {};
try {
  await browser.viewport(1440, 900);
  for (const dark of [false, true]) {
    await browser.dark(dark);
    for (const [name, path] of PAGES) {
      if (path === "/sign-in") {
        await browser.evaluate("sessionStorage.clear()");
      }
      await browser.goto(`${BASE}${path}`);
      if (path === "/sign-in") await Bun.sleep(500);
      await Bun.sleep(1200);
      await browser.evaluate(AXE);
      const json = await browser.evaluate(
        `axe.run(document, { runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"] } }).then((r) => JSON.stringify(r.violations.map((v) => ({ id: v.id, impact: v.impact, help: v.help, nodes: v.nodes.map((n) => ({ target: n.target, failureSummary: n.failureSummary })) }))))`,
      );
      const violations = JSON.parse(String(json)) as Violation[];
      const key = `${name}${dark ? " (dark)" : ""}`;
      report[key] = violations;
      for (const v of violations) for (const n of v.nodes) n.target.push(String(await browser.evaluate(`document.querySelector(${JSON.stringify(n.target[0])})?.outerHTML.slice(0, 160)`)));
      console.log(`${key}: ${violations.length ? violations.map((v) => `${v.id} [${v.impact}] ×${v.nodes.length}`).join(", ") : "no violations"}`);
      if (path === "/sign-in") {
        await browser.fill("input[type=email]", email);
        await browser.fill("input[type=password]", password);
        await browser.click("Sign in");
        await browser.settle();
      }
    }
  }
} finally {
  await browser.close();
}
writeFileSync(resolve(import.meta.dir, "../docs/axe.json"), JSON.stringify(report, null, 2));
