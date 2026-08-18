// QueueTaskDialog: modal wrapper around TaskEditor for "queue from here".
// Mount once in the app shell; toggle via queueTaskDialogStore.open/close.
// Callers: app root shell (mounted alongside other persistent modals).
// Depends on: queueTaskDialogStore (store/queueTaskDialog.ts), TaskEditor.

import type { Component } from "solid-js";
import { Show } from "solid-js";
import { TaskEditor } from "./TaskEditor.tsx";
import { Dialog } from "./Settings/md/Dialog.tsx";
import { queueTaskDialogStore } from "../store/queueTaskDialog.ts";

// ─── component ─────────────────────────────────────────────────────────────

export const QueueTaskDialog: Component = () => {
  const handleClose = () => {
    queueTaskDialogStore.close();
  };

  return (
    <Dialog open={queueTaskDialogStore.isOpen()} onClose={handleClose} headline="Queue a task">
      {/* Show gate (perf sweep C1.4): the always-mounted CLOSED dialog must
          not keep TaskEditor's 5 MWC fields mounted — workerOptions() would
          recompute on every worker write from boot. Mounted only while open. */}
      <Show when={queueTaskDialogStore.isOpen()}>
        <TaskEditor
          defaultBody={queueTaskDialogStore.prefillBody()}
          defaultCwd={queueTaskDialogStore.prefillCwd()}
          onEnqueued={handleClose}
          onCancel={handleClose}
          showCancel={true}
        />
      </Show>
    </Dialog>
  );
};
