// Managed account settings combine credential controls, authorized devices, and logout.
// The settings route renders this pane only for the signed-in account context.
// Auth modules own server mutations while this component serializes reactive UI transitions.

import { Show, createSignal, onCleanup, onMount } from "solid-js";
import {
  MANAGED_CREDENTIALS_UNAVAILABLE_MESSAGE,
  MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE,
  MANAGED_PASSWORD_ADD_FAILED_MESSAGE,
  ManagedAccountBusyGate,
  ManagedGoogleIdentityUnavailableError,
  addManagedPassword,
  beginManagedGoogleLink,
  getManagedCredentials,
  type ManagedAccountBusyAction,
  type ManagedCredentials,
} from "../../auth/managed-credentials.ts";
import { ManagedNewPasswordError } from "../../auth/managed-account.ts";
import { logoutManagedBrowser } from "../../auth/managed-logout.ts";
import { AuthorizedDevicesCard } from "./DevicesPane.tsx";
import { Button, Card, List, ListRow, TextField } from "./md/primitives.tsx";


/** Managed coordinators have one owner account and one immutable coordinator
 * scope. Account actions live together so the managed UI never exposes the
 * self-hosted organization, dashboard, pairing, or connection controls. */
export function AccountPane() {
  const actionGate = new ManagedAccountBusyGate();
  const [busyAction, setBusyAction] = createSignal<ManagedAccountBusyAction | null>(null);
  const [logoutError, setLogoutError] = createSignal("");
  const [credentials, setCredentials] = createSignal<ManagedCredentials | null>(null);
  const [credentialsLoading, setCredentialsLoading] = createSignal(true);
  const [credentialsError, setCredentialsError] = createSignal("");
  const [methodError, setMethodError] = createSignal("");
  const [passwordFormOpen, setPasswordFormOpen] = createSignal(false);
  const [password, setPassword] = createSignal("");
  const [passwordConfirmation, setPasswordConfirmation] = createSignal("");
  const [passwordError, setPasswordError] = createSignal("");
  let credentialsRequest = 0;
  let disposed = false;
  let addPasswordTrigger: HTMLButtonElement | undefined;
  let passwordInput: HTMLElement | undefined;
  let passwordConfirmationInput: HTMLElement | undefined;
  let passwordStatus: HTMLSpanElement | undefined;

  const busy = () => busyAction() !== null;

  function beginAction(action: ManagedAccountBusyAction): boolean {
    if (!actionGate.begin(action)) return false;
    setBusyAction(actionGate.current);
    return true;
  }

  function finishAction(action: ManagedAccountBusyAction): void {
    if (actionGate.finish(action)) setBusyAction(actionGate.current);
  }

  function clearPasswordSecrets(): void {
    setPassword("");
    setPasswordConfirmation("");
  }

  function closePasswordForm(): void {
    clearPasswordSecrets();
    setPasswordError("");
    setPasswordFormOpen(false);
    queueMicrotask(() => {
      if (!disposed && !passwordFormOpen()) addPasswordTrigger?.focus();
    });
  }

  function openPasswordForm(): void {
    setPasswordError("");
    setPasswordFormOpen(true);
    queueMicrotask(() => {
      if (!disposed && passwordFormOpen()) passwordInput?.focus();
    });
  }

  async function refreshCredentials(): Promise<void> {
    const request = ++credentialsRequest;
    setCredentialsLoading(true);
    setCredentialsError("");
    try {
      const next = await getManagedCredentials();
      if (disposed || request !== credentialsRequest) return;
      setCredentials(next);
    } catch {
      if (disposed || request !== credentialsRequest) return;
      setCredentialsError(MANAGED_CREDENTIALS_UNAVAILABLE_MESSAGE);
    } finally {
      if (!disposed && request === credentialsRequest) setCredentialsLoading(false);
    }
  }

  async function connectGoogle(): Promise<void> {
    if (!beginAction("google")) return;
    setMethodError("");
    try {
      const result = await beginManagedGoogleLink();
      window.location.assign(result.authorizationUrl);
    } catch (error) {
      setMethodError(error instanceof ManagedGoogleIdentityUnavailableError
        ? MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE
        : MANAGED_CREDENTIALS_UNAVAILABLE_MESSAGE);
      finishAction("google");
    }
  }

  async function submitPassword(): Promise<void> {
    if (!beginAction("password")) return;
    setPasswordError("");
    try {
      const next = await addManagedPassword({
        password: password(),
        confirmation: passwordConfirmation(),
      });
      setCredentials(next);
      clearPasswordSecrets();
      setPasswordError("");
      setPasswordFormOpen(false);
      queueMicrotask(() => {
        if (!disposed) passwordStatus?.focus();
      });
    } catch (error) {
      const focusTarget = error instanceof ManagedNewPasswordError
        && error.issue === "confirmation-mismatch"
        ? passwordConfirmationInput
        : passwordInput;
      clearPasswordSecrets();
      setPasswordError(error instanceof ManagedNewPasswordError
        ? error.message
        : MANAGED_PASSWORD_ADD_FAILED_MESSAGE);
      queueMicrotask(() => {
        if (!disposed && passwordFormOpen()) focusTarget?.focus();
      });
    } finally {
      finishAction("password");
    }
  }

  async function logout(): Promise<void> {
    if (!beginAction("logout")) return;
    setLogoutError("");
    try {
      await logoutManagedBrowser();
    } catch (error) {
      setLogoutError(error instanceof Error ? error.message : String(error));
      finishAction("logout");
    }
  }

  function devicesBusyChanged(next: boolean): void {
    if (next) {
      beginAction("devices");
    } else {
      finishAction("devices");
    }
  }

  onMount(() => void refreshCredentials());
  onCleanup(() => {
    disposed = true;
    credentialsRequest++;
    clearPasswordSecrets();
  });

  return (
    <div data-testid="settings-account-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Coordinator"
        supporting="This account has one dedicated coordinator."
      >
        <ListRow
          leading="dns"
          headline={<span data-testid="account-coordinator-hostname">{window.location.hostname}</span>}
          support="Current coordinator hostname"
          testId="account-coordinator"
        />
      </Card>

      <Card
        title="Sign-in methods"
        supporting="These methods sign in to this Roost account. Credential details stay only in this page while it is open."
      >
        <Show when={credentialsLoading()}>
          <p aria-live="polite" class="md-body-m" style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>
            Loading sign-in methods…
          </p>
        </Show>
        <Show when={!credentialsLoading() && credentialsError()}>
          <div style={{ display: "flex", "flex-direction": "column", "align-items": "flex-start", gap: "var(--md-space-3)" }}>
            <p role="alert" data-testid="account-credentials-error" class="md-body-m" style={{ margin: "0", color: "var(--md-sys-color-error)" }}>
              {credentialsError()}
            </p>
            <Button variant="tonal" disabled={busy()} onClick={() => void refreshCredentials()}>
              Try again
            </Button>
          </div>
        </Show>
        <Show when={!credentialsLoading() && !credentialsError() && credentials()}>
          {(current) => (
            <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-4)" }}>
              <List>
                <ListRow
                  leading="mail"
                  headline="Email"
                  support={<span data-testid="account-credential-email">{current().email}</span>}
                  testId="account-credential-email-row"
                />
                <ListRow
                  leading="account_circle"
                  headline="Google"
                  support={<span data-testid="account-google-status">{current().googleLinked ? "Connected" : "Not connected"}</span>}
                  testId="account-google-row"
                />
                <ListRow
                  leading="password"
                  headline="Password"
                  support={(
                    <span
                      ref={passwordStatus}
                      data-testid="account-password-status"
                      tabIndex={-1}
                    >
                      {current().hasPassword ? "Set" : "Not set"}
                    </span>
                  )}
                  testId="account-password-row"
                />
              </List>

              <Show when={!current().googleLinked}>
                <div style={{ display: "flex", "flex-direction": "column", "align-items": "flex-start", gap: "var(--md-space-3)" }}>
                  <Button
                    variant="tonal"
                    icon="add_link"
                    data-testid="account-connect-google"
                    disabled={busy()}
                    aria-busy={busyAction() === "google"}
                    onClick={() => void connectGoogle()}
                  >
                    {busyAction() === "google" ? "Connecting…" : "Connect Google"}
                  </Button>
                </div>
              </Show>

              <Show when={current().googleLinked && !current().hasPassword}>
                <Show
                  when={passwordFormOpen()}
                  fallback={
                    <Button
                      ref={(element) => { addPasswordTrigger = element; }}
                      variant="tonal"
                      icon="password"
                      data-testid="account-add-password"
                      disabled={busy()}
                      aria-controls="account-add-password-form"
                      aria-expanded="false"
                      onClick={openPasswordForm}
                    >
                      Add password
                    </Button>
                  }
                >
                  <div
                    data-testid="account-add-password-form"
                    id="account-add-password-form"
                    role="group"
                    aria-label="Add password"
                    style={{ display: "flex", "flex-direction": "column", "align-items": "flex-start", gap: "var(--md-space-3)" }}
                  >
                    <TextField
                      ref={(element) => { passwordInput = element; }}
                      type="password"
                      label="New password"
                      testId="account-new-password"
                      value={password()}
                      onInput={setPassword}
                      disabled={busy()}
                      ariaDescribedBy="account-password-hint account-password-error"
                      onKeyDown={(event) => {
                        if (event.key === "Enter") void submitPassword();
                        if (event.key === "Escape" && !busy()) {
                          event.preventDefault();
                          closePasswordForm();
                        }
                      }}
                    />
                    <TextField
                      ref={(element) => { passwordConfirmationInput = element; }}
                      type="password"
                      label="Confirm password"
                      testId="account-confirm-password"
                      value={passwordConfirmation()}
                      onInput={setPasswordConfirmation}
                      disabled={busy()}
                      ariaDescribedBy="account-password-error"
                      onKeyDown={(event) => {
                        if (event.key === "Enter") void submitPassword();
                        if (event.key === "Escape" && !busy()) {
                          event.preventDefault();
                          closePasswordForm();
                        }
                      }}
                    />
                    <p id="account-password-hint" class="md-body-s" style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>
                      Use 12–1,024 characters.
                    </p>
                    <Show when={passwordError()}>
                      <p id="account-password-error" role="alert" data-testid="account-password-error" class="md-body-m" style={{ margin: "0", color: "var(--md-sys-color-error)" }}>
                        {passwordError()}
                      </p>
                    </Show>
                    <div style={{ display: "flex", gap: "var(--md-space-2)" }}>
                      <Button
                        variant="filled"
                        data-testid="account-add-password-submit"
                        disabled={busy()}
                        aria-busy={busyAction() === "password"}
                        onClick={() => void submitPassword()}
                      >
                        {busyAction() === "password" ? "Adding…" : "Add password"}
                      </Button>
                      <Button
                        variant="text"
                        data-testid="account-add-password-cancel"
                        disabled={busy()}
                        onClick={closePasswordForm}
                      >
                        Cancel
                      </Button>
                    </div>
                  </div>
                </Show>
              </Show>

              <Show when={methodError()}>
                <p role="alert" data-testid="account-method-error" class="md-body-m" style={{ margin: "0", color: "var(--md-sys-color-error)" }}>
                  {methodError()}
                </p>
              </Show>
            </div>
          )}
        </Show>
      </Card>

      <AuthorizedDevicesCard
        accountScoped
        disabled={busy()}
        onBusyChange={devicesBusyChanged}
      />

      <Card
        title="Browser session"
        supporting="Signing out revokes this browser on the coordinator before removing its local account data."
      >
        <div style={{ display: "flex", "flex-direction": "column", "align-items": "flex-start", gap: "var(--md-space-3)" }}>
          <Button
            variant="tonal"
            icon="logout"
            data-testid="managed-sign-out"
            disabled={busy()}
            aria-busy={busyAction() === "logout"}
            onClick={() => void logout()}
          >
            {busyAction() === "logout" ? "Signing out…" : "Sign out this browser"}
          </Button>
          <Show when={logoutError()}>
            <p
              role="alert"
              data-testid="managed-sign-out-error"
              class="md-body-m"
              style={{ margin: "0", color: "var(--md-sys-color-error)" }}
            >
              {logoutError()}
            </p>
          </Show>
        </div>
      </Card>
    </div>
  );
}
