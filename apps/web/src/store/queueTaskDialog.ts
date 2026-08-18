// Module-level state for the "Queue a task" dialog. Extracted from
// QueueTaskDialog.tsx so the store lives in the reactive-global-state tier
// beside uiStore.ts; QueueTaskDialog.tsx remains the mounted host and reads
// isOpen/prefillCwd/prefillBody from here.
// Callers: CommandPalette.data.ts ("Queue new task" action).

import { createSignal } from "solid-js";

const [open, setOpen] = createSignal(false);
const [prefillCwd, setPrefillCwd] = createSignal<string | undefined>(undefined);
const [prefillBody, setPrefillBody] = createSignal<string | undefined>(undefined);

export const queueTaskDialogStore = {
  isOpen: open,
  prefillCwd,
  prefillBody,
  open(opts?: { cwd?: string; body?: string }): void {
    setPrefillCwd(opts?.cwd);
    setPrefillBody(opts?.body);
    setOpen(true);
  },
  close(): void {
    setOpen(false);
  },
} as const;
