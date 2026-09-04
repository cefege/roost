// Server-confirmed dashboard selection and the client-side scope cutover.
// The selected ID is a transport hint only; AuthDashboardAccess remains the
// authority that validates it against the authenticated account membership.

import { batch } from "solid-js";
import type { AuthDashboardAccessResponse } from "@roost/shared/proto/coordinator_pb";
import { X_ROOST_DASHBOARD_ID } from "@roost/shared/wire/headers";
import { coordClient } from "../connect.ts";
import {
  clearDashboardScopedRootData,
  clearAccountRootStateForLogout,
  invalidateDashboardResources,
  isValidDashboardAccess,
  rootStore,
  setDashboardAccess,
} from "./root.ts";
import type { DashboardAccessSnapshot } from "./root.ts";
import { resetTerminalStream } from "./terminal-stream.ts";
import { resetTerminalOutboundState } from "../ws/sync-outbound.ts";
import {
  closeSyncForDashboardSwitch,
  holdSyncForDashboardSwitch,
  releaseSyncAfterDashboardSwitch,
  resetLastSeenSyncEventId,
} from "./sync.ts";
import { setRoutableFps } from "./sync-routable.ts";
import { resetSyncHydration } from "./sync-hydrated.ts";
import { resetOptimisticSpawnState } from "./optimisticSpawn.ts";
import { resetPendingCloses } from "../lib/pendingClose.ts";
import { resetCursorPollTicker } from "../lib/cursorPollTicker.ts";
import { resetUserTerminalInput } from "../lib/userTerminalInput.ts";
import { clearAgentConfigForDashboardSwitch, loadAgentConfig } from "../lib/agents.ts";
import { resetSpawnSessionRuntime } from "../lib/spawnSession.ts";
import { resetResizeDrags } from "../lib/resizeDrag.ts";
import { resetScrollbackBackfillState } from "../lib/scrollbackBackfillState.ts";
import { closeCmdPalette, closeHelp } from "../lib/keyboardShortcuts.ts";
import { closeTransferDialog } from "../lib/transferDialog.ts";
import { clearCommandPaletteCacheForAccountBoundary } from "../components/CommandPalette.data.ts";
import { resetAgentStatusProjection } from "./agent-status.ts";
import { resetSyncHandlerRuntimeForDashboardBoundary } from "./sync-handlers.ts";
import { clearToastsForAccountBoundary } from "./toastStore.ts";
import { closeRenameDialog } from "./renameDialog.ts";
import { clearQueueTaskDialogForLogout } from "./queueTaskDialog.ts";
import { clearTransfersForLogout } from "./transfers.ts";
import { clearSpotlight } from "./spotlight.ts";

const REMEMBERED_DASHBOARD_KEY = "roost.dashboardId";
let dashboardAccessRequestGeneration = 0;
let activeDashboardSwitchRequestGeneration: number | null = null;
let bootstrapDashboardAccessInFlight: {
  candidate: string | null;
  promise: Promise<boolean>;
} | null = null;

/** A local value is only a startup/switch hint. It is never applied to the
 * root selection until the coordinator returns it in AuthDashboardAccess. */
export function rememberedDashboardId(): string | null {
  try {
    const value = localStorage.getItem(REMEMBERED_DASHBOARD_KEY);
    return value && value.trim() ? value : null;
  } catch {
    return null;
  }
}

export function rememberDashboardSelectionHint(dashboardId: string | null): void {
  try {
    if (dashboardId) localStorage.setItem(REMEMBERED_DASHBOARD_KEY, dashboardId);
    else localStorage.removeItem(REMEMBERED_DASHBOARD_KEY);
  } catch {
    // Persistence is optional. The confirmed in-memory selection still wins.
  }
}

export interface DashboardResourceToken {
  readonly generation: number;
  readonly dashboardId: string | null;
}

/** Capture a guard for asynchronous UI work tied to the current scope. */
export function captureDashboardResourceToken(): DashboardResourceToken {
  return {
    generation: rootStore.dashboard_generation,
    dashboardId: rootStore.selected_dashboard_id,
  };
}

/** A response/action captured before a dashboard cutover must be discarded. */
export function isCurrentDashboardResourceToken(token: DashboardResourceToken): boolean {
  return token.generation === rootStore.dashboard_generation
    && token.dashboardId === rootStore.selected_dashboard_id;
}

function clearDashboardRuntimeState(): void {
  // Stop every timer/callback that can name a prior session before Solid
  // unmounts its panes and before the next dashboard's socket can open.
  resetCursorPollTicker();
  resetUserTerminalInput();
  resetResizeDrags();
  resetPendingCloses();
  resetOptimisticSpawnState();
  resetSpawnSessionRuntime();
  resetAgentStatusProjection();
  resetSyncHandlerRuntimeForDashboardBoundary();
  resetTerminalStream();
  resetTerminalOutboundState();
  resetScrollbackBackfillState();
  resetLastSeenSyncEventId();
  clearAgentConfigForDashboardSwitch();
  clearCommandPaletteCacheForAccountBoundary();
  closeCmdPalette();
  clearToastsForAccountBoundary();
  clearDashboardScopedRootData();
  setRoutableFps(null);
  resetSyncHydration();
}

/** Discard overlays that may retain scoped paths, names, or actions. */
function clearDashboardScopedOverlayState(): void {
  closeHelp();
  closeTransferDialog();
  closeRenameDialog();
  clearQueueTaskDialogForLogout();
  clearTransfersForLogout();
  clearSpotlight();
}

function settleDashboardSwitchHoldAtAccountBoundary(): void {
  activeDashboardSwitchRequestGeneration = null;
  releaseSyncAfterDashboardSwitch();
  closeSyncForDashboardSwitch();
}

/** Discard every account-derived value after browser-device rejection. */
export function suspendDashboardScopedClientState(): void {
  dashboardAccessRequestGeneration++;
  bootstrapDashboardAccessInFlight = null;
  batch(() => {
    clearDashboardScopedOverlayState();
    clearDashboardRuntimeState();
    clearAccountRootStateForLogout();
  });
  rememberDashboardSelectionHint(null);
  settleDashboardSwitchHoldAtAccountBoundary();
}

/** Forget every confirmed account/dashboard authority at logout. */
export function clearAccountScopedClientStateForLogout(): void {
  dashboardAccessRequestGeneration++;
  bootstrapDashboardAccessInFlight = null;
  batch(() => {
    clearDashboardScopedOverlayState();
    clearDashboardRuntimeState();
    clearAccountRootStateForLogout();
  });
  rememberDashboardSelectionHint(null);
  settleDashboardSwitchHoldAtAccountBoundary();
}

function snapshotFromProto(response: AuthDashboardAccessResponse): DashboardAccessSnapshot {
  return {
    account_id: response.accountId,
    organizations: response.organizations.map((organization) => ({
      id: organization.id,
      slug: organization.slug,
      name: organization.name,
      role: organization.role,
    })),
    dashboards: response.dashboards.map((dashboard) => ({
      id: dashboard.id,
      organization_id: dashboard.organizationId,
      slug: dashboard.slug,
      name: dashboard.name,
      organization_role: dashboard.organizationRole,
      dashboard_role: dashboard.dashboardRole,
    })),
    selected_dashboard_id: response.selectedDashboardId || null,
    capabilities: response.capabilities,
  };
}

interface DashboardSwitchAttempt {
  readonly candidate: string;
  readonly requestGeneration: number;
  readonly resourceGeneration: number;
  readonly previousAccess: DashboardAccessSnapshot | null;
}

function confirmedDashboardAccessSnapshot(): DashboardAccessSnapshot | null {
  const snapshot: DashboardAccessSnapshot = {
    account_id: rootStore.account_id ?? "",
    organizations: Object.values(rootStore.organizations).map((organization) => ({ ...organization })),
    dashboards: Object.values(rootStore.dashboards).map((dashboard) => ({ ...dashboard })),
    selected_dashboard_id: rootStore.selected_dashboard_id,
    capabilities: [...rootStore.effective_capabilities],
  };
  return rootStore.account_id && rootStore.selected_dashboard_id && isValidDashboardAccess(snapshot)
    ? snapshot
    : null;
}

function isCurrentDashboardSwitch(attempt: DashboardSwitchAttempt): boolean {
  return activeDashboardSwitchRequestGeneration === attempt.requestGeneration
    && attempt.requestGeneration === dashboardAccessRequestGeneration
    && attempt.resourceGeneration === rootStore.dashboard_generation;
}

function rehydrateConfirmedDashboard(requestGeneration?: number): void {
  if (
    requestGeneration !== undefined
    && activeDashboardSwitchRequestGeneration !== requestGeneration
  ) return;
  activeDashboardSwitchRequestGeneration = null;
  releaseSyncAfterDashboardSwitch();
  void loadAgentConfig();
}

function restoreDashboardAfterFailedSwitch(attempt: DashboardSwitchAttempt): void {
  try {
    if (attempt.previousAccess && !setDashboardAccess(attempt.previousAccess)) {
      throw new Error("could not restore confirmed dashboard access");
    }
    rememberDashboardSelectionHint(attempt.previousAccess?.selected_dashboard_id ?? null);
  } finally {
    rehydrateConfirmedDashboard(attempt.requestGeneration);
  }
}

function beginDashboardSwitch(candidate: string): DashboardSwitchAttempt {
  const previousAccess = confirmedDashboardAccessSnapshot();
  const requestGeneration = ++dashboardAccessRequestGeneration;
  activeDashboardSwitchRequestGeneration = requestGeneration;
  bootstrapDashboardAccessInFlight = null;
  holdSyncForDashboardSwitch();
  try {
    batch(() => {
      invalidateDashboardResources();
      clearDashboardScopedOverlayState();
      clearDashboardRuntimeState();
    });
  } catch (error) {
    rehydrateConfirmedDashboard(requestGeneration);
    throw error;
  }
  return {
    candidate,
    requestGeneration,
    resourceGeneration: rootStore.dashboard_generation,
    previousAccess,
  };
}

/** Confirm an exact login response through a caller-supplied prefixed signed
 * client without publishing scope into the current document. The full reload
 * will commit the same protected response through the normal bootstrap path. */
export async function confirmDashboardAccessWithClient(
  client: Pick<typeof coordClient, "authDashboardAccess">,
  dashboardId: string,
): Promise<boolean> {
  const response = await client.authDashboardAccess(
    {},
    { headers: { [X_ROOST_DASHBOARD_ID]: dashboardId } },
  );
  const snapshot = snapshotFromProto(response);
  return snapshot.selected_dashboard_id === dashboardId
    && isValidDashboardAccess(snapshot);
}
export async function selectDashboardFromServerWithClient(
  client: Pick<typeof coordClient, "authDashboardAccess">,
  dashboardId: string,
): Promise<boolean> {
  const response = await client.authDashboardAccess(
    {},
    { headers: { [X_ROOST_DASHBOARD_ID]: dashboardId } },
  );
  const snapshot = snapshotFromProto(response);
  return snapshot.selected_dashboard_id === dashboardId
    && commitServerConfirmedDashboardAccess(snapshot);
}

/** Fetch scope server-side. A picker owns the request generation until its
 * Sync hold settles, so lifecycle/bootstrap refreshes cannot supersede it. */
async function requestDashboardAccess(candidate: string | null): Promise<boolean> {
  if (activeDashboardSwitchRequestGeneration !== null) return false;
  const requestGeneration = ++dashboardAccessRequestGeneration;
  const response = await coordClient.authDashboardAccess(
    {},
    candidate
      ? { headers: { [X_ROOST_DASHBOARD_ID]: candidate } }
      : undefined,
  );
  // A focus refresh started before a picker action must not restore the
  // previous dashboard after the newer server-confirmed response lands.
  if (requestGeneration !== dashboardAccessRequestGeneration) return false;
  return commitServerConfirmedDashboardAccess(snapshotFromProto(response));
}

/** Bootstrap from the remembered hint, which the coordinator may replace. */
export async function bootstrapDashboardAccess(): Promise<boolean> {
  const candidate = rootStore.selected_dashboard_id ?? rememberedDashboardId();
  if (bootstrapDashboardAccessInFlight?.candidate === candidate) {
    return bootstrapDashboardAccessInFlight.promise;
  }
  const promise = requestDashboardAccess(candidate);
  bootstrapDashboardAccessInFlight = { candidate, promise };
  try {
    return await promise;
  } finally {
    if (bootstrapDashboardAccessInFlight?.promise === promise) {
      bootstrapDashboardAccessInFlight = null;
    }
  }
}

/** Clear old scope and hold Sync while the coordinator confirms the candidate.
 * Only the current attempt may publish; failure restores the prior scope. */
export async function selectDashboardFromServer(dashboardId: string): Promise<boolean> {
  if (
    activeDashboardSwitchRequestGeneration !== null
    && (!dashboardId || dashboardId === rootStore.selected_dashboard_id)
  ) return false;
  if (!dashboardId || dashboardId === rootStore.selected_dashboard_id) {
    const requestGeneration = ++dashboardAccessRequestGeneration;
    const response = await coordClient.authDashboardAccess(
      {},
      dashboardId
        ? { headers: { [X_ROOST_DASHBOARD_ID]: dashboardId } }
        : undefined,
    );
    if (requestGeneration !== dashboardAccessRequestGeneration) return false;
    const snapshot = snapshotFromProto(response);
    return snapshot.selected_dashboard_id === dashboardId
      && commitServerConfirmedDashboardAccess(snapshot);
  }

  const attempt = beginDashboardSwitch(dashboardId);
  let response: AuthDashboardAccessResponse;
  try {
    response = await coordClient.authDashboardAccess(
      {},
      { headers: { [X_ROOST_DASHBOARD_ID]: dashboardId } },
    );
  } catch (error) {
    if (!isCurrentDashboardSwitch(attempt)) return false;
    restoreDashboardAfterFailedSwitch(attempt);
    throw error;
  }

  if (!isCurrentDashboardSwitch(attempt)) return false;
  const snapshot = snapshotFromProto(response);
  if (
    snapshot.selected_dashboard_id !== attempt.candidate
    || !isValidDashboardAccess(snapshot)
  ) {
    restoreDashboardAfterFailedSwitch(attempt);
    return false;
  }
  if (!setDashboardAccess(snapshot)) {
    restoreDashboardAfterFailedSwitch(attempt);
    return false;
  }
  rememberDashboardSelectionHint(snapshot.selected_dashboard_id);
  rehydrateConfirmedDashboard(attempt.requestGeneration);
  return true;
}

/** Revalidate the current selected membership on lifecycle refresh. */
export async function refreshDashboardAccess(): Promise<boolean> {
  return requestDashboardAccess(rootStore.selected_dashboard_id);
}

/**
 * Install scope data only after it came from AuthDashboardAccess. A changed
 * selected ID cuts over all dashboard data under the same no-dial hold used by
 * an interactive switch.
 */
export function commitServerConfirmedDashboardAccess(
  snapshot: DashboardAccessSnapshot,
): boolean {
  if (!isValidDashboardAccess(snapshot)) return false;
  const selectionChanged = rootStore.selected_dashboard_id !== snapshot.selected_dashboard_id;
  if (selectionChanged) {
    holdSyncForDashboardSwitch();
    batch(() => {
      invalidateDashboardResources();
      clearDashboardRuntimeState();
      // Validated above. Keep this assertion local to make a future root-state
      // regression fail loudly rather than dialing with an unconfirmed scope.
      if (!setDashboardAccess(snapshot)) throw new Error("invalid dashboard access response");
    });
    rehydrateConfirmedDashboard();
  } else {
    setDashboardAccess(snapshot);
  }
  rememberDashboardSelectionHint(snapshot.selected_dashboard_id);
  return true;
}
