// Managed users need a credential entry point that reflects coordinator-provided auth options.
// The public login route renders this form and redirects successful sessions into the app.
// Gateway, login, Google, and web-key modules remain authoritative for each auth transition.

import { useNavigate } from "@solidjs/router";
import {
  createEffect,
  createResource,
  createSignal,
  onCleanup,
  Show,
} from "solid-js";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import {
  getManagedAuthConfig,
  startManagedGoogle,
} from "../auth/managed-auth-gateway.ts";
import {
  managedGoogleEnabled,
  managedSignupRouteEnabled,
} from "../auth/managed-signup-policy.ts";
import { clearCapturedFragmentCredential } from "../auth/fragment-credential.ts";
import { clearManagedGoogleCompletionProgress } from "../auth/managed-auth-progress.ts";
import { Button } from "./Settings/md/Button.tsx";
import { Dialog } from "./Settings/md/Dialog.tsx";
import {
  TextField,
  type TextFieldElement,
} from "./Settings/md/TextField.tsx";
import {
  loginManagedBrowser,
  managedLoginErrorMessage,
} from "../auth/managed-login.ts";
import { isResetWebKeyEligible, resetWebKey } from "../auth/web-key.ts";
import { ROUTES } from "../routes.ts";
import { hasConfirmedDashboardAccess, rootStore } from "../store/root.ts";

type LoginPhase = "ready" | "password" | "google";

export function ManagedLogin() {
  const navigate = useNavigate();
  const [config, { refetch: refetchConfig }] = createResource(() => getManagedAuthConfig());
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [phase, setPhase] = createSignal<LoginPhase>("ready");
  const [errorMessage, setErrorMessage] = createSignal("");
  const [resetConfirmationOpen, setResetConfirmationOpen] = createSignal(false);
  const [resettingKey, setResettingKey] = createSignal(false);
  let emailInput: TextFieldElement | undefined;
  let passwordInput: TextFieldElement | undefined;
  let googleButton: HTMLElement | undefined;
  let resetKeyButton: HTMLElement | undefined;
  let configStatus: HTMLDivElement | undefined;
  let configRetryButton: HTMLElement | undefined;
  let disposed = false;
  let loginAttempted = false;
  const [resetEligible] = createResource(
    () => rootStore.browser_unauthorized && rootStore.coord_identity?.saas_mode === true,
    async (unauthorized) => unauthorized ? isResetWebKeyEligible("managed") : false,
  );

  function focusAfterRender(target: () => HTMLElement | undefined): void {
    queueMicrotask(() => {
      if (!disposed) target()?.focus();
    });
  }

  createEffect(() => {
    if (!loginAttempted && !rootStore.browser_unauthorized && hasConfirmedDashboardAccess()) {
      navigate(ROUTES.APP, { replace: true });
    }
  });

  async function continueWithGoogle(): Promise<void> {
    if (busy() || !googleAvailable()) return;
    loginAttempted = true;
    setPhase("google");
    setErrorMessage("");
    try {
      const authorizationUrl = await startManagedGoogle({ intent: "login" });
      clearManagedGoogleCompletionProgress();
      clearCapturedFragmentCredential("email-signup");
      location.assign(authorizationUrl);
    } catch {
      setErrorMessage("Google sign-in couldn’t start. Check your connection and try again.");
      setPhase("ready");
    }
  }

  async function submit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (busy()) return;

    loginAttempted = true;
    const submittedPassword = password();
    setPhase("password");
    setErrorMessage("");
    try {
      await loginManagedBrowser({ email: email(), password: submittedPassword });
      setPassword("");
      // loginManagedBrowser has started a full replace navigation so every
      // protected transport is reconstructed with the selected account prefix.
      return;
    } catch (error) {
      setPassword("");
      setErrorMessage(managedLoginErrorMessage(error));
      setPhase("ready");
      focusAfterRender(() => passwordInput);
    }
  }


  function resetDialogClosed(): void {
    setResetConfirmationOpen(false);
    if (!resettingKey()) focusAfterRender(() => resetKeyButton);
  }

  async function resetRejectedKey(): Promise<void> {
    if (busy()) return;
    setResettingKey(true);
    setErrorMessage("");
    try {
      await resetWebKey("managed");
    } catch (error) {
      setErrorMessage(managedLoginErrorMessage(error));
    } finally {
      setResettingKey(false);
      if (resetConfirmationOpen()) setResetConfirmationOpen(false);
      else focusAfterRender(() => resetKeyButton);
    }
  }

  async function retryManagedConfig(): Promise<void> {
    if (busy() || config.loading) return;
    configStatus?.focus();
    await refetchConfig();
    if (config.error) {
      focusAfterRender(() => configRetryButton);
      return;
    }
    focusAfterRender(() => googleAvailable() ? googleButton : emailInput);
  }

  const busy = () => phase() !== "ready" || resettingKey();
  const resolvedConfig = () => config.loading || config.error ? undefined : config();
  const googleAvailable = () => managedGoogleEnabled(resolvedConfig());
  const loginDescription = () => {
    if (config.loading) {
      return "Checking available sign-in options. You can still use email and password.";
    }
    if (config.error) {
      return "You can still use email and password while other sign-in options are unavailable.";
    }
    return googleAvailable()
      ? "Choose Google or use your email and password."
      : "Use your email and password.";
  };

  onCleanup(() => {
    disposed = true;
  });

  return (
    <ManagedAuthLayout
      testId="managed-login"
      title="Sign in"
      description={loginDescription()}
    >
      <div class="managed-auth-form" aria-busy={busy()}>
        <Show when={config.loading && !config.error}>
          <p class="managed-auth-hint" role="status" aria-live="polite">
            Checking available sign-in options…
          </p>
        </Show>

        <Show when={config.error}>
          <div
            ref={(element) => { configStatus = element; }}
            data-testid="managed-login-config-status"
            class="managed-auth-status"
            aria-busy={config.loading || undefined}
            tabIndex={-1}
          >
            <p
              class="managed-auth-error"
              id="managed-login-config-error"
              role="alert"
              aria-live="polite"
            >
              Roost couldn’t check all sign-in options. Check your connection and retry.
            </p>
            <Button
              ref={(element) => { configRetryButton = element; }}
              class="managed-auth-submit"
              data-testid="managed-login-config-retry"
              type="button"
              variant="text"
              disabled={busy() || config.loading}
              aria-describedby="managed-login-config-error"
              onClick={() => void retryManagedConfig()}
            >
              {config.loading ? "Checking sign-in options…" : "Retry sign-in options"}
            </Button>
          </div>
        </Show>
        <Show when={googleAvailable()}>
          <Button
            ref={(element) => { googleButton = element; }}
            class="managed-auth-submit managed-google-button"
            data-testid="managed-login-google"
            type="button"
            variant="tonal"
            disabled={busy()}
            onClick={() => void continueWithGoogle()}
          >
            {phase() === "google" ? "Opening Google…" : "Continue with Google"}
          </Button>

          <div class="managed-auth-divider" aria-hidden="true">
            <span>or use email and password</span>
          </div>
        </Show>

        <form
          class="managed-auth-form"
          onSubmit={(event) => void submit(event)}
          aria-busy={phase() === "password"}
        >
          <TextField
            ref={(element) => { emailInput = element; }}
            class="managed-auth-text-field"
            label="Email"
            testId="managed-login-email"
            type="email"
            inputMode="email"
            autocomplete="username"
            required
            disabled={busy()}
            value={email()}
            onInput={(value) => setEmail(value)}
            ariaDescribedBy="managed-login-error"
          />

          <TextField
            ref={(element) => { passwordInput = element; }}
            class="managed-auth-text-field"
            label="Password"
            testId="managed-login-password"
            type="password"
            autocomplete="current-password"
            required
            disabled={busy()}
            value={password()}
            onInput={(value) => setPassword(value)}
            ariaDescribedBy="managed-login-error"
          />

          <p
            class="managed-auth-error"
            id="managed-login-error"
            role="alert"
            aria-live="polite"
          >
            {errorMessage()}
          </p>

          <Button
            class="managed-auth-submit"
            data-testid="managed-login-submit"
            type="submit"
            variant="filled"
            disabled={busy()}
          >
            {phase() === "password" ? "Signing in…" : "Sign in"}
          </Button>

          <Show when={resetEligible()}>
            <Button
              ref={(element) => { resetKeyButton = element; }}
              class="managed-auth-submit"
              data-testid="managed-login-reset-key"
              type="button"
              variant="text"
              disabled={busy()}
              onClick={() => setResetConfirmationOpen(true)}
            >
              Reset this browser key
            </Button>
          </Show>

          <div class="managed-auth-links">
            <a class="managed-auth-link" href={ROUTES.FORGOT_PASSWORD}>Forgot password?</a>
            <Show when={managedSignupRouteEnabled(ROUTES.SIGNUP, resolvedConfig())}>
              <a class="managed-auth-link" href={ROUTES.SIGNUP}>New to Roost? Create account</a>
            </Show>
          </div>
        </form>

        <Dialog
          open={resetConfirmationOpen()}
          onClose={resetDialogClosed}
          headline="Reset this browser key?"
          actions={
            <>
              <Button
                type="button"
                variant="text"
                disabled={resettingKey()}
                onClick={() => setResetConfirmationOpen(false)}
              >
                Cancel
              </Button>
              <Button
                data-testid="managed-login-reset-key-confirm"
                type="button"
                variant="filled"
                disabled={resettingKey()}
                onClick={() => void resetRejectedKey()}
              >
                {resettingKey() ? "Resetting…" : "Reset browser key"}
              </Button>
            </>
          }
        >
          <p class="managed-auth-hint">
            This removes the rejected key stored in this browser. You’ll need to sign in again
            to authorize a new browser key.
          </p>
        </Dialog>
      </div>
    </ManagedAuthLayout>
  );
}
