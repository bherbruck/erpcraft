// Screenshots of one sales order in three UIs, for the guidance-first re-test in COMPARISON.md:
// shadcn (../ui, served by the server on 8600), the existing Astryx page and the fresh one
// (the preview on 5174), each at 1440 and 820 wide.
// bun scripts/fresh-screenshots.ts

import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { launchBrowser } from "./browser";
import { adminCredentials, SERVER } from "./lib";

const ASTRYX = process.env.SPIKE_URL ?? "http://127.0.0.1:5174";
const OUT = resolve(import.meta.dir, "../docs/screenshots");
mkdirSync(OUT, { recursive: true });

const ORDER = process.env.ORDER_ID ?? "01a1ea99-5a5b-7361-a3ee-ce746505d37e";
const COMPANY = process.env.COMPANY ?? "OHIO";
const ONLY = process.env.ONLY; // e.g. "fresh" to shoot one version

const { email, password } = adminCredentials();
const login = await fetch(`${SERVER}/api/auth/login`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ email, password }),
});
const { token } = (await login.json()) as { token: string };

const versions = [
  { name: "shadcn", base: SERVER, path: `/c/${COMPANY}/d/sales_order/${ORDER}` },
  { name: "astryx", base: ASTRYX, path: `/c/${COMPANY}/d/sales_order/${ORDER}` },
  { name: "fresh", base: ASTRYX, path: `/c/${COMPANY}/fresh/sales_order/${ORDER}` },
].filter((version) => !ONLY || ONLY.split(",").includes(version.name));

const browser = await launchBrowser(9351);
try {
  for (const version of versions) {
    // Both apps keep the session token in sessionStorage under the same key.
    await browser.goto(`${version.base}/sign-in`);
    await browser.evaluate(`sessionStorage.setItem("erpcraft.token", ${JSON.stringify(token)})`);
    for (const width of [1440, 820]) {
      await browser.viewport(width, width === 1440 ? 1000 : 1180);
      await browser.goto(`${version.base}${version.path}`);
      await browser.settle();
      await Bun.sleep(1200);
      const file = `${OUT}/fresh-${version.name}-${width}.png`;
      await browser.screenshot(file);
      console.log("wrote", file);
    }
  }
  if (browser.errors.length > 0) console.log("page errors:", browser.errors);
} finally {
  await browser.close();
}
