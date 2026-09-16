// Machine picker for the /agent pane. Reads the fleet from the existing worker
// replica in rootStore (agent panes add no registry of their own) and navigates
// to /agent/<workerFp>, which is the surface's source of truth for placement.
//
// Rendered by AgentPane.tsx above the session list.

import { createMemo, For, Show } from "solid-js";
import { Chip, EmptyState } from "../Settings/md/primitives.tsx";
import { rootStore } from "../../store/root.ts";
import { workerOnline } from "../../store/sync.ts";

export interface AgentMachineOption {
  fp: string;
  label: string;
  online: boolean;
}

/** The fleet, newest heartbeat first, as the picker and AgentPane both need it. */
export function agentMachineOptions(): AgentMachineOption[] {
  return Object.values(rootStore.workers)
    .sort((left, right) => right.last_seen_ms - left.last_seen_ms)
    .map((worker) => ({
      fp: worker.fp,
      label: worker.label,
      online: workerOnline(worker),
    }));
}

export function AgentMachinePicker(props: {
  selectedFp: string | null;
  onSelect: (workerFp: string) => void;
}) {
  const options = createMemo(agentMachineOptions);

  return (
    <Show
      when={options().length > 0}
      fallback={
        <EmptyState
          icon="dns"
          title="No machines yet"
          supporting="Pair a machine with roost deploy, then open an agent session on it."
        />
      }
    >
      <div
        role="group"
        aria-label="Agent machine"
        data-testid="agent-machine-picker"
        style={{ display: "flex", gap: "var(--md-space-2)", "flex-wrap": "wrap" }}
      >
        <For each={options()}>
          {(option) => (
            <Chip
              label={option.online ? option.label : `${option.label} (offline)`}
              icon={option.fp === props.selectedFp ? "check" : "dns"}
              selected={option.fp === props.selectedFp}
              onClick={() => props.onSelect(option.fp)}
              testId={`agent-machine-${option.fp}`}
            />
          )}
        </For>
      </div>
    </Show>
  );
}
