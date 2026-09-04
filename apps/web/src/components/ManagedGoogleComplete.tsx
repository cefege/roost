// Google completion may finish either browser authentication or an account-link request.
// The callback route polls the gateway and returns users to the appropriate managed surface.
// Credential assertions and managed-google state determine which completion path is permitted.

import { onCleanup, onMount, createSignal, Show } from "solid-js";
import {
  completeManagedGoogleLink,
  isManagedGoogleLinkAssertion,
  MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE,
} from "../auth/managed-credentials.ts";
import {
  getManagedAuthResult,
  type ManagedAuthResult,
} from "../auth/managed-auth-gateway.ts";
import { completeManagedGoogleAuthentication } from "../auth/managed-google.ts";
import { ROUTES } from "../routes.ts";
import { ManagedAuthLayout } from "./ManagedAuthLayout.tsx";
import { Button } from "./Settings/md/Button.tsx";

const GOOGLE_COMPLETION_ERROR =
  "Google sign-in couldn’t be completed. Check your connection and try again.";

type CompletionPhase = "polling" | "finishing" | "retry" | "terminal";

export function ManagedGoogleComplete() {
  const [phase, setPhase] = createSignal<CompletionPhase>("polling");
  const [message, setMessage] = createSignal("Confirming your Google sign-in…");
  const [showLogin, setShowLogin] = createSignal(false);
  let disposed = false;
  let pollTimer: number | undefined;
  let completionAttempt = 0;
  let statusSummary: HTMLParagraphElement | undefined;

  function attemptIsCurrent(attempt: number): boolean {
    return !disposed && attempt === completionAttempt;
  }

  function focusStatus(attempt: number): void {
    queueMicrotask(() => {
      if (attemptIsCurrent(attempt)) statusSummary?.focus();
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

  async function finishResult(result: ManagedAuthResult, attempt: number): Promise<void> {
    if (!attemptIsCurrent(attempt)) return;
    if (result.state === "pending") {
      setPhase("polling");
      setMessage("Preparing your private Roost account…");
      schedulePoll(result.retryAfterMs, attempt);
      return;
    }
    if (result.state === "proof-required") {
      setPhase("terminal");
      setShowLogin(true);
      setMessage("Sign in with your existing method, then connect Google in Settings.");
      focusStatus(attempt);
      return;
    }
    if (result.state === "capacity") {
      setPhase("terminal");
      setMessage("Roost is temporarily full. Please try again later.");
      focusStatus(attempt);
      return;
    }
    if (result.state === "failed") {
      setPhase("terminal");
      setShowLogin(true);
      setMessage("Google sign-in was canceled or couldn’t be completed. Your current Roost account was not changed.");
      focusStatus(attempt);
      return;
    }
    if (!("routeKey" in result)) throw new Error("invalid terminal result");

    setPhase("finishing");
    setMessage(result.state === "awaiting-device" ? "Securing this browser…" : "Finishing sign-in…");
    try {
      if (result.state === "ready" && result.assertion && isManagedGoogleLinkAssertion(result.assertion)) {
        await completeManagedGoogleLink({
          routeKey: result.routeKey,
          assertion: result.assertion,
        });
        if (!attemptIsCurrent(attempt)) return;
        location.replace("/settings/account");
        return;
      }
      if (result.state === "ready" && !result.assertion) throw new Error("missing assertion");
      await completeManagedGoogleAuthentication({
        routeKey: result.routeKey,
        ...(result.state === "ready" ? { assertion: result.assertion } : {}),
      });
    } catch (error) {
      if (!attemptIsCurrent(attempt)) return;
      setPhase("retry");
      setMessage(
        error instanceof Error && error.message === MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE
          ? MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE
          : GOOGLE_COMPLETION_ERROR,
      );
      focusStatus(attempt);
    }
  }

  async function pollResult(attempt: number): Promise<void> {
    if (!attemptIsCurrent(attempt)) return;
    try {
      const result = await getManagedAuthResult();
      if (!attemptIsCurrent(attempt)) return;
      await finishResult(result, attempt);
    } catch {
      if (!attemptIsCurrent(attempt)) return;
      setPhase("retry");
      setMessage(GOOGLE_COMPLETION_ERROR);
      focusStatus(attempt);
    }
  }

  function startCompletionAttempt(): void {
    if (disposed) return;
    const attempt = ++completionAttempt;
    clearTimeout(pollTimer);
    pollTimer = undefined;
    setShowLogin(false);
    setPhase("polling");
    setMessage("Confirming your Google sign-in…");
    focusStatus(attempt);
    void pollResult(attempt);
  }

  function retryCompletion(): void {
    if (phase() !== "retry") return;
    startCompletionAttempt();
  }

  onMount(startCompletionAttempt);
  onCleanup(() => {
    disposed = true;
    completionAttempt++;
    clearTimeout(pollTimer);
  });

  return (
    <ManagedAuthLayout
      testId="managed-google-complete"
      title="Continue to Roost"
      description="Roost is confirming your account and this browser."
    >
      <div
        class="managed-auth-status"
        aria-busy={phase() === "polling" || phase() === "finishing"}
      >
        <p
          ref={statusSummary}
          data-testid="managed-google-complete-status"
          role="status"
          aria-live="polite"
          tabIndex={-1}
        >
          {message()}
        </p>
        <Show when={phase() === "retry"}>
          <Button
            class="managed-auth-submit"
            data-testid="managed-google-complete-retry"
            type="button"
            variant="filled"
            onClick={retryCompletion}
          >
            Try again
          </Button>
        </Show>
        <Show when={showLogin()}>
          <div class="managed-auth-links">
            <a class="managed-auth-link" href={ROUTES.LOGIN}>Return to sign in</a>
            <a class="managed-auth-link" href={ROUTES.SIGNUP}>Create another account</a>
          </div>
        </Show>
      </div>
    </ManagedAuthLayout>
  );
}
