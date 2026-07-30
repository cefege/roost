// Vite config for @roost/web SPA.
// Output: apps/web/dist/ — served by coord (R0.1, R4.3).
// No SolidStart/Vinxi — plain Vite + vite-plugin-solid.

import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import reactPlugin from "@vitejs/plugin-react";
import { resolve as resolvePath } from "node:path";
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

const OMP_COLLAB_SOURCE = resolvePath(import.meta.dirname, "../../vendor/omp/packages/collab-web/src");
const REACT_TSX = [/\/vendor\/omp\/packages\/collab-web\/src\/.*\.tsx$/, /\/components\/agent\/OmpSessionSurface\.tsx$/];

export default defineConfig({
  plugins: [
    solidPlugin({ exclude: REACT_TSX }),
    reactPlugin({ include: REACT_TSX }),
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
          // Connect RPCs hit /roost.v1.<Service>/* at root, and the live sync
          // feed is a WebSocket at /ws/coord-sync — neither lives under /api,
          // so both need their own proxy entry or the dev app 404s every call
          // against vite and shows no coord data. Keyed on the /roost.v1.
          // prefix so services added later (crpc2+) proxy without a new rule.
          "/api": {
            target: coordUrl,
            secure: false, // tailscale cert is trusted via tailscale CA; node may not know
            changeOrigin: true,
            ws: true, // /api/events is a WebSocket — vite must upgrade & forward
          },
          "/roost.v1.": {
            target: coordUrl,
            secure: false,
            changeOrigin: true,
          },
          "/ws/coord-sync": {
            target: coordUrl,
            secure: false,
            changeOrigin: true,
            ws: true,
          },
        }
      : undefined,
  },
  resolve: {
    alias: {
      "@oh-my-pi/collab-web": resolvePath(OMP_COLLAB_SOURCE, "index.ts"),
      "@oh-my-pi/pi-wire": resolvePath(import.meta.dirname, "../../vendor/omp/packages/wire/src/index.ts"),
      "lucide-react": resolvePath(import.meta.dirname, "node_modules/lucide-react"),
      "marked": resolvePath(import.meta.dirname, "node_modules/marked"),
    },
    dedupe: ["react", "react-dom"],
    conditions: ["browser"],
  },
});
