// Shown by CellTerminal over a VIEWED pane that never received a screen frame —
// a dead "breadcrumb" session (open row, no live PTY). Replaces the silent
// blank pane with an explicit state + escape hatches. The wrapper is
// click-through (pointer-events:none) so only the card is interactive.

import { Show } from "solid-js";

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
        padding: "24px",
        "pointer-events": "none",
        "z-index": "5",
      }}
    >
      <div
        style={{
          "pointer-events": "auto",
          "max-width": "360px",
          display: "flex",
          "flex-direction": "column",
          gap: "10px",
          padding: "20px 22px",
          "border-radius": "12px",
          background: "var(--surface-1)",
          border: "1px solid var(--border-strong)",
          color: "var(--text-hi)",
          "text-align": "center",
        }}
      >
        <div style={{ "font-size": "15px", "font-weight": "600" }}>
          This terminal isn't responding
        </div>
        <div style={{ "font-size": "13px", color: "var(--text-lo)", "line-height": "1.4" }}>
          Its process may have stopped. The tab stays put so you keep your place.
        </div>
        <div style={{ display: "flex", gap: "8px", "justify-content": "center", "margin-top": "6px" }}>
          <button
            type="button"
            data-testid="terminal-offline-retry"
            onClick={() => props.onRetry()}
            style={{
              "font-family": "inherit",
              "font-size": "13px",
              padding: "7px 14px",
              "border-radius": "8px",
              border: "1px solid var(--border-strong)",
              background: "var(--surface-2)",
              color: "var(--text-hi)",
              cursor: "pointer",
            }}
          >
            Retry
          </button>
          <Show when={props.hasSibling}>
            <button
              type="button"
              data-testid="terminal-offline-open-sibling"
              onClick={() => props.onOpenSibling()}
              style={{
                "font-family": "inherit",
                "font-size": "13px",
                padding: "7px 14px",
                "border-radius": "8px",
                border: "1px solid transparent",
                background: "var(--accent)",
                color: "var(--on-accent)",
                cursor: "pointer",
              }}
            >
              Open another terminal here
            </button>
          </Show>
        </div>
      </div>
    </div>
  );
}
