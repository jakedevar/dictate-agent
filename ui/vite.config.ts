import { resolve } from "node:path";
import { defineConfig } from "vite";

// Two entry points: the hub (Preact) and the Flow bar (no framework at all —
// it is on screen within a frame of `recording`, so it carries nothing it does
// not need). No remote resources, no analytics, no service worker.
export default defineConfig({
  clearScreen: false,
  server: { host: "127.0.0.1", port: 1420, strictPort: true },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    rollupOptions: {
      input: {
        index: resolve(import.meta.dirname, "index.html"),
        hud: resolve(import.meta.dirname, "hud.html"),
      },
    },
  },
});
