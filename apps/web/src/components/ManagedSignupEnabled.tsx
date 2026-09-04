// Enabled managed signup owns provider controls and email or Google enrollment actions.
// ManagedSignup gates this component on resolved coordinator policy before mounting it.
// Gateway responses and Turnstile state drive the form without inventing eligibility.

import type { ManagedAuthConfig } from "../auth/managed-auth-gateway.ts";
import { createSignal, onCleanup, onMount, Show } from "solid-js";
import {
  ManagedAuthGatewayError,
  startManagedEmailSignup,
  startManagedGoogle,
} from "../auth/managed-auth-gateway.ts";
import {
  managedGoogleEnabled,
  managedSignupRouteEnabled,
} from "../auth/managed-signup-policy.ts";
import { clearCapturedFragmentCredential } from "../auth/fragment-credential.ts";
import { clearManagedGoogleCompletionProgress } from "../auth/managed-auth-progress.ts";
import { ROUTES } from "../routes.ts";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import { Button } from "./Settings/md/Button.tsx";
import {
  TextField,
  type TextFieldElement,
} from "./Settings/md/TextField.tsx";
import { TurnstileWidget } from "./TurnstileWidget.tsx";

export const MANAGED_SIGNUP_PENDING_MESSAGE =
  "If this address is eligible for a new account, we sent a verification link.";
export const MANAGED_SIGNUP_CAPACITY_MESSAGE = "Roost is temporarily full.";
const HUMAN_VERIFICATION_MESSAGE = "Complete human verification and try again.";
const SIGNUP_CONNECTION_MESSAGE = "Roost couldn’t start signup. Check your connection and try again.";

type SignupAction = "idle" | "google" | "email";

export function ManagedSignupEnabled(props: { config: ManagedAuthConfig }) {
  const [email, setEmail] = createSignal("");
  const [turnstileToken, setTurnstileToken] = createSignal<string | null>(null);
  const [resetNonce, setResetNonce] = createSignal(0);
  const [action, setAction] = createSignal<SignupAction>("idle");
  const [message, setMessage] = createSignal("");
  const [isError, setIsError] = createSignal(false);
  let emailInput: TextFieldElement | undefined;
  let messageSummary: HTMLParagraphElement | undefined;
  let disposed = false;

  function requireTurnstile(): string | null {
    const token = turnstileToken();
    if (token) return token;
    setMessage(HUMAN_VERIFICATION_MESSAGE);
    setIsError(true);
    return null;
  }

  async function continueWithGoogle(): Promise<void> {
    if (!managedSignupRouteEnabled(ROUTES.SIGNUP, props.config)
      || !managedGoogleEnabled(props.config)
      || action() !== "idle") return;
    const token = requireTurnstile();
    if (!token) return;

    messageSummary?.focus();
    setAction("google");
    setMessage("Opening Google sign-in…");
    setIsError(false);
    try {
      const authorizationUrl = await startManagedGoogle({
        intent: "signup",
        turnstileToken: token,
      });
      setResetNonce((value) => value + 1);
      clearManagedGoogleCompletionProgress();
      clearCapturedFragmentCredential("email-signup");
      location.assign(authorizationUrl);
    } catch (error) {
      setResetNonce((value) => value + 1);
      setAction("idle");
      const full = error instanceof ManagedAuthGatewayError
        && error.code === "signup-unavailable";
      setMessage(full ? MANAGED_SIGNUP_CAPACITY_MESSAGE : SIGNUP_CONNECTION_MESSAGE);
      setIsError(!full);
    }
  }

  async function submitEmail(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (!managedSignupRouteEnabled(ROUTES.SIGNUP, props.config) || action() !== "idle") return;
    const token = requireTurnstile();
    if (!token) return;

    messageSummary?.focus();
    setAction("email");
    setMessage("Sending a verification link…");
    setIsError(false);
    try {
      await startManagedEmailSignup({ email: email().trim(), turnstileToken: token });
      setEmail("");
      setMessage(MANAGED_SIGNUP_PENDING_MESSAGE);
    } catch (error) {
      const full = error instanceof ManagedAuthGatewayError
        && error.code === "signup-unavailable";
      const verificationFailed = error instanceof ManagedAuthGatewayError
        && error.code === "verification-failed";
      setMessage(
        full
          ? MANAGED_SIGNUP_CAPACITY_MESSAGE
          : verificationFailed
            ? HUMAN_VERIFICATION_MESSAGE
            : SIGNUP_CONNECTION_MESSAGE,
      );
      setIsError(!full);
    } finally {
      setResetNonce((value) => value + 1);
      setAction("idle");
    }
  }

  onMount(() => {
    queueMicrotask(() => {
      if (!disposed) emailInput?.focus();
    });
  });

  onCleanup(() => {
    disposed = true;
  });

  const busy = () => action() !== "idle";

  return (
    <ManagedAuthLayout
      testId="managed-signup"
      title="Create your Roost account"
      description="Start with Google, or use a verified email address."
    >
      <div class="managed-auth-form" aria-busy={busy()}>
        <Show when={managedGoogleEnabled(props.config)}>
          <Button
            class="managed-auth-submit managed-google-button"
            data-testid="managed-signup-google"
            type="button"
            variant="tonal"
            disabled={busy() || !turnstileToken()}
            onClick={() => void continueWithGoogle()}
          >
            {action() === "google" ? "Opening Google…" : "Continue with Google"}
          </Button>

          <div class="managed-auth-divider" aria-hidden="true">
            <span>or continue with email</span>
          </div>
        </Show>

        <form class="managed-auth-form" onSubmit={(event) => void submitEmail(event)}>
          <TextField
            ref={(element) => { emailInput = element; }}
            class="managed-auth-text-field"
            label="Email"
            testId="managed-signup-email"
            type="email"
            inputMode="email"
            autocomplete="email"
            required
            disabled={busy()}
            value={email()}
            onInput={setEmail}
            ariaDescribedBy="managed-signup-message"
          />

          <Show when={props.config.turnstileSiteKey}>
            {(siteKey) => (
              <TurnstileWidget
                siteKey={siteKey()}
                disabled={busy()}
                resetNonce={resetNonce()}
                onToken={setTurnstileToken}
              />
            )}
          </Show>

          <p
            ref={(element) => { messageSummary = element; }}
            class={isError() ? "managed-auth-error" : "managed-auth-message"}
            id="managed-signup-message"
            data-testid="managed-signup-message"
            role={isError() ? "alert" : "status"}
            aria-live="polite"
            tabIndex={-1}
          >
            {message()}
          </p>

          <Button
            class="managed-auth-submit"
            data-testid="managed-signup-email-submit"
            type="submit"
            variant="filled"
            disabled={busy() || !turnstileToken()}
          >
            {action() === "email" ? "Sending…" : "Continue with email"}
          </Button>
        </form>

        <div class="managed-auth-links">
          <a class="managed-auth-link" href={ROUTES.LOGIN}>Already have an account? Sign in</a>
        </div>
      </div>
    </ManagedAuthLayout>
  );
}
