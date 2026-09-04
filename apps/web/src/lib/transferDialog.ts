// Open state for the cross-worker transfer beta availability dialog.
// Session row context menus open it; the App-level host renders it.
// Managed logout closes it with the rest of account-sensitive UI state.

import { createSignal } from "solid-js";

const [transferDialogOpen, setTransferDialogOpen] = createSignal(false);

export { transferDialogOpen };

export function openTransferDialog(): void {
  setTransferDialogOpen(true);
}

export function closeTransferDialog(): void {
  setTransferDialogOpen(false);
}
