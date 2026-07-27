import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { CoordinatorMovePhase, type CoordinatorMoveBlocker } from "@roost/shared/proto/coordinator_pb";
import { coordClient } from "../../connect.ts";
import { relocateBrowserToCoordinator } from "../../auth/coordinator-relocation.ts";
import { isPageVisible } from "../../lib/pageVisible.ts";
import { isFailedMovePhase, moveDialogCanClose, MOVE_POLL_FAILURE_LIMIT } from "../../lib/coordinatorMove.ts";
import { Button, Icon, IconButton } from "./md/primitives.tsx";

const PHASE_LABELS: Partial<Record<CoordinatorMovePhase, string>> = {
  [CoordinatorMovePhase.PREPARING_TARGET]: "Preparing target…",
  [CoordinatorMovePhase.STAGING_WORKERS]: "Staging workers…",
  [CoordinatorMovePhase.DRAINING_SOURCE]: "Pausing coordinator writes…",
  [CoordinatorMovePhase.COPYING_STATE]: "Copying coordinator state…",
  [CoordinatorMovePhase.WAITING_FOR_WORKERS]: "Reconnecting workers…",
  [CoordinatorMovePhase.COMMITTING]: "Finalizing…",
  [CoordinatorMovePhase.COMMITTED]: "Finalizing…",
  [CoordinatorMovePhase.ROLLING_BACK]: "Restoring the current coordinator…",
  [CoordinatorMovePhase.ROLLED_BACK]: "Coordinator move rolled back",
  [CoordinatorMovePhase.FAILED]: "Coordinator move failed",
};

/** The one place a transparent handoff isn't transparent: the rescue path for a
 *  device that was asleep is source-mediated, so it is stranded once the old
 *  machine leaves the tailnet. */
const POST_COMMIT_GUIDANCE =
  "Devices that weren't open during the move must re-pair — open the new coordinator on that device, "
  + "or use Settings → Devices there. Keep the old machine online until you have.";

export function coordinatorMovePhaseLabel(phase: CoordinatorMovePhase): string {
  return PHASE_LABELS[phase] ?? "Preparing coordinator move…";
}

function focusableChildren(panel: HTMLElement): HTMLElement[] {
  return [...panel.querySelectorAll<HTMLElement>(
    'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
  )].filter((element) => element.offsetParent !== null);
}

export function CoordinatorMoveDialog(props: {
  targetWorkerFp: string;
  onClose: () => void;
  /** Set when reattaching to a move that was already running before this page
   *  loaded (MachinesPane reads coord_identity.handoff_id). Skips preflight. */
  resumeHandoffId?: string;
}) {
  const [eligible, setEligible] = createSignal(false);
  const [checking, setChecking] = createSignal(false);
  const [sourceUrl, setSourceUrl] = createSignal("");
  const [targetUrl, setTargetUrl] = createSignal("");
  const [blockers, setBlockers] = createSignal<CoordinatorMoveBlocker[]>([]);
  const [handoffId, setHandoffId] = createSignal(props.resumeHandoffId ?? "");
  const [phase, setPhase] = createSignal<CoordinatorMovePhase | null>(null);
  // Two channels: `error` is the orchestrator's own diagnostic, `transportError`
  // is "can't reach the coordinator right now". Folding them together let a
  // one-tick blip erase the real reason a move failed.
  const [error, setError] = createSignal("");
  const [transportError, setTransportError] = createSignal("");
  const [pollFailures, setPollFailures] = createSignal(0);
  const [started, setStarted] = createSignal(Boolean(props.resumeHandoffId));
  const [manualFallback, setManualFallback] = createSignal(false);
  let panel!: HTMLDivElement;
  let poll: ReturnType<typeof setInterval> | null = null;
  let disposed = false;
  let relocating = false;

  const stopPolling = (): void => {
    if (!poll) return;
    clearInterval(poll);
    poll = null;
  };
  const failed = (): boolean => phase() !== null && isFailedMovePhase(phase()!);
  const canClose = (): boolean => moveDialogCanClose({
    started: started(), phase: phase(), manualFallback: manualFallback(), pollFailures: pollFailures(),
  });
  const pollGaveUp = (): boolean => pollFailures() >= MOVE_POLL_FAILURE_LIMIT;
  const committed = (): boolean => phase() === CoordinatorMovePhase.COMMITTED;

  function runPreflight(): void {
    setChecking(true);
    coordClient.coordinatorMovePreflight({ targetWorkerFp: props.targetWorkerFp }).then((result) => {
      if (disposed) return;
      setChecking(false);
      setEligible(result.eligible);
      setSourceUrl(result.sourceUrl);
      setTargetUrl(result.targetUrl);
      setBlockers(result.blockers);
    }).catch((cause) => {
      if (disposed) return;
      setChecking(false);
      setError(cause instanceof Error ? cause.message : String(cause));
    });
  }

  function startPolling(): void {
    stopPolling();
    void pollStatus();
    poll = setInterval(() => { if (isPageVisible()) void pollStatus(); }, 500);
  }

  async function relocateAfterCommit(): Promise<void> {
    if (relocating) return;
    relocating = true;
    // "in-flight" means the sync frame's own call already won the race — a
    // success, not the "Could not redirect automatically" this used to show on
    // every clean move.
    if (await relocateBrowserToCoordinator(handoffId(), targetUrl()) === "failed") {
      setManualFallback(true);
      setError("Could not redirect automatically. Open the new coordinator manually.");
    }
  }

  async function pollStatus(): Promise<void> {
    const id = handoffId();
    if (!id) return;
    try {
      const status = await coordClient.coordinatorMoveStatus({ handoffId: id });
      if (disposed) return;
      setPollFailures(0);
      setTransportError("");
      setPhase(status.phase);
      // Clear a resolved error instead of leaving a stale one pinned.
      setError(status.error ?? "");
      if (!targetUrl()) setTargetUrl(status.targetUrl);
      if (!sourceUrl()) setSourceUrl(status.sourceUrl);
      if (status.phase === CoordinatorMovePhase.COMMITTED) {
        stopPolling();
        void relocateAfterCommit();
      } else if (isFailedMovePhase(status.phase)) {
        stopPolling();
        setError(status.error || "The coordinator move did not complete.");
      }
    } catch (cause) {
      if (disposed) return;
      const failures = pollFailures() + 1;
      setPollFailures(failures);
      setTransportError(cause instanceof Error ? cause.message : String(cause));
      if (failures >= MOVE_POLL_FAILURE_LIMIT) stopPolling();
    }
  }

  async function beginMove(): Promise<void> {
    if (started()) return;
    setStarted(true);
    // Seed the phase so the footer, phase line and close affordance don't all
    // vanish for the duration of a coordinatorMoveStart that re-runs preflight
    // server-side.
    setPhase(CoordinatorMovePhase.PREPARING_TARGET);
    setError("");
    try {
      const result = await coordClient.coordinatorMoveStart({ targetWorkerFp: props.targetWorkerFp });
      setHandoffId(result.handoffId);
      startPolling();
    } catch (cause) {
      setStarted(false);
      setPhase(null);
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  onMount(() => {
    panel.focus();
    if (props.resumeHandoffId) startPolling();
    else runPreflight();
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === "Escape" && canClose()) {
        event.preventDefault();
        props.onClose();
        return;
      }
      if (event.key !== "Tab") return;
      const targets = focusableChildren(panel);
      if (targets.length === 0) return;
      const first = targets[0]!;
      const last = targets[targets.length - 1]!;
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    onCleanup(() => document.removeEventListener("keydown", onKeyDown));
  });
  onCleanup(() => {
    disposed = true;
    stopPolling();
  });

  return (
    <div
      data-testid="coordinator-move-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="coordinator-move-title"
      aria-describedby="coordinator-move-description"
      onClick={(event) => { if (event.target === event.currentTarget && canClose()) props.onClose(); }}
      style={{ position: "fixed", inset: "0", display: "grid", "place-items": "center", padding: "16px", background: "color-mix(in srgb, var(--md-scrim) 65%, transparent)", "z-index": "200" }}
    >
      <div ref={panel} tabIndex={-1} style={{ width: "560px", "max-width": "100%", padding: "24px", background: "var(--bg-elev-1)", "border-radius": "var(--md-shape-md)", outline: "none" }}>
        <div style={{ display: "flex", "justify-content": "space-between", "align-items": "center", gap: "16px" }}>
          <h2 id="coordinator-move-title" style={{ margin: "0" }}>Move coordinator</h2>
          <Show when={canClose()}><IconButton icon="close" label="Close" onClick={props.onClose} /></Show>
        </div>
        <p id="coordinator-move-description">Roost copies coordinator state, reconnects every worker, and moves open browser tabs. Terminal processes keep running in their workers.</p>
        <div aria-label="Coordinator move route" style={{ display: "grid", "grid-template-columns": "minmax(0, 1fr) auto minmax(0, 1fr)", gap: "12px", "align-items": "center", padding: "14px", background: "var(--md-sys-color-surface-container)", "border-radius": "var(--md-shape-sm)" }}>
          <div style={{ "min-width": "0" }}><div class="md-label-s">Current coordinator</div><div style={{ overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{sourceUrl() || "Checking…"}</div></div>
          <Icon name="arrow_forward" />
          <div style={{ "min-width": "0" }}><div class="md-label-s">New coordinator</div><div style={{ overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{targetUrl() || "Checking…"}</div></div>
        </div>
        <Show when={blockers().length}>
          <h3 class="md-label-m" id="coordinator-move-blockers-title" style={{ margin: "16px 0 4px" }}>Why this move is blocked</h3>
          <ul id="coordinator-move-blockers" data-testid="coordinator-move-blockers" style={{ padding: "0 0 0 20px", margin: "0" }}>
            <For each={blockers()}>{(blocker) => <li data-blocker-code={blocker.code}>{blocker.message}</li>}</For>
          </ul>
        </Show>
        <Show when={phase() !== null}><p data-testid="coordinator-move-phase" aria-live="polite">{coordinatorMovePhaseLabel(phase()!)}</p></Show>
        <Show when={error()}><p data-testid="coordinator-move-error" role="alert">{error()}</p></Show>
        <Show when={transportError() && !pollGaveUp()}>
          <p data-testid="coordinator-move-transport-error" class="md-label-s">Can't reach the coordinator (retrying)…</p>
        </Show>
        <Show when={pollGaveUp()}>
          <p data-testid="coordinator-move-poll-gave-up" role="alert">Lost contact with the coordinator: {transportError()}</p>
        </Show>
        <Show when={committed() || manualFallback()}>
          <p data-testid="coordinator-move-post-commit">{POST_COMMIT_GUIDANCE}</p>
        </Show>
        {/* A source that dies during COMMITTING never flips manualFallback — the
            poll just stops answering — so without pollGaveUp the only offer is a
            Retry that re-polls a dead host. */}
        <Show when={(manualFallback() || pollGaveUp()) && targetUrl()}><a data-testid="coordinator-move-target-link" href={targetUrl()}>Open new coordinator</a></Show>
        <div style={{ display: "flex", "justify-content": "flex-end", gap: "8px", "margin-top": "20px" }}>
          <Show when={!started()}>
            <Button variant="text" onClick={props.onClose}>Cancel</Button>
            <Button variant="text" data-testid="coordinator-move-recheck" disabled={checking()} onClick={runPreflight}>Re-check</Button>
            <Button
              variant="filled"
              data-testid="coordinator-move-confirm"
              aria-describedby={blockers().length ? "coordinator-move-blockers" : undefined}
              disabled={checking() || !eligible()}
              onClick={() => void beginMove()}
            >
              {checking() ? "Checking…" : "Move coordinator"}
            </Button>
          </Show>
          <Show when={started() && pollGaveUp()}>
            <Button variant="tonal" data-testid="coordinator-move-retry" onClick={() => { setPollFailures(0); startPolling(); }}>Retry</Button>
          </Show>
          <Show when={started() && canClose()}>
            <Button variant="filled" onClick={props.onClose}>Close</Button>
          </Show>
        </div>
      </div>
    </div>
  );
}
