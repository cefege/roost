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
    <Show when={queueTaskDialogStore.isOpen()}>
      <Dialog
        open
        onClose={handleClose}
        headline="Queue a task"
        showCloseButton={false}
      >
        <TaskEditor
          defaultBody={queueTaskDialogStore.prefillBody()}
          defaultCwd={queueTaskDialogStore.prefillCwd()}
          defaultWorkerFp={queueTaskDialogStore.prefillWorkerFp()}
          onEnqueued={handleClose}
          onCancel={handleClose}
          showCancel={true}
        />
      </Dialog>
    </Show>
  );
};
