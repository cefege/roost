// Owns rename, removal confirmation, and live metrics for one registered
// machine. The worker projection supplies the coordinator-authoritative update
// operation; MachineUpdateDetails derives its state, actions, and report.
// MachinesPane owns list scope; shared M3 primitives preserve row interaction.

import { createSignal, onCleanup, Show } from "solid-js";
import type { Worker } from "@roost/shared/wire";
import { rootStore } from "../../store/root.ts";
import { workerOnline } from "../../store/sync.ts";
import { applyWorkerDeleteResponse } from "../../store/worker-removal.ts";
import { coordClient } from "../../connect.ts";
import { addToast } from "../../store/toastStore.ts";
import { Button, Chip, Icon, ListRow, MetricTile, StatusDot, TextField } from "./md/primitives.tsx";
import { formatBytes } from "../../lib/format.ts";
import { supportedWorkerPlatform } from "../../lib/nativePath.ts";
import { machinePlatformIcon } from "../../lib/machineActions.ts";
import {
  deriveMachineUpdatePresentation,
  MachineUpdateDetails,
} from "./MachineUpdateDetails.tsx";

function formatBps(bps: number): string {
  if (bps >= 1_073_741_824) return `${(bps / 1_073_741_824).toFixed(1)} GB/s`;
  if (bps >= 1_048_576) return `${(bps / 1_048_576).toFixed(1)} MB/s`;
  if (bps >= 1024) return `${(bps / 1024).toFixed(0)} KB/s`;
  return `${bps} B/s`;
}

function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  if (diff < 5_000) return "just now";
  if (diff < 60_000) return `${Math.floor(diff / 1000)}s ago`;
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  return `${Math.floor(diff / 3_600_000)}h ago`;
}


export function MachineCard(props: { worker: Worker }) {
  const w = () => props.worker;
  const isStale = () => !workerOnline(w());

  const [detailsOpen, setDetailsOpen] = createSignal(false);
  const [renaming, setRenaming] = createSignal(false);
  const [renameLabel, setRenameLabel] = createSignal("");
  const [renameBusy, setRenameBusy] = createSignal(false);
  const [renameErr, setRenameErr] = createSignal("");
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const [deleteBusy, setDeleteBusy] = createSignal(false);
  let confirmTimer: ReturnType<typeof setTimeout> | undefined;

  const updatePresentation = () =>
    deriveMachineUpdatePresentation({
      workerGitSha: w().git_sha,
      coordinatorGitSha: rootStore.coord_identity?.git_sha ?? null,
      online: workerOnline(w()),
      operation: w().update_operation,
    });

  function beginRename() {
    setRenameLabel(w().label);
    setRenameErr("");
    setRenaming(true);
  }

  function cancelRename() {
    setRenaming(false);
    setRenameErr("");
  }

  async function submitRename(event: Event) {
    event.preventDefault();
    const label = renameLabel().trim();
    if (!label) {
      setRenameErr("Label required");
      return;
    }
    setRenameBusy(true);
    setRenameErr("");
    try {
      await coordClient.workersRename({ fp: w().fp, label });
      setRenaming(false);
      addToast("Machine renamed");
    } catch (error) {
      setRenameErr(error instanceof Error ? error.message : String(error));
    } finally {
      setRenameBusy(false);
    }
  }

  function beginConfirmDelete() {
    setDetailsOpen(true);
    setConfirmDelete(true);
    clearTimeout(confirmTimer);
    confirmTimer = setTimeout(() => {
      confirmTimer = undefined;
      setConfirmDelete(false);
    }, 4000);
  }

  function cancelConfirmDelete() {
    clearTimeout(confirmTimer);
    confirmTimer = undefined;
    setConfirmDelete(false);
  }

  async function doDelete() {
    cancelConfirmDelete();
    setDeleteBusy(true);
    try {
      const response = await coordClient.workersDelete({ fp: w().fp });
      if (!applyWorkerDeleteResponse(w().fp, response)) {
        addToast("Machine removal was not confirmed", "err");
        setDeleteBusy(false);
        return;
      }
      addToast("Machine credential permanently removed. Saved terminals and workspaces remain available offline.");
    } catch (error) {
      addToast(error instanceof Error ? error.message : "Delete failed", "err");
      setDeleteBusy(false);
    }
  }

  const memRatio = () => {
    const metrics = w().host_metrics;
    return metrics && metrics.mem_total_bytes > 0
      ? metrics.mem_used_bytes / metrics.mem_total_bytes
      : undefined;
  };
  const diskRatio = () => {
    const metrics = w().host_metrics;
    return metrics && metrics.disk_total_bytes > 0
      ? metrics.disk_used_bytes / metrics.disk_total_bytes
      : undefined;
  };
  const support = () => [
    isStale() ? "Offline" : "Online",
    w().os,
  ].join(" · ");

  onCleanup(() => {
    clearTimeout(confirmTimer);
    confirmTimer = undefined;
  });

  return (
    <div
      class="machines-worker"
      data-testid={`machines-worker-row-${w().fp}`}
      style={{ opacity: deleteBusy() ? 0.4 : 1, transition: "opacity 0.15s" }}
    >
      <ListRow
        leading={<Icon name={machinePlatformIcon(supportedWorkerPlatform(w().os))} />}
        headline={w().label}
        support={support()}
        trailing={
          <>
            <StatusDot status={isStale() ? "offline" : "ok"} title={isStale() ? "Offline" : "Online"} />
            <Chip
              label={updatePresentation().label}
              selected={updatePresentation().state === "available"}
              testId={`machines-update-state-${w().fp}`}
              title={`Coordinator release ${rootStore.coord_identity?.git_sha?.slice(0, 8) ?? "unknown"}`}
            />
            <Button
              variant="destructive"
              size="icon-sm"
              icon="delete_outline"
              aria-label={`Remove ${w().label}`}
              data-testid={`machines-delete-quick-btn-${w().fp}`}
              onClick={beginConfirmDelete}
              disabled={deleteBusy()}
            />
            <Button
              variant="ghost"
              size="sm"
              icon={detailsOpen() ? "expand_less" : "expand_more"}
              aria-expanded={detailsOpen()}
              aria-controls={`machines-worker-details-${w().fp}`}
              onClick={() => setDetailsOpen((open) => !open)}
            >
              Details
            </Button>
          </>
        }
      />

      <Show when={detailsOpen()}>
        <div
          id={`machines-worker-details-${w().fp}`}
          class="machines-worker-details"
        >
          <Show
            when={!renaming()}
            fallback={
              <form
                data-testid={`machines-rename-form-${w().fp}`}
                onSubmit={(event) => void submitRename(event)}
                style={{ display: "flex", gap: "var(--md-space-2)", "align-items": "center" }}
              >
                <TextField
                  testId="machines-rename-input"
                  label="Label"
                  value={renameLabel()}
                  onInput={setRenameLabel}
                  style={{ flex: 1, "min-width": 0 }}
                />
                <Button variant="default" data-testid="machines-rename-save" disabled={renameBusy()}>
                  {renameBusy() ? "Saving…" : "Save"}
                </Button>
                <Button variant="ghost" data-testid="machines-rename-cancel" onClick={cancelRename}>
                  Cancel
                </Button>
              </form>
            }
          >
            <div class="machines-worker-details__identity">
              <span>{isStale() ? `Last seen ${relativeTime(w().last_seen_ms)}` : "Live connection"}</span>
              <span>{w().reachable_addr ?? "Address unknown"}</span>
              <span>Fingerprint {w().fp.slice(0, 12)}…</span>
              <Show when={w().git_sha}>
                {(gitSha) => <span>Worker version {gitSha().slice(0, 8)}</span>}
              </Show>
            </div>
          </Show>

          <MachineUpdateDetails
            worker={w()}
            presentation={updatePresentation()}
          />

          <Show when={renameErr()}>
            <div class="md-body-s" style={{ color: "var(--md-sys-color-error)" }}>{renameErr()}</div>
          </Show>

          <Show when={!isStale() && w().host_metrics}>
            {(metrics) => (
              <div class="md-metric-grid">
                <MetricTile icon="memory" label="CPU" value={`${metrics().cpu_pct.toFixed(0)}%`} ratio={metrics().cpu_pct / 100} />
                <MetricTile icon="memory_alt" label="Memory" value={memRatio() !== undefined ? `${Math.round(memRatio()! * 100)}%` : "—"} support={`${formatBytes(metrics().mem_used_bytes)} of ${formatBytes(metrics().mem_total_bytes)}`} ratio={memRatio()} />
                <MetricTile icon="hard_drive" label="Disk" value={diskRatio() !== undefined ? `${Math.round(diskRatio()! * 100)}%` : "—"} support={`${formatBytes(metrics().disk_used_bytes)} of ${formatBytes(metrics().disk_total_bytes)}`} ratio={diskRatio()} />
                <MetricTile icon="network_check" label="Network" value={formatBps(metrics().net_rx_bps + metrics().net_tx_bps)} support={`↓ ${formatBps(metrics().net_rx_bps)} · ↑ ${formatBps(metrics().net_tx_bps)}`} />
              </div>
            )}
          </Show>

          <div class="machines-worker-details__actions">
            <Button variant="ghost" icon="edit" data-testid={`machines-rename-btn-${w().fp}`} onClick={beginRename}>
              Rename
            </Button>
            <Show when={confirmDelete()}>
              <span class="md-body-s" data-testid={`machines-delete-explanation-${w().fp}`}>
                Removes this credential; saved terminals and workspaces stay offline.
              </span>
            </Show>
            <Show
              when={confirmDelete()}
              fallback={
                <Button variant="destructive" icon="delete_outline" data-testid={`machines-delete-btn-${w().fp}`} onClick={beginConfirmDelete} disabled={deleteBusy()}>
                  Remove
                </Button>
              }
            >
              <Button variant="destructive" data-testid={`machines-confirm-delete-btn-${w().fp}`} onClick={() => void doDelete()}>
                Confirm remove
              </Button>
              <Button variant="ghost" data-testid={`machines-cancel-delete-btn-${w().fp}`} onClick={cancelConfirmDelete}>
                Cancel
              </Button>
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
}
