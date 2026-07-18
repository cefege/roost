// QueueTaskDialog: modal wrapper around TaskEditor for "queue from here".
// Mount once in the app shell; toggle via queueTaskDialogStore.open/close.
// Callers: app root shell (mounted alongside other persistent modals).
// Depends on: queueTaskDialogStore (module-level signal), TaskEditor.

import type { Component } from "solid-js";
import { createSignal, Show } from "solid-js";
import { TaskEditor } from "./TaskEditor.tsx";
import { Dialog } from "./Settings/md/Dialog.tsx";

// ─── module-level dialog store (no external dep needed) ─────────────────────

const [open, setOpen] = createSignal(false);
const [prefillCwd, setPrefillCwd] = createSignal<string | undefined>(undefined);
const [prefillBody, setPrefillBody] = createSignal<string | undefined>(undefined);

export const queueTaskDialogStore = {
  isOpen: open,
  open(opts?: { cwd?: string; body?: string }): void {
    setPrefillCwd(opts?.cwd);
    setPrefillBody(opts?.body);
    setOpen(true);
  },
  close(): void {
    setOpen(false);
  },
} as const;

// ─── component ─────────────────────────────────────────────────────────────

export const QueueTaskDialog: Component = () => {
  const handleClose = () => {
    queueTaskDialogStore.close();
  };

  return (
    <Dialog open={open()} onClose={handleClose} headline="Queue a task">
      {/* Show gate (perf sweep C1.4): the always-mounted CLOSED dialog must
          not keep TaskEditor's 5 MWC fields mounted — workerOptions() would
          recompute on every worker write from boot. Mounted only while open. */}
      <Show when={open()}>
        <TaskEditor
          defaultBody={prefillBody()}
          defaultCwd={prefillCwd()}
          onEnqueued={handleClose}
          onCancel={handleClose}
          showCancel={true}
        />
      </Show>
    </Dialog>
  );
};
