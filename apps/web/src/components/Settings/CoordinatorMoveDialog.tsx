import { createSignal, onCleanup, Show } from "solid-js";
import { coordClient } from "../../connect.ts";
import { Button, IconButton } from "./md/primitives.tsx";

export function CoordinatorMoveDialog(props: { targetWorkerFp: string; onClose: () => void }) {
  const [eligible, setEligible] = createSignal(false);
  const [targetUrl, setTargetUrl] = createSignal("");
  const [blockers, setBlockers] = createSignal<string[]>([]);
  const [handoffId, setHandoffId] = createSignal("");
  const [phase, setPhase] = createSignal("");
  const [error, setError] = createSignal("");
  let poll: ReturnType<typeof setInterval> | null = null;
  void coordClient.coordinatorMovePreflight({ targetWorkerFp: props.targetWorkerFp }).then((result) => {
    setEligible(result.eligible); setTargetUrl(result.targetUrl); setBlockers(result.blockers.map((blocker) => blocker.message));
  }).catch((cause) => setError(cause instanceof Error ? cause.message : String(cause)));
  onCleanup(() => { if (poll) clearInterval(poll); });

  async function beginMove() {
    try {
      const result = await coordClient.coordinatorMoveStart({ targetWorkerFp: props.targetWorkerFp });
      setHandoffId(result.handoffId);
      poll = setInterval(() => void coordClient.coordinatorMoveStatus({ handoffId: result.handoffId }).then((status) => {
        setPhase(status.phase.toString());
        if (status.error) setError(status.error);
        if (status.phase === 7 || status.phase === 9 || status.phase === 10) { if (poll) clearInterval(poll); }
      }).catch((cause) => setError(cause instanceof Error ? cause.message : String(cause))), 500);
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
  }

  return <div data-testid="coordinator-move-dialog" role="dialog" aria-modal="true" style={{ position: "fixed", inset: "0", display: "grid", "place-items": "center", background: "color-mix(in srgb, var(--md-scrim) 65%, transparent)", "z-index": "200" }}>
    <div style={{ width: "520px", "max-width": "calc(100vw - 32px)", padding: "24px", background: "var(--bg-elev-1)", "border-radius": "var(--md-shape-md)" }}>
      <div style={{ display: "flex", "justify-content": "space-between" }}><h2>Move coordinator</h2><Show when={!handoffId()}><IconButton icon="close" label="Close" onClick={props.onClose} /></Show></div>
      <p>Roost copies coordinator state, reconnects every worker, and moves open browser tabs. Terminal processes keep running in their workers.</p>
      <p><strong>Target:</strong> {targetUrl()}</p>
      <Show when={blockers().length}><ul>{blockers().map((message) => <li>{message}</li>)}</ul></Show>
      <Show when={phase()}><p data-testid="coordinator-move-phase">{phase()}</p></Show>
      <Show when={error()}><p data-testid="coordinator-move-error">{error()}</p></Show>
      <div style={{ display: "flex", "justify-content": "flex-end", gap: "8px" }}><Show when={!handoffId()}><Button variant="text" onClick={props.onClose}>Cancel</Button><Button variant="filled" data-testid="coordinator-move-confirm" disabled={!eligible()} onClick={() => void beginMove()}>Move coordinator</Button></Show><Show when={handoffId() && error()}><Button variant="filled" onClick={props.onClose}>Close</Button></Show></div>
    </div>
  </div>;
}
