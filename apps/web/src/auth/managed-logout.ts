// This module owns managed logout and tenant-switch destruction of browser authority and state.
// Login, activation, reset, and account settings call it before another identity can be used.
// It depends on coordinator revocation probes and every account-scoped web store that must be cleared.
// Explicit coordinator rejection gates key deletion when a logout response is ambiguous.

import {
  coordClient,
  invalidateFixedCoordinatorClientForTenantRouteSwitch,
  makeCoordinatorClientForSigner,
} from "../connect.ts";
import {
  clearWebKeyMaterialForLogout,
  probeCurrentWebKey,
  signCoordinatorJwt,
  type WebKeyProbeResult,
} from "./web-key.ts";
import { clearManagedAuthCeremoniesForLogout } from "./managed-auth-session.ts";
import { clearCoordinatorRelocationRuntimeForLogout } from "./coordinator-relocation.ts";
import { unsubscribeFromPush } from "../lib/push-client.ts";
import {
  broadcastTenantRouteSwitch,
  clearPendingTenantRouteSwitch,
  pendingTenantRouteSwitch,
  storedTenantRouteKey,
  tenantCoordinatorBaseUrl,
} from "./tenant-routing.ts";
import { clearNotificationStateForLogout } from "../lib/notifyPrefs.ts";
import { invalidateDeepgramKey } from "../lib/deepgramKey.ts";
import { clearKeytermLexiconForLogout } from "../lib/keytermLexicon.ts";
import { clearInputHistoryForLogout } from "../lib/terminalInputHistory.ts";
import { clearComposerDraftsForLogout } from "../lib/composerDrafts.ts";
import { clearLastVisitedForLogout } from "../lib/lastVisited.ts";
import { clearLastWorkspaceForLogout } from "../lib/lastWorkspace.ts";
import { clearSidebarRecentForLogout } from "../lib/sidebarRecent.ts";
import { clearAgentSeenForLogout } from "../lib/agentSeen.ts";
import { clearAgentNotificationClaimsForAccountBoundary } from "../lib/agentNotificationClaim.ts";
import { closeCmdPalette, closeHelp } from "../lib/keyboardShortcuts.ts";
import { closeTransferDialog } from "../lib/transferDialog.ts";
import { clearVoiceStateForLogout } from "../lib/voiceState.ts";
import { clearAccountScopedClientStateForLogout } from "../store/dashboard-selection.ts";
import { clearPaneLayoutsForLogout } from "../store/paneLayoutStore.ts";
import { clearSpotlight } from "../store/spotlight.ts";
import { closeRenameDialog } from "../store/renameDialog.ts";
import { clearQueueTaskDialogForLogout } from "../store/queueTaskDialog.ts";
import { clearTransfersForLogout } from "../store/transfers.ts";
import { suspendSyncForTenantRouteSwitch } from "../store/sync.ts";

export const MANAGED_LOGOUT_UNCONFIRMED_MESSAGE =
  "Roost couldn’t confirm that this browser was signed out. Check your connection and try again.";

export interface ManagedLogoutDependencies {
  authLogout: () => Promise<{ ok: boolean }>;
  probeCurrentDevice: () => Promise<WebKeyProbeResult>;
  unsubscribePush: () => Promise<void>;
  clearClientState: () => void;
  clearWebKeyMaterial: () => Promise<void>;
  replaceLocation: (path: string) => void;
}

/** Clear every account-derived in-memory and persisted browser state. Browser
 * appearance and input preferences deliberately survive; account identifiers,
 * terminal content, dictation vocabulary, paths, and selection hints do not. */
export function clearAccountSensitiveStateForLogout(): void {
  clearAccountScopedClientStateForLogout();
  clearComposerDraftsForLogout();
  clearLastVisitedForLogout();
  clearLastWorkspaceForLogout();
  clearSidebarRecentForLogout();
  clearPaneLayoutsForLogout();
  clearAgentSeenForLogout();
  clearTransfersForLogout();
  clearNotificationStateForLogout();
  clearAgentNotificationClaimsForAccountBoundary();
  invalidateDeepgramKey();
  clearKeytermLexiconForLogout();
  clearInputHistoryForLogout();
  clearVoiceStateForLogout();
  clearSpotlight();
  closeRenameDialog();
  closeTransferDialog();
  clearQueueTaskDialogForLogout();
  closeCmdPalette();
  closeHelp();
  try { document.getSelection()?.removeAllRanges(); } catch { /* no document */ }
}

/** Explicit logout additionally discards per-tab proofs and ceremony progress.
 * Tenant switching deliberately uses the account-only cleanup above so the
 * credential driving that switch survives until its destination confirms it. */
export function clearManagedBrowserStateForLogout(): void {
  clearAccountSensitiveStateForLogout();
  clearManagedAuthCeremoniesForLogout();
  clearCoordinatorRelocationRuntimeForLogout();
}

const defaultDependencies: ManagedLogoutDependencies = {
  authLogout: () => coordClient.authLogout({}),
  probeCurrentDevice: () => probeCurrentWebKey("managed"),
  unsubscribePush: () => unsubscribeFromPush({ waitForRegistration: false }),
  clearClientState: clearManagedBrowserStateForLogout,
  clearWebKeyMaterial: clearWebKeyMaterialForLogout,
  replaceLocation: (path) => location.replace(path),
};
export interface ManagedTenantSwitchDependencies {
  unsubscribePreviousPush: (previousRouteKey: string | null) => Promise<void>;
  revokePreviousDevice: (previousRouteKey: string | null) => Promise<void>;
  clearClientState: () => void;
  clearWebKeyMaterial: () => Promise<void>;
}


const managedTenantSwitchDependencies: ManagedTenantSwitchDependencies = {
  revokePreviousDevice: async (previousRouteKey) => {
    const baseUrl = previousRouteKey
      ? tenantCoordinatorBaseUrl(previousRouteKey)
      : typeof location === "undefined" ? "http://localhost" : location.origin;
    const previousClient = makeCoordinatorClientForSigner(signCoordinatorJwt, baseUrl);
    await previousClient.authLogout({});
  },
  unsubscribePreviousPush: async (previousRouteKey) => {
    const baseUrl = previousRouteKey
      ? tenantCoordinatorBaseUrl(previousRouteKey)
      : typeof location === "undefined" ? "http://localhost" : location.origin;
    const previousClient = makeCoordinatorClientForSigner(signCoordinatorJwt, baseUrl);
    await unsubscribeFromPush({
      waitForRegistration: false,
      client: previousClient,
    });
  },
  clearClientState: clearAccountSensitiveStateForLogout,
  clearWebKeyMaterial: clearWebKeyMaterialForLogout,
};

/** Destroy the prior tenant's browser authority before a newly resolved route
 * is persisted or reloaded. Push removal is best-effort server-side, while
 * account state and both committed/staged browser keys are mandatory cleanup. */
export async function clearManagedTenantBrowserState(
  previousRouteKey: string | null,
  dependencies: ManagedTenantSwitchDependencies = managedTenantSwitchDependencies,
): Promise<void> {
  if (previousRouteKey !== null) {
    await dependencies.revokePreviousDevice(previousRouteKey);
  }
  try {
    await dependencies.unsubscribePreviousPush(previousRouteKey);
  } catch {
    // The local subscription is removed first; stale server rows are pruned
    // when the now-unreachable device is revoked or reconciled.
  }
  invalidateFixedCoordinatorClientForTenantRouteSwitch();
  suspendSyncForTenantRouteSwitch();
  dependencies.clearClientState();
  await dependencies.clearWebKeyMaterial();
}

/** Prepare an email-selected route without persisting it. The caller commits
 * only after it has actually invoked a request against the resolved prefix. */
export async function prepareManagedTenantRouteSwitch(
  nextRouteKey: string,
  dependencies: ManagedTenantSwitchDependencies = managedTenantSwitchDependencies,
): Promise<boolean> {
  const previousRouteKey = storedTenantRouteKey();
  if (previousRouteKey === nextRouteKey) return false;
  await clearManagedTenantBrowserState(previousRouteKey, dependencies);
  return true;
}

/** Finish the switch staged synchronously by an activation/reset link before
 * rendering can start a protected request with authority from the old route. */
export async function completePendingTenantRouteSwitch(
  dependencies: ManagedTenantSwitchDependencies = managedTenantSwitchDependencies,
): Promise<boolean> {
  const pending = pendingTenantRouteSwitch();
  if (!pending) return false;
  await clearManagedTenantBrowserState(pending.previousRouteKey, dependencies);
  clearPendingTenantRouteSwitch(pending.routeKey);
  broadcastTenantRouteSwitch();
  return true;
}


/** Revoke on the coordinator before destroying the only local signing key.
 * A missing/failed response is not success: the same persisted key must receive
 * an explicit device-layer rejection before local cleanup and navigation run. */
export async function logoutManagedBrowser(
  dependencies: ManagedLogoutDependencies = defaultDependencies,
): Promise<void> {
  let revoked = false;
  try {
    revoked = (await dependencies.authLogout()).ok === true;
  } catch {
    // A committed logout can lose its response. Prove it below with the same key.
  }

  if (!revoked) {
    try {
      revoked = await dependencies.probeCurrentDevice() === "device-rejected";
    } catch {
      revoked = false;
    }
  }
  if (!revoked) throw new Error(MANAGED_LOGOUT_UNCONFIRMED_MESSAGE);

  try { await dependencies.unsubscribePush(); } catch { /* browser-local best effort */ }
  dependencies.clearClientState();
  await dependencies.clearWebKeyMaterial();
  dependencies.replaceLocation("/login");
}
