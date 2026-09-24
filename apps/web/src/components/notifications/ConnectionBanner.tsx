// ConnectionBanner — surfaces "offline" and "coordinator unreachable" states.
// Subscribes to navigator.onLine events and polls window.__roostCoordHealth
// (set by store/sync.ts in production; test hooks set it directly).
// Callers: App.tsx (always mounted; internal Show gate).
// Exposes: data-testid="connection-banner" data-banner-reason="offline"|"coord-unreachable"|"coord-unreachable-direct-live"|"coord-mixed-content"
import { createSignal, onMount, onCleanup, Show } from "solid-js";
import type { Component } from "solid-js";
import { Button, StatusDot, Surface } from "../Settings/md/primitives.tsx";
import {
  hasLivenessQualifiedDirectTerminal,
} from "../../store/local-transport-indicator.ts";
import { isPageVisible } from "../../browser/pageVisible.ts";
import { reconnectNow } from "../../store/sync-redial.ts";
import type { CoordHealthSnapshot } from "../../store/sync-health.ts";


type BannerReason =
  | "offline"
  | "coord-unreachable"
  | "coord-unreachable-direct-live"
  | "coord-mixed-content"
  | null;

const COORD_STALE_MS = 10_000;

function readCoordHealth(): CoordHealthSnapshot | null {
  return (window as Window & { __roostCoordHealth?: CoordHealthSnapshot }).__roostCoordHealth ?? null;
}


export const ConnectionBanner: Component = () => {
  const [reason, setReason] = createSignal<BannerReason>(null);
  // The underlying error (ConnectError message / HTTP status) behind an
  // "unreachable" — shown as a tooltip so the banner is diagnosable instead of
  // a bare "unreachable". Empty when the trigger was staleness, not an error.
  const [detail, setDetail] = createSignal<string>("");
  const directTerminalsStillLive = () => reason() === "coord-unreachable-direct-live";

  function evaluate(): void {
    if (!navigator.onLine) {
      setReason("offline");
      return;
    }
    const health = readCoordHealth();
    if (health) {
      const now = performance.now();
      const lastOk = health.lastSuccessMs;
      // Staleness only counts while the tab is VISIBLE: the health poller skips
      // hidden tabs (sync.ts), so a backgrounded tab is "stale" by definition —
      // treating that as "unreachable" is a false alarm that wrongly says coord
      // is down when the tab was just in the background. A real failure still
      // surfaces via lastResult.kind === "unreachable".
      const stale = isPageVisible() && lastOk !== null && now - lastOk > COORD_STALE_MS;
      const lastResult = health.lastResult;
      if (stale || (lastResult && lastResult.kind === "unreachable")) {
        const lastErr = lastResult?.error ?? "";
        setDetail(lastErr || (stale ? `no response in ${Math.round(COORD_STALE_MS / 1000)}s` : ""));
        if (lastErr.includes("mixed") || lastErr.includes("blocked")) {
          setReason("coord-mixed-content");
        } else if (hasLivenessQualifiedDirectTerminal()) {
          // A confirmed direct terminal may continue independently, so an outage
          // banner must not claim that its terminal is unavailable.
          setReason("coord-unreachable-direct-live");
        } else {
          setReason("coord-unreachable");
        }
        return;
      }
    }
    setDetail("");
    setReason(null);
  }

  onMount(() => {
    evaluate();
    const onOffline = () => setReason("offline");
    const onOnline = () => evaluate();
    window.addEventListener("offline", onOffline);
    window.addEventListener("online", onOnline);
    // Hidden-tab gate: no point evaluating a banner nobody can see.
    const timer = setInterval(() => { if (isPageVisible()) evaluate(); }, 2_000);
    onCleanup(() => {
      window.removeEventListener("offline", onOffline);
      window.removeEventListener("online", onOnline);
      clearInterval(timer);
    });
  });

  return (
    <Show when={reason() != null}>
      <div
        data-testid="connection-banner"
        data-banner-reason={reason()!}
        title={detail() || undefined}
        style={{
          position: "fixed",
          inset: "0 0 auto",
          "z-index": "50",
        }}
      >
        <Surface
          level={2}
          elevation={2}
          radius="xs"
          style={{
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            gap: "var(--md-space-3)",
            padding: "var(--md-space-2) var(--md-space-4)",
            "border-bottom": `var(--workbench-border-width) solid ${
              directTerminalsStillLive() ? "var(--status-warn)" : "var(--md-sys-color-error)"
            }`,
            color: "var(--md-sys-color-on-surface)",
            font: "var(--md-body-s-weight) var(--md-body-s-size)/var(--md-body-s-line) var(--md-font)",
          }}
        >
          {/* Partial degradation, not an outage: direct terminals can remain
              available while coordinator-owned fleet controls cannot. */}
          <StatusDot status={directTerminalsStillLive() ? "warn" : "error"} />
          <Show when={reason() === "offline"}>
            <span>Offline — check your network connection</span>
          </Show>
          <Show when={reason() === "coord-unreachable" || reason() === "coord-unreachable-direct-live"}>
            <span>
              {directTerminalsStillLive()
                ? "Coordinator unreachable — direct terminals may remain available; fleet controls unavailable"
                : "Coordinator unreachable — sessions paused"}
            </span>
            <Button
              variant="secondary"
              size="sm"
              data-testid="connection-banner-reconnect"
              onClick={() => {
                reconnectNow();
                setReason(null); // optimistic; evaluate() re-confirms in ≤2s
              }}
            >
              Reconnect
            </Button>
          </Show>
          <Show when={reason() === "coord-mixed-content"}>
            <span>Coordinator HTTP blocked (mixed content) — use HTTPS</span>
          </Show>
        </Surface>
      </div>
    </Show>
  );
};
