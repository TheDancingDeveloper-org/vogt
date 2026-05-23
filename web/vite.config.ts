import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// Backend dev server target — `cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910`
const BACKEND = "http://127.0.0.1:8910";

export default defineConfig({
  plugins: [solid()],
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      "/api": {
        target: BACKEND,
        changeOrigin: true,
        ws: true,
      },
      "/healthz": BACKEND,
    },
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
  },
});
