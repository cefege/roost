// VersionBanner — "a newer SPA build is on disk, reload to get it" nudge.
// The running tab's git sha is baked at build time (VITE_BUILD_SHA, ALSO
// injected as <meta name="roost-build-sha"> into index.html by vite.config.ts).
// coord serves index.html no-store, so re-fetching it yields the sha of the
// dist ON DISK RIGHT NOW. Banner fires iff this tab's sha != the served sha —
// so Reload (fresh index.html → new hashed bundle → new baked sha) ALWAYS
// clears it. We deliberately do NOT compare against coord's live git HEAD
// (coord_identity.git_sha): HEAD advances on coord restart without a SPA
// rebuild, firing a banner reload can never fix — the "reload does nothing"
// bug (push.ts:75). served-dist sha is the only reload-resolvable signal.
// Re-checked on mount + window focus (a deploy can land while you're away).
// Copy is reassuring, not alarming ("Roost just updated", "your sessions are
// safe") + a primary up-arrow (NOT an error dot) so it never reads like the
// red ConnectionBanner "coordinator dead" state. "Later" suppresses the nudge
// for the currently-served sha; a newer build re-nudges.
// Layout: bottom-left fixed card (NOT a full-width top bar — that shifted the
// whole app layout down). Elevated + primary-bordered so it reads as important.
// Callers: App.tsx (always mounted; internal Show gate).

import { type Component, Show, createSignal, onCleanup, onMount } from "solid-js";
import { Button } from "./Settings/md/Button.tsx";

const BUILD_SHA = (import.meta.env as { VITE_BUILD_SHA?: string }).VITE_BUILD_SHA;

async function fetchServedSha(): Promise<string | null> {
  try {
    const res = await fetch("/index.html", { cache: "no-store" });
    if (!res.ok) return null;
    const doc = new DOMParser().parseFromString(await res.text(), "text/html");
    return doc.querySelector('meta[name="roost-build-sha"]')?.getAttribute("content") ?? null;
  } catch {
    return null;
  }
}

export const VersionBanner: Component = () => {
  const [servedSha, setServedSha] = createSignal<string | null>(null);
  // The served sha the user chose "Later" on — suppresses the nudge for THIS
  // build only. A newer deploy (served sha changes) clears it, re-nudging.
  const [dismissedSha, setDismissedSha] = createSignal<string | null>(null);
  // 60 s cooldown (perf sweep C1.7): window focus can fire in rapid bursts
  // (⌘Tab flapping) — don't pay a fetch + DOMParser per flap.
  let lastCheck = 0;
  const refresh = () => {
    const now = Date.now();
    if (now - lastCheck < 60_000) return;
    lastCheck = now;
    void fetchServedSha().then(setServedSha);
  };

  onMount(() => {
    refresh();
    window.addEventListener("focus", refresh);
    onCleanup(() => window.removeEventListener("focus", refresh));
  });

  // Fail safe: no signal in dev (unstamped "dev") or before the first fetch.
  const isStale = (): boolean => {
    const served = servedSha();
    if (!BUILD_SHA || BUILD_SHA === "dev") return false;
    if (!served || served === "dev") return false;
    if (served === dismissedSha()) return false;
    return served !== BUILD_SHA;
  };

  return (
    <Show when={isStale()}>
      <div
        data-testid="version-banner"
        style={{
          position: "fixed",
          bottom: "16px",
          left: "16px",
          "max-width": "340px",
          "z-index": "49", // just under ConnectionBanner (50)
          display: "flex",
          "align-items": "flex-start",
          gap: "12px",
          padding: "12px 14px",
          "border-radius": "var(--md-shape-md)",
          "font-size": "13px",
          "font-family":
            'Roboto, "Helvetica Neue", -apple-system, BlinkMacSystemFont, system-ui, sans-serif',
          background: "var(--md-surface-container-high)",
          color: "var(--md-on-surface)",
          border: "1px solid var(--md-primary)",
          "box-shadow": "var(--md-elev-3)",
        }}
      >
        <svg
          aria-hidden="true" width="18" height="18" viewBox="0 0 24 24"
          fill="none" stroke="var(--md-primary)" stroke-width="2.2"
          stroke-linecap="round" stroke-linejoin="round"
          style={{ "flex-shrink": "0" }}
        >
          <path d="M12 19V5M5 12l7-7 7 7" />
        </svg>
        <span
          style={{ display: "flex", "flex-direction": "column", gap: "8px", "line-height": "1.35" }}
        >
          <span style={{ "font-weight": "600" }}>Roost just updated</span>
          <span style={{ color: "var(--md-on-surface-variant)" }}>
            A newer version is ready. Your sessions are safe — reload when convenient.
          </span>
          <span style={{ display: "flex", gap: "8px", "margin-top": "2px" }}>
            <Button
              variant="filled"
              data-testid="version-banner-reload"
              onClick={() => window.location.reload()}
            >Reload now</Button>
            <Button
              variant="text"
              data-testid="version-banner-later"
              onClick={() => setDismissedSha(servedSha())}
            >Later</Button>
          </span>
        </span>
      </div>
    </Show>
  );
};
