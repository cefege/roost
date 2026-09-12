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

import { Show, createSignal, onCleanup, onMount } from "solid-js";
import type { Component } from "solid-js";
import { Button, Icon, Surface } from "./Settings/md/primitives.tsx";

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
          bottom: "var(--md-space-4)",
          left: "var(--md-space-4)",
          "z-index": "49", // just under ConnectionBanner (50)
        }}
      >
        <Surface
          level={2}
          elevation={3}
          radius="md"
          style={{
            display: "flex",
            "align-items": "flex-start",
            gap: "var(--md-space-3)",
            padding: "var(--md-space-3) var(--md-space-4)",
            "max-width": "min(42ch, calc(100vw - var(--md-space-8)))",
            border: "var(--workbench-border-width) solid var(--md-sys-color-primary)",
            color: "var(--md-sys-color-on-surface)",
          }}
        >
          <Icon
            name="arrow_upward"
            size="sm"
            style={{ "flex-shrink": "0", color: "var(--md-sys-color-primary)" }}
          />
          <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}>
            <span class="md-title-s">Roost just updated</span>
            <span class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
              A newer version is ready. Your sessions are safe — reload when convenient.
            </span>
            <div style={{ display: "flex", gap: "var(--md-space-2)", "margin-top": "var(--md-space-1)" }}>
              <Button
                variant="default"
                size="sm"
                data-testid="version-banner-reload"
                onClick={() => window.location.reload()}
              >
                Reload now
              </Button>
              <Button
                variant="ghost"
                size="sm"
                data-testid="version-banner-later"
                onClick={() => setDismissedSha(servedSha())}
              >
                Later
              </Button>
            </div>
          </div>
        </Surface>
      </div>
    </Show>
  );
};
