import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { CoordinatorMovePhase } from "@roost/shared/proto/coordinator_pb";
import { coordClient } from "../../connect.ts";
import { relocateBrowserToCoordinator } from "../../auth/coordinator-relocation.ts";
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

export function coordinatorMovePhaseLabel(phase: CoordinatorMovePhase): string {
  return PHASE_LABELS[phase] ?? "Preparing coordinator move…";
}

function isFailedPhase(phase: CoordinatorMovePhase): boolean {
  return phase === CoordinatorMovePhase.ROLLED_BACK || phase === CoordinatorMovePhase.FAILED;
}

function focusableChildren(panel: HTMLElement): HTMLElement[] {
  return [...panel.querySelectorAll<HTMLElement>(
    'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
  )].filter((element) => element.offsetParent !== null);
}

export function CoordinatorMoveDialog(props: { targetWorkerFp: string; onClose: () => void }) {
  const [eligible, setEligible] = createSignal(false);
  const [sourceUrl, setSourceUrl] = createSignal("");
  const [targetUrl, setTargetUrl] = createSignal("");
  const [blockers, setBlockers] = createSignal<string[]>([]);
  const [handoffId, setHandoffId] = createSignal("");
  const [phase, setPhase] = createSignal<CoordinatorMovePhase | null>(null);
  const [error, setError] = createSignal("");
  const [started, setStarted] = createSignal(false);
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
  const failed = (): boolean => phase() !== null && isFailedPhase(phase()!);
  const canClose = (): boolean => !started() || failed() || manualFallback();

  void coordClient.coordinatorMovePreflight({ targetWorkerFp: props.targetWorkerFp }).then((result) => {
    if (disposed) return;
    setEligible(result.eligible);
    setSourceUrl(result.sourceUrl);
    setTargetUrl(result.targetUrl);
    setBlockers(result.blockers.map((blocker) => blocker.message));
  }).catch((cause) => {
    if (!disposed) setError(cause instanceof Error ? cause.message : String(cause));
  });

  async function relocateAfterCommit(): Promise<void> {
    if (relocating) return;
    relocating = true;
    if (!await relocateBrowserToCoordinator(handoffId(), targetUrl())) {
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
      setPhase(status.phase);
      if (status.error) setError(status.error);
      if (status.phase === CoordinatorMovePhase.COMMITTED) {
        stopPolling();
        void relocateAfterCommit();
      } else if (isFailedPhase(status.phase)) {
        stopPolling();
        setError(status.error || "The coordinator move did not complete.");
      }
    } catch (cause) {
      if (!disposed) setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  async function beginMove(): Promise<void> {
    if (started()) return;
    setStarted(true);
    setError("");
    try {
      const result = await coordClient.coordinatorMoveStart({ targetWorkerFp: props.targetWorkerFp });
      setHandoffId(result.handoffId);
      void pollStatus();
      poll = setInterval(() => void pollStatus(), 500);
    } catch (cause) {
      setStarted(false);
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  onMount(() => {
    panel.focus();
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
        <Show when={blockers().length}><ul style={{ padding: "0 0 0 20px" }}>{blockers().map((message) => <li>{message}</li>)}</ul></Show>
        <Show when={phase() !== null}><p data-testid="coordinator-move-phase" aria-live="polite">{coordinatorMovePhaseLabel(phase()!)}</p></Show>
        <Show when={error()}><p data-testid="coordinator-move-error" role="alert">{error()}</p></Show>
        <Show when={manualFallback() && targetUrl()}><a data-testid="coordinator-move-target-link" href={targetUrl()}>Open new coordinator</a></Show>
        <div style={{ display: "flex", "justify-content": "flex-end", gap: "8px", "margin-top": "20px" }}>
          <Show when={!started()}>
            <Button variant="text" onClick={props.onClose}>Cancel</Button>
            <Button variant="filled" data-testid="coordinator-move-confirm" disabled={!eligible()} onClick={() => void beginMove()}>Move coordinator</Button>
          </Show>
          <Show when={started() && (failed() || manualFallback())}>
            <Button variant="filled" onClick={props.onClose}>Close</Button>
          </Show>
        </div>
      </div>
    </div>
  );
}
