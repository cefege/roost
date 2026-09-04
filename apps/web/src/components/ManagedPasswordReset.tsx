// Password reset credentials must be redeemed once without leaking authoritative denial details.
// The reset route collects a policy-compliant native password from the credential holder.
// Managed-account and fragment-credential modules own validation, redemption, and token state.

import {
  NATIVE_PASSWORD_MAX_LENGTH,
  NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH,
} from "@roost/shared/native-credentials";
import { createSignal, onMount } from "solid-js";
import {
  isAuthoritativeCredentialDenial,
  managedNewPasswordIssue,
  managedNewPasswordIssueMessage,
  managedResetErrorMessage,
  MANAGED_RESET_DENIED_MESSAGE,
  redeemManagedPasswordReset,
} from "../auth/managed-account.ts";
import { peekCapturedFragmentCredential } from "../auth/fragment-credential.ts";
import { ROUTES } from "../routes.ts";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import { Button } from "./Settings/md/Button.tsx";
import {
  TextField,
  type TextFieldElement,
} from "./Settings/md/TextField.tsx";

type ResetPhase = "ready" | "submitting";

export function ManagedPasswordReset() {
  const captured = peekCapturedFragmentCredential();
  const resetRouteKey = captured?.kind === "reset" ? captured.routeKey : undefined;
  const resetToken = captured?.kind === "reset" ? captured.token : "";
  const [tokenAvailable, setTokenAvailable] = createSignal(Boolean(resetToken));
  const [password, setPassword] = createSignal("");
  const [confirmation, setConfirmation] = createSignal("");
  const [phase, setPhase] = createSignal<ResetPhase>("ready");
  const [errorMessage, setErrorMessage] = createSignal(
    resetToken ? "" : MANAGED_RESET_DENIED_MESSAGE,
  );
  let passwordInput: TextFieldElement | undefined;
  let confirmationInput: TextFieldElement | undefined;
  let errorSummary: HTMLParagraphElement | undefined;

  async function submit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (phase() !== "ready" || !tokenAvailable()) return;

    const issue = managedNewPasswordIssue(password(), confirmation());
    if (issue) {
      setErrorMessage(managedNewPasswordIssueMessage(issue));
      queueMicrotask(() => {
        (issue === "confirmation-mismatch" ? confirmationInput : passwordInput)?.focus();
      });
      return;
    }

    setPhase("submitting");
    setErrorMessage("");
    try {
      await redeemManagedPasswordReset({
        routeKey: resetRouteKey,
        token: resetToken,
        password: password(),
        confirmation: confirmation(),
      });
    } catch (error) {
      const authoritativeDenial = isAuthoritativeCredentialDenial(error);
      if (authoritativeDenial) setTokenAvailable(false);
      setPassword("");
      setConfirmation("");
      setErrorMessage(managedResetErrorMessage(error));
      setPhase("ready");
      queueMicrotask(() => {
        (authoritativeDenial ? errorSummary : passwordInput)?.focus();
      });
    }
  }

  const busy = () => phase() === "submitting";
  const disabled = () => busy() || !tokenAvailable();

  onMount(() => {
    if (!tokenAvailable()) queueMicrotask(() => errorSummary?.focus());
  });

  return (
    <ManagedAuthLayout
      testId="managed-password-reset"
      title="Choose a new password"
      description="Resetting the owner password signs out every browser for this account."
    >
      <form class="managed-auth-form" onSubmit={(event) => void submit(event)} aria-busy={busy()}>
        <TextField
          ref={(element) => { passwordInput = element; }}
          class="managed-auth-text-field"
          type="password"
          label="New password"
          testId="managed-reset-password"
          value={password()}
          onInput={setPassword}
          autocomplete="new-password"
          minLength={NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}
          maxLength={NATIVE_PASSWORD_MAX_LENGTH}
          required
          autofocus={tokenAvailable()}
          disabled={disabled()}
          ariaDescribedBy="managed-reset-password-hint managed-reset-error"
        />
        <p class="managed-auth-hint" id="managed-reset-password-hint">
          Use {NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}–{NATIVE_PASSWORD_MAX_LENGTH} characters.
        </p>

        <TextField
          ref={(element) => { confirmationInput = element; }}
          class="managed-auth-text-field"
          type="password"
          label="Confirm new password"
          testId="managed-reset-confirmation"
          value={confirmation()}
          onInput={setConfirmation}
          autocomplete="new-password"
          minLength={NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}
          maxLength={NATIVE_PASSWORD_MAX_LENGTH}
          required
          disabled={disabled()}
          ariaDescribedBy="managed-reset-error"
        />

        <p
          ref={errorSummary}
          class="managed-auth-error"
          id="managed-reset-error"
          data-testid="managed-reset-error"
          role="alert"
          aria-live="polite"
          tabIndex={-1}
        >
          {errorMessage()}
        </p>

        <Button
          class="managed-auth-submit"
          data-testid="managed-reset-submit"
          type="submit"
          variant="filled"
          disabled={disabled()}
        >
          {busy() ? "Resetting…" : "Reset password"}
        </Button>

        <div class="managed-auth-links">
          <a class="managed-auth-link" href={ROUTES.FORGOT_PASSWORD}>Request a new reset link</a>
          <a class="managed-auth-link" href={ROUTES.LOGIN}>Return to sign in</a>
        </div>
      </form>
    </ManagedAuthLayout>
  );
}
