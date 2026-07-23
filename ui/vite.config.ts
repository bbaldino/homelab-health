import { defineConfig } from "vite";
import preact from "@preact/preset-vite";

export default defineConfig({
  plugins: [preact()],
  build: { outDir: "dist", emptyOutDir: true },
  server: {
    // dev-only: proxy API calls to the running Rust backend for HMR.
    proxy: { "/api": "http://localhost:8080" },
  },
});
