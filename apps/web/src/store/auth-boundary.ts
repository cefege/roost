// Credential-boundary teardown and the async guard every authenticated UI
// flow fences itself with. sync-bootstrap calls the teardown when the
// coordinator rejects this browser's device credential; each reset below stops
// a timer or callback that can otherwise outlive that credential.
// Token holders read the root store's auth generation through this owner.

import { batch } from "solid-js";
import {
  clearAccountRootStateForLogout,
  clearAuthScopedRootData,
  rootStore,
} from "./root.ts";
import { resetTerminalStream } from "./terminal-stream.ts";
import { resetTerminalOutboundState } from "../ws/sync-outbound.ts";
import { resetLastSeenSyncEventId } from "./sync.ts";
import { setRoutableFps } from "./sync-routable.ts";
import { resetSyncHydration } from "./sync-hydrated.ts";
import { resetOptimisticSpawnState } from "./optimisticSpawn.ts";
import { resetPendingCloses } from "../lib/pendingClose.ts";
import { resetCursorPollTicker } from "../lib/cursorPollTicker.ts";
import { resetUserTerminalInput } from "../lib/userTerminalInput.ts";
import { resetContentSearchRuntimeForAuthBoundary } from "../lib/globalContentSearchRuntime.ts";
import { clearAgentConfigForAuthBoundary } from "../lib/agents.ts";
import { resetSpawnSessionRuntime } from "../lib/spawnSession.ts";
import { resetResizeDrags } from "../lib/resizeDrag.ts";
import { resetScrollbackBackfillState } from "../lib/scrollbackBackfillState.ts";
import { closeCmdPalette, closeHelp } from "../lib/keyboardShortcuts.ts";
import { closeTransferDialog } from "../lib/transferDialog.ts";
import { clearCommandPaletteCacheForAccountBoundary } from "../components/CommandPalette.data.ts";
import { resetAgentStatusProjection } from "./agent-status.ts";
import { resetSyncHandlerRuntimeForAuthBoundary } from "./sync-handlers.ts";
import { clearToastsForAccountBoundary } from "./toastStore.ts";
import { closeRenameDialog } from "./renameDialog.ts";
import { clearQueueTaskDialogForLogout } from "./queueTaskDialog.ts";
import { clearTransfersForLogout } from "./transfers.ts";
import { clearSpotlight } from "./spotlight.ts";

export interface AuthResourceToken {
  readonly generation: number;
}

/** Capture a guard for asynchronous UI work tied to the current credential. */
export function captureAuthResourceToken(): AuthResourceToken {
  return { generation: rootStore.auth_generation };
}

/** A response/action captured before a credential boundary must be discarded. */
export function isCurrentAuthResourceToken(token: AuthResourceToken): boolean {
  return token.generation === rootStore.auth_generation;
}

/** Discard every account-derived value after browser-device rejection. */
export function suspendAuthenticatedClientState(): void {
  batch(() => {
    clearAuthScopedOverlayState();
    clearAuthScopedRuntimeState();
    clearAccountRootStateForLogout();
  });
}

function clearAuthScopedRuntimeState(): void {
  // Stop every timer/callback that can name a prior session before Solid
  // unmounts its panes and before another socket can open.
  resetCursorPollTicker();
  resetUserTerminalInput();
  resetContentSearchRuntimeForAuthBoundary();
  resetResizeDrags();
  resetPendingCloses();
  resetOptimisticSpawnState();
  resetSpawnSessionRuntime();
  resetAgentStatusProjection();
  resetSyncHandlerRuntimeForAuthBoundary();
  resetTerminalStream();
  resetTerminalOutboundState();
  resetScrollbackBackfillState();
  resetLastSeenSyncEventId();
  clearAgentConfigForAuthBoundary();
  clearCommandPaletteCacheForAccountBoundary();
  closeCmdPalette();
  clearToastsForAccountBoundary();
  clearAuthScopedRootData();
  setRoutableFps(null);
  resetSyncHydration();
}

/** Discard overlays that may retain paths, names, or actions. */
function clearAuthScopedOverlayState(): void {
  closeHelp();
  closeTransferDialog();
  closeRenameDialog();
  clearQueueTaskDialogForLogout();
  clearTransfersForLogout();
  clearSpotlight();
}
