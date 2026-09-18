// The one card shown while a terminal is opening: a constant eyebrow, a single
// determinate 4→100% meter, one friendly step line, and the technical detail
// collapsed until a step is slow or stuck.
// Mounted by MainPane (bootstrap steps) and CellTerminal (pane steps); the two
// instances share one band table so their hand-off reads as one journey.
// Depends on terminalStartupProgress.ts, pageVisible.ts, and md primitives.

import {
  Show,
  batch,
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
  untrack,
} from "solid-js";
import type { JSX } from "solid-js";
import { Surface } from "./Settings/md/primitives.tsx";
import { isPageVisible } from "../lib/pageVisible.ts";
import {
  TERMINAL_STARTUP_STEPS,
  terminalStartupChunkDetail,
  terminalStartupCompletesJourney,
  terminalStartupPercent,
  type TerminalStartupStage,
} from "../lib/terminalStartupProgress.ts";
import type { TerminalViewHandleStatus } from "../store/terminal-stream.ts";
import "./TerminalStartupOverlay.css";

export interface TerminalStartupNotice {
  stage: TerminalStartupStage;
  /** Screen-reader announcement line; also the collapsed technical detail head. */
  title: string;
  detail: string;
  actions?: JSX.Element;
  /** Chunked-baseline assembly progress; subdivides the frame band. */
  progress?: { received: number; total: number } | null;
  /** Diagnosis line explaining where a stalled attach is stuck. */
  stuckReason?: string | null;
  /** Smoke diagnostics bind a loading transition to its terminal owner. */
  sessionId?: string;
}

const VIEWPORT_CONFLICT_DETAIL =
  "Another terminal view changed this screen. Reconnecting automatically.";

// A notice that vanishes for a frame between two steps is not a completion:
// declaring one would flash 100% in the middle of the journey.
const FINISH_GRACE_MS = 80;
const FINISH_HOLD_MS = 220;
const SAMPLE_INTERVAL_MS = 200;
const DETAILS_AFTER_SECONDS = 4;

// Presentation only: retry control is driven by structured viewport results.
function isStaleOrConflictingViewportReason(reason: string): boolean {
  const normalized = reason.toLowerCase();
  return normalized.includes("stale") || normalized.includes("conflicting");
}

export function terminalViewportLoadingNotice(
  pending: boolean,
  status: TerminalViewHandleStatus | null,
): TerminalStartupNotice {
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

export function TerminalStartupOverlay(props: { notice: TerminalStartupNotice | null }) {
  const [held, setHeld] = createSignal<TerminalStartupNotice | null>(null);
  const [finishing, setFinishing] = createSignal(false);
  const [sampledPercent, setSampledPercent] = createSignal(0);
  const [elapsedSeconds, setElapsedSeconds] = createSignal(0);
  const [announcement, setAnnouncement] = createSignal({ title: "", detail: "" });
  let announcedStage: TerminalStartupStage | undefined;
  // Never reset on a stage change: a status regression (accepted → pending on a
  // lease refresh, or a drop into retry) must hold the bar still, not rewind it.
  let displayedFloor = 0;
  let finishTimer: ReturnType<typeof setTimeout> | null = null;

  createEffect(() => {
    const next = props.notice;
    if (next) {
      clearTimeout(finishTimer ?? undefined);
      finishTimer = null;
      // A journey that already declared completion must not replay its band if
      // the notice comes back: hold the bar at the top of the track instead.
      if (untrack(finishing)) displayedFloor = 99;
      batch(() => {
        setFinishing(false);
        setHeld(next);
      });
      return;
    }
    const previous = untrack(held);
    if (!previous) return;
    // A bootstrap step hands off to the pane's own card, so finishing its meter
    // at 100% would make the journey restart at 46%. Leave silently instead.
    if (!terminalStartupCompletesJourney(previous.stage)) {
      setHeld(null);
      return;
    }
    if (finishTimer) return;
    finishTimer = setTimeout(() => {
      setFinishing(true);
      finishTimer = setTimeout(() => {
        finishTimer = null;
        // One batch, card first: clearing `finishing` while the card is still
        // mounted would restart the sampler and repaint the pre-completion
        // percent, so every hand-off would end on a visible 100 → 96 rewind.
        batch(() => {
          setHeld(null);
          setFinishing(false);
        });
      }, FINISH_HOLD_MS);
    }, FINISH_GRACE_MS);
  });
  onCleanup(() => clearTimeout(finishTimer ?? undefined));

  // Depends on the STAGE and the finishing flag only, so a progress-only update
  // (a fresh object from the caller's memo) never restarts the step clock.
  createEffect(on([() => held()?.stage ?? null, finishing], ([stage, done]) => {
    if (stage === null) {
      displayedFloor = 0;
      announcedStage = undefined;
      setSampledPercent(0);
      setElapsedSeconds(0);
      return;
    }
    if (stage !== announcedStage) {
      announcedStage = stage;
      const notice = untrack(held);
      setAnnouncement({ title: notice?.title ?? "", detail: notice?.detail ?? "" });
    }
    if (done) {
      setSampledPercent(100);
      return;
    }
    const startedAt = performance.now();
    setElapsedSeconds(0);
    const sampleMeter = (): void => {
      const stageElapsedMs = performance.now() - startedAt;
      const next = terminalStartupPercent({
        stage,
        stageElapsedMs,
        chunks: untrack(held)?.progress ?? null,
        floor: displayedFloor,
      });
      displayedFloor = next;
      setSampledPercent(next);
      setElapsedSeconds(Math.floor(stageElapsedMs / 1_000));
    };
    sampleMeter();
    const timer = setInterval(() => {
      if (!isPageVisible()) return;
      sampleMeter();
    }, SAMPLE_INTERVAL_MS);
    onCleanup(() => clearInterval(timer));
  }));

  const slow = createMemo(() =>
    elapsedSeconds() >= DETAILS_AFTER_SECONDS
    || !!held()?.stuckReason
    || held()?.stage === "retry");

  // Derived, not read straight off the sampler: the sampler is an effect, so on
  // the tick a stage first appears the card would otherwise paint the previous
  // stage's percent (or 0) for one frame and the meter would visibly rewind.
  const percent = createMemo(() => {
    const notice = held();
    if (!notice) return 0;
    if (finishing()) return 100;
    return Math.max(sampledPercent(), TERMINAL_STARTUP_STEPS[notice.stage].start);
  });

  return (
    <Show when={held()}>
      {(notice) => (
        <div
          class="terminal-startup"
          data-testid="terminal-loading-status"
          data-stage={notice().stage}
          data-session-id={notice().sessionId}
          data-elapsed-seconds={elapsedSeconds()}
          data-percent={Math.round(percent())}
          data-phase={finishing() ? "complete" : "loading"}
        >
          <div
            class="terminal-startup__announce"
            role="status"
            aria-live="polite"
            aria-atomic="true"
          >
            {announcement().title}. {announcement().detail}
          </div>
          <Surface level={1} elevation={2} radius="lg" pad={6} border class="terminal-startup__card">
            <div class="terminal-startup__eyebrow md-label-m" data-testid="terminal-loading-title">
              Opening terminal
            </div>
            <div class="terminal-startup__percent md-headline-s" data-testid="terminal-loading-percent">
              {Math.round(percent())}%
            </div>
            <div
              class="terminal-startup__track"
              data-testid="terminal-loading-progress"
              role="progressbar"
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow={Math.round(percent())}
            >
              <div
                class="terminal-startup__fill"
                data-testid="terminal-loading-progress-fill"
                style={{ width: `${percent()}%` }}
              />
            </div>
            <div class="terminal-startup__step md-body-s" data-testid="terminal-loading-detail">
              {TERMINAL_STARTUP_STEPS[notice().stage].label}
            </div>
            <div
              class="terminal-startup__details"
              data-testid="terminal-loading-details"
              hidden={!slow()}
            >
              <div class="md-body-s" data-testid="terminal-loading-technical">{notice().detail}</div>
              <Show when={terminalStartupChunkDetail(notice().progress)}>
                {(part) => (
                  <div class="md-label-m" data-testid="terminal-loading-progress-label">{part()}</div>
                )}
              </Show>
              <div class="md-label-m" data-testid="terminal-loading-elapsed" aria-hidden="true">
                This step has taken {elapsedSeconds()}s
              </div>
              <Show when={notice().stuckReason}>
                {(reason) => (
                  <div class="md-body-s" data-testid="terminal-loading-stuck-reason">{reason()}</div>
                )}
              </Show>
            </div>
            {/* Outside the collapsible details: the stuck-terminal escape hatch
                appears after 600ms and must not wait on the 4s reveal. */}
            <Show when={notice().actions}>
              <div class="terminal-startup__actions">{notice().actions}</div>
            </Show>
          </Surface>
        </div>
      )}
    </Show>
  );
}
