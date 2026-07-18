// Module-level transfer console state — mirrors lib/deployConsole.ts.
// Mounted ONCE at App-shell level via TransferConsoleHost so it
// survives sidebar rekeying when worker presence updates fire.

import { createSignal } from "solid-js";

export interface TransferConsoleJob {
  jobId: string;
  srcLabel: string;
  dstLabel: string;
  srcPath: string;
  dstPath: string;
}

const [_active, _setActive] = createSignal<TransferConsoleJob | null>(null);

export const activeTransfer = _active;

export function openTransferConsole(job: TransferConsoleJob): void {
  _setActive(job);
}

export function closeTransferConsole(): void {
  _setActive(null);
}
