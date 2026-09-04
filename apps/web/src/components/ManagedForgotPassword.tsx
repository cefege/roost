// Password recovery must acknowledge requests without revealing whether an account exists.
// The public recovery route submits an email and renders the coordinator's neutral response.
// Managed-account recovery policy owns the request while this component owns form state.

import { createSignal, Show } from "solid-js";
import {
  MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT,
  requestManagedPasswordReset,
} from "../auth/managed-account.ts";
import { ROUTES } from "../routes.ts";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import { Button } from "./Settings/md/Button.tsx";
import { TextField } from "./Settings/md/TextField.tsx";
import { rootStore } from "../store/root.ts";

type ForgotPhase = "ready" | "submitting" | "acknowledged";

export function ManagedForgotPassword() {
  const [email, setEmail] = createSignal("");
  const [phase, setPhase] = createSignal<ForgotPhase>("ready");
  const [acknowledgement, setAcknowledgement] = createSignal("");

  async function submit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (phase() !== "ready") return;

    setPhase("submitting");
    const message = await requestManagedPasswordReset(
      email(),
      undefined,
      rootStore.coord_identity?.saas_mode !== false,
    );
    setEmail("");
    setAcknowledgement(message);
    setPhase("acknowledged");
  }

  return (
    <ManagedAuthLayout
      testId="managed-forgot-password"
      title="Reset your password"
      description="Enter your email to request a reset link, if password reset is available."
    >
      <Show
        when={phase() !== "acknowledged"}
        fallback={
          <div class="managed-auth-status">
            <p data-testid="managed-forgot-password-ack" role="status" aria-live="polite">
              {acknowledgement() || MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT}
            </p>
            <div class="managed-auth-links">
              <a class="managed-auth-link" href={ROUTES.LOGIN}>Return to sign in</a>
            </div>
          </div>
        }
      >
        <form
          class="managed-auth-form"
          onSubmit={(event) => void submit(event)}
          aria-busy={phase() === "submitting"}
        >
          <TextField
            class="managed-auth-text-field"
            label="Email"
            testId="managed-forgot-password-email"
            type="email"
            inputMode="email"
            autocomplete="username"
            required
            autofocus
            disabled={phase() === "submitting"}
            value={email()}
            onInput={setEmail}
          />

          <Button
            class="managed-auth-submit"
            data-testid="managed-forgot-password-submit"
            type="submit"
            variant="filled"
            disabled={phase() === "submitting"}
          >
            {phase() === "submitting" ? "Sending…" : "Send reset link"}
          </Button>

          <div class="managed-auth-links">
            <a class="managed-auth-link" href={ROUTES.LOGIN}>Return to sign in</a>
          </div>
        </form>
      </Show>
    </ManagedAuthLayout>
  );
}
