// Shown by CellTerminal over a VIEWED pane that never received a screen frame —
// a dead "breadcrumb" session (open row, no live PTY). Replaces the silent
// blank pane with an explicit state + escape hatches. The wrapper is
// click-through (pointer-events:none) so only the card is interactive.

import {
  Show,
  createEffect,
  createSignal,
  onCleanup,
  untrack,
  type JSX,
} from "solid-js";
import { isPageVisible } from "../lib/pageVisible.ts";
import type { TerminalViewportOwnerStatus } from "../ws/sync-outbound.ts";

export type TerminalLoadingStage =
  | "identity"
  | "authorization"
  | "sync"
  | "sessions"
  | "spawn"
  | "measure"
  | "viewport"
  | "retry"
  | "frame"
  | "render";

export interface TerminalLoadingNoticeProps {
  stage: TerminalLoadingStage;
  title: string;
  detail: string;
  actions?: JSX.Element;
}

export function terminalViewportLoadingNotice(
  pending: boolean,
  status: TerminalViewportOwnerStatus | null,
): TerminalLoadingNoticeProps {
  if (pending) {
    return {
      stage: "spawn",
      title: "Starting terminal process",
      detail: "Waiting for the coordinator to confirm the new PTY.",
    };
  }
  if (status === null) {
    return {
      stage: "measure",
      title: "Measuring terminal view",
      detail: "Waiting for the visible pane size before requesting a screen.",
    };
  }
  switch (status.status) {
    case "pending":
      return {
        stage: "viewport",
        title: "Requesting terminal viewport",
        detail: "Waiting for the worker to accept this pane's size.",
      };
    case "retrying":
      return {
        stage: "retry",
        title: "Retrying terminal viewport",
        detail: `${status.reason.slice(0, 200)} Next attempt in ${Math.max(1, Math.ceil(status.retryInMs / 1000))}s.`,
      };
    case "repairing":
      return {
        stage: "frame",
        title: "Waiting for terminal screen",
        detail: `Worker accepted ${status.effectiveCols}×${status.effectiveRows}; waiting for a full frame.`,
      };
    case "ready":
      return {
        stage: "render",
        title: "Rendering terminal screen",
        detail: "The frame arrived; waiting for browser layout and paint.",
      };
    case "rejected":
    case "superseded":
      return {
        stage: "retry",
        title: "Re-establishing terminal viewport",
        detail: `${status.reason.slice(0, 200)} Re-establishing the viewport.`,
      };
  }
  const unreachable: never = status;
  return unreachable;
}

export function TerminalLoadingNotice(props: TerminalLoadingNoticeProps) {
  const [elapsedSeconds, setElapsedSeconds] = createSignal(0);
  const [announcement, setAnnouncement] = createSignal({
    title: props.title,
    detail: props.detail,
  });
  let announcedStage: TerminalLoadingStage | undefined;

  createEffect(() => {
    const stage = props.stage;
    const stageStartedAt = performance.now();
    setElapsedSeconds(0);
    if (stage !== announcedStage) {
      announcedStage = stage;
      setAnnouncement({
        title: untrack(() => props.title),
        detail: untrack(() => props.detail),
      });
    }
    const timer = setInterval(() => {
      if (!isPageVisible()) return;
      setElapsedSeconds(Math.floor((performance.now() - stageStartedAt) / 1000));
    }, 1_000);
    onCleanup(() => clearInterval(timer));
  });

  return (
    <div
      data-testid="terminal-loading-status"
      data-stage={props.stage}
      data-elapsed-seconds={elapsedSeconds()}
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
        role="status"
        aria-live="polite"
        aria-atomic="true"
        style={{
          position: "absolute",
          width: "1px",
          height: "1px",
          padding: "0",
          margin: "-1px",
          overflow: "hidden",
          clip: "rect(0, 0, 0, 0)",
          "white-space": "nowrap",
          border: "0",
        }}
      >
        {announcement().title}. {announcement().detail}
      </div>
      <div
        style={{
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
        <div
          data-testid="terminal-loading-title"
          style={{ "font-size": "15px", "font-weight": "600" }}
        >
          {props.title}
        </div>
        <div
          data-testid="terminal-loading-detail"
          style={{ "font-size": "13px", color: "var(--text-lo)", "line-height": "1.4" }}
        >
          {props.detail}
        </div>
        <div
          aria-hidden="true"
          data-testid="terminal-loading-elapsed"
          style={{ "font-size": "12px", color: "var(--text-lo)" }}
        >
          This step has taken {elapsedSeconds()}s
        </div>
        <Show when={props.actions}>
          <div
            style={{
              display: "flex",
              gap: "8px",
              "justify-content": "center",
              "margin-top": "6px",
              "pointer-events": "auto",
            }}
          >
            {props.actions}
          </div>
        </Show>
      </div>
    </div>
  );
}

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
      {/* Discrete state change (pane went dead / came back), not a stream — safe
          to announce. The cell grid itself must NEVER get a live region: a
          streaming pane would flood the screen reader row by row. */}
      <div
        aria-live="polite"
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
