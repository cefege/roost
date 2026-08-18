// ui-cc reporter — tells coord what THIS browser tab is showing (UiReportState)
// so agents can SEE the spatial model: active path, the active folder's pane
// tiling (layout_json = the Layout tree as-is, same JSON-string pattern as
// DiagSnapshot.spa_state_json), focused pane, and the selected tab per pane.
// Coord keys the report by (fp, tab_id) and re-broadcasts it as a ui_state
// Sync frame; the SPA never consumes those (its own reflection).
//
// Triggers: every layout commit (paneLayoutStore.onLayoutCommit), router
// location changes (UiBridge's effect → scheduleUiStateReport), tab return to
// visible, and a 60s heartbeat (coord's per-tab TTL is 5min — the heartbeat
// keeps live-but-idle tabs listed). All funnel through ONE 300ms trailing
// debounce, so a burst of focus clicks costs one RPC. Sends are best-effort:
// coord offline / pre-auth first paint drops silently, next trigger retries.
//
// Wired by components/UiBridge.tsx (mounted once in App's RootShell): the reporter
// needs the router pathname, injected as a plain accessor so this module
// stays free of router context.

import { coordClient } from "../connect.ts";
import { getTabId } from "../auth/tab-id.ts";
import { onLayoutCommit, resolveLayout } from "../store/paneLayoutStore.ts";
import { allLeaves } from "../store/paneLayout.ts";
import { activeSessionForPath } from "../store/selectors.ts";
import { folderKeyOf } from "./folderKey.ts";
import { liveIdsForFolder } from "./deckOps.ts";

const DEBOUNCE_MS = 300;
const HEARTBEAT_MS = 60_000;

let _getPath: (() => string) | null = null;
let _debounce: Timer | undefined;

function _send(): void {
  if (!_getPath) return;
  const path = _getPath();
  // Same folder-bucket derivation as MainPane → TerminalDeck: URL-active OPEN
  // session → folderKeyOf. Off a terminal route (or session closed) there is
  // no layout to report — send anyway with empty folderKey/layout so an agent
  // still learns the tab's navigation state.
  const active = activeSessionForPath(path);
  const open = active && active.status === "open" ? active : null;
  const fk = open ? folderKeyOf(open) : null;
  const layout = fk ? resolveLayout(fk, liveIdsForFolder(fk)) : null;
  void coordClient.uiReportState({
    tabId: getTabId(),
    activePath: path,
    folderKey: fk ?? "",
    layoutJson: layout ? JSON.stringify(layout) : "",
    focusedPaneId: layout?.focusedPaneId ?? "",
    visibleSessionIds: layout
      ? allLeaves(layout.root).map((l) => l.selectedTab).filter(Boolean)
      : [],
  }).catch(() => { /* best-effort — see module comment */ });
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
