// This module owns completion of emailed managed-account signup credentials.
// The verification screen calls it after entry has captured and scrubbed the fragment token.
// It depends on tenant cleanup, coordinator activation, dashboard confirmation, and the web key.
// Same-tab progress makes retries safe without repeating destructive tenant cleanup.

import { isTenantRouteKey } from "@roost/shared/tenant-route";
import {
  makeCoordinatorClientForSigner,
  makePublicCoordinatorClient,
} from "../connect.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";
import { ROUTES } from "../routes.ts";
import { confirmDashboardAccessWithClient } from "../store/dashboard-selection.ts";
import { resumeBootstrapAfterDeviceAuthorization } from "../store/sync-bootstrap.ts";
import {
  ManagedActivationScopeError,
  ManagedCredentialDeniedError,
  ManagedNewPasswordError,
  managedNewPasswordIssue,
  isAuthoritativeCredentialDenial,
} from "./managed-account.ts";
import { clearCapturedFragmentCredential } from "./fragment-credential.ts";
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
  clearManagedEmailSignupActivationProgress,
  managedEmailSignupCleanupCompleted,
  recordManagedEmailSignupCleanup,
} from "./managed-auth-progress.ts";


interface OwnerActivationRequest {
  token: string;
  newPassword: string;
  sshPubkeyB64: string;
  label: string;
}

export interface ManagedEmailSignupActivationDependencies {
  cleanupPreviousTenant: (routeKey: string) => Promise<unknown>;
  cleanupAlreadyCompleted: (routeKey: string) => boolean;
  recordCleanupCompleted: (routeKey: string) => void;
  publicKeyB64: () => Promise<string>;
  activateOwner: (
    routeKey: string,
    request: OwnerActivationRequest,
  ) => Promise<{ dashboardId: string }>;
  browserLabel: () => string;
  confirmDashboard: (routeKey: string, dashboardId: string) => Promise<boolean>;
  commitRoute: (routeKey: string) => boolean;
  markKeyAuthorized: () => void;
  resumeBootstrap: () => void;
  clearCredential: () => void;
  clearProgress: () => void;
  replaceLocation: (path: string) => void;
}


const defaultDependencies: ManagedEmailSignupActivationDependencies = {
  cleanupPreviousTenant: prepareManagedTenantRouteSwitch,
  cleanupAlreadyCompleted: managedEmailSignupCleanupCompleted,
  recordCleanupCompleted: recordManagedEmailSignupCleanup,
  publicKeyB64: getPublicKeyB64,
  activateOwner: (routeKey, request) =>
    makePublicCoordinatorClient(routeKey).authOwnerActivate(request),
  browserLabel: browserSelfLabel,
  confirmDashboard: (routeKey, dashboardId) =>
    confirmDashboardAccessWithClient(
      makeCoordinatorClientForSigner(
        signCoordinatorJwt,
        tenantCoordinatorBaseUrl(routeKey),
      ),
      dashboardId,
    ),
  commitRoute: commitTenantRouteKey,
  markKeyAuthorized: markCurrentWebKeyAuthorized,
  resumeBootstrap: resumeBootstrapAfterDeviceAuthorization,
  clearCredential: () => { clearCapturedFragmentCredential("email-signup"); },
  clearProgress: clearManagedEmailSignupActivationProgress,
  replaceLocation: (path) => location.replace(path),
};

export async function activateManagedEmailSignup(
  input: {
    routeKey: string;
    token: string;
    password: string;
    confirmation: string;
  },
  dependencies: ManagedEmailSignupActivationDependencies = defaultDependencies,
): Promise<{ dashboardId: string }> {
  const issue = managedNewPasswordIssue(input.password, input.confirmation);
  if (issue) throw new ManagedNewPasswordError(issue);
  if (!isTenantRouteKey(input.routeKey) || !input.token) {
    dependencies.clearCredential();
    throw new ManagedCredentialDeniedError("activation");
  }

  if (!dependencies.cleanupAlreadyCompleted(input.routeKey)) {
    await dependencies.cleanupPreviousTenant(input.routeKey);
    dependencies.recordCleanupCompleted(input.routeKey);
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
    if (isAuthoritativeCredentialDenial(error)) {
      dependencies.clearCredential();
      dependencies.clearProgress();
    }
    throw error;
  }

  const dashboardId = response.dashboardId.trim();
  if (
    !dashboardId
    || !await dependencies.confirmDashboard(input.routeKey, dashboardId)
  ) {
    throw new ManagedActivationScopeError();
  }
  if (!dependencies.commitRoute(input.routeKey)) throw new ManagedActivationScopeError();

  dependencies.markKeyAuthorized();
  dependencies.resumeBootstrap();
  dependencies.clearCredential();
  dependencies.clearProgress();
  dependencies.replaceLocation(ROUTES.APP);
  return { dashboardId };
}
