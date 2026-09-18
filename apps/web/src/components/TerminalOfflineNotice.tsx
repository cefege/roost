// Shown by CellTerminal over a VIEWED pane that never received a screen frame —
// a dead "breadcrumb" session (open row, no live PTY). Replaces the silent
// blank pane with an explicit state + escape hatches. The wrapper is
// click-through (pointer-events:none) so only the card is interactive.

import { Show } from "solid-js";
import { Button, Surface } from "./Settings/md/primitives.tsx";

export interface TerminalOfflineNoticeProps {
  onRetry: () => void;
  onOpenSibling: () => void;
  hasSibling: boolean;
}

export function TerminalOfflineNotice(props: TerminalOfflineNoticeProps) {
  return (
    <div
      data-testid="terminal-offline-notice"
      style={{
        position: "absolute",
        inset: "0",
        display: "flex",
        "align-items": "center",
        "justify-content": "center",
        padding: "var(--md-space-6)",
        "pointer-events": "none",
        "z-index": "5",
      }}
    >
      {/* Discrete state change (pane went dead / came back), not a stream — safe
          to announce. The cell grid itself must NEVER get a live region: a
          streaming pane would flood the screen reader row by row. */}
      <Surface
        level={1}
        elevation={2}
        radius="md"
        pad={5}
        border
        aria-live="polite"
        style={{
          width: "min(100%, 45ch)",
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-3)",
          color: "var(--md-sys-color-on-surface)",
          "text-align": "center",
          "pointer-events": "auto",
        }}
      >
        <div class="md-title-s">
          This terminal isn't responding
        </div>
        <div class="md-body-m" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
          Its process may have stopped. The tab stays put so you keep your place.
        </div>
        <div style={{ display: "flex", gap: "var(--md-space-2)", "justify-content": "center", "margin-top": "var(--md-space-1)", "flex-wrap": "wrap" }}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            data-testid="terminal-offline-retry"
            onClick={() => props.onRetry()}
          >
            Retry
          </Button>
          <Show when={props.hasSibling}>
            <Button
              type="button"
              variant="default"
              size="sm"
              data-testid="terminal-offline-open-sibling"
              onClick={() => props.onOpenSibling()}
            >
              Open another terminal here
            </Button>
          </Show>
        </div>
      </Surface>
    </div>
  );
}
