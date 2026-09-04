// Owns the registered-machine list, enrollment dialog, and move-resume banner.
// Each MachineCard keeps row-local rename, removal, metric, and move controls
// isolated so list refreshes do not merge their interaction state.
// SettingsRoot mounts this pane against the shared worker and coordinator stores.

import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { rootStore } from "../../store/root.ts";
import { coordClient } from "../../connect.ts";
import { MachineDeployDialog } from "../MachineDeployDialog.tsx";
import {
  CoordinatorMoveDialog,
  coordinatorMovePhaseLabel,
} from "./CoordinatorMoveDialog.tsx";
import { Card, Button, EmptyState } from "./md/primitives.tsx";
import { coordinatorMoveControlsVisible } from "../../lib/coordinatorMove.ts";
import { isPageVisible } from "../../lib/pageVisible.ts";
import { CoordinatorMovePhase } from "@roost/shared/proto/coordinator_pb";
import { MachineCard } from "./MachineCard.tsx";
export function MachinesPane() {
  const workers = createMemo(() =>
    Object.values(rootStore.workers).sort((a, b) => b.last_seen_ms - a.last_seen_ms),
  );
  const [showDeploy, setShowDeploy] = createSignal(false);
  const [resumeDialog, setResumeDialog] = createSignal(false);
  // A reload during a move used to lose it entirely: no progress, no phase, and
  // writes failing with "coordinator move in progress" for no visible reason.
  // coord_identity.handoff_id is already populated for any role/phase.
  const runningMove = createMemo(() => {
    const identity = rootStore.coord_identity;
    if (!coordinatorMoveControlsVisible(identity)) return null;
    return identity?.handoff_id && !identity.relocated_to_url ? identity.handoff_id : null;
  });
  const [movePhase, setMovePhase] = createSignal<CoordinatorMovePhase | null>(null);
  // The TARGET's handoff file stays role=TARGET/phase=COMMITTED forever and
  // never carries relocated_to_url, so `handoff_id && !relocated_to_url` alone
  // would pin this banner on the new coordinator permanently. Only a polled,
  // non-terminal phase means a move is actually running; a not-yet-known phase
  // shows nothing.
  const moveInFlight = (): string | null => {
    const id = runningMove();
    const current = movePhase();
    if (!id || current === null) return null;
    if (current === CoordinatorMovePhase.COMMITTED
      || current === CoordinatorMovePhase.ROLLED_BACK
      || current === CoordinatorMovePhase.FAILED) return null;
    return id;
  };

  createEffect(() => {
    const identity = rootStore.coord_identity;
    const id = runningMove();
    if (!coordinatorMoveControlsVisible(identity) || !id || identity?.public_listener === true) {
      setMovePhase(null);
      return;
    }
    let cancelled = false;
    const tick = (): void => {
      void coordClient.coordinatorMoveStatus({ handoffId: id })
        .then((status) => { if (!cancelled) setMovePhase(status.phase); })
        .catch(() => { /* banner just keeps its last known phase */ });
    };
    tick();
    const timer = setInterval(() => { if (isPageVisible()) tick(); }, 2_000);
    onCleanup(() => { cancelled = true; clearInterval(timer); });
  });

  return (
    <div data-testid="settings-machines-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Show when={coordinatorMoveControlsVisible(rootStore.coord_identity) && rootStore.coord_identity?.public_listener === true}>
        <Card
          variant="elevated"
          title="Coordinator moves require private access"
          supporting="Coordinator moves are unavailable from the public web address. Open Roost through its private Tailscale address to move it."
        >
          <span />
        </Card>
      </Show>
      <Show when={coordinatorMoveControlsVisible(rootStore.coord_identity) && moveInFlight()}>
        <Card
          variant="elevated"
          title="Coordinator move in progress"
          trailing={
            <Button variant="tonal" data-testid="machines-resume-move-btn" onClick={() => setResumeDialog(true)}>
              Show progress
            </Button>
          }
        >
          <p data-testid="machines-move-banner-phase" aria-live="polite" style={{ margin: "0" }}>
            {coordinatorMovePhaseLabel(movePhase()!)}
          </p>
        </Card>
      </Show>
      <Card
        supporting="Each machine running the Roost worker registers here automatically. A single machine can host the coordinator, the worker, and the browser — N=1 is first-class."
        title="Machines"
        trailing={
          <Button
            variant="filled"
            icon="add"
            data-testid="machines-add-btn"
            onClick={() => setShowDeploy(true)}
          >
            Add machine
          </Button>
        }
      >
        <Show when={workers().length === 0}>
          <EmptyState
            icon="desktop_mac"
            title="No machines yet"
            supporting="Pair your first machine to start spawning sessions. The worker registers itself the first time it boots."
            action={
              <Button variant="filled" icon="add" onClick={() => setShowDeploy(true)}>
                Add machine
              </Button>
            }
          />
        </Show>
      </Card>

      <For each={workers()}>
        {(worker) => <MachineCard worker={worker} />}
      </For>

      {/* This is worker enrollment, not coordinator-originated deployment; it
          remains available for managed accounts. */}
      <Show when={showDeploy()}>
        <MachineDeployDialog onClose={() => setShowDeploy(false)} />
      </Show>
      <Show when={coordinatorMoveControlsVisible(rootStore.coord_identity) && resumeDialog() && moveInFlight() && rootStore.coord_identity?.public_listener !== true}>
        <CoordinatorMoveDialog
          targetWorkerFp=""
          resumeHandoffId={moveInFlight()!}
          onClose={() => setResumeDialog(false)}
        />
      </Show>
    </div>
  );
}
