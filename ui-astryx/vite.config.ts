import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const SERVER = process.env.ERPCRAFT_URL ?? "http://127.0.0.1:8600";
const proxy = { "/api": { target: SERVER, changeOrigin: true } };
// The policy the erpcraft binary serves the UI with, so the preview catches anything it would block
// (a font or script from another origin, an inline script).
const headers = {
  "content-security-policy":
    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'",
};

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  // The browser only talks to this origin; /api is proxied to the erpcraft server.
  server: { port: 5174, strictPort: true, host: "127.0.0.1", proxy, allowedHosts: true },
  preview: { port: 5174, strictPort: true, host: "127.0.0.1", proxy, allowedHosts: true, headers },
});
