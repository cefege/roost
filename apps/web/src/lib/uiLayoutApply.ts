// Browser adapter for the acknowledged apply-layout command.
// It derives the live folder from the router and canonical session selector,
// delegates the sole mutation to applyLayoutDocument, then publishes a result
// and bounded correlation diagnostic after the navigation attempt.
// Direct Sync leaf imports keep this inbound path from cycling through sync.ts.

import { create } from "@bufbuild/protobuf";
import { diag } from "@roost/shared/diag";
import { layoutDocumentFromProto } from "@roost/shared/layout-document-proto";
import { UiApplyLayoutResultSchema, type UiCommandFrame } from "@roost/shared/proto/sync_pb";
import { getTabId } from "../auth/tab-id.ts";
import { sessionHref } from "../routes.ts";
import { applyLayoutDocument } from "../store/paneLayoutDocument.ts";
import { projectOptimisticSpawnMembership } from "../store/optimisticSpawn.ts";
import { activeSessionForPath, liveSessionIdsForFolder } from "../store/selectors.ts";
import { sendSyncV2Command } from "../store/sync-domain-state.ts";
import { currentSyncV2SocketId } from "../store/sync-link-state.ts";
import { clearSpotlight } from "../store/spotlight.ts";
import { folderKeyOf } from "./folderKey.ts";
import {
  executeTargetedUiLayoutApply,
  rejectTargetedUiLayoutApplyWithoutBridge,
  type UiLayoutApplyResult,
  type UiLayoutApplyTargetDependencies,
} from "./uiLayoutApplyCore.ts";

export interface UiLayoutApplyIo {
  readonly getPath: () => string;
  readonly navigate: (href: string) => void;
}

const targetDependencies: UiLayoutApplyTargetDependencies = {
  currentTabId: getTabId,
  currentSocketId: currentSyncV2SocketId,
  sendResult: sendApplyResult,
  recordDiagnostic: (event, { correlation_id, outcome }) => {
    diag(event, { correlation_id, outcome });
  },
};

export function handleUiLayoutApply(frame: UiCommandFrame, io: UiLayoutApplyIo): boolean {
  return executeTargetedUiLayoutApply(frame, {
    ...targetDependencies,
    activeFolder: () => {
      const activeSession = activeSessionForPath(io.getPath());
      if (!activeSession || activeSession.status !== "open") return null;
      const folderKey = folderKeyOf(activeSession);
      const membership = projectOptimisticSpawnMembership(
        liveSessionIdsForFolder(folderKey),
      );
      return {
        folderKey,
        activeSessionId: activeSession.id,
        liveSessionIds: membership.authoritativeSessionIds,
        hasClientOnlySession: membership.hasClientOnlySession,
      };
    },
    decodeDocument: layoutDocumentFromProto,
    applyDocument: applyLayoutDocument,
    clearSpotlight,
    navigateToSession: (sessionId) => { io.navigate(sessionHref(sessionId)); },
  });
}

export function rejectUiLayoutApplyWithoutBridge(frame: UiCommandFrame): boolean {
  return rejectTargetedUiLayoutApplyWithoutBridge(frame, targetDependencies);
}

function sendApplyResult(result: UiLayoutApplyResult): boolean {
  return sendSyncV2Command({
    case: "uiApplyLayoutResult",
    value: create(UiApplyLayoutResultSchema, {
      correlationId: result.correlationId,
      outcome: result.outcome,
      reason: result.reason,
    }),
  });
}
