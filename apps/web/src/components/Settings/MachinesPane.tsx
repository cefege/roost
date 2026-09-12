// Owns the registered-machine list and the enrollment dialog.
// Each MachineCard keeps row-local rename, removal, and metric controls
// isolated so list refreshes do not merge their interaction state.
// SettingsRoot mounts this pane against the shared worker store.

import { createMemo, createSignal, For, Show } from "solid-js";
import { rootStore } from "../../store/root.ts";
import { MachineDeployDialog } from "../MachineDeployDialog.tsx";
import { Card, Button, EmptyState } from "./md/primitives.tsx";
import { MachineCard } from "./MachineCard.tsx";
export function MachinesPane() {
  const workers = createMemo(() =>
    Object.values(rootStore.workers).sort((a, b) => b.last_seen_ms - a.last_seen_ms),
  );
  const [showDeploy, setShowDeploy] = createSignal(false);

  return (
    <div data-testid="settings-machines-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        supporting="Each machine running the Roost worker registers here automatically. A single machine can host the coordinator, the worker, and the browser — N=1 is first-class."
        title="Machines"
        trailing={
          <Show when={workers().length > 0}>
            <Button variant="default" icon="add"
            data-testid="machines-add-btn"
            onClick={() => setShowDeploy(true)}>
              Add machine
            </Button>
          </Show>
        }
      >
        <Show when={workers().length === 0}>
          <EmptyState
            icon="desktop_mac"
            title="No machines yet"
            supporting="Pair your first machine to start spawning sessions. The worker registers itself the first time it boots."
            action={
              <Button variant="default" icon="add" onClick={() => setShowDeploy(true)}>
                Add machine
              </Button>
            }
          />
        </Show>
      </Card>

      <For each={workers()}>
        {(worker) => <MachineCard worker={worker} />}
      </For>

      <Show when={showDeploy()}>
        <MachineDeployDialog onClose={() => setShowDeploy(false)} />
      </Show>
    </div>
  );
}
