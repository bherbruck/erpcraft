import { defineConfig } from "@playwright/test";

const CHROME = process.env.CHROME ?? "/usr/bin/google-chrome";

export default defineConfig({
  testDir: "e2e",
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:5175",
    viewport: { width: 1280, height: 800 },
    launchOptions: { executablePath: CHROME },
    permissions: ["clipboard-read", "clipboard-write"],
  },
  webServer: {
    command: "bun run demo",
    url: "http://127.0.0.1:5175",
    reuseExistingServer: true,
    timeout: 30_000,
  },
});
