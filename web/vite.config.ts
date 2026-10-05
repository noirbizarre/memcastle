import vue from "@vitejs/plugin-vue"
import { fileURLToPath, URL } from "node:url"
import { defineConfig } from "vite"
import { discoverDaemon } from "./dev/daemon.ts"

// The daemon serves this build under `/ui/` (assets resolve to `web/dist`, in a worktree as in a package), so every
// URL in the build is relative to that prefix. `src/api/client.ts` calls `/api` on the same origin and nothing else.
export default defineConfig(async ({ command }) => ({
  base: "/ui/",
  plugins: [vue()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
      // The brand is the documentation's icon, not a copy of it: one file to change when the logo does.
      "@brand": fileURLToPath(new URL("../docs/images", import.meta.url)),
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Nothing that ships to /usr/share should carry a source map: the daemon would serve the sources.
    sourcemap: false,
    chunkSizeWarningLimit: 1200,
  },
  server: {
    // The dev server may read the icon that lives beside the documentation, outside this package.
    fs: { allow: [".."] },
    // Development against a running daemon: the page is the dev server's, `/api` is the daemon's.
    proxy: command === "serve" ? { "/api": { target: await discoverDaemon(), changeOrigin: false } } : undefined,
  },
  test: {
    environment: "happy-dom",
    include: ["test/**/*.test.ts"],
    // The daemon suite spawns a real `memcastle serve`; it runs under `bun test` (see package.json).
    exclude: ["test/daemon/**", "node_modules/**"],
  },
}))
