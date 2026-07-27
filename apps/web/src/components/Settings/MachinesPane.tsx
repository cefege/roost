// MachinesPane — workers registered with this coord.
// Lists rootStore.workers with reachable_addr, last_seen, host_metrics.
// Per-row: inline rename (workersRename), delete with confirm gate
// (workersDelete hard-deletes row + authorized_key).
// "Add machine" opens MachineDeployDialog to mint a bootstrap token.
// M3: each worker rendered as an elevated Card with a metric-tile grid
// (CPU / Memory / Disk / Network); status pill + relative-time chip in
// the header; rename + delete sit in an action row with M3 buttons.
// Callers: SettingsRoot.tsx. Depends on: rootStore.workers, coordClient,
// MachineDeployDialog, toastStore, md/primitives.

import { createMemo, createSignal, For, Show } from "solid-js";
import type { Worker } from "@roost/shared/wire";
import { rootStore } from "../../store/root.ts";
import { workerOnline } from "../../store/sync.ts";
import { coordClient } from "../../connect.ts";
import { addToast } from "../../lib/toastStore.ts";
import { MachineDeployDialog } from "../MachineDeployDialog.tsx";
import { CoordinatorMoveDialog } from "./CoordinatorMoveDialog.tsx";
import { Card, Button, MetricTile, EmptyState, Icon, TextField } from "./md/primitives.tsx";
import { formatBytes } from "../../lib/format.ts";

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

function WorkerRow(props: { worker: Worker }) {
  const w = () => props.worker;
  // A2: stale = not routable (coord WS membership), not just heartbeat age.
  const isStale = () => !workerOnline(w());

  const [renaming, setRenaming] = createSignal(false);
  const [renameLabel, setRenameLabel] = createSignal("");
  const [renameBusy, setRenameBusy] = createSignal(false);
  const [renameErr, setRenameErr] = createSignal("");

  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const [deleteBusy, setDeleteBusy] = createSignal(false);
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;
  const [moveDialog, setMoveDialog] = createSignal(false);
  const isCoordinator = () => {
    const url = rootStore.coord_identity?.public_url;
    if (!url || !w().reachable_addr) return false;
    try { return new URL(url).hostname.toLowerCase() === w().reachable_addr!.toLowerCase(); }
    catch { return false; }
  };

  function beginRename() {
    setRenameLabel(w().label);
    setRenameErr("");
    setRenaming(true);
  }
  function cancelRename() {
    setRenaming(false);
    setRenameErr("");
  }
  async function submitRename(e: Event) {
    e.preventDefault();
    const label = renameLabel().trim();
    if (!label) { setRenameErr("Label required"); return; }
    setRenameBusy(true);
    setRenameErr("");
    try {
      await coordClient.workersRename({ fp: w().fp, label });
      setRenaming(false);
      addToast("Machine renamed");
    } catch (err) {
      setRenameErr(err instanceof Error ? err.message : String(err));
    } finally {
      setRenameBusy(false);
    }
  }

  function beginConfirmDelete() {
    setConfirmDelete(true);
    if (confirmTimer !== null) clearTimeout(confirmTimer);
    confirmTimer = setTimeout(() => {
      confirmTimer = null;
      setConfirmDelete(false);
    }, 4000);
  }
  function cancelConfirmDelete() {
    if (confirmTimer !== null) { clearTimeout(confirmTimer); confirmTimer = null; }
    setConfirmDelete(false);
  }
  async function doDelete() {
    cancelConfirmDelete();
    setDeleteBusy(true);
    try {
      await coordClient.workersDelete({ fp: w().fp });
      addToast("Machine removed");
    } catch (err) {
      addToast(err instanceof Error ? err.message : "Delete failed", "err");
      setDeleteBusy(false);
    }
  }

  const memRatio = () => {
    const m = w().host_metrics;
    return m && m.mem_total_bytes > 0 ? m.mem_used_bytes / m.mem_total_bytes : undefined;
  };
  const diskRatio = () => {
    const m = w().host_metrics;
    return m && m.disk_total_bytes > 0 ? m.disk_used_bytes / m.disk_total_bytes : undefined;
  };

  // Header trailing: status pill + relative last-seen + actions.
  const headerTrailing = (
    <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
      <span
        class="md-label-m"
        style={{
          display: "inline-flex",
          "align-items": "center",
          gap: "var(--md-space-2)",
          padding: "4px 10px",
          "border-radius": "var(--md-shape-full)",
          background: isStale() ? "var(--md-sys-color-surface-container-high)" : "var(--md-sys-color-secondary-container)",
          color: isStale() ? "var(--md-sys-color-on-surface-variant)" : "var(--md-sys-color-on-secondary-container)",
        }}
      >
        <span style={{
          width: "8px", height: "8px", "border-radius": "50%",
          background: isStale() ? "var(--md-sys-color-on-surface-variant)" : "var(--md-success)",
        }} />
        {isStale() ? "Stale" : "Online"}
      </span>
      <span class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
        {relativeTime(w().last_seen_ms)}
      </span>
    </div>
  );

  return (
    <div data-testid={`machines-worker-row-${w().fp}`} style={{ opacity: deleteBusy() ? 0.4 : 1, transition: "opacity 0.15s" }}>
      <Show when={moveDialog()}><CoordinatorMoveDialog targetWorkerFp={w().fp} onClose={() => setMoveDialog(false)} /></Show>
      <Card variant="elevated" trailing={headerTrailing}>
        <Show
          when={!renaming()}
          fallback={
            <form
              data-testid={`machines-rename-form-${w().fp}`}
              onSubmit={(e) => void submitRename(e)}
              style={{ display: "flex", gap: "var(--md-space-2)", "align-items": "center" }}
            >
              <TextField
                testId="machines-rename-input"
                label="Label"
                value={renameLabel()}
                onInput={(v) => setRenameLabel(v)}
                style={{ flex: 1, "min-width": 0 }}
              />
              <Button variant="filled" data-testid="machines-rename-save" disabled={renameBusy()}>
                {renameBusy() ? "Saving…" : "Save"}
              </Button>
              <Button variant="text" data-testid="machines-rename-cancel" onClick={cancelRename}>
                Cancel
              </Button>
            </form>
          }
        >
          <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-3)" }}>
            <Icon name="desktop_mac" size="lg" style={{ color: "var(--md-sys-color-primary)" }} />
            <div style={{ flex: 1, "min-width": 0 }}>
              <div class="md-title-m" style={{ color: "var(--md-sys-color-on-surface)" }}>{w().label}</div>
              <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
                {w().os}
                {" · "}
                {w().reachable_addr ?? "address unknown"}
                {" · fp "}
                <span style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}>{w().fp.slice(0, 12)}…</span>
                <Show when={w().git_sha}>
                  {" · sha "}
                  <span style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}>{w().git_sha!.slice(0, 8)}</span>
                </Show>
              </div>
              <Show when={w().keeper_stale}>
                {(stale) => (
                  <div class="md-body-s" style={{ color: "var(--md-sys-color-error)", "margin-top": "var(--md-space-1)" }}>
                    ⚠ keeper running stale code ({stale().slice(0, 8)}) — run <span style={{ "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace" }}>roost keeper-refresh</span>
                  </div>
                )}
              </Show>
              <Show when={w().git_sha && rootStore.coord_identity?.git_sha && w().git_sha !== rootStore.coord_identity!.git_sha}>
                <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)", "margin-top": "var(--md-space-1)" }}>
                  ⚠ worker sha drifts from coord ({rootStore.coord_identity!.git_sha.slice(0, 8)})
                </div>
              </Show>
            </div>
          </div>
        </Show>

        <Show when={renameErr()}>
          <div class="md-body-s" style={{ color: "var(--md-sys-color-error)" }}>{renameErr()}</div>
        </Show>

        {/* Metrics are only meaningful while the machine is live (routable).
            A dead/offline worker keeps its LAST host_metrics in the store
            forever — showing CPU/mem/disk for a Mac that's been off for hours
            is worse than nothing. Gate on liveness; show an offline note. */}
        <Show
          when={!isStale() && w().host_metrics}
          fallback={
            <Show when={isStale()}>
              <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
                Offline — no live metrics (last seen {relativeTime(w().last_seen_ms)})
              </div>
            </Show>
          }
        >
          {(metrics) => (
            <div class="md-metric-grid">
              <MetricTile
                icon="memory"
                label="CPU"
                value={`${metrics().cpu_pct.toFixed(0)}%`}
                ratio={metrics().cpu_pct / 100}
              />
              <MetricTile
                icon="memory_alt"
                label="Memory"
                value={memRatio() !== undefined ? `${Math.round(memRatio()! * 100)}%` : "—"}
                support={`${formatBytes(metrics().mem_used_bytes)} of ${formatBytes(metrics().mem_total_bytes)}`}
                ratio={memRatio()}
              />
              <MetricTile
                icon="hard_drive"
                label="Disk"
                value={diskRatio() !== undefined ? `${Math.round(diskRatio()! * 100)}%` : "—"}
                support={`${formatBytes(metrics().disk_used_bytes)} of ${formatBytes(metrics().disk_total_bytes)}`}
                ratio={diskRatio()}
              />
              <MetricTile
                icon="network_check"
                label="Network"
                value={formatBps(metrics().net_rx_bps + metrics().net_tx_bps)}
                support={`↓ ${formatBps(metrics().net_rx_bps)} · ↑ ${formatBps(metrics().net_tx_bps)}`}
              />
            </div>
          )}
        </Show>

        <Show when={!renaming()}>
          <div style={{ display: "flex", "justify-content": "flex-end", gap: "var(--md-space-2)" }}>
            <Button
              variant="text"
              icon="edit"
              data-testid={`machines-rename-btn-${w().fp}`}
              onClick={beginRename}
            >
              Rename
            </Button>
            <Show when={isCoordinator()}>
              <span data-testid={`machines-coordinator-pill-${w().fp}`} class="md-label-m">Coordinator</span>
            </Show>
            <Show when={!isCoordinator()}>
              <Button
                variant="tonal"
                data-testid={`machines-move-coordinator-btn-${w().fp}`}
                disabled={isStale() || !w().reachable_addr || w().git_sha !== rootStore.coord_identity?.git_sha}
                onClick={() => setMoveDialog(true)}
              >
                Move coordinator here
              </Button>
            </Show>
            <Show
              when={confirmDelete()}
              fallback={
                <Button
                  variant="text"
                  icon="delete_outline"
                  data-testid={`machines-delete-btn-${w().fp}`}
                  onClick={beginConfirmDelete}
                  disabled={deleteBusy()}
                  style={{ color: "var(--md-sys-color-error)" }}
                >
                  Remove
                </Button>
              }
            >
              <Button
                variant="filled"
                data-testid={`machines-confirm-delete-btn-${w().fp}`}
                onClick={() => void doDelete()}
                style={{ background: "var(--md-sys-color-error)", color: "var(--md-on-primary)" }}
              >
                Confirm remove
              </Button>
              <Button
                variant="text"
                data-testid={`machines-cancel-delete-btn-${w().fp}`}
                onClick={cancelConfirmDelete}
              >
                Cancel
              </Button>
            </Show>
          </div>
        </Show>
      </Card>
    </div>
  );
}

export function MachinesPane() {
  const workers = createMemo(() =>
    Object.values(rootStore.workers).sort((a, b) => b.last_seen_ms - a.last_seen_ms),
  );
  const [showDeploy, setShowDeploy] = createSignal(false);

  return (
    <div data-testid="settings-machines-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        supporting="Each Mac running the Roost worker registers here automatically. A single Mac can host the coordinator, the worker, and the browser — N=1 is first-class."
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
            supporting="Pair your first Mac to start spawning sessions. The worker registers itself the first time it boots."
            action={
              <Button variant="filled" icon="add" onClick={() => setShowDeploy(true)}>
                Add machine
              </Button>
            }
          />
        </Show>
      </Card>

      <For each={workers()}>
        {(worker) => <WorkerRow worker={worker} />}
      </For>

      <Show when={showDeploy()}>
        <MachineDeployDialog onClose={() => setShowDeploy(false)} />
      </Show>
    </div>
  );
}
