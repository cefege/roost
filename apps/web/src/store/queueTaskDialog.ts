// Dashboard-scoped state for the mounted “Queue a task” dialog. Palette
// actions may prefill the active folder and pin its worker; QueueTaskDialog
// passes those values into TaskEditor only while the dialog is open.
// Dashboard selection and logout boundaries clear every captured value.

import { createSignal } from "solid-js";

const [open, setOpen] = createSignal(false);
const [prefillCwd, setPrefillCwd] = createSignal<string | undefined>(undefined);
const [prefillBody, setPrefillBody] = createSignal<string | undefined>(undefined);
const [prefillWorkerFp, setPrefillWorkerFp] = createSignal<string | undefined>(undefined);

export const queueTaskDialogStore = {
  isOpen: open,
  prefillCwd,
  prefillBody,
  prefillWorkerFp,
  open(opts?: { cwd?: string; body?: string; workerFp?: string }): void {
    setPrefillCwd(opts?.cwd);
    setPrefillBody(opts?.body);
    setPrefillWorkerFp(opts?.workerFp);
    setOpen(true);
  },
  close(): void {
    setOpen(false);
  },
} as const;

export function clearQueueTaskDialogForLogout(): void {
  setOpen(false);
  setPrefillCwd(undefined);
  setPrefillBody(undefined);
  setPrefillWorkerFp(undefined);
}
