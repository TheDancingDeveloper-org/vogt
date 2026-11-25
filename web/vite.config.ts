import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import type { Plugin } from "vite";

// Backend dev server target — `cargo run -p vogt-engine-server -- --bind 127.0.0.1:8910`
const BACKEND = "http://127.0.0.1:8910";

// `src/index.tsx` awaits the runtime transport before it imports `./App`, so
// the App chunk would download one round trip late. Preload it from the HTML
// (fetch and parse only, not evaluate) so the download overlaps the transport
// await. The import stays dynamic, so the demo seeding order is unchanged.
// The chunk name is content-hashed, so the link is injected from the bundle.
function preloadAppChunk(): Plugin {
  return {
    name: "vogt-preload-app-chunk",
    apply: "build",
    transformIndexHtml: {
      order: "post",
      handler(html, ctx) {
        const chunk = Object.values(ctx.bundle ?? {}).find(
          (item) =>
            item.type === "chunk" &&
            item.facadeModuleId != null &&
            /[\\/]src[\\/]App\.tsx$/.test(item.facadeModuleId),
        );
        if (!chunk) {
          throw new Error("preloadAppChunk: App chunk not found in bundle");
        }
        const tag = `<link rel="modulepreload" crossorigin href="/${chunk.fileName}">`;
        return html.replace("</head>", `    ${tag}\n  </head>`);
      },
    },
  };
}

export default defineConfig({
  plugins: [solid(), preloadAppChunk()],
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
    // `scripts/check_bundle.py` reads this to work out what the first screen
    // actually costs: which chunk is the entry, and which chunks it pulls in
    // statically rather than when a route asks for them.
    manifest: true,
    sourcemap: process.env.VITE_SOURCEMAP === "1",
  },
});
