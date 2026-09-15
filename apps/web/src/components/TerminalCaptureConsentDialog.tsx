// Token-based consent confirmation for opt-in terminal incident capture: owns the
// wording that terminal text and raw PTY output may contain secrets, the retention
// and lease bounds shown to the operator, and the confirm/cancel actions.
// TerminalContextMenu renders it; consent must be acknowledged before anything is sent.
// Depends on shared terminal-capture limits and the M3 Dialog/Button primitives.

import { type Component } from "solid-js";
import { TERMINAL_CAPTURE_LIMITS } from "@roost/shared/terminal-capture";
// Per-file primitive imports, not the barrel: the barrel drags Kobalte and
// router-linked rows into the terminal pane's module graph.
import { Button } from "./Settings/md/Button.tsx";
import { Dialog } from "./Settings/md/Dialog.tsx";
import type { TerminalCaptureConsentKind } from "./terminalCaptureMenuController.ts";

const RETENTION_HOURS = Math.round(TERMINAL_CAPTURE_LIMITS.retentionMs / 3_600_000);
const LEASE_MINUTES = Math.round(TERMINAL_CAPTURE_LIMITS.leaseMs / 60_000);

export const TerminalCaptureConsentDialog: Component<{
  kind: TerminalCaptureConsentKind | null;
  onConfirm: () => void;
  onCancel: () => void;
  /** The menu dismisses before consent is given, so its trigger is gone by the
   *  time the dialog closes; the owner restores pane focus instead. */
  onCloseAutoFocus?: (event: Event) => void;
}> = (props) => {
  const capturing = () => props.kind === "capture";
  return (
    <Dialog
      open={props.kind !== null}
      onClose={props.onCancel}
      onCloseAutoFocus={props.onCloseAutoFocus}
      testId="ctx-debug-consent"
      headline={capturing() ? "Capture terminal diagnostic" : "Start terminal debugging"}
      description={
        "Terminal text and raw PTY output are recorded. They may contain secrets — "
        + "passwords, tokens, file contents — and are readable by anyone who can sign in as you."
      }
      actions={
        <>
          <Button variant="outline" data-testid="ctx-debug-consent-cancel" onClick={props.onCancel}>
            Cancel
          </Button>
          <Button variant="default" data-testid="ctx-debug-consent-confirm" onClick={props.onConfirm}>
            {capturing() ? "Capture" : "Start recording"}
          </Button>
        </>
      }
    >
      <div style={{ display: "grid", gap: "var(--md-space-3)" }}>
        <p class="md-body-m" style={{ margin: "0" }}>
          The bundle is written on the machine that owns this terminal, downloaded only
          through your authenticated session, and deleted after at most {RETENTION_HOURS} hours.
        </p>
        <p class="md-body-m" style={{ margin: "0" }}>
          Confirming grants a {LEASE_MINUTES}-minute debugging lease for this terminal only.
          No other session is recorded, and an expired lease must be started again.
        </p>
        <p class="md-body-m" style={{ margin: "0" }} data-testid="ctx-debug-consent-prehistory">
          {capturing()
            ? "This terminal is not recording, so the capture has no prehistory: it contains the current browser state plus whatever the worker already retained, with every missing range reported in the bundle."
            : "Recording starts now; earlier output is not part of the lease."}
        </p>
      </div>
    </Dialog>
  );
};
