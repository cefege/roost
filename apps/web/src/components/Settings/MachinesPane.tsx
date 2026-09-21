// Owns the registered-machine list and the enrollment dialog.
// Each MachineCard keeps row-local rename, removal, and metric controls
// isolated so list refreshes do not merge their interaction state.
// SettingsRoot mounts this pane against the shared worker store.

import { createMemo, createSignal, For, Show } from "solid-js";
import { rootStore } from "../../store/root.ts";
import { refreshCoordAndWorkers } from "../../store/sync-bootstrap.ts";
import { MachineDeployDialog } from "../MachineDeployDialog.tsx";
import { Card, Button, EmptyState, List } from "./md/primitives.tsx";
import { MachineCard } from "./MachineCard.tsx";
import { noteMachineUpdateStatusRefreshed } from "./machine-update-deploy.ts";
export function MachinesPane() {
  const workers = createMemo(() =>
    Object.values(rootStore.workers).sort((a, b) => b.last_seen_ms - a.last_seen_ms),
  );
  const [showDeploy, setShowDeploy] = createSignal(false);

  const [refreshing, setRefreshing] = createSignal(false);
  const [refreshError, setRefreshError] = createSignal("");

  async function refreshStatus() {
    if (refreshing()) return;
    setRefreshing(true);
    setRefreshError("");
    try {
      if (await refreshCoordAndWorkers()) {
        noteMachineUpdateStatusRefreshed();
      } else {
        setRefreshError("Refresh failed. Last known machine status is still shown.");
      }
    } catch {
      setRefreshError("Refresh failed. Last known machine status is still shown.");
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <div data-testid="settings-machines-pane">
      <Card
        supporting={workers().length === 1 ? "1 machine" : `${workers().length} machines`}
        title="Machines"
        trailing={
          <div class="md-list-row__trailing">
            <Button
              variant="ghost"
              icon="refresh"
              data-testid="machines-refresh-status"
              disabled={refreshing()}
              onClick={() => void refreshStatus()}
            >
              {refreshing() ? "Refreshing…" : "Refresh status"}
            </Button>
            <Button
              variant="default"
              icon="add"
              data-testid="machines-add-btn"
              onClick={() => setShowDeploy(true)}
            >
              Add machine
            </Button>
          </div>
        }
      >
        <Show when={refreshError()}>
          <span class="md-body-s" data-testid="machines-refresh-status-error">
            {refreshError()}
          </span>
        </Show>
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
