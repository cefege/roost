// This module owns the browser-binding step after managed Google authentication returns.
// The completion screen calls it with a scrubbed gateway assertion before entering the app.
// It depends on gateway binding, tenant cleanup, coordinator confirmation, and the web key.
// Same-tab progress keeps retries safe without treating an unconfirmed dashboard as authorized.

import { isTenantRouteKey } from "@roost/shared/tenant-route";
import {
  makeCoordinatorClientForSigner,
  makePublicCoordinatorClient,
} from "../connect.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";
import { ROUTES } from "../routes.ts";
import {
  confirmDashboardAccessWithClient,
  rememberDashboardSelectionHint,
} from "../store/dashboard-selection.ts";
import { resumeBootstrapAfterDeviceAuthorization } from "../store/sync-bootstrap.ts";
import { clearCapturedFragmentCredential } from "./fragment-credential.ts";
import { bindManagedGoogleDevice } from "./managed-auth-gateway.ts";
import { prepareManagedTenantRouteSwitch } from "./managed-logout.ts";
import {
  commitTenantRouteKey,
  tenantCoordinatorBaseUrl,
} from "./tenant-routing.ts";
import {
  getPublicKeyB64,
  markCurrentWebKeyAuthorized,
  signCoordinatorJwt,
} from "./web-key.ts";
import {
  clearManagedGoogleCompletionProgress,
  managedGoogleCleanupCompleted,
  recordManagedGoogleCleanup,
} from "./managed-auth-progress.ts";


interface FederatedContinueRequest {
  assertion: string;
  sshPubkeyB64: string;
  label: string;
}

export interface ManagedGoogleCompletionDependencies {
  cleanupPreviousTenant: (routeKey: string) => Promise<unknown>;
  cleanupAlreadyCompleted: (routeKey: string) => boolean;
  recordCleanupCompleted: (routeKey: string) => void;
  clearCompletionProgress: () => void;
  publicKeyB64: () => Promise<string>;
  bindDevice: (sshPubkeyB64: string) => Promise<{ routeKey: string; assertion: string }>;
  federatedContinue: (
    routeKey: string,
    request: FederatedContinueRequest,
  ) => Promise<{ dashboardId: string }>;
  confirmDashboard: (routeKey: string, dashboardId: string) => Promise<boolean>;
  commitRoute: (routeKey: string) => boolean;
  browserLabel: () => string;
  rememberDashboardHint: (dashboardId: string) => void;
  markKeyAuthorized: () => void;
  resumeBootstrap: () => void;
  clearStaleCredentials: () => void;
  replaceLocation: (path: string) => void;
}

const defaultDependencies: ManagedGoogleCompletionDependencies = {
  cleanupPreviousTenant: prepareManagedTenantRouteSwitch,
  cleanupAlreadyCompleted: managedGoogleCleanupCompleted,
  recordCleanupCompleted: recordManagedGoogleCleanup,
  clearCompletionProgress: clearManagedGoogleCompletionProgress,
  publicKeyB64: getPublicKeyB64,
  bindDevice: bindManagedGoogleDevice,
  federatedContinue: (routeKey, request) =>
    makePublicCoordinatorClient(routeKey).authFederatedContinue(request),
  confirmDashboard: (routeKey, dashboardId) =>
    confirmDashboardAccessWithClient(
      makeCoordinatorClientForSigner(
        signCoordinatorJwt,
        tenantCoordinatorBaseUrl(routeKey),
      ),
      dashboardId,
    ),
  commitRoute: commitTenantRouteKey,
  browserLabel: browserSelfLabel,
  rememberDashboardHint: rememberDashboardSelectionHint,
  markKeyAuthorized: markCurrentWebKeyAuthorized,
  resumeBootstrap: resumeBootstrapAfterDeviceAuthorization,
  clearStaleCredentials: () => {
    clearCapturedFragmentCredential("activation");
    clearCapturedFragmentCredential("reset");
    clearCapturedFragmentCredential("email-signup");
  },
  replaceLocation: (path) => location.replace(path),
};

export class ManagedGoogleCompletionError extends Error {
  constructor() {
    super("managed Google authentication could not be confirmed");
    this.name = "ManagedGoogleCompletionError";
  }
}

/**
 * Finish a Google login/signup without selecting the returned tenant until the
 * prefixed federated RPC and its signed, exact dashboard proof both succeed.
 * The assertion is kept only on this stack; the gateway's HttpOnly receipt
 * provides reload/replay recovery.
 */
export async function completeManagedGoogleAuthentication(
  input: { routeKey: string; assertion?: string },
  dependencies: ManagedGoogleCompletionDependencies = defaultDependencies,
): Promise<{ dashboardId: string }> {
  if (!isTenantRouteKey(input.routeKey)) throw new ManagedGoogleCompletionError();

  if (!dependencies.cleanupAlreadyCompleted(input.routeKey)) {
    await dependencies.cleanupPreviousTenant(input.routeKey);
    dependencies.recordCleanupCompleted(input.routeKey);
  }

  const sshPubkeyB64 = await dependencies.publicKeyB64();
  let assertion = input.assertion;
  if (!assertion) {
    const bound = await dependencies.bindDevice(sshPubkeyB64);
    if (bound.routeKey !== input.routeKey) throw new ManagedGoogleCompletionError();
    assertion = bound.assertion;
  }
  if (!assertion) throw new ManagedGoogleCompletionError();

  const response = await dependencies.federatedContinue(input.routeKey, {
    assertion,
    sshPubkeyB64,
    label: dependencies.browserLabel(),
  });
  const dashboardId = response.dashboardId.trim();
  if (!dashboardId || !await dependencies.confirmDashboard(input.routeKey, dashboardId)) {
    throw new ManagedGoogleCompletionError();
  }
  if (!dependencies.commitRoute(input.routeKey)) throw new ManagedGoogleCompletionError();

  dependencies.rememberDashboardHint(dashboardId);
  dependencies.markKeyAuthorized();
  dependencies.resumeBootstrap();
  dependencies.clearStaleCredentials();
  dependencies.clearCompletionProgress();
  dependencies.replaceLocation(ROUTES.APP);
  return { dashboardId };
}
