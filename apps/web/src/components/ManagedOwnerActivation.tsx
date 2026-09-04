// A newly deployed managed owner must redeem the captured activation credential exactly once.
// The activation route collects the first native password and then enters the managed app.
// Managed-account policy and fragment-credential state enforce validation and denial handling.

import {
  NATIVE_PASSWORD_MAX_LENGTH,
  NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH,
} from "@roost/shared/native-credentials";
import { createSignal, onMount } from "solid-js";
import {
  activateManagedOwner,
  isAuthoritativeCredentialDenial,
  managedActivationErrorMessage,
  managedNewPasswordIssue,
  managedNewPasswordIssueMessage,
  MANAGED_ACTIVATION_DENIED_MESSAGE,
} from "../auth/managed-account.ts";
import { peekCapturedFragmentCredential } from "../auth/fragment-credential.ts";
import { ROUTES } from "../routes.ts";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import { Button } from "./Settings/md/Button.tsx";
import {
  TextField,
  type TextFieldElement,
} from "./Settings/md/TextField.tsx";

type ActivationPhase = "ready" | "submitting";

export function ManagedOwnerActivation() {
  const captured = peekCapturedFragmentCredential();
  const activationRouteKey = captured?.kind === "activation" ? captured.routeKey : "";
  const activationToken = captured?.kind === "activation" ? captured.token : "";
  const [tokenAvailable, setTokenAvailable] = createSignal(Boolean(activationToken));
  const [password, setPassword] = createSignal("");
  const [confirmation, setConfirmation] = createSignal("");
  const [phase, setPhase] = createSignal<ActivationPhase>("ready");
  const [errorMessage, setErrorMessage] = createSignal(
    activationToken ? "" : MANAGED_ACTIVATION_DENIED_MESSAGE,
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
      await activateManagedOwner({
        routeKey: activationRouteKey,
        token: activationToken,
        password: password(),
        confirmation: confirmation(),
      });
    } catch (error) {
      const authoritativeDenial = isAuthoritativeCredentialDenial(error);
      if (authoritativeDenial) setTokenAvailable(false);
      setPassword("");
      setConfirmation("");
      setErrorMessage(managedActivationErrorMessage(error));
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
      testId="managed-activation"
      title="Create your password"
      description="Finish setting up your Roost account."
    >
      <form class="managed-auth-form" onSubmit={(event) => void submit(event)} aria-busy={busy()}>
        <TextField
          ref={(element) => { passwordInput = element; }}
          class="managed-auth-text-field"
          type="password"
          label="Password"
          testId="managed-activation-password"
          value={password()}
          onInput={setPassword}
          autocomplete="new-password"
          minLength={NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}
          maxLength={NATIVE_PASSWORD_MAX_LENGTH}
          required
          autofocus={tokenAvailable()}
          disabled={disabled()}
          ariaDescribedBy="managed-activation-password-hint managed-activation-error"
        />
        <p class="managed-auth-hint" id="managed-activation-password-hint">
          Use {NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}–{NATIVE_PASSWORD_MAX_LENGTH} characters.
        </p>

        <TextField
          ref={(element) => { confirmationInput = element; }}
          class="managed-auth-text-field"
          type="password"
          label="Confirm password"
          testId="managed-activation-confirmation"
          value={confirmation()}
          onInput={setConfirmation}
          autocomplete="new-password"
          minLength={NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}
          maxLength={NATIVE_PASSWORD_MAX_LENGTH}
          required
          disabled={disabled()}
          ariaDescribedBy="managed-activation-error"
        />

        <p
          ref={errorSummary}
          class="managed-auth-error"
          id="managed-activation-error"
          data-testid="managed-activation-error"
          role="alert"
          aria-live="polite"
          tabIndex={-1}
        >
          {errorMessage()}
        </p>

        <Button
          class="managed-auth-submit"
          data-testid="managed-activation-submit"
          type="submit"
          variant="filled"
          disabled={disabled()}
        >
          {busy() ? "Finishing…" : "Finish setup"}
        </Button>

        <div class="managed-auth-links">
          <a class="managed-auth-link" href={ROUTES.LOGIN}>Already activated? Sign in</a>
        </div>
      </form>
    </ManagedAuthLayout>
  );
}
