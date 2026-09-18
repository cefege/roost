// The update affordance for one machine: the explicit Update action, the
// deferred-until-online explanation, and any failure text. MachineCard owns the
// row (identity, rename, removal, metrics) and passes the state the ONE fleet
// classifier decided, so this file never compares SHAs and never re-derives
// reachability. Callers: components/Settings/MachineCard.tsx.
// Depends on: machine-update-deploy.ts, md primitives, store/toastStore.ts.

import { createSignal, Show } from "solid-js";
import { WORKER_UPDATE_LABELS, type WorkerUpdateState } from "@roost/shared/fleet-update";
import { addToast } from "../../store/toastStore.ts";
import { Button } from "./md/primitives.tsx";
import { startMachineUpdateDeploy } from "./machine-update-deploy.ts";

export type MachineUpdateDetailsProps = {
  fp: string;
  state: WorkerUpdateState;
  /** The coordinator's own SHA — the release this machine is converging on. */
  expectedGitSha: string | null;
};

export function MachineUpdateDetails(props: MachineUpdateDetailsProps) {
  const [updateErr, setUpdateErr] = createSignal("");

  async function startUpdate() {
    const expectedGitSha = props.expectedGitSha;
    if (!expectedGitSha) {
      setUpdateErr("Coordinator release is unknown; reconnect and retry");
      return;
    }
    setUpdateErr("");
    const failure = await startMachineUpdateDeploy(props.fp, expectedGitSha);
    if (failure) {
      setUpdateErr(failure);
      return;
    }
    addToast("Machine updated");
  }

  return (
    <Show when={props.state !== "up-to-date" && props.state !== "unknown"}>
      <div class="machines-worker-details__actions">
        <Show when={props.state === "update-deferred"}>
          <span
            class="md-body-s"
            data-testid={`machines-update-deferred-${props.fp}`}
            style={{ color: "var(--md-sys-color-on-surface-variant)" }}
          >
            {WORKER_UPDATE_LABELS["update-deferred"]}. Roost updates this machine automatically when it
            comes back online.
          </span>
        </Show>
        <Show when={props.state === "update-available" || props.state === "updating"}>
          <Button
            variant="default"
            icon="system_update_alt"
            data-testid={`machines-update-btn-${props.fp}`}
            onClick={() => void startUpdate()}
            disabled={props.state === "updating"}
          >
            {props.state === "updating" ? WORKER_UPDATE_LABELS.updating : "Update"}
          </Button>
        </Show>
        <Show when={updateErr()}>
          <span
            class="md-body-s"
            data-testid={`machines-update-error-${props.fp}`}
            style={{ color: "var(--md-sys-color-error)" }}
          >
            {updateErr()}
          </span>
        </Show>
      </div>
    </Show>
  );
}
