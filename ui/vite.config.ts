import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const SERVER = process.env.ERPCRAFT_URL ?? "http://127.0.0.1:8600";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  server: {
    port: 5173,
    strictPort: true,
    host: "127.0.0.1",
    // The browser only ever talks to this origin, so no CORS is needed.
    proxy: {
      "/api": { target: SERVER, changeOrigin: true },
      "/mcp": { target: SERVER, changeOrigin: true },
    },
  },
});
