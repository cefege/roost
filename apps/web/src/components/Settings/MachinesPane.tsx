// Owns the registered-machine list and the enrollment dialog.
// Each MachineCard keeps row-local rename, removal, and metric controls
// isolated so list refreshes do not merge their interaction state.
// SettingsRoot mounts this pane against the shared worker store.

import { createMemo, createSignal, For, Show } from "solid-js";
import { rootStore } from "../../store/root.ts";
import { MachineDeployDialog } from "../MachineDeployDialog.tsx";
import { Card, Button, EmptyState, List } from "./md/primitives.tsx";
import { MachineCard } from "./MachineCard.tsx";
export function MachinesPane() {
  const workers = createMemo(() =>
    Object.values(rootStore.workers).sort((a, b) => b.last_seen_ms - a.last_seen_ms),
  );
  const [showDeploy, setShowDeploy] = createSignal(false);

  return (
    <div data-testid="settings-machines-pane">
      <Card
        supporting={workers().length === 1 ? "1 machine" : `${workers().length} machines`}
        title="Machines"
        trailing={
          <Button
            variant="default"
            icon="add"
            data-testid="machines-add-btn"
            onClick={() => setShowDeploy(true)}
          >
            Add machine
          </Button>
        }
      >
        <Show
          when={workers().length > 0}
          fallback={
            <EmptyState
              icon="desktop_mac"
              title="No machines yet"
              supporting="Add a machine to start spawning sessions."
              action={
                <Button variant="default" icon="add" onClick={() => setShowDeploy(true)}>
                  Add machine
                </Button>
              }
            />
          }
        >
          <List contained>
            <For each={workers()}>
              {(worker) => <MachineCard worker={worker} />}
            </For>
          </List>
        </Show>
      </Card>

      <Show when={showDeploy()}>
        <MachineDeployDialog onClose={() => setShowDeploy(false)} />
      </Show>
    </div>
  );
}
