// Pure orchestration for the one acknowledged browser layout command.
// The browser shell supplies current tab/socket identity, active-folder state,
// the portable layout adapter, navigation, and Sync result publication.
// Exact target fences are rechecked before every acknowledgement.

import {
  UiApplyLayoutOutcome,
  type LayoutDocumentV1 as ProtoLayoutDocumentV1,
  type UiCommandFrame,
} from "@roost/shared/proto/sync_pb";

export const UI_LAYOUT_APPLY_REJECTION = {
  bridgeUnavailable: "The target tab UI bridge is unavailable.",
  noActiveFolder: "The target tab is not viewing a live folder.",
  invalidDocument: "The layout document is invalid for the current folder.",
} as const;
const UI_LAYOUT_APPLY_DIAGNOSTIC_CORRELATION_MAX_CODE_POINTS = 128;

export interface UiLayoutApplyFolder {
  readonly folderKey: string;
  readonly activeSessionId: string;
  readonly liveSessionIds: readonly string[];
  readonly hasClientOnlySession: boolean;
}

export interface UiLayoutApplyResult {
  readonly correlationId: string;
  readonly outcome: UiApplyLayoutOutcome;
  readonly reason?: string;
}

export interface UiLayoutApplySettlementDiagnostic {
  readonly correlation_id: string;
  readonly outcome: "applied" | "rejected";
}

export interface UiLayoutApplyTargetDependencies {
  readonly currentTabId: () => string;
  readonly currentSocketId: () => string | null;
  readonly sendResult: (result: UiLayoutApplyResult) => boolean;
  readonly recordDiagnostic: (
    event: string,
    diagnostic: UiLayoutApplySettlementDiagnostic,
  ) => void;
}

export interface UiLayoutApplyDependencies extends UiLayoutApplyTargetDependencies {
  readonly activeFolder: () => UiLayoutApplyFolder | null;
  readonly decodeDocument: (document: ProtoLayoutDocumentV1) => unknown;
  readonly applyDocument: (
    folderKey: string,
    document: unknown,
    liveSessionIds: readonly string[],
  ) => { readonly selectedSessionId: string | null };
  readonly clearSpotlight: () => void;
  readonly navigateToSession: (sessionId: string) => void;
}

interface ExactApplyTarget {
  readonly tabId: string;
  readonly socketId: string;
  readonly correlationId: string;
}

/** Returns false for every non-apply frame so legacy dispatch stays separate. */
export function executeTargetedUiLayoutApply(
  frame: UiCommandFrame,
  dependencies: UiLayoutApplyDependencies,
): boolean {
  const command = frame.command?.command;
  if (command?.case !== "applyLayout") return false;
  const target = exactCurrentTarget(frame, dependencies);
  if (!target) return true;

  let folder: UiLayoutApplyFolder | null = null;
  try {
    folder = dependencies.activeFolder();
  } catch {
    return rejectCurrent(target, dependencies, UI_LAYOUT_APPLY_REJECTION.noActiveFolder);
  }
  if (!isCurrentLiveFolder(folder)) {
    return rejectCurrent(target, dependencies, UI_LAYOUT_APPLY_REJECTION.noActiveFolder);
  }
  const protoDocument = command.value.document;
  if (!protoDocument) {
    return rejectCurrent(target, dependencies, UI_LAYOUT_APPLY_REJECTION.invalidDocument);
  }

  let selectedSessionId: string | null;
  try {
    const document = dependencies.decodeDocument(protoDocument);
    selectedSessionId = dependencies.applyDocument(
      folder.folderKey,
      document,
      folder.liveSessionIds,
    ).selectedSessionId;
  } catch {
    return rejectCurrent(target, dependencies, UI_LAYOUT_APPLY_REJECTION.invalidDocument);
  }

  try {
    dependencies.clearSpotlight();
    if (selectedSessionId) dependencies.navigateToSession(selectedSessionId);
  } finally {
    // A completed commit is execution even if a browser-local follow-up throws.
    // Reporting rejection here could invite a retry of an already-applied tree.
    acknowledgeCurrent(target, dependencies, UiApplyLayoutOutcome.APPLIED);
  }
  return true;
}

/** A missing router bridge is an execution rejection, but only for its exact target. */
export function rejectTargetedUiLayoutApplyWithoutBridge(
  frame: UiCommandFrame,
  dependencies: UiLayoutApplyTargetDependencies,
): boolean {
  if (frame.command?.command.case !== "applyLayout") return false;
  const target = exactCurrentTarget(frame, dependencies);
  if (!target) return true;
  return rejectCurrent(target, dependencies, UI_LAYOUT_APPLY_REJECTION.bridgeUnavailable);
}

function exactCurrentTarget(
  frame: UiCommandFrame,
  dependencies: UiLayoutApplyTargetDependencies,
): ExactApplyTarget | null {
  if (
    !frame.targetTabId
    || !frame.targetSocketId
    || !frame.correlationId
    || frame.targetTabId !== dependencies.currentTabId()
    || frame.targetSocketId !== dependencies.currentSocketId()
  ) return null;
  return {
    tabId: frame.targetTabId,
    socketId: frame.targetSocketId,
    correlationId: frame.correlationId,
  };
}

function isCurrentLiveFolder(folder: UiLayoutApplyFolder | null): folder is UiLayoutApplyFolder {
  return !!folder?.folderKey
    && !!folder.activeSessionId
    && folder.liveSessionIds.includes(folder.activeSessionId)
    && !folder.hasClientOnlySession;
}

function rejectCurrent(
  target: ExactApplyTarget,
  dependencies: UiLayoutApplyTargetDependencies,
  reason: string,
): true {
  acknowledgeCurrent(target, dependencies, UiApplyLayoutOutcome.REJECTED, reason);
  return true;
}

function acknowledgeCurrent(
  target: ExactApplyTarget,
  dependencies: UiLayoutApplyTargetDependencies,
  outcome: UiApplyLayoutOutcome,
  reason?: string,
): void {
  try {
    dependencies.recordDiagnostic("ui_cc.layout_apply_settled", {
      correlation_id: boundedDiagnosticCorrelation(target.correlationId),
      outcome: outcome === UiApplyLayoutOutcome.APPLIED ? "applied" : "rejected",
    });
  } catch {
    // Diagnostic sinks cannot change an acknowledged command's semantics.
  }
  if (
    dependencies.currentTabId() !== target.tabId
    || dependencies.currentSocketId() !== target.socketId
  ) return;
  dependencies.sendResult({
    correlationId: target.correlationId,
    outcome,
    reason,
  });
}

function boundedDiagnosticCorrelation(correlationId: string): string {
  let bounded = "";
  let codePointCount = 0;
  for (const codePoint of correlationId) {
    if (codePointCount >= UI_LAYOUT_APPLY_DIAGNOSTIC_CORRELATION_MAX_CODE_POINTS) break;
    bounded += codePoint;
    codePointCount++;
  }
  return bounded;
}
