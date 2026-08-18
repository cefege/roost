// Mounts TransferConsoleModal exactly once at App-shell level so it
// survives sidebar rekeying from worker presence updates.

import { Show } from "solid-js";
import { TransferConsoleModal } from "./TransferConsoleModal.tsx";
import { activeTransfer, closeTransferConsole } from "../store/transferConsole.ts";

export function TransferConsoleHost() {
  return (
    <Show when={activeTransfer()}>
      {(job) => (
        <TransferConsoleModal
          jobId={job().jobId}
          srcLabel={job().srcLabel}
          dstLabel={job().dstLabel}
          srcPath={job().srcPath}
          dstPath={job().dstPath}
          onClose={closeTransferConsole}
        />
      )}
    </Show>
  );
}
