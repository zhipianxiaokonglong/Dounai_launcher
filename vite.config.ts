import { defineConfig } from "vite";
import { resolve } from "node:path";

export default defineConfig({
  root: ".",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    sourcemap: false,
    rollupOptions: { input: { main: "index.html" } },
  },
  server: { port: 5175, strictPort: true },
  test: { environment: "node", include: ["tests/**/*.test.ts"] },
  resolve: { alias: { "/manifests": resolve("manifests"), "/bundles": resolve("bundles") } },
  publicDir: false,
});