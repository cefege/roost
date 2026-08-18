// ui-cc dispatcher SHELL — executes agent-driven UiCommand frames (Sync
// `ui_command`, published by coord's UiDispatch) against the SAME pure layout
// ops + deckOps helpers that user gestures run, so an agent's "place this
// split" is indistinguishable from a drag-to-edge. The PURE mapping
// (applyUiCommandToLayout, frameAccepted) lives in lib/uiCommandCore.ts —
// unit-tested there without loading this module's store/side-effect imports.
// This shell owns: tab targeting, folder-bucket resolution from rootStore,
// commit + the doSelect navigation coupling (selecting a session navigates to
// it, exactly like a strip click).
// store/sync.ts forwards frames via _dispatchUiCommand; UiBridge (components/
// UiBridge.tsx) registers the handler with router navigate bound. Frames are
// fire-and-forget: invalid ones drop with ONE console.warn, never throw.

import type { UiCommand, UiCommandFrame } from "@roost/shared/proto/sync_pb";
import { diag, signal } from "@roost/shared/diag";
import { findLeafOfTab, type Layout } from "../store/paneLayout.ts";
import { commitLayout, resolveLayout } from "../store/paneLayoutStore.ts";
import { rootStore } from "../store/root.ts";
import type { Session } from "@roost/shared/wire";
import { activeSessionForPath } from "../store/selectors.ts";
import { spotlightSessionId, setSpotlightSessionId, clearSpotlight } from "../store/spotlight.ts";
import { getTabId } from "../auth/tab-id.ts";
import { isCompact } from "./windowSizeClass.ts";
import { folderKeyOf } from "./folderKey.ts";
import { applyUiCommandToLayout, frameAccepted } from "./uiCommandCore.ts";
import {
  liveIdsForFolder, selectTabOp, focusPaneOp, closeSessionOp, type DeckOpsCtx,
} from "./deckOps.ts";


// ── shell ────────────────────────────────────────────────────────────────────

export interface UiCommandIo {
  navigate: (href: string) => void;
  /** Current router pathname — resolves the URL-active session (arrange's
   *  target folder; closeSessionOp's "was the closed tab viewed" check). */
  getPath: () => string;
}

/** One warn per dropped frame — frames are fire-and-forget, never throw. */
function dropUnknown(kind: string, sid: string): void {
  console.warn("[ui-cc] ui_command_unknown_session", { kind, sid });
}

/** DeckOpsCtx over a folder bucket, layout resolved live from the store —
 *  the dispatcher's stand-in for TerminalDeck's folderKey/layout memos. */
function ctxFor(fk: string, io: UiCommandIo): DeckOpsCtx {
  return {
    folderKey: () => fk,
    layout: () => resolveLayout(fk, liveIdsForFolder(fk)),
    activeSessionId: () => {
      const s = activeSessionForPath(io.getPath());
      return s && s.status === "open" ? s.id : null;
    },
    navigate: io.navigate,
  };
}

/** The floated pane, iff the command's layout shows the spotlit session — the
 *  same "spotlight follows an in-pane tab swap" input TerminalDeck computes
 *  from its view() (compact never floats). */
function spotlitPaneIdIn(l: Layout): string | null {
  const sid = spotlightSessionId();
  if (!sid || isCompact()) return null;
  const leaf = findLeafOfTab(l.root, sid);
  return leaf && leaf.selectedTab === sid ? leaf.paneId : null;
}

function openSession(sid: string): Session | null {
  const s = rootStore.sessions[sid];
  return s && s.status === "open" ? s : null;
}

export function handleUiCommand(frame: UiCommandFrame, io: UiCommandIo): void {
  // Targeted frame for another tab (empty targetTabId = broadcast, accept).
  if (!frameAccepted(frame, getTabId())) return;
  const cmd = frame.command;
  const c = cmd?.command;
  if (!cmd || !c?.case) return;
  switch (c.case) {
    case "navigate":
      if (c.value.path) io.navigate(c.value.path);
      return;
    case "spotlight": {
      if (c.value.off) { clearSpotlight(); return; }
      const s = openSession(c.value.sessionId);
      if (!s) return dropUnknown(c.case, c.value.sessionId);
      setSpotlightSessionId(s.id);
      return;
    }
    case "closeTab": {
      // Same soft-close path as the tab ✕: pendingClose undo + deferred kill.
      const s = openSession(c.value.sessionId);
      if (!s) return dropUnknown(c.case, c.value.sessionId);
      closeSessionOp(ctxFor(folderKeyOf(s), io), s);
      return;
    }
    case "selectTab": {
      const s = openSession(c.value.sessionId);
      if (!s) return dropUnknown(c.case, c.value.sessionId);
      // resolveLayout's reconcile folds every open live session in, so the
      // tab is guaranteed present — selectTabOp navigates like a strip click.
      const fk = folderKeyOf(s);
      const l = resolveLayout(fk, liveIdsForFolder(fk));
      selectTabOp(ctxFor(fk, io), s.id, spotlitPaneIdIn(l));
      return;
    }
    case "focusPane": {
      const s = openSession(c.value.sessionId);
      if (!s) return dropUnknown(c.case, c.value.sessionId);
      const fk = folderKeyOf(s);
      const leaf = findLeafOfTab(resolveLayout(fk, liveIdsForFolder(fk)).root, s.id);
      if (!leaf) return dropUnknown(c.case, s.id);
      // Deck semantics: focusing a pane navigates to ITS SELECTED tab (which
      // may differ from the addressed session when it's a background tab).
      focusPaneOp(ctxFor(fk, io), leaf.paneId);
      return;
    }
    case "placeSplit": {
      // Both ends must be live: the anchor names the folder bucket; a not-yet-
      // synced sessionId would be pruned by the next reconcile anyway.
      const anchor = openSession(c.value.anchorSessionId);
      const moved = openSession(c.value.sessionId);
      if (!anchor || !moved) return dropUnknown(c.case, anchor ? c.value.sessionId : c.value.anchorSessionId);
      applyPure(folderKeyOf(anchor), cmd, c.case, io);
      return;
    }
    case "moveTab": {
      const dest = openSession(c.value.destSessionId);
      const moved = openSession(c.value.sessionId);
      if (!dest || !moved) return dropUnknown(c.case, dest ? c.value.sessionId : c.value.destSessionId);
      applyPure(folderKeyOf(dest), cmd, c.case, io);
      return;
    }
    case "arrange": {
      // No session in the command — arrange targets the folder being VIEWED.
      const s = activeSessionForPath(io.getPath());
      const active = s && s.status === "open" ? s : null;
      if (!active) return dropUnknown(c.case, "");
      applyPure(folderKeyOf(active), cmd, c.case, io);
      return;
    }
  }
}

/** Resolve → pure core → commit; placeSplit keeps the doSelect coupling by
 *  navigating to the freshly placed session (splitLeaf already focused it). */
function applyPure(fk: string, cmd: UiCommand, kind: string, io: UiCommandIo): void {
  const liveIds = liveIdsForFolder(fk);
  const next = applyUiCommandToLayout(resolveLayout(fk, liveIds), cmd, liveIds);
  if (!next) return dropUnknown(kind, "");
  commitLayout(fk, next);
  const c = cmd.command;
  if (c.case === "placeSplit") io.navigate(`/s/${c.value.sessionId}`);
}

// ── frame registry (store/sync.ts → UiBridge) ────────────────────────────────

let _handler: ((frame: UiCommandFrame) => void) | null = null;

/** UiBridge registers the navigate-bound handler; returns the unregister. */
export function registerUiCommandHandler(fn: (frame: UiCommandFrame) => void): () => void {
  _handler = fn;
  return () => { if (_handler === fn) _handler = null; };
}

/** sync.ts frame switch entry. No bridge mounted yet → frame drops silently
 *  (coord's `delivered` count tells the agent whether anyone was listening). */
export function _dispatchUiCommand(frame: UiCommandFrame): void {
  try { _handler?.(frame); } catch (e) {
    const command = frame.command?.command.case ?? "unknown";
    diag("ui_cc.handler_failed", { command, error: String(e) });
    signal("diag.corruption_signal", { kind: "ui_command_dropped", command, cooldownKey: "ui_cc" });
    console.warn("[ui-cc] ui_command_handler_failed", e);
  }
}
