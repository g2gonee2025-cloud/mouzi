import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// This file is evaluated by Node, so `process` exists at runtime, but the
// project has no `@types/node` dependency, so it is not in the type system.
// Declared here in module scope (no global pollution, and a future
// `@types/node` would simply be shadowed rather than conflict) instead of
// suppressing the error.
declare const process: { env: { TAURI_DEV_HOST?: string } };

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
