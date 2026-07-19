// Gmail-style undo snackbars for soft-closed terminals. ONE dark card per
// pending close, stacked bottom-center of the viewport. Two lines: the closed
// terminal's name ("<name> closed") over a dimmer "<folder> · <server>" line,
// "Undo" on the right, and a thin coral bar draining over that card's own 5s
// window. Each card owns its timer independently — see lib/pendingClose.ts.
// Bottom-center (not bottom-left) keeps the stack clear of the sidebar's New
// Session FAB and the bottom-right toast stack.
//
// Tokens are the confirmed-GLOBAL ones from theme-vars.css (colors, --md-shape-*,
// --md-elev-*). NOT the Settings-scoped --md-space-*/type ramp — this mounts at
// App root where those aren't loaded (L11 undefined-var trap); px is deliberate.

import { For, Show } from "solid-js";
import { pendingCloses, undoOne, UNDO_WINDOW_MS } from "../lib/pendingClose.ts";
import { Button } from "./Settings/md/Button.tsx";

export function UndoCloseBanner() {
  return (
    <Show when={pendingCloses().length > 0}>
      <style>{`
        @keyframes undo-snackbar-in {
          from { opacity: 0; transform: translateY(20px); }
          to   { opacity: 1; transform: translateY(0); }
        }
        @keyframes undo-snackbar-bar {
          from { transform: scaleX(1); }
          to   { transform: scaleX(0); }
        }
      `}</style>
      <div
        data-testid="undo-close-stack"
        style={{
          position: "fixed",
          left: "50%",
          transform: "translateX(-50%)",
          bottom: "24px",
          "z-index": 200,
          display: "flex",
          "flex-direction": "column",
          "align-items": "center",
          gap: "8px",
          "pointer-events": "none",
        }}
      >
        <For each={pendingCloses()}>
          {(entry) => {
            // Snapshot (view fields set once at close time); a plain const is
            // fine — no post-close reactivity, matching the old label render.
            const sub = [entry.folder, entry.server].filter(Boolean).join(" · ");
            return (
              <div
                data-testid="undo-close-banner"
                data-session-id={entry.sessionId}
                style={{
                  position: "relative",
                  display: "flex",
                  "align-items": "center",
                  gap: "12px",
                  background: "var(--md-surface-container-highest)",
                  color: "var(--md-on-surface)",
                  padding: "11px 10px 11px 16px",
                  "border-radius": "var(--md-shape-md)",
                  "box-shadow": "var(--md-elev-4)",
                  "font-family": "inherit",
                  "min-width": "300px",
                  "max-width": "min(568px, calc(100vw - 40px))",
                  overflow: "hidden",
                  "pointer-events": "auto",
                  animation: "undo-snackbar-in 180ms var(--md-sys-motion-easing-standard-decelerate)",
                }}
              >
                <div style={{ flex: 1, "min-width": 0, display: "flex", "flex-direction": "column", gap: "1px" }}>
                  <span
                    data-testid="undo-snackbar-text"
                    style={{
                      "white-space": "nowrap",
                      overflow: "hidden",
                      "text-overflow": "ellipsis",
                      "font-size": "13px",
                      "line-height": 1.35,
                    }}
                  ><span style={{ "font-weight": 600 }}>{entry.terminalName}</span> closed</span>
                  <Show when={sub}>
                    <span
                      data-testid="undo-snackbar-sub"
                      style={{
                        "white-space": "nowrap",
                        overflow: "hidden",
                        "text-overflow": "ellipsis",
                        "font-size": "12px",
                        "line-height": 1.3,
                        color: "var(--md-on-surface-variant)",
                      }}
                    >{sub}</span>
                  </Show>
                </div>
                <Button
                  variant="text"
                  data-testid="undo-snackbar-action"
                  onClick={() => undoOne(entry.sessionId)}
                >Undo</Button>
                {/* Coral countdown bar, scales 1→0 over this card's own window.
                    The card mounts once (stable view identity in pendingClose.ts),
                    so the animation runs exactly once from when this close was
                    scheduled. */}
                <span
                  aria-hidden="true"
                  style={{
                    position: "absolute",
                    left: 0,
                    right: 0,
                    bottom: 0,
                    height: "2px",
                    background: "var(--md-primary)",
                    "transform-origin": "left center",
                    animation: `undo-snackbar-bar ${UNDO_WINDOW_MS}ms linear forwards`,
                  }}
                />
              </div>
            );
          }}
        </For>
      </div>
    </Show>
  );
}
