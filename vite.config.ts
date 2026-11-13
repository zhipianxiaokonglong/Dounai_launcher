import { defineConfig } from "vite";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// 版本号只在构建期读一次，运行期不再访问文件系统。
const pkg = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf-8"),
) as { version: string };

export default defineConfig({
  root: ".",
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
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