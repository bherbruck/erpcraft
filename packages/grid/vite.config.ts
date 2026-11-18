import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The demo page: the grid with the plain reference renderer, no design system.
export default defineConfig({
  root: "demo",
  plugins: [react()],
  server: { host: "127.0.0.1", port: 5175, strictPort: true },
});
