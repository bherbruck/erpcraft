// A headless Chrome driven over its DevTools protocol, for the screenshot and
// end-to-end scripts. No test framework and no downloaded driver: Chrome is
// started with a debugging port and spoken to over a WebSocket.

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { asArray, asObject, asString, type JsonObject } from "./lib";

const CHROME = process.env.CHROME ?? "/usr/bin/google-chrome";

const KEY_CODES: Record<string, number> = {
  Enter: 13,
  Escape: 27,
  ArrowDown: 40,
  ArrowUp: 38,
  Delete: 46,
  Tab: 9,
};

export const CTRL = 2;

/** How long a page may take to render before that counts as a failure. */
const RENDER_PATIENCE = 90_000;

export interface Browser {
  /** Messages of uncaught exceptions and console errors since the start. */
  errors: string[];
  send(method: string, params?: JsonObject): Promise<JsonObject>;
  /** Runs an expression in the page and returns its value. */
  evaluate(expression: string): Promise<unknown>;
  /** Waits until the page has rendered and nothing on it has said "loading" for a moment. */
  settle(): Promise<void>;
  goto(url: string): Promise<void>;
  viewport(width: number, height: number): Promise<void>;
  dark(on: boolean): Promise<void>;
  /** Focuses the first element matching the selector and selects its text. */
  focus(selector: string): Promise<void>;
  /** Types text at the cursor, as a keyboard would. */
  insert(text: string): Promise<void>;
  /** Replaces the content of an input. */
  fill(selector: string, text: string): Promise<void>;
  /** Sets a date input to a day (YYYY-MM-DD), as picking it in the calendar would. */
  setDate(selector: string, day: string): Promise<void>;
  /** Puts a local file into a file input, as a person choosing it would. */
  setFile(selector: string, path: string): Promise<void>;
  /** Chooses an option of a native select. */
  select(selector: string, value: string): Promise<void>;
  /** Presses one key: a name from KEY_CODES, or a single character. */
  key(name: string, modifiers?: number): Promise<void>;
  /** Clicks the first enabled element matching the selector whose text is exactly `text`. */
  click(text: string, selector?: string): Promise<void>;
  /**
   * Clicks the checkbox or radio button whose accessible name (its aria-label, else the text of
   * its label) is `name` or starts with it, as a person reading the labels would.
   */
  choose(name: string): Promise<void>;
  text(): Promise<string>;
  /** Path and query of the current address. */
  location(): Promise<string>;
  screenshot(file: string): Promise<void>;
  close(): Promise<void>;
}

export async function launchBrowser(port: number): Promise<Browser> {
  const profile = mkdtempSync(join(tmpdir(), "erpcraft-chrome-"));
  const chrome = Bun.spawn(
    [
      CHROME,
      "--headless=new",
      "--no-sandbox",
      "--disable-gpu",
      "--hide-scrollbars",
      // The print view prints from a window of its own; a script clicking Print is not a user gesture.
      "--disable-popup-blocking",
      `--user-data-dir=${profile}`,
      `--remote-debugging-port=${port}`,
      "about:blank",
    ],
    { stdout: "ignore", stderr: "ignore" },
  );

  let address: string | null = null;
  for (let attempt = 0; attempt < 50 && address === null; attempt += 1) {
    try {
      const targets = asArray(await (await fetch(`http://127.0.0.1:${port}/json/list`)).json());
      const page = targets.map(asObject).find((target) => target.type === "page");
      if (page) address = asString(page.webSocketDebuggerUrl);
    } catch {
      // Chrome is still starting.
    }
    if (address === null) await Bun.sleep(200);
  }
  if (address === null) {
    chrome.kill();
    throw new Error(`Chrome did not start (${CHROME})`);
  }

  const socket = new WebSocket(address);
  await new Promise((done) => socket.addEventListener("open", done, { once: true }));
  let nextId = 0;
  const waiting = new Map<number, (result: JsonObject) => void>();
  const errors: string[] = [];
  // Requests the page has sent and not had an answer to: work under way the DOM may not show yet.
  const inFlight = new Set<string>();
  socket.addEventListener("message", (event) => {
    const message = asObject(JSON.parse(String(event.data)));
    if (typeof message.id === "number") {
      waiting.get(message.id)?.(message.result ? asObject(message.result) : {});
      waiting.delete(message.id);
    } else if (message.method === "Page.javascriptDialogOpening") {
      // "Leave this page?" from a form with unsaved changes: a script moving on means yes.
      void send("Page.handleJavaScriptDialog", { accept: true });
    } else if (message.method === "Network.requestWillBeSent") {
      inFlight.add(asString(asObject(message.params).requestId));
    } else if (message.method === "Network.loadingFinished") {
      inFlight.delete(asString(asObject(message.params).requestId));
    } else if (message.method === "Network.loadingFailed" && asObject(message.params).canceled === true) {
      inFlight.delete(asString(asObject(message.params).requestId));
    } else if (message.method === "Network.loadingFailed" && asObject(message.params).canceled !== true) {
      inFlight.delete(asString(asObject(message.params).requestId));
      // A request the network refused (not one the page called off) is an error worth seeing.
      errors.push(`network: ${JSON.stringify(message.params).slice(0, 300)}`);
    } else if (message.method === "Runtime.exceptionThrown") {
      errors.push(JSON.stringify(message.params).slice(0, 500));
    } else if (message.method === "Runtime.consoleAPICalled" && asObject(message.params).type === "error") {
      errors.push(JSON.stringify(message.params).slice(0, 500));
    }
  });

  function send(method: string, params: JsonObject = {}): Promise<JsonObject> {
    nextId += 1;
    const id = nextId;
    socket.send(JSON.stringify({ id, method, params }));
    return new Promise((done) => waiting.set(id, done));
  }

  async function evaluate(expression: string): Promise<unknown> {
    const answer = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    return answer.result ? asObject(answer.result).value : undefined;
  }

  // Installed before the page's own scripts: the hook React's development build reports its commits
  // to (the one React DevTools installs), reduced to keeping each root, so that the harness can see
  // whether React still has work scheduled. It changes nothing about how the app renders.
  const REACT_HOOK = `window.__REACT_DEVTOOLS_GLOBAL_HOOK__ = {
    supportsFiber: true, isDisabled: false, renderers: new Map(),
    inject() { return 1; },
    onCommitFiberRoot(_id, root) { (window.__reactRoots ??= new Set()).add(root); },
    onCommitFiberUnmount() {}, onPostCommitFiberRoot() {}, setStrictMode() {}, checkDCE() {},
  };`;

  // Runs in the page: lets every task the page has already queued run (React renders in tasks it
  // posts through a MessageChannel, which run in order), then reports how many DOM changes there
  // have been so far and whether anything says it is busy.
  const PAGE_STATE = `(async () => {
    if (!window.__mutations) {
      window.__mutations = 0;
      new MutationObserver((records) => { window.__mutations += records.length; })
        .observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    }
    // A page that is not being drawn (one printing, one in the background) runs no animation frames,
    // so only tasks are waited for, and never longer than a moment each.
    for (let round = 0; round < 3; round += 1) {
      await Promise.race([
        new Promise((done) => { const channel = new MessageChannel(); channel.port1.onmessage = () => done(); channel.port2.postMessage(0); }),
        new Promise((done) => setTimeout(done, 500)),
      ]);
    }
    const rendered = document.getElementById('root')?.childElementCount > 0;
    // Work React has scheduled and not committed yet, such as a navigation the router renders in a
    // transition: the roots it committed say so in their pending lanes (see REACT_HOOK).
    const reactPending = [...(window.__reactRoots ?? [])].some((root) => root.pendingLanes !== 0);
    const busy = document.querySelectorAll('.animate-pulse, [aria-busy=true]').length > 0 || document.body.innerText.includes('Loading…');
    return JSON.stringify({ mutations: window.__mutations, rendered, busy, reactPending });
  })()`;

  async function settle(): Promise<void> {
    // Settled means: no request of the page's is still waiting for its answer, the page has run
    // every task it had queued (renders, effects) and changed nothing while doing so, and nothing
    // on it says it is loading. Each of those is observed, not waited out. A fresh page from the
    // development server can take several seconds to render at all, and much longer while the
    // machine is busy compiling something else; past 90 seconds that is an error.
    const deadline = Date.now() + RENDER_PATIENCE;
    let last = -1;
    let stable = 0;
    while (stable < 2) {
      if (Date.now() > deadline) {
        const rendered = await evaluate("document.getElementById('root')?.childElementCount > 0");
        if (rendered !== true)
          throw new Error(
            `the page did not render within ${RENDER_PATIENCE / 1000} seconds (${await evaluate("location.href")})`,
          );
        return;
      }
      const state = asObject(JSON.parse(String((await evaluate(PAGE_STATE)) ?? "{}")) as JsonObject);
      const mutations = typeof state.mutations === "number" ? state.mutations : -1;
      const quiet =
        state.rendered === true &&
        state.busy !== true &&
        state.reactPending !== true &&
        inFlight.size === 0 &&
        mutations === last;
      stable = quiet ? stable + 1 : 0;
      if (process.env.DEBUG_SETTLE) console.log("settle", Date.now() % 100000, inFlight.size, JSON.stringify(state));
      last = mutations;
      if (!quiet) await Bun.sleep(20);
    }
  }

  const quoted = (value: string): string => JSON.stringify(value);

  async function focus(selector: string): Promise<void> {
    const found = await evaluate(
      `(() => { const e = document.querySelector(${quoted(selector)}); if (!e) return false; e.focus(); if (e.select) e.select(); return true; })()`,
    );
    if (found !== true) throw new Error(`no element matches ${selector}`);
  }

  async function insert(text: string): Promise<void> {
    await send("Input.insertText", { text });
    await Bun.sleep(60);
  }

  async function press(name: string, modifiers = 0): Promise<void> {
    const code = KEY_CODES[name] ?? name.toUpperCase().charCodeAt(0);
    const event = { key: name, code: name, windowsVirtualKeyCode: code, nativeVirtualKeyCode: code, modifiers };
    await send("Input.dispatchKeyEvent", { type: "keyDown", ...event });
    await send("Input.dispatchKeyEvent", { type: "keyUp", ...event });
    await Bun.sleep(80);
  }

  await send("Runtime.enable");
  await send("Page.enable");
  await send("Network.enable");
  await send("Page.addScriptToEvaluateOnNewDocument", { source: REACT_HOOK });

  return {
    errors,
    send,
    evaluate,
    settle,
    async goto(url) {
      // Wait for the new document before judging whether it has settled: right after
      // a navigation starts, the page on screen is still the one being left. A change
      // of the fragment alone loads no new document, so there is nothing to wait for.
      const here = String(await evaluate("location.href.split('#')[0]"));
      const reloads = here !== url.split("#")[0];
      await evaluate("window.__leaving = true");
      await send("Page.navigate", { url });
      for (let attempt = 0; reloads && attempt < 200; attempt += 1) {
        if ((await evaluate("window.__leaving !== true && document.readyState === 'complete'")) === true) break;
        await Bun.sleep(50);
      }
      await settle();
    },
    async viewport(width, height) {
      await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
    },
    async dark(on) {
      await send("Emulation.setEmulatedMedia", {
        features: [{ name: "prefers-color-scheme", value: on ? "dark" : "light" }],
      });
    },
    focus,
    insert,
    async fill(selector, text) {
      await focus(selector);
      await insert(text);
      await settle();
    },
    async setDate(selector, day) {
      // Typing into a date input depends on the locale's order of day and month; the calendar does not.
      const found = await evaluate(
        `(() => { const e = document.querySelector(${quoted(selector)}); if (!e) return false; Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(e, ${quoted(day)}); e.dispatchEvent(new Event("input", { bubbles: true })); return true; })()`,
      );
      if (found !== true) throw new Error(`no element matches ${selector}`);
      await settle();
    },
    async setFile(selector, path) {
      const root = asObject((await send("DOM.getDocument", { depth: 0 })).root);
      const found = await send("DOM.querySelector", { nodeId: root.nodeId ?? 0, selector });
      if (!found.nodeId) throw new Error(`no file input matches ${selector}`);
      await send("DOM.setFileInputFiles", { nodeId: found.nodeId, files: [path] });
      await settle();
    },
    async select(selector, value) {
      const found = await evaluate(
        `(() => { const e = document.querySelector(${quoted(selector)}); if (!e) return false; Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(e, ${quoted(value)}); e.dispatchEvent(new Event("change", { bubbles: true })); return true; })()`,
      );
      if (found !== true) throw new Error(`no select matches ${selector}`);
      await settle();
    },
    key: press,
    async click(text, selector = "button") {
      const found = await evaluate(
        `(() => { const e = [...document.querySelectorAll(${quoted(selector)})].find((b) => b.textContent.trim() === ${quoted(text)} && !b.disabled); if (!e) return false; e.click(); return true; })()`,
      );
      if (found !== true && selector === "button") {
        // Not a button on the page: a person would look in the page's "More actions" menu next.
        const opened = await evaluate(
          `(() => { const t = document.querySelector("[data-more-actions]"); if (!t) return false; t.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerType: "mouse" })); return true; })()`,
        );
        if (opened === true) {
          await settle();
          const chosen = await evaluate(
            `(() => { const e = [...document.querySelectorAll("[role=menuitem]")].find((m) => m.textContent.trim() === ${quoted(text)} && !m.hasAttribute("data-disabled")); if (!e) { document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); return false; } e.click(); return true; })()`,
          );
          if (chosen === true) {
            await settle();
            return;
          }
          await press("Escape");
        }
      }
      if (found !== true) throw new Error(`nothing to click: ${selector} with the text "${text}"`);
      await settle();
    },
    async choose(name) {
      const found = await evaluate(
        `(() => {
          const name = ${quoted(name)};
          const nameOf = (e) => (e.getAttribute("aria-label") ?? e.labels?.[0]?.textContent ?? e.closest("label")?.textContent ?? "").trim();
          const box = [...document.querySelectorAll("[role=checkbox], [role=radio]")]
            .find((e) => !e.disabled && (nameOf(e) === name || nameOf(e).startsWith(name)));
          if (!box) return false;
          box.click();
          return true;
        })()`,
      );
      if (found !== true) throw new Error(`no checkbox or radio button named "${name}"`);
      await settle();
    },
    async text() {
      return String(await evaluate("document.body.innerText"));
    },
    async location() {
      return String(await evaluate("location.pathname + location.search"));
    },
    async screenshot(file) {
      const picture = await send("Page.captureScreenshot", { format: "png" });
      await Bun.write(file, Buffer.from(asString(picture.data), "base64"));
    },
    async close() {
      socket.close();
      chrome.kill();
      await chrome.exited;
      rmSync(profile, { recursive: true, force: true });
    },
  };
}
