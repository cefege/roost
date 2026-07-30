// Entry point — mounts App into #app.
// Called by index.html. No SolidStart; plain Vite.
// Applies persisted theme before first render to prevent flash-of-wrong-theme.
// CSS vars (--bg-base, --bg-elev-1, --border-strong, --text-hi, --text-lo)
// defined in styles/theme-vars.css; data-theme set here before render.

import { render } from "solid-js/web";
import { App } from "./App.tsx";
import { loadTheme, applyTheme } from "./lib/theme.ts";
import { loadAgentConfig } from "./lib/agents.ts";
import { installSpaDiag, installSignalShip } from "./lib/diag.ts";
import { installLeakWatch } from "./lib/leakWatch.ts";
import "./lib/keyboardInset.ts"; // side effect: track soft-keyboard inset via --kb-offset
import { diag, signal } from "@roost/shared/diag";
import { effectiveAttempts, shouldReloadForChunkError } from "./lib/chunkError.ts";
import "./styles/theme-vars.css";
import "./styles/syntax-vars.css";
import "./styles/sidebar.css";
import "./styles/voice-input.css";
import "./styles/settings-dense.css";
import "./styles/drive.css";

// Apply before Solid renders any component so data-theme is set on first paint.
applyTheme(loadTheme());

// Tier-1 signal channel — ALWAYS on (ships anomalies/errors to coord
// *.err.log even with the diag firehose off). Tier-2 diag firehose is
// gated by localStorage.roostDiag. Install BEFORE first render so
// spa.uncaught catches setup throws.
installSignalShip();
installSpaDiag();
// Global error catch — Solid reactive throws + chunk-load failures go
// to window.onerror; rejected promises to onunhandledrejection. Both
// are otherwise invisible (console.error gets eaten in app-corner windows).

// Chunk-mismatch recovery: after a redeploy the old tab's lazy() import
// hashes are stale. Vite fires vite:preloadError for every failed dynamic
// import(). Auto-reload is the only correct recovery — the new index.html
// (never cached) points to current-hash chunks. Guard against offline and
// tight reload loops with a cooldown.
//
// vite:preloadError covers Vite's dynamic-import wrapper ONLY. A TOP-LEVEL
// module script that 404s never fires it — that arrives as a plain window
// `error` ("Importing a module script failed" in Safari), which is what a
// production tab hit on 2026-07-29 after a redeploy. Both paths share the
// counter below so the loop guard can't be defeated by alternating them.
const CHUNK_RELOAD_KEY = "roost.chunkReload";
function readChunkReloadState(): { attempts: number; lastReloadAt: number } {
  try {
    const raw = sessionStorage.getItem(CHUNK_RELOAD_KEY);
    if (!raw) return { attempts: 0, lastReloadAt: 0 };
    const parsed: unknown = JSON.parse(raw);
    if (parsed && typeof parsed === "object" && "attempts" in parsed && "lastReloadAt" in parsed) {
      const a = parsed.attempts;
      const l = parsed.lastReloadAt;
      return {
        attempts: typeof a === "number" ? a : 0,
        lastReloadAt: typeof l === "number" ? l : 0,
      };
    }
  } catch { /* sessionStorage unavailable / malformed → treat as first attempt */ }
  return { attempts: 0, lastReloadAt: 0 };
}
// sessionStorage, not a module global: the reload discards module state, so an
// in-memory counter would reset every time and loop forever.
function recoverFromChunkError(message: string | undefined | null): boolean {
  const stored = readChunkReloadState();
  const now = Date.now();
  // Expire a stale count so a long-lived tab keeps self-healing across deploys.
  const attempts = effectiveAttempts(stored.attempts, stored.lastReloadAt, now);
  const go = shouldReloadForChunkError({
    message,
    now,
    lastReloadAt: stored.lastReloadAt,
    online: navigator.onLine,
    attempts,
  });
  if (!go) return false;
  try {
    sessionStorage.setItem(
      CHUNK_RELOAD_KEY,
      JSON.stringify({ attempts: attempts + 1, lastReloadAt: Date.now() }),
    );
  } catch { /* can't persist → the cooldown below still bounds one reload */ }
  signal("spa.chunk_reload", { msg: String(message ?? "").slice(0, 120), attempt: attempts + 1 });
  window.location.reload();
  return true;
}
window.addEventListener("vite:preloadError", () => {
  recoverFromChunkError("failed to fetch dynamically imported module");
});

window.addEventListener("error", (e) => {
  // Stale-chunk errors are recoverable; reload instead of reporting a crash.
  if (recoverFromChunkError(e.message)) return;
  signal("spa.uncaught", {
    kind: "error",
    msg: e.message,
    file: e.filename,
    line: e.lineno,
    col: e.colno,
    stack8: e.error?.stack?.slice(0, 240) ?? null,
    // cooldownKey: per file:line so distinct crashes aren't coalesced,
    // but a tight render-loop throw at one site can't flood the channel.
    cooldownKey: `${e.filename}:${e.lineno}`,
  });
});
window.addEventListener("unhandledrejection", (e) => {
  const r = e.reason as { message?: string; stack?: string } | undefined;
  // A failed lazy() import REJECTS rather than throwing, so this is the path a
  // stale route chunk actually takes — the 2026-07-29 production report arrived
  // here (empty stack, message only). Recover before reporting a crash.
  if (recoverFromChunkError(r?.message ?? String(e.reason ?? ""))) return;
  signal("spa.uncaught", {
    kind: "rejection",
    msg: String(r?.message ?? r ?? ""),
    stack8: r?.stack?.slice(0, 240) ?? null,
    cooldownKey: `rej:${String(r?.message ?? "").slice(0, 60)}`,
  });
});
// iOS app-lifecycle (firehose context — not signal). iOS Safari freezes
// backgrounded tabs (Page Lifecycle `freeze`/`resume`) and restores them
// from bfcache (`pageshow` with persisted=true) WITHOUT re-running module
// init — which is exactly when the sync stream / input channel silently
// die. These markers let a deep-dive correlate "came back dead" with the
// reconnect.give_up signal. visibilitychange is already handled in sync.ts.
document.addEventListener("freeze", () => diag("app.freeze", {}));
document.addEventListener("resume", () => diag("app.resume", {}));
window.addEventListener("pageshow", (e) => {
  diag("app.pageshow", { persisted: (e as PageTransitionEvent).persisted });
});

async function mountApp(): Promise<void> {
  const root = document.getElementById("app");
  if (!root) throw new Error("no #app element");
  render(() => <App />, root);
  // Always-on leak watchdog: periodic accumulator sample + long-task correlation,
  // shipped Tier-1 so a natural multi-day run self-reports what grows / when it
  // stalls (freeze-hunt evidence we can't reproduce naturally).
  installLeakWatch();
  void loadAgentConfig();
}

void mountApp();
