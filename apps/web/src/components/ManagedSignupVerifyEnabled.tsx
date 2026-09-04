// Enabled email-signup verification redeems the captured credential and collects a password.
// ManagedSignupVerify resolves signup policy before mounting this state machine.
// Fragment credentials and managed-account modules enforce lifecycle and denial handling.

import {
  NATIVE_PASSWORD_MAX_LENGTH,
  NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH,
} from "@roost/shared/native-credentials";
import { createSignal, onCleanup, onMount, Show } from "solid-js";
import {
  clearCapturedFragmentCredential,
  markCapturedEmailSignupSubmitted,
  peekCapturedFragmentCredential,
} from "../auth/fragment-credential.ts";
import type { ManagedAuthResult } from "../auth/managed-auth-gateway.ts";
import {
  getManagedAuthResult,
  ManagedAuthGatewayError,
  verifyManagedEmailSignup,
} from "../auth/managed-auth-gateway.ts";
import {
  isAuthoritativeCredentialDenial,
  managedActivationErrorMessage,
  managedNewPasswordIssue,
  managedNewPasswordIssueMessage,
} from "../auth/managed-account.ts";
import { activateManagedEmailSignup } from "../auth/managed-signup-verify.ts";
import { clearManagedEmailSignupActivationProgress } from "../auth/managed-auth-progress.ts";
import { ROUTES } from "../routes.ts";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import { Button } from "./Settings/md/Button.tsx";
import {
  TextField,
  type TextFieldElement,
} from "./Settings/md/TextField.tsx";

const INVALID_VERIFICATION_MESSAGE =
  "This verification link is invalid or has expired. Open the newest signup email and try again.";
const PROOF_REQUIRED_MESSAGE =
  "This email already belongs to a Roost account. Sign in, reset your password, or contact support.";
const PROVISIONING_ERROR_MESSAGE =
  "Roost couldn’t finish setting up your account. Check your connection and try again.";

type VerifyPhase =
  | "verifying"
  | "provisioning"
  | "password"
  | "activating"
  | "retry"
  | "terminal";

export function ManagedSignupVerifyEnabled() {
  const captured = peekCapturedFragmentCredential();
  const emailCredential = captured?.kind === "email-signup" ? captured : null;
  const token = emailCredential?.token ?? "";
  const [phase, setPhase] = createSignal<VerifyPhase>(token ? "verifying" : "terminal");
  const [routeKey, setRouteKey] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [confirmation, setConfirmation] = createSignal("");
  const [message, setMessage] = createSignal(token ? "Verifying your email…" : INVALID_VERIFICATION_MESSAGE);
  let pollTimer: number | undefined;
  let disposed = false;
  let completionAttempt = 0;
  let verificationSubmitted = emailCredential?.submittedAtMs !== undefined;
  let passwordInput: TextFieldElement | undefined;
  let confirmationInput: TextFieldElement | undefined;
  let statusSummary: HTMLParagraphElement | undefined;

  function attemptIsCurrent(attempt: number): boolean {
    return !disposed && attempt === completionAttempt;
  }

  function focusStatus(attempt: number): void {
    queueMicrotask(() => {
      if (
        attemptIsCurrent(attempt)
        && phase() !== "password"
        && phase() !== "activating"
      ) statusSummary?.focus();
    });
  }

  function schedulePoll(delayMs: number, attempt: number): void {
    if (!attemptIsCurrent(attempt)) return;
    clearTimeout(pollTimer);
    pollTimer = window.setTimeout(() => {
      pollTimer = undefined;
      void pollResult(attempt);
    }, delayMs);
  }

  function acceptResult(result: ManagedAuthResult, attempt: number): void {
    if (!attemptIsCurrent(attempt)) return;
    if (result.state === "pending") {
      setPhase("provisioning");
      setMessage("Preparing your private Roost account…");
      schedulePoll(result.retryAfterMs, attempt);
      return;
    }
    if (result.state === "ready" && result.assertion === undefined) {
      setRouteKey(result.routeKey);
      setPhase("password");
      setMessage("");
      queueMicrotask(() => {
        if (attemptIsCurrent(attempt) && phase() === "password") passwordInput?.focus();
      });
      return;
    }
    if (result.state === "proof-required") {
      clearCapturedFragmentCredential("email-signup");
      clearManagedEmailSignupActivationProgress();
      setPhase("terminal");
      setMessage(PROOF_REQUIRED_MESSAGE);
      focusStatus(attempt);
      return;
    }
    if (result.state === "capacity") {
      setPhase("terminal");
      setMessage("Roost is temporarily full. Please try again later.");
      focusStatus(attempt);
      return;
    }
    setPhase("terminal");
    setMessage(PROVISIONING_ERROR_MESSAGE);
    focusStatus(attempt);
  }

  async function pollResult(attempt: number): Promise<void> {
    if (!attemptIsCurrent(attempt)) return;
    if (peekCapturedFragmentCredential()?.kind !== "email-signup") {
      setPhase("terminal");
      setMessage(INVALID_VERIFICATION_MESSAGE);
      focusStatus(attempt);
      return;
    }
    setPhase("provisioning");
    setMessage("Preparing your private Roost account…");
    try {
      const result = await getManagedAuthResult();
      if (!attemptIsCurrent(attempt)) return;
      acceptResult(result, attempt);
    } catch {
      if (!attemptIsCurrent(attempt)) return;
      setPhase("retry");
      setMessage(PROVISIONING_ERROR_MESSAGE);
      focusStatus(attempt);
    }
  }

  async function beginVerification(attempt: number): Promise<void> {
    if (!emailCredential || !attemptIsCurrent(attempt)) return;
    if (!verificationSubmitted) {
      try {
        await verifyManagedEmailSignup(token);
        if (!attemptIsCurrent(attempt)) return;
        markCapturedEmailSignupSubmitted();
        verificationSubmitted = true;
      } catch (error) {
        if (!attemptIsCurrent(attempt)) return;
        if (error instanceof ManagedAuthGatewayError && error.status === 400) {
          try {
            const result = await getManagedAuthResult();
            if (!attemptIsCurrent(attempt)) return;
            if (result.state !== "failed") {
              markCapturedEmailSignupSubmitted();
              verificationSubmitted = true;
              acceptResult(result, attempt);
              return;
            }
          } catch {
            if (!attemptIsCurrent(attempt)) return;
          }
          clearCapturedFragmentCredential("email-signup");
          setPhase("terminal");
          setMessage(INVALID_VERIFICATION_MESSAGE);
          focusStatus(attempt);
          return;
        }
        setPhase("retry");
        setMessage(PROVISIONING_ERROR_MESSAGE);
        focusStatus(attempt);
        return;
      }
    }
    await pollResult(attempt);
  }

  function startVerificationAttempt(): void {
    if (disposed) return;
    const attempt = ++completionAttempt;
    clearTimeout(pollTimer);
    pollTimer = undefined;
    if (!emailCredential) {
      setPhase("terminal");
      setMessage(INVALID_VERIFICATION_MESSAGE);
      focusStatus(attempt);
      return;
    }
    setPhase(verificationSubmitted ? "provisioning" : "verifying");
    setMessage(
      verificationSubmitted
        ? "Preparing your private Roost account…"
        : "Verifying your email…",
    );
    focusStatus(attempt);
    void beginVerification(attempt);
  }

  function retryVerification(): void {
    if (phase() !== "retry") return;
    startVerificationAttempt();
  }

  async function submitPassword(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (phase() !== "password") return;
    const issue = managedNewPasswordIssue(password(), confirmation());
    if (issue) {
      setMessage(managedNewPasswordIssueMessage(issue));
      const attempt = completionAttempt;
      queueMicrotask(() => {
        if (!attemptIsCurrent(attempt) || phase() !== "password") return;
        (issue === "confirmation-mismatch" ? confirmationInput : passwordInput)?.focus();
      });
      return;
    }

    const attempt = completionAttempt;
    setPhase("activating");
    setMessage("");
    try {
      await activateManagedEmailSignup({
        routeKey: routeKey(),
        token,
        password: password(),
        confirmation: confirmation(),
      });
    } catch (error) {
      if (!attemptIsCurrent(attempt)) return;
      setPassword("");
      setConfirmation("");
      if (isAuthoritativeCredentialDenial(error)) {
        setPhase("terminal");
        setMessage(INVALID_VERIFICATION_MESSAGE);
        focusStatus(attempt);
        return;
      }
      setPhase("password");
      setMessage(managedActivationErrorMessage(error));
      queueMicrotask(() => {
        if (attemptIsCurrent(attempt) && phase() === "password") passwordInput?.focus();
      });
    }
  }

  onMount(startVerificationAttempt);
  onCleanup(() => {
    disposed = true;
    completionAttempt++;
    clearTimeout(pollTimer);
  });

  const passwordPhase = () => phase() === "password" || phase() === "activating";

  return (
    <ManagedAuthLayout
      testId="managed-signup-verify"
      title={passwordPhase() ? "Create your password" : "Finish setting up your account"}
      description={passwordPhase()
        ? "Finish setting up your Roost account."
        : "Keep this tab open while Roost prepares your private account."}
    >
      <Show
        when={passwordPhase()}
        fallback={
          <div class="managed-auth-status" aria-busy={phase() === "verifying" || phase() === "provisioning"}>
            <p
              ref={statusSummary}
              class={phase() === "terminal" || phase() === "retry" ? "managed-auth-message" : undefined}
              data-testid="managed-signup-verify-status"
              role={phase() === "terminal" || phase() === "retry" ? "alert" : "status"}
              aria-live="polite"
              tabIndex={-1}
            >
              {message()}
            </p>
            <Show when={phase() === "retry"}>
              <Button
                data-testid="managed-signup-verify-retry"
                type="button"
                variant="filled"
                onClick={retryVerification}
              >
                Try again
              </Button>
            </Show>
            <Show when={phase() === "terminal"}>
              <div class="managed-auth-links">
                <a class="managed-auth-link" href={ROUTES.LOGIN}>Sign in</a>
                <a class="managed-auth-link" href={ROUTES.FORGOT_PASSWORD}>Reset password</a>
                <a class="managed-auth-link" href="mailto:support@roosttt.com">Contact support</a>
              </div>
            </Show>
          </div>
        }
      >
        <form
          class="managed-auth-form"
          onSubmit={(event) => void submitPassword(event)}
          aria-busy={phase() === "activating"}
        >
          <TextField
            ref={(element) => { passwordInput = element; }}
            class="managed-auth-text-field"
            type="password"
            label="Password"
            testId="managed-signup-password"
            value={password()}
            onInput={setPassword}
            autocomplete="new-password"
            minLength={NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}
            maxLength={NATIVE_PASSWORD_MAX_LENGTH}
            required
            disabled={phase() === "activating"}
            ariaDescribedBy="managed-signup-password-hint managed-signup-verify-error"
          />
          <p class="managed-auth-hint" id="managed-signup-password-hint">
            Use {NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}–{NATIVE_PASSWORD_MAX_LENGTH} characters.
          </p>

          <TextField
            ref={(element) => { confirmationInput = element; }}
            class="managed-auth-text-field"
            type="password"
            label="Confirm password"
            testId="managed-signup-confirmation"
            value={confirmation()}
            onInput={setConfirmation}
            autocomplete="new-password"
            minLength={NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH}
            maxLength={NATIVE_PASSWORD_MAX_LENGTH}
            required
            disabled={phase() === "activating"}
            ariaDescribedBy="managed-signup-verify-error"
          />

          <p
            class="managed-auth-error"
            id="managed-signup-verify-error"
            data-testid="managed-signup-verify-error"
            role="alert"
            aria-live="polite"
          >
            {message()}
          </p>

          <Button
            class="managed-auth-submit"
            data-testid="managed-signup-activate"
            type="submit"
            variant="filled"
            disabled={phase() === "activating"}
          >
            {phase() === "activating" ? "Finishing…" : "Finish setup"}
          </Button>
        </form>
      </Show>
    </ManagedAuthLayout>
  );
}
