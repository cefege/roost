// Gmail-style undo snackbars for soft-closed terminals. ONE dark card per
// pending close. Two lines: the closed terminal's name ("<name> closed") over a
// dimmer "<folder> · <server>" line, "Undo" on the right, and a thin coral bar
// draining over that card's own 5s window. Each card owns its timer
// independently — see lib/pendingClose.ts.
// Rendered as notification-dock children: NotificationDock.tsx owns placement,
// width and pointer-events for every transient surface, and NotificationDock.css
// owns the undo-snackbar keyframes used here.

import { For, Show } from "solid-js";
import { pendingCloses, undoOne, UNDO_WINDOW_MS } from "../../lib/pendingClose.ts";
import { Button, Surface } from "../Settings/md/primitives.tsx";

export function UndoCloseBanner() {
  return (
    <For each={pendingCloses()}>
      {(entry) => {
        // Snapshot (view fields set once at close time); a plain const is
        // fine — no post-close reactivity, matching the old label render.
        const sub = [entry.folder, entry.server].filter(Boolean).join(" · ");
        return (
          <div
            data-testid="undo-close-banner"
            data-session-id={entry.sessionId}
          >
            <Surface
              level={3}
              elevation={4}
              radius="md"
              style={{
                position: "relative",
                display: "flex",
                "align-items": "center",
                gap: "var(--md-space-3)",
                color: "var(--md-sys-color-on-surface)",
                padding: "var(--md-space-3) var(--md-space-2) var(--md-space-3) var(--md-space-4)",
                overflow: "hidden",
                animation: "undo-snackbar-in var(--md-sys-motion-duration-short3) var(--md-sys-motion-easing-standard-decelerate)",
              }}
            >
              <div style={{ flex: 1, "min-width": 0, display: "flex", "flex-direction": "column", gap: "var(--md-space-1)" }}>
                <span
                  data-testid="undo-snackbar-text"
                  class="md-body-m"
                  style={{
                    "white-space": "nowrap",
                    overflow: "hidden",
                    "text-overflow": "ellipsis",
                  }}
                >
                  <span class="md-label-l">{entry.terminalName}</span> closed
                </span>
                <Show when={sub}>
                  <span
                    data-testid="undo-snackbar-sub"
                    class="md-body-s"
                    style={{
                      "white-space": "nowrap",
                      overflow: "hidden",
                      "text-overflow": "ellipsis",
                      color: "var(--md-sys-color-on-surface-variant)",
                    }}
                  >
                    {sub}
                  </span>
                </Show>
              </div>
              <Button
                variant="ghost"
                size="sm"
                data-testid="undo-snackbar-action"
                onClick={() => undoOne(entry.sessionId)}
              >
                Undo
              </Button>
              {/* Coral countdown bar, scales 1→0 over this card's own window.
                  The card mounts once (stable view identity in pendingClose.ts),
                  so the animation runs exactly once from when this close was
                  scheduled. */}
              <span
                aria-hidden="true"
                style={{
                  position: "absolute",
                  inset: "auto 0 0",
                  height: "var(--workbench-border-width)",
                  background: "var(--md-sys-color-primary)",
                  "transform-origin": "left center",
                  animation: `undo-snackbar-bar ${UNDO_WINDOW_MS}ms linear forwards`,
                }}
              />
            </Surface>
          </div>
        );
      }}
    </For>
  );
}
