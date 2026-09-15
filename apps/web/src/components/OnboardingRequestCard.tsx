// The "I don't have a code" half of the pairing surface: one button that posts
// this browser's public key, then the ephemeral id plus a live poll status while
// an already-paired browser approves it.
//
// Split out of Onboarding.tsx, which sat exactly at the 400-line cap.
// Callers: Onboarding.tsx. Depends on: Settings/md primitives, solid-js Show.

import { Show, type JSX } from "solid-js";
import { Card, StatusDot, Surface, Button } from "./Settings/md/primitives.tsx";

export type PairPollStatus =
  | "idle"
  | "pending"
  | "approved"
  | "denied"
  | "expired"
  | "error";

export function OnboardingRequestCard(props: {
  ephemeralId: string | null;
  pollStatus: PairPollStatus;
  busy: boolean;
  onStart: () => void;
}): JSX.Element {
  const statusIndicator = () => {
    switch (props.pollStatus) {
      case "approved":
        return "ok";
      case "denied":
      case "expired":
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
              You'll get a short code — open Roost on the paired browser and approve the
              request from <strong>Settings → Devices</strong>.
            </p>
            <div>
              <Button
                variant="secondary"
                data-testid="onboarding-pair-start-btn"
                onClick={props.onStart}
                disabled={props.busy}
              >
                {props.busy ? "..." : "Request approval"}
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
            On the already-paired browser, open <strong>Settings → Devices</strong>.
            You'll see this code listed under "Pending pair requests" — click
            <strong> Approve</strong>. This page reloads itself when approved.
          </p>
          <Surface
            level={2}
            radius="sm"
            pad={3}
            border
            data-testid="onboarding-pair-ephemeral-id"
          >
            <code class="md-title-m" style={{ display: "block", "overflow-wrap": "anywhere" }}>
              {props.ephemeralId}
            </code>
          </Surface>
          <div
            data-testid="onboarding-pair-poll-status"
            role="status"
            aria-live="polite"
            aria-atomic="true"
            style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}
          >
            <StatusDot status={statusIndicator()} />
            <span class="md-body-s">
              <Show when={props.pollStatus === "pending"}>Waiting for approval…</Show>
              <Show when={props.pollStatus === "approved"}>Approved. Reloading…</Show>
              <Show when={props.pollStatus === "denied"}>Request denied.</Show>
              <Show when={props.pollStatus === "expired"}>Request expired — request again.</Show>
              <Show when={props.pollStatus === "error"}>Poll error — try again.</Show>
            </span>
          </div>
        </div>
      </Show>
    </Card>
  );
}
