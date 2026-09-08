// ConnectionBanner — surfaces "offline" and "coordinator unreachable" states.
// Subscribes to navigator.onLine events and polls window.__roostCoordHealth
// (set by store/sync.ts in production; test hooks set it directly).
// Callers: App.tsx (always mounted; internal Show gate).
// Exposes: data-testid="connection-banner" data-banner-reason="offline"|"coord-unreachable"|"coord-mixed-content"

import { type Component, createSignal, onMount, onCleanup, Show } from "solid-js";
import { Button } from "./Settings/md/Button.tsx";
import { isPageVisible } from "../lib/pageVisible.ts";

export interface CoordHealthSnapshot {
  lastSuccessMs: number | null;
  lastErrorMs: number | null;
  lastResult: { kind: string; error?: string } | null;
}

type BannerReason = "offline" | "coord-unreachable" | "coord-mixed-content" | null;

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
          top: "0",
          left: "0",
          right: "0",
          "z-index": "50",
          display: "flex",
          "align-items": "center",
          "justify-content": "center",
          gap: "10px",
          padding: "8px 16px",
          "font-size": "13px",
          "font-family":
            'Roboto, "Helvetica Neue", -apple-system, BlinkMacSystemFont, system-ui, sans-serif',
          // M3 dark-theme error surface: container-high bg + on-surface
          // text + error-colored accent dot + a hairline error border.
          background: "var(--md-surface-container-high)",
          color: "var(--md-on-surface)",
          "border-bottom": "1px solid var(--md-error)",
          "box-shadow": "var(--md-elev-2)",
        }}
      >
        <span
          aria-hidden="true"
          style={{
            width: "8px", height: "8px", "border-radius": "50%",
            background: "var(--md-error)", "flex-shrink": "0",
          }}
        />
        <Show when={reason() === "offline"}>
          <span>Offline — check your network connection</span>
        </Show>
        <Show when={reason() === "coord-unreachable"}>
          <span>Coordinator unreachable — sessions paused</span>
          <Button
            variant="tonal"
            data-testid="connection-banner-reconnect"
            onClick={async () => {
              const { reconnectNow } = await import("../store/sync.ts");
              reconnectNow();
              setReason(null); // optimistic; evaluate() re-confirms in ≤2s
            }}
          >Reconnect</Button>
        </Show>
        <Show when={reason() === "coord-mixed-content"}>
          <span>Coordinator HTTP blocked (mixed content) — use HTTPS</span>
        </Show>
      </div>
    </Show>
  );
};
