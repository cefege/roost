// Signup policy resolution owns the public loading, fetch-error, and settled-denial states.
// Both signup entry routes use this surface before any enrollment provider can mount.
// ManagedAuthLayout supplies the branded shell while retry remains an explicit caller action.

import { createEffect, Match, Switch } from "solid-js";
import { MANAGED_SIGNUP_UNAVAILABLE_MESSAGE } from "../auth/managed-signup-policy.ts";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import { Button } from "./Settings/md/Button.tsx";

type SignupPolicyState = "loading" | "error" | "unavailable";
type ManagedSignupPolicyStateProps = { testId: string } & (
  | { state: "loading" | "unavailable"; onRetry?: never }
  | { state: "error"; onRetry: () => void }
);

const POLICY_COPY: Record<SignupPolicyState, { title: string; description: string }> = {
  loading: {
    title: "Checking account availability",
    description: "Roost is checking whether account creation is available.",
  },
  error: {
    title: "Roost couldn’t check account availability",
    description: "Check your connection and try again.",
  },
  unavailable: {
    title: "Account creation unavailable",
    description: MANAGED_SIGNUP_UNAVAILABLE_MESSAGE,
  },
};

export function ManagedSignupPolicyState(props: ManagedSignupPolicyStateProps) {
  let summary: HTMLDivElement | undefined;
  let previousState: SignupPolicyState | undefined;

  createEffect(() => {
    const nextState = props.state;
    if (previousState !== undefined && previousState !== nextState) {
      queueMicrotask(() => summary?.focus());
    }
    previousState = nextState;
  });
  return (
    <ManagedAuthLayout
      testId={props.testId}
      title={POLICY_COPY[props.state].title}
      description={POLICY_COPY[props.state].description}
    >
      <div
        ref={summary}
        class="managed-auth-status"
        aria-busy={props.state === "loading"}
        tabIndex={-1}
        autofocus
      >
        <Switch>
          <Match when={props.state === "loading"}>
            <p role="status" aria-live="polite">Checking signup settings…</p>
          </Match>
          <Match when={props.state === "error"}>
            <p class="managed-auth-error" role="alert" aria-live="polite">
              Signup settings could not be loaded.
            </p>
            <Button type="button" variant="filled" onClick={() => props.onRetry?.()}>
              Try again
            </Button>
          </Match>
          <Match when={props.state === "unavailable"}>
            <p role="status">Account creation is unavailable.</p>
          </Match>
        </Switch>
      </div>
    </ManagedAuthLayout>
  );
}
