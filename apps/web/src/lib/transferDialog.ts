// Module-level state for the "Transfer files…" dialog. Opened from
// SessionRow's right-click menu. Carries the source context (worker,
// cwd) so the dialog can pre-fill the form.

import { createSignal } from "solid-js";

export interface TransferDialogContext {
  srcFp: string;
  srcLabel: string;
  srcPath: string;
}

const [_active, _setActive] = createSignal<TransferDialogContext | null>(null);

export const activeTransferDialog = _active;

export function openTransferDialog(ctx: TransferDialogContext): void {
  _setActive(ctx);
}

export function closeTransferDialog(): void {
  _setActive(null);
}
