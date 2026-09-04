// This module owns email and password login for a tenant-routed managed coordinator.
// The login screen calls it to resolve a route, bind this browser key, and confirm dashboard access.
// It depends on tenant cleanup, public and signed coordinator clients, and the browser identity.
// Authorization is recorded only after the returned dashboard succeeds on the resolved coordinator.

import { Code, ConnectError } from "@connectrpc/connect";
import { isTenantRouteKey } from "@roost/shared/tenant-route";
import {
  makeCoordinatorClientForSigner,
  makePublicCoordinatorClient,
} from "../connect.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";
import {
  confirmDashboardAccessWithClient,
  rememberDashboardSelectionHint,
} from "../store/dashboard-selection.ts";
import { ROUTES } from "../routes.ts";
import {
  clearCapturedFragmentCredential,
} from "./fragment-credential.ts";
import {
  prepareManagedTenantRouteSwitch,
} from "./managed-logout.ts";
import {
  commitTenantRouteKey,
  resolveTenantRouteKey,
  tenantCoordinatorBaseUrl,
} from "./tenant-routing.ts";
import {
  getPublicKeyB64,
  markCurrentWebKeyAuthorized,
  signCoordinatorJwt,
} from "./web-key.ts";

export const GENERIC_CREDENTIAL_ERROR = "Email or password is incorrect.";
export const MANAGED_LOGIN_CONNECTION_ERROR = "Roost couldn’t be reached. Check your connection and try again.";
export const MANAGED_LOGIN_SCOPE_ERROR = "Your account doesn’t have an active dashboard. Contact your Roost administrator.";

interface PasswordLoginRequest {
  email: string;
  password: string;
  sshPubkeyB64: string;
  label: string;
}

export interface ManagedLoginDependencies {
  resolveRouteKey: (email: string) => Promise<string>;
  prepareRouteSwitch: (routeKey: string) => Promise<unknown>;
  passwordLogin: (
    routeKey: string,
    request: PasswordLoginRequest,
  ) => Promise<{ dashboardId: string }>;
  confirmDashboard: (routeKey: string, dashboardId: string) => Promise<boolean>;
  persistRouteKey: (routeKey: string) => boolean;
  publicKeyB64: () => Promise<string>;
  browserLabel: () => string;
  rememberDashboardHint: (dashboardId: string) => void;
  markKeyAuthorized: () => void;
  clearStaleCredential: () => void;
  replaceLocation: (path: string) => void;
}

const defaultDependencies: ManagedLoginDependencies = {
  resolveRouteKey: resolveTenantRouteKey,
  prepareRouteSwitch: prepareManagedTenantRouteSwitch,
  passwordLogin: (routeKey, request) =>
    makePublicCoordinatorClient(routeKey).authPasswordLogin(request),
  confirmDashboard: (routeKey, dashboardId) =>
    confirmDashboardAccessWithClient(
      makeCoordinatorClientForSigner(
        signCoordinatorJwt,
        tenantCoordinatorBaseUrl(routeKey),
      ),
      dashboardId,
    ),
  persistRouteKey: commitTenantRouteKey,
  publicKeyB64: getPublicKeyB64,
  browserLabel: browserSelfLabel,
  rememberDashboardHint: rememberDashboardSelectionHint,
  markKeyAuthorized: markCurrentWebKeyAuthorized,
  clearStaleCredential: () => {
    clearCapturedFragmentCredential("activation");
    clearCapturedFragmentCredential("reset");
  },
  replaceLocation: (path) => location.replace(path),
};

export class ManagedLoginScopeError extends Error {
  constructor() {
    super("managed login returned no confirmed dashboard");
    this.name = "ManagedLoginScopeError";
  }
}

/** Resolve the account's routing-only key, destroy any previous tenant
 * authority, and bind a fresh/existing non-extractable key through a one-off
 * prefixed public client. An explicit prefixed signed client must then confirm
 * the exact returned dashboard before authorization and full navigation. */
export async function loginManagedBrowser(
  input: { email: string; password: string },
  dependencies: ManagedLoginDependencies = defaultDependencies,
): Promise<{ dashboardId: string }> {
  const email = input.email.trim();
  if (!email || !input.password) throw new ConnectError(GENERIC_CREDENTIAL_ERROR, Code.Unauthenticated);

  const routeKey = await dependencies.resolveRouteKey(email);
  if (!isTenantRouteKey(routeKey)) throw new TypeError("invalid account route");
  await dependencies.prepareRouteSwitch(routeKey);
  dependencies.clearStaleCredential();

  const sshPubkeyB64 = await dependencies.publicKeyB64();
  let response: { dashboardId: string } | undefined;
  let requestFailed = false;
  let requestError: unknown;
  try {
    response = await dependencies.passwordLogin(routeKey, {
      email,
      password: input.password,
      sshPubkeyB64,
      label: dependencies.browserLabel(),
    });
  } catch (error) {
    requestError = error;
    requestFailed = true;
  }

  // Commit only after the public RPC was actually invoked against this route.
  if (!dependencies.persistRouteKey(routeKey)) {
    throw new TypeError("Unable to persist the account route");
  }
  if (requestFailed) throw requestError;

  const dashboardId = response?.dashboardId.trim() ?? "";
  if (!dashboardId) throw new ManagedLoginScopeError();
  if (!await dependencies.confirmDashboard(routeKey, dashboardId)) {
    throw new ManagedLoginScopeError();
  }
  dependencies.rememberDashboardHint(dashboardId);
  dependencies.markKeyAuthorized();
  dependencies.replaceLocation(ROUTES.APP);
  return { dashboardId };
}

export function managedLoginErrorMessage(error: unknown): string {
  if (error instanceof ManagedLoginScopeError) return MANAGED_LOGIN_SCOPE_ERROR;
  if (error instanceof TypeError) return MANAGED_LOGIN_CONNECTION_ERROR;
  if (error instanceof ConnectError) {
    if (
      error.code === Code.Unavailable
      || error.code === Code.DeadlineExceeded
      || error.code === Code.Aborted
      || error.code === Code.Internal
      || error.code === Code.Unknown
    ) {
      return MANAGED_LOGIN_CONNECTION_ERROR;
    }
  }
  return GENERIC_CREDENTIAL_ERROR;
}
