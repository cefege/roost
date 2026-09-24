// Requester-facing "Request access" card on the pairing page: the one primary
// Request approval action, token-bound poll status, and six-digit verification
// entry. PairingRequesterProvider owns the ceremony; Onboarding passes its
// signals here. The opaque request ID is never rendered.

import { Show } from "solid-js";
import type { JSX } from "solid-js";
import { Card, StatusDot, Button, TextField } from "../Settings/md/primitives.tsx";

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
  /** The page's alert already reports this failure, so the card shows no second indicator. */
  requestFailed?: boolean;
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
    <Card data-testid="onboarding-pair-step" title="Request access" variant="outlined">
      <div class="pairing-request">
        <Show
          when={props.ephemeralId !== null}
          fallback={
            <>
              <p class="md-body-m pairing-request__supporting">
                A browser that is already paired approves the request, then shows a
                6-digit code for you to enter here.
              </p>
              <div>
                <Button
                  variant="default"
                  data-testid="onboarding-pair-start-btn"
                  onClick={props.onStart}
                  disabled={props.busy}
                >
                  {props.busy ? "Requesting…" : props.pollStatus === "error" ? "Request again" : "Request approval"}
                </Button>
              </div>
            </>
          }
        >
          <Show when={!props.requestFailed}>
            <div
              data-testid="onboarding-pair-poll-status"
              class="pairing-request__status"
              role="status"
              aria-live="polite"
              aria-atomic="true"
            >
              <StatusDot status={statusIndicator()} />
              <span class="md-body-m">
                <Show when={props.confirmationError !== null}>{props.confirmationError}</Show>
                <Show when={props.confirmationError === null && props.pollStatus === "pending"}>
                  Waiting for approval on another paired browser…
                </Show>
                <Show when={props.confirmationError === null && props.pollStatus === "verification_required"}>
                  Approval received. Enter the 6-digit code shown on the paired browser.
                </Show>
                <Show when={props.pollStatus === "denied"}>Request denied.</Show>
                <Show when={props.pollStatus === "expired"}>Request expired — request again.</Show>
                <Show when={props.pollStatus === "verification_failed"}>
                  Too many incorrect codes — request again.
                </Show>
                <Show when={props.pollStatus === "completed"}>Pairing completed.</Show>
                <Show when={props.confirmationError === null && props.pollStatus === "error"}>
                  Request failed — request again.
                </Show>
              </span>
            </div>
          </Show>
          <Show when={props.pollStatus === "verification_required"}>
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
          </Show>
          <Show when={props.pollStatus !== "idle" && props.pollStatus !== "pending"}>
            <div class="pairing-request__actions">
              <Show when={props.pollStatus === "verification_required"}>
                <Button
                  data-testid="onboarding-pair-confirm"
                  disabled={props.busy || props.verificationCode.trim() === ""}
                  onClick={props.onConfirm}
                >
                  {props.busy ? "Verifying…" : "Verify browser"}
                </Button>
              </Show>
              <Button variant="secondary" onClick={props.onStart} disabled={props.busy}>
                {props.pollStatus === "verification_required" ? "Start over" : "Request again"}
              </Button>
            </div>
          </Show>
        </Show>
      </div>
    </Card>
  );
}
