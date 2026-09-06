// Reports this browser tab's route and portable pane layout to the coordinator.
// Only coordinator-admitted session identities cross terminal routes or
// LayoutDocumentV1 bindings; runtime pane IDs and placeholders stay local.
// UiBridge owns lifecycle triggers and supplies the live router pathname.

import { create } from "@bufbuild/protobuf";
import { layoutDocumentToProto } from "@roost/shared/layout-document-proto";
import {
  UiReportStateRequestSchema,
  type UiReportStateRequest,
} from "@roost/shared/proto/sync_pb";
import { coordClient } from "../connect.ts";
import { getTabId } from "../auth/tab-id.ts";
import { exportLayoutDocument } from "../store/paneLayoutDocument.ts";
import { onLayoutCommit } from "../store/paneLayoutStore.ts";
import {
  isClientOnlyOptimisticSpawn,
  projectOptimisticSpawnMembership,
} from "../store/optimisticSpawn.ts";
import { activeSessionForPath, liveSessionIdsForFolder } from "../store/selectors.ts";
import { folderKeyOf } from "./folderKey.ts";

const DEBOUNCE_MS = 300;
const HEARTBEAT_MS = 60_000;

let _getPath: (() => string) | null = null;
let _debounce: Timer | undefined;

/** Resolve only a live coordinator-admitted session for hydration reporting. */
export function authoritativeUiReportSessionId(path: string): string | null {
  const activeSession = activeSessionForPath(path);
  return activeSession?.status === "open"
    && !isClientOnlyOptimisticSpawn(activeSession.id)
    ? activeSession.id
    : null;
}

/** Re-report when an unchanged active route gains its authoritative session. */
export function scheduleUiStateReportOnSessionResolution(
  currentSessionId: string | null,
  previousSessionId: string | null | undefined,
  scheduleReport: () => void = scheduleUiStateReport,
): void {
  if (
    currentSessionId
    && (previousSessionId === null || previousSessionId === undefined)
  ) scheduleReport();
}

/** Build the exact typed payload used by both the RPC and focused tests. */
export function _buildUiStateReport(path: string): UiReportStateRequest {
  const activeSession = activeSessionForPath(path);
  const directSessionPath = path.startsWith("/s/");
  const activeSessionIsAuthoritative = activeSession?.status === "open"
    && !isClientOnlyOptimisticSpawn(activeSession.id);
  const openSession = activeSession?.status === "open" ? activeSession : null;
  const folderKey = openSession ? folderKeyOf(openSession) : null;
  const liveSessionIds = folderKey
    ? projectOptimisticSpawnMembership(
      liveSessionIdsForFolder(folderKey),
    ).authoritativeSessionIds
    : [];
  return create(UiReportStateRequestSchema, {
    tabId: getTabId(),
    activePath: directSessionPath && !activeSessionIsAuthoritative ? "" : path,
    folderKey: folderKey ?? "",
    layoutDocument: folderKey
      ? layoutDocumentToProto(exportLayoutDocument(folderKey, liveSessionIds))
      : undefined,
  });
}

function _send(): void {
  if (!_getPath) return;
  try {
    void coordClient.uiReportState(_buildUiStateReport(_getPath()))
      .catch(() => { /* best-effort — the next lifecycle trigger retries */ });
  } catch {
    // A malformed local layout remains browser-local and cannot poison a timer.
  }
}

/** Coalesce any trigger into one trailing send. Safe to call before init
 *  (no-op) — e.g. a layout commit on a page without the bridge mounted. */
export function scheduleUiStateReport(): void {
  if (!_getPath) return;
  clearTimeout(_debounce);
  _debounce = setTimeout(() => { _debounce = undefined; _send(); }, DEBOUNCE_MS);
}

/** Start reporting; returns the dispose fn (UiBridge calls it on cleanup).
 *  `getPath` = live router pathname accessor. */
export function initUiStateReport(getPath: () => string): () => void {
  _getPath = getPath;
  const offCommit = onLayoutCommit(scheduleUiStateReport);
  // visibilitychange → visible: re-report on tab return, matching the
  // viewer-claim freshness semantics (a backgrounded tab's report may be
  // minutes stale the moment the user comes back).
  const onVis = () => { if (document.visibilityState === "visible") scheduleUiStateReport(); };
  document.addEventListener("visibilitychange", onVis);
  const heartbeat = setInterval(scheduleUiStateReport, HEARTBEAT_MS);
  scheduleUiStateReport(); // initial report — the tab exists
  return () => {
    offCommit();
    document.removeEventListener("visibilitychange", onVis);
    clearInterval(heartbeat);
    clearTimeout(_debounce);
    _debounce = undefined;
    _getPath = null;
  };
}
