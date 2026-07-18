// Vite config for @roost/web SPA.
// Output: apps/web/dist/ — served by coord (R0.1, R4.3).
// No SolidStart/Vinxi — plain Vite + vite-plugin-solid.

import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { execSync } from "node:child_process";

// Live git sha baked into the bundle so the running tab knows its own build.
// The SPA compares it to coord's live git_sha (coord_identity) and offers a
// "reload" nudge on mismatch — correct because push builds the SPA to coord's
// commit (roost-cli/push.ts). Full HEAD to match coord/git-sha.ts format.
function resolveBuildSha(): string {
  try { return execSync("git rev-parse HEAD").toString().trim(); }
  catch { return process.env.ROOST_GIT_SHA ?? "dev"; }
}
const BUILD_SHA = resolveBuildSha();

// Dev-only /api proxy target — your coord URL from .env.local. Unset → no
// proxy (SPA still loads; /api calls need ROOST_COORDINATOR_URL to reach coord).
const coordUrl = process.env.ROOST_COORDINATOR_URL;
if (!coordUrl) console.warn("[vite] ROOST_COORDINATOR_URL unset — /api dev proxy disabled (set it in .env.local)");

export default defineConfig({
  plugins: [
    solidPlugin(),
    // Stamp the build sha into the served index.html so a running tab can
    // re-fetch it (coord serves index.html no-store) and learn the sha of the
    // dist ON DISK — the only "newer build" signal a reload can actually
    // resolve. See VersionBanner.tsx. ponytail: 1 meta tag, no wire field.
    {
      name: "roost-build-sha-meta",
      transformIndexHtml() {
        return [{ tag: "meta", attrs: { name: "roost-build-sha", content: BUILD_SHA }, injectTo: "head" as const }];
      },
    },
  ],
  define: {
    "import.meta.env.VITE_APP_VERSION": JSON.stringify("2.0.0"),
    "import.meta.env.VITE_BUILD_SHA": JSON.stringify(BUILD_SHA),
  },
  build: {
    outDir: "dist",
    target: "esnext",
    rollupOptions: {
      output: {
        // Park node_modules in its own hash so editing app code only
        // invalidates the small app chunk — the stable vendor chunk
        // (wterm/protobuf/connect/solid) stays client-cached across deploys.
        // Route/overlay code-splitting policy (perf sweep C2): cold surfaces
        // (Settings, DesignGallery, Onboarding, Help, HelpOverlay,
        // FileViewerSheet, CommandPaletteBody) are `lazy()` components — each
        // gets its own chunk fetched on first visit/open. Everything else
        // stays in the eager app chunk; no ad-hoc import() churn beyond those.
        manualChunks: (id) => (id.includes("node_modules") ? "vendor" : undefined),
      },
    },
  },
  server: {
    host: true,
    allowedHosts: [".ts.net"],
    proxy: coordUrl
      ? {
          "/api": {
            target: coordUrl,
            secure: false, // tailscale cert is trusted via tailscale CA; node may not know
            changeOrigin: true,
            ws: true, // /api/events is a WebSocket — vite must upgrade & forward
          },
        }
      : undefined,
  },
  resolve: {
    conditions: ["browser"],
  },
});
