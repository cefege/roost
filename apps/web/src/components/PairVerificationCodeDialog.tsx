// Trusted approvers see a one-time verification code only in this local dialog.
// PairApprovalProvider owns the persisted secret and the pairing lifecycle; this
// presentational component renders the code and lifecycle state, and routes
// every dismissal (close, Escape, backdrop, Cancel request) to one cancel callback.

import { Show } from "solid-js";
import type { JSX } from "solid-js";
import { PairingStatusNotice } from "./PairingStatusNotice.tsx";
import { Button, Dialog, Surface } from "./Settings/md/primitives.tsx";
import "./PairVerificationCodeDialog.css";

/** awaiting: the requester may still confirm; cancelling: the denial is in
 *  flight; reload_required: this client can no longer track the ceremony. */
export type PairCodeDialogState = "awaiting" | "cancelling" | "reload_required";

export function groupPairVerificationCode(code: string): string {
  return /^\d{6}$/.test(code) ? `${code.slice(0, 3)} ${code.slice(3)}` : code;
}

export function PairVerificationCodeDialog(props: {
  open: boolean;
  verificationCode: string;
  requesterLabel: string;
  state: PairCodeDialogState;
  onCancel: () => void;
  onReload: () => void;
}): JSX.Element {
  const reloadRequired = () => props.state === "reload_required";
  return (
    <Dialog
      open={props.open}
      onClose={props.onCancel}
      headline="Finish pairing"
      description={
        <span>
          Enter this code on {props.requesterLabel || "the requesting browser"}.
          <Show when={!reloadRequired()}>
            {" "}This window closes automatically when pairing completes.
          </Show>
        </span>
      }
      actions={
        <>
          <Show when={reloadRequired()}>
            <Button data-testid="pair-verification-code-reload" onClick={props.onReload}>
              Reload page
            </Button>
          </Show>
          <Button
            variant="outline"
            data-testid="pair-verification-code-cancel"
            disabled={props.state === "cancelling"}
            aria-busy={props.state === "cancelling"}
            onClick={props.onCancel}
          >
            {props.state === "cancelling" ? "Cancelling…" : "Cancel request"}
          </Button>
        </>
      }
      testId="pair-verification-code-dialog"
      showCloseButton
    >
      <Surface
        level={2}
        radius="sm"
        pad={4}
        border
        data-testid="pair-verification-code"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        <code class="md-display-s pair-code-dialog__code">
          {groupPairVerificationCode(props.verificationCode)}
        </code>
      </Surface>
      <Show when={reloadRequired()}>
        <div class="pair-code-dialog__notice">
          <PairingStatusNotice
            tone="error"
            message="This page can no longer follow the pairing. Reload it to continue."
          />
        </div>
      </Show>
    </Dialog>
  );
}
