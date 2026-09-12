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
} from "solid-js";
import type { JSX } from "solid-js";
import { Button, Surface } from "./Settings/md/primitives.tsx";
import { isPageVisible } from "../lib/pageVisible.ts";
import { terminalLoadingProgressView } from "../lib/terminalLoadingProgress.ts";
import type { TerminalViewHandleStatus } from "../store/terminal-stream.ts";

export type TerminalLoadingStage =
  | "identity"
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
  /** Chunked-baseline assembly progress; null/undefined renders the
   * indeterminate bar. Scrollback backfill and single-frame baselines never
   * set it, so they stay indeterminate. */
  progress?: { received: number; total: number } | null;
  /** Diagnosis line explaining where a stalled attach is stuck. */
  stuckReason?: string | null;
}

const VIEWPORT_CONFLICT_DETAIL =
  "Another terminal view changed this screen. Reconnecting automatically.";

// Presentation only: retry control is driven by structured viewport results.
function isStaleOrConflictingViewportReason(reason: string): boolean {
  const normalized = reason.toLowerCase();
  return normalized.includes("stale") || normalized.includes("conflicting");
}

export function terminalViewportLoadingNotice(
  pending: boolean,
  status: TerminalViewHandleStatus | null,
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
        detail: "Waiting for the coordinator to accept this terminal view.",
      };
    case "accepted":
      return status.baselineReady
        ? {
            stage: "render",
            title: "Rendering terminal screen",
            detail: "The full screen arrived; waiting for browser layout and paint.",
          }
        : {
            stage: "frame",
            title: "Waiting for terminal screen",
            detail: `View accepted at ${status.effectiveCols}×${status.effectiveRows}; waiting for its full baseline.`,
          };
    case "unavailable":
      return {
        stage: "retry",
        title: "Terminal stream unavailable",
        detail: `${status.reason.slice(0, 200)} The active view will retry on its next lease refresh.`,
      };
    case "rejected":
      return {
        stage: "retry",
        title: "Terminal view rejected",
        detail: isStaleOrConflictingViewportReason(status.reason)
          ? VIEWPORT_CONFLICT_DETAIL
          : status.reason.slice(0, 200),
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
  const progressView = () => terminalLoadingProgressView(props.progress);
  const trackStyle = {
    height: "var(--md-space-1)",
    width: "100%",
    overflow: "hidden",
    "border-radius": "var(--md-shape-full)",
    background: "var(--md-sys-color-surface-container-highest)",
  } as const;
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
      setElapsedSeconds(Math.floor((performance.now() - stageStartedAt) / 1_000));
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
        padding: "var(--md-space-6)",
        "pointer-events": "none",
        "z-index": "5",
      }}
    >
      <style>{`
        /* 25% x 400% exits the track fully on both sides; a shorter stub
           would end the loop inside the track and visibly snap back. */
        @keyframes terminal-loading-indeterminate {
          from { transform: translateX(-100%); }
          to   { transform: translateX(400%); }
        }
        @media (prefers-reduced-motion: reduce) {
          [data-testid="terminal-loading-progress-fill"] {
            animation: none;
            /* A parked indeterminate stub reads as a false "stuck 22%";
               hide it instead of implying determinate progress. */
            opacity: 0;
          }
        }
      `}</style>
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
      <Surface
        level={1}
        elevation={2}
        radius="md"
        pad={5}
        border
        style={{
          width: "min(100%, 45ch)",
          display: "flex",
          "flex-direction": "column",
          gap: "var(--md-space-3)",
          color: "var(--md-sys-color-on-surface)",
          "text-align": "center",
        }}
      >
        <div
          data-testid="terminal-loading-title"
          class="md-title-s"
        >
          {props.title}
        </div>
        <div
          data-testid="terminal-loading-detail"
          class="md-body-m"
          style={{ color: "var(--md-sys-color-on-surface-variant)" }}
        >
          {props.detail}
        </div>
        <div
          aria-hidden="true"
          data-testid="terminal-loading-elapsed"
          class="md-label-m"
          style={{ color: "var(--md-sys-color-on-surface-variant)" }}
        >
          This step has taken {elapsedSeconds()}s
        </div>
        <div
          data-testid="terminal-loading-progress"
          style={{
            display: "flex",
            "flex-direction": "column",
            gap: "var(--md-space-1)",
          }}
        >
          {/* Indeterminate animation is decorative; determinate track is semantic. */}
          <Show
            when={progressView()}
            fallback={(
              <div aria-hidden="true" style={trackStyle}>
                <div
                  data-testid="terminal-loading-progress-fill"
                  style={{
                    height: "100%",
                    width: "25%",
                    "border-radius": "var(--md-shape-full)",
                    background: "var(--md-sys-color-primary)",
                    animation: "terminal-loading-indeterminate 1.2s var(--md-sys-motion-easing-standard) infinite",
                  }}
                />
              </div>
            )}
          >
            {(view) => (
              <div
                role="progressbar"
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow={view().percent}
                style={trackStyle}
              >
                <div
                  data-testid="terminal-loading-progress-fill"
                  style={{
                    height: "100%",
                    width: `${view().percent}%`,
                    "border-radius": "var(--md-shape-full)",
                    background: "var(--md-sys-color-primary)",
                    transition: "width var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-standard)",
                  }}
                />
              </div>
            )}
          </Show>
          <Show when={progressView()}>
            {(view) => (
              <div
                data-testid="terminal-loading-progress-label"
                class="md-label-m"
                style={{ color: "var(--md-sys-color-on-surface-variant)" }}
              >
                {view().label}
              </div>
            )}
          </Show>
        </div>
        <Show when={props.stuckReason}>
          {(reason) => (
            <div
              data-testid="terminal-loading-stuck-reason"
              class="md-body-s"
              style={{ color: "var(--md-sys-color-on-surface-variant)" }}
            >
              {reason()}
            </div>
          )}
        </Show>
        <Show when={props.actions}>
          <div
            style={{
              display: "flex",
              gap: "var(--md-space-2)",
              "justify-content": "center",
              "margin-top": "var(--md-space-1)",
              "pointer-events": "auto",
            }}
          >
            {props.actions}
          </div>
        </Show>
      </Surface>
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
