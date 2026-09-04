// This module owns managed owner activation and password-reset credential transactions.
// Managed account screens call it to validate passwords, bind browser keys, and finish safe resets.
// It depends on tenant routing, coordinator clients, dashboard confirmation, and logout cleanup.

import { Code, ConnectError } from "@connectrpc/connect";
import {
  isNativePasswordLengthValid,
  NATIVE_PASSWORD_MAX_LENGTH,
  NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH,
} from "@roost/shared/native-credentials";
import { isTenantRouteKey } from "@roost/shared/tenant-route";
import { makeCoordinatorClientForSigner, makePublicCoordinatorClient } from "../connect.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";
import { ROUTES } from "../routes.ts";
import { selectDashboardFromServerWithClient } from "../store/dashboard-selection.ts";
import { hasConfirmedDashboardAccess, rootStore } from "../store/root.ts";
import { resumeBootstrapAfterDeviceAuthorization } from "../store/sync-bootstrap.ts";
import { clearCapturedFragmentCredential } from "./fragment-credential.ts";
import {
  clearAccountSensitiveStateForLogout,
  prepareManagedTenantRouteSwitch,
} from "./managed-logout.ts";
import {
  clearWebKeyMaterialForLogout,
  getPublicKeyB64,
  markCurrentWebKeyAuthorized,
  signCoordinatorJwt,
} from "./web-key.ts";
import {
  commitTenantRouteKey,
  resolveTenantRouteKey,
  tenantCoordinatorBaseUrl,
} from "./tenant-routing.ts";

export type ManagedNewPasswordIssue = "too-short" | "too-long" | "confirmation-mismatch";
type ManagedCredentialKind = "activation" | "reset";

export const MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT =
  "If this account can use password reset, a link is on its way.";
export const MANAGED_ACTIVATION_DENIED_MESSAGE =
  "This activation link is invalid or has expired. Ask the Roost operator for a new link.";
export const MANAGED_RESET_DENIED_MESSAGE =
  "This password reset link is invalid or has expired. Request a new link.";
export const MANAGED_ACCOUNT_CONNECTION_ERROR =
  "Roost couldn’t be reached. Check your connection and try again.";
export const MANAGED_ACTIVATION_SCOPE_ERROR =
  "Your account was activated, but Roost couldn’t confirm this coordinator. Check your connection and try again.";

export function managedNewPasswordIssue(
  password: string,
  confirmation: string,
): ManagedNewPasswordIssue | null {
  if (password.length < NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH) return "too-short";
  if (!isNativePasswordLengthValid(password, NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH)) return "too-long";
  if (password !== confirmation) return "confirmation-mismatch";
  return null;
}

export function managedNewPasswordIssueMessage(issue: ManagedNewPasswordIssue): string {
  switch (issue) {
    case "too-short":
      return `Use at least ${NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH} characters.`;
    case "too-long":
      return `Use no more than ${NATIVE_PASSWORD_MAX_LENGTH} characters.`;
    case "confirmation-mismatch":
      return "The passwords don’t match.";
  }
}

export class ManagedNewPasswordError extends Error {
  constructor(readonly issue: ManagedNewPasswordIssue) {
    super(managedNewPasswordIssueMessage(issue));
    this.name = "ManagedNewPasswordError";
  }
}

export class ManagedActivationScopeError extends Error {
  constructor() {
    super("owner activation returned no confirmed coordinator dashboard");
    this.name = "ManagedActivationScopeError";
  }
}

export class ManagedCredentialDeniedError extends Error {
  constructor(readonly kind: "activation" | "reset") {
    super(`${kind} credential was denied`);
    this.name = "ManagedCredentialDeniedError";
  }
}

export function isAuthoritativeCredentialDenial(error: unknown): boolean {
  return error instanceof ManagedCredentialDeniedError
    || (error instanceof ConnectError && error.code === Code.PermissionDenied);
}

function assertManagedNewPassword(password: string, confirmation: string): void {
  const issue = managedNewPasswordIssue(password, confirmation);
  if (issue) throw new ManagedNewPasswordError(issue);
}

interface OwnerActivationRequest {
  token: string;
  newPassword: string;
  sshPubkeyB64: string;
  label: string;
}

export interface ManagedOwnerActivationDependencies {
  publicKeyB64: () => Promise<string>;
  activateOwner: (
    routeKey: string,
    request: OwnerActivationRequest,
  ) => Promise<{ dashboardId: string }>;
  browserLabel: () => string;
  confirmDashboard: (routeKey: string, dashboardId: string) => Promise<boolean>;
  confirmedDashboardId: () => string | null;
  markKeyAuthorized: () => void;
  resumeBootstrap: () => void;
  clearCredential: (kind: ManagedCredentialKind) => boolean;
  replaceLocation: (path: string) => void;
}

const ownerActivationDependencies: ManagedOwnerActivationDependencies = {
  publicKeyB64: getPublicKeyB64,
  activateOwner: (routeKey, request) =>
    makePublicCoordinatorClient(routeKey).authOwnerActivate(request),
  browserLabel: browserSelfLabel,
  confirmDashboard: (routeKey, dashboardId) =>
    selectDashboardFromServerWithClient(
      makeCoordinatorClientForSigner(signCoordinatorJwt, tenantCoordinatorBaseUrl(routeKey)),
      dashboardId,
    ),
  confirmedDashboardId: () => hasConfirmedDashboardAccess() ? rootStore.selected_dashboard_id : null,
  markKeyAuthorized: markCurrentWebKeyAuthorized,
  resumeBootstrap: resumeBootstrapAfterDeviceAuthorization,
  clearCredential: clearCapturedFragmentCredential,
  replaceLocation: (path) => location.replace(path),
};

/** Bind the coordinator-origin non-extractable key, then cross the ordinary
 * signed dashboard boundary before treating the owner browser as authorized. */
export async function activateManagedOwner(
  input: { routeKey: string; token: string; password: string; confirmation: string },
  dependencies: ManagedOwnerActivationDependencies = ownerActivationDependencies,
): Promise<{ dashboardId: string }> {
  assertManagedNewPassword(input.password, input.confirmation);
  if (!input.token) {
    dependencies.clearCredential("activation");
    throw new ManagedCredentialDeniedError("activation");
  }

  const sshPubkeyB64 = await dependencies.publicKeyB64();
  let response: { dashboardId: string };
  try {
    response = await dependencies.activateOwner(input.routeKey, {
      token: input.token,
      newPassword: input.password,
      sshPubkeyB64,
      label: dependencies.browserLabel(),
    });
  } catch (error) {
    if (isAuthoritativeCredentialDenial(error)) dependencies.clearCredential("activation");
    throw error;
  }

  const dashboardId = response.dashboardId.trim();
  if (!dashboardId) throw new ManagedActivationScopeError();
  let confirmed = false;
  try {
    confirmed = await dependencies.confirmDashboard(input.routeKey, dashboardId);
  } catch {
    // Activation committed, but device-scoped dashboard authority is not yet
    // proven. This is not evidence that the captured credential was denied.
    throw new ManagedActivationScopeError();
  }
  if (!confirmed || dependencies.confirmedDashboardId() !== dashboardId) {
    throw new ManagedActivationScopeError();
  }

  dependencies.markKeyAuthorized();
  dependencies.resumeBootstrap();
  dependencies.clearCredential("activation");
  dependencies.replaceLocation(ROUTES.APP);
  return { dashboardId };
}

interface PasswordResetStartRequest {
  email: string;
}

export interface ManagedPasswordResetRequestDependencies {
  resolveRouteKey: (email: string) => Promise<string>;
  prepareRouteSwitch: (routeKey: string) => Promise<unknown>;
  requestReset: (
    routeKey: string,
    request: PasswordResetStartRequest,
  ) => Promise<unknown>;
  persistRouteKey: (routeKey: string) => boolean;
  clearStaleCredential: () => void;
}

const passwordResetRequestDependencies: ManagedPasswordResetRequestDependencies = {
  resolveRouteKey: resolveTenantRouteKey,
  prepareRouteSwitch: prepareManagedTenantRouteSwitch,
  requestReset: (routeKey, request) =>
    makePublicCoordinatorClient(routeKey).authPasswordResetRequest(request),
  persistRouteKey: commitTenantRouteKey,
  clearStaleCredential: () => {
    clearCapturedFragmentCredential("activation");
    clearCapturedFragmentCredential("reset");
  },
};

/** The public response is deliberately identical for unknown accounts, rate
 * limits, and delivery failures. The coordinator owns any retryable delivery. */
export async function requestManagedPasswordReset(
  email: string,
  dependencies: ManagedPasswordResetRequestDependencies = passwordResetRequestDependencies,
  sharedManaged = true,
): Promise<string> {
  if (!sharedManaged) {
    try {
      await makePublicCoordinatorClient().authPasswordResetRequest({ email: email.trim() });
    } catch {
      // Reset acknowledgement is deliberately uniform.
    }
    return MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT;
  }
  const normalizedEmail = email.trim();
  try {
    const routeKey = await dependencies.resolveRouteKey(normalizedEmail);
    if (!isTenantRouteKey(routeKey)) throw new TypeError("invalid account route");
    await dependencies.prepareRouteSwitch(routeKey);
    dependencies.clearStaleCredential();
    try {
      await dependencies.requestReset(routeKey, { email: normalizedEmail });
    } finally {
      // A rejection is intentionally indistinguishable, but the call still
      // targeted this route and future reloads must preserve that selection.
      if (!dependencies.persistRouteKey(routeKey)) {
        throw new TypeError("Unable to persist the account route");
      }
    }
  } catch {
    // Account existence, routing, and outbox state are deliberately uniform.
  }
  return MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT;
}

interface PasswordResetRedeemRequest {
  token: string;
  newPassword: string;
}

export interface ManagedPasswordResetRedeemDependencies {
  redeemReset: (
    routeKey: string | undefined,
    request: PasswordResetRedeemRequest,
  ) => Promise<{ ok: boolean }>;
  clearClientState: () => void;
  clearWebKeyMaterial: () => Promise<void>;
  clearCredential: (kind: ManagedCredentialKind) => boolean;
  replaceLocation: (path: string) => void;
}

const passwordResetRedeemDependencies: ManagedPasswordResetRedeemDependencies = {
  redeemReset: (routeKey, request) =>
    makePublicCoordinatorClient(routeKey).authPasswordResetRedeem(request),
  clearClientState: clearAccountSensitiveStateForLogout,
  clearWebKeyMaterial: clearWebKeyMaterialForLogout,
  clearCredential: clearCapturedFragmentCredential,
  replaceLocation: (path) => location.replace(path),
};

/** Destroy browser authority only after the coordinator confirms that the reset
 * transaction committed and revoked every account device. */
export async function redeemManagedPasswordReset(
  input: { routeKey?: string; token: string; password: string; confirmation: string },
  dependencies: ManagedPasswordResetRedeemDependencies = passwordResetRedeemDependencies,
): Promise<void> {
  assertManagedNewPassword(input.password, input.confirmation);
  if (!input.token) {
    dependencies.clearCredential("reset");
    throw new ManagedCredentialDeniedError("reset");
  }

  let response: { ok: boolean };
  try {
    response = await dependencies.redeemReset(input.routeKey, {
      token: input.token,
      newPassword: input.password,
    });
  } catch (error) {
    if (isAuthoritativeCredentialDenial(error)) dependencies.clearCredential("reset");
    throw error;
  }
  if (!response.ok) {
    dependencies.clearCredential("reset");
    throw new ManagedCredentialDeniedError("reset");
  }

  dependencies.clearCredential("reset");
  dependencies.clearClientState();
  await dependencies.clearWebKeyMaterial();
  dependencies.replaceLocation(ROUTES.LOGIN);
}

export function managedActivationErrorMessage(error: unknown): string {
  if (error instanceof ManagedNewPasswordError) return error.message;
  if (error instanceof ManagedActivationScopeError) return MANAGED_ACTIVATION_SCOPE_ERROR;
  if (isAuthoritativeCredentialDenial(error)) return MANAGED_ACTIVATION_DENIED_MESSAGE;
  return MANAGED_ACCOUNT_CONNECTION_ERROR;
}

export function managedResetErrorMessage(error: unknown): string {
  if (error instanceof ManagedNewPasswordError) return error.message;
  if (isAuthoritativeCredentialDenial(error)) return MANAGED_RESET_DENIED_MESSAGE;
  return MANAGED_ACCOUNT_CONNECTION_ERROR;
}
