import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const SERVER = process.env.ERPCRAFT_URL ?? "http://127.0.0.1:8600";
const proxy = { "/api": { target: SERVER, changeOrigin: true } };

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  // The browser only talks to this origin; /api is proxied to the erpcraft server.
  server: { port: 5174, strictPort: true, host: "127.0.0.1", proxy, allowedHosts: true },
  preview: { port: 5174, strictPort: true, host: "127.0.0.1", proxy, allowedHosts: true },
});
