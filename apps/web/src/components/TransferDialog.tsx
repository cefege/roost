// Informational beta dialog for the deferred cross-worker transfer feature.
// The session context menu opens it, and App mounts this host once.
// It intentionally offers only working alternatives and never issues an RPC.

import { Button, Dialog } from "./Settings/md/primitives.tsx";
import {
  closeTransferDialog,
  transferDialogOpen,
} from "../lib/transferDialog.ts";

export function TransferDialogHost() {
  return (
    <Dialog
      open={transferDialogOpen()}
      onClose={closeTransferDialog}
      headline="Cross-worker transfer (beta)"
      actions={
        <Button
          variant="text"
          data-testid="transfer-dialog-close"
          onClick={closeTransferDialog}
        >
          Close
        </Button>
      }
    >
      <p data-testid="transfer-dialog-body">
        Cross-worker transfer is not available in v0.5.0. Use the terminal to run rsync or scp.
      </p>
    </Dialog>
  );
}

