// Trusted approvers see a one-time verification code only in this local dialog.
// PairApprovalProvider owns its persisted secret and supplies it after an
// acknowledged approval; this presentational component never authorizes a requester.

import type { JSX } from "solid-js";
import { Button, Dialog, Surface } from "./Settings/md/primitives.tsx";

export function groupPairVerificationCode(code: string): string {
  return /^\d{6}$/.test(code) ? `${code.slice(0, 3)} ${code.slice(3)}` : code;
}

export function PairVerificationCodeDialog(props: {
  open: boolean;
  verificationCode: string;
  requesterLabel: string;
  onClose: () => void;
}): JSX.Element {
  return (
    <Dialog
      open={props.open}
      onClose={props.onClose}
      headline="Verify browser pairing"
      description={
        <span>
          Type this code on {props.requesterLabel || "the requesting browser"}.
        </span>
      }
      actions={
        <Button data-testid="pair-verification-code-done" onClick={props.onClose}>
          Done
        </Button>
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
        <code
          class="md-display-s"
          style={{
            display: "block",
            margin: 0,
            color: "var(--md-sys-color-on-surface)",
            "text-align": "center",
            "user-select": "text",
          }}
        >
          {groupPairVerificationCode(props.verificationCode)}
        </code>
      </Surface>
      <p
        class="md-body-m"
        style={{ margin: "var(--md-space-3) 0 0", color: "var(--md-sys-color-on-surface-variant)" }}
      >
        Dismissing this dialog does not authorize the browser. Pairing completes
        only after the requester submits the matching code.
      </p>
    </Dialog>
  );
}
