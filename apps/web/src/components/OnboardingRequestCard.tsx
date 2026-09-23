// Requester-facing pairing card: request correlation, token-bound poll status,
// and six-digit verification entry. Onboarding owns the requester token and
// confirmation RPC; this component keeps every state in one live status surface.

import { Show } from "solid-js";
import type { JSX } from "solid-js";
import { Card, StatusDot, Surface, Button, TextField } from "./Settings/md/primitives.tsx";

export type PairPollStatus =
  | "idle"
  | "pending"
  | "verification_required"
  | "denied"
  | "expired"
  | "verification_failed"
  | "completed"
  | "error";

export function OnboardingRequestCard(props: {
  ephemeralId: string | null;
  pollStatus: PairPollStatus;
  verificationCode: string;
  confirmationError: string | null;
  busy: boolean;
  onStart: () => void;
  onVerificationCodeInput: (value: string) => void;
  onConfirm: () => void;
}): JSX.Element {
  const statusIndicator = () => {
    if (props.confirmationError !== null) return "error";
    switch (props.pollStatus) {
      case "completed":
        return "ok";
      case "denied":
      case "expired":
      case "verification_failed":
        return "warn";
      case "error":
        return "error";
      default:
        return "info";
    }
  };

  return (
    <Card
      data-testid="onboarding-pair-step"
      title="I don't have a code"
      supporting="Request approval from a browser that's already paired."
      variant="outlined"
    >
      <Show
        when={props.ephemeralId !== null}
        fallback={
          <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
            <p
              class="md-body-m"
              style={{ margin: 0, color: "var(--md-sys-color-on-surface-variant)" }}
            >
              The paired browser will review this request, then show a six-digit
              verification code for you to enter here.
            </p>
            <div>
              <Button
                variant="secondary"
                data-testid="onboarding-pair-start-btn"
                onClick={props.onStart}
                disabled={props.busy}
              >
                {props.busy ? "..." : props.pollStatus === "error" ? "Request again" : "Request approval"}
              </Button>
            </div>
          </div>
        }
      >
        <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
          <p
            class="md-body-m"
            style={{ margin: 0, color: "var(--md-sys-color-on-surface-variant)" }}
          >
            On the already-paired browser, open <strong>Settings → Devices</strong>,
            review this request, and select <strong>Approve</strong>.
          </p>
          <Surface
            level={2}
            radius="sm"
            pad={3}
            border
            data-testid="onboarding-pair-ephemeral-id"
          >
            <span class="md-label-m">Request ID</span>
            <code class="md-title-m" style={{ display: "block", "overflow-wrap": "anywhere" }}>
              {props.ephemeralId}
            </code>
          </Surface>
          <Show when={props.pollStatus === "verification_required"}>
            <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}>
              <TextField
                value={props.verificationCode}
                onInput={props.onVerificationCodeInput}
                label="Verification code"
                placeholder="123 456"
                inputMode="numeric"
                autocomplete="one-time-code"
                maxLength={7}
                testId="onboarding-pair-verification-input"
                ariaInvalid={props.confirmationError !== null}
                error={props.confirmationError ?? undefined}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && !event.repeat && !props.busy) props.onConfirm();
                }}
              />
              <div>
                <Button
                  data-testid="onboarding-pair-confirm"
                  disabled={props.busy || props.verificationCode.trim() === ""}
                  onClick={props.onConfirm}
                >
                  {props.busy ? "Verifying…" : "Verify browser"}
                </Button>
              </div>
            </div>
          </Show>
          <div
            data-testid="onboarding-pair-poll-status"
            role="status"
            aria-live="polite"
            aria-atomic="true"
            style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}
          >
            <StatusDot status={statusIndicator()} />
            <span class="md-body-s">
              <Show when={props.confirmationError !== null}>{props.confirmationError}</Show>
              <Show when={props.confirmationError === null && props.pollStatus === "pending"}>
                Waiting for approval…
              </Show>
              <Show when={props.confirmationError === null && props.pollStatus === "verification_required"}>
                Approval received. Enter the code shown on the paired browser.
              </Show>
              <Show when={props.pollStatus === "denied"}>Request denied.</Show>
              <Show when={props.pollStatus === "expired"}>Request expired — request again.</Show>
              <Show when={props.pollStatus === "verification_failed"}>
                Too many incorrect codes — request again.
              </Show>
              <Show when={props.pollStatus === "completed"}>Pairing completed.</Show>
              <Show when={props.pollStatus === "error"}>Poll error — try again.</Show>
            </span>
          </div>
          <Show
            when={
              props.pollStatus === "verification_required"
              || props.pollStatus === "denied"
              || props.pollStatus === "expired"
              || props.pollStatus === "verification_failed"
              || props.pollStatus === "completed"
              || props.pollStatus === "error"
            }
          >
            <div>
              <Button variant="secondary" onClick={props.onStart} disabled={props.busy}>
                {props.pollStatus === "verification_required" ? "Start over" : "Request again"}
              </Button>
            </div>
          </Show>
        </div>
      </Show>
    </Card>
  );
}
