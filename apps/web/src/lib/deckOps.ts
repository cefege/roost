// Deck ops shared by TerminalDeck (user gestures) and the agent command
// channel (lib/uiCommandDispatch.ts) — repo L11: one solution, every callsite.
// Each op takes a DeckOpsCtx instead of closing over deck-local memos so the
// dispatcher can rebuild the same context from a session's folder bucket.
//
// ctx.layout MUST be a LIVE accessor (re-resolves after a commitLayout):
// selectTabOp's spotlight follow reads the layout AFTER its own commit, and
// closeSessionOp deliberately CAPTURES layout/folderKey before its async undo
// window — both behaviors are byte-for-byte the pre-extraction deck closures.

import type { Session } from "@roost/shared/wire";
import { batch } from "solid-js";
import { diag } from "@roost/shared/diag";
import { rootStore } from "../store/root.ts";
import { commitLayout } from "../store/paneLayoutStore.ts";
import {
  selectTab, focusPane, closeTab, findLeafOfTab, allLeaves, type Layout,
} from "../store/paneLayout.ts";
import { setSpotlightSessionId } from "../store/spotlight.ts";
import { isPendingSpawn, abortOptimisticSpawn } from "../store/optimisticSpawn.ts";
import { scheduleClose, isPendingClose } from "./pendingClose.ts";
import { closeLabelsFor, siblingOrHomeHref, killAfterUndo } from "./closeSession.ts";
import { folderKeyOf } from "./folderKey.ts";

export interface DeckOpsCtx {
  /** Folder bucket the ops commit into (null = no active folder → no-op). */
  folderKey: () => string | null;
  /** LIVE layout accessor — must reflect a commitLayout made moments ago. */
  layout: () => Layout | null;
  /** URL-active session id (TerminalDeck: props.activeSessionId). */
  activeSessionId: () => string | null;
  navigate: (href: string) => void;
}

/** Apply a pure transform to the ctx's CURRENT layout + persist — the same
 *  resolve→transform→commit path TerminalDeck's local `apply` takes. */
function applyTo(ctx: DeckOpsCtx, fn: (l: Layout) => Layout): void {
  const fk = ctx.folderKey();
  const l = ctx.layout();
  if (fk && l) commitLayout(fk, fn(l));
}

/** Sessions that belong in a folder's layout — the same live-set filter as
 *  TerminalDeck's liveIds memo (EXCLUDE pending-close so a soft-closed tab's
 *  pane can collapse; see the comment there). Shared by the ui-cc reporter
 *  and dispatcher, which resolve layouts outside the deck. */
export function liveIdsForFolder(fk: string): string[] {
  return Object.values(rootStore.sessions)
    .filter((s) => s.status === "open" && folderKeyOf(s) === fk && !isPendingClose(s.id))
    .sort((a, b) => a.created_at - b.created_at)
    .map((s) => s.id);
}

/** Select a tab (click / agent select_tab): select + focus its pane, navigate
 *  to it. `spotlitPaneId` = the currently floated pane, captured BEFORE the
 *  commit — if the selected tab lands in that pane, the spotlight follows it
 *  (swapping tabs inside a floated card keeps the card up). */
export function selectTabOp(ctx: DeckOpsCtx, id: string, spotlitPaneId: string | null): void {
  applyTo(ctx, (l) => selectTab(l, id));
  ctx.navigate(`/s/${id}`);
  if (spotlitPaneId) {
    const l = ctx.layout();
    if (l && findLeafOfTab(l.root, id)?.paneId === spotlitPaneId) setSpotlightSessionId(id);
  }
}

/** Focus a pane (body click / agent focus_pane) and navigate to its selected
 *  tab — focusing IS selecting what that pane shows. No-op when already focused. */
export function focusPaneOp(ctx: DeckOpsCtx, paneId: string): void {
  const l = ctx.layout();
  if (!l || l.focusedPaneId === paneId) return;
  const leaf = allLeaves(l.root).find((le) => le.paneId === paneId);
  applyTo(ctx, (cur) => focusPane(cur, paneId));
  if (leaf?.selectedTab) ctx.navigate(`/s/${leaf.selectedTab}`);
}

/** Soft-close a session (tab ✕ / agent close_tab): pendingClose undo window +
 *  deferred kill, landing the view on closeTab's own focus pick. */
export function closeSessionOp(ctx: DeckOpsCtx, s: Session): void {
  // No real PTY yet — just drop the placeholder + tab. doNewTab's wasAborted
  // branch reaps the spawn once it lands, so no orphan PTY survives.
  if (isPendingSpawn(s.id)) { abortOptimisticSpawn(s.id); applyTo(ctx, (l) => closeTab(l, s.id)); return; }
  const _t0 = performance.now();
  // Land the view synchronously on a sensible destination BEFORE the kill RPC
  // round-trips, so closing the viewed tab never leaves the pane blank for the
  // undo window (gap #2). fk/before captured (not read live) so Undo re-commits
  // the exact pre-close tiling into the right folder even if the user moved on.
  const fk = ctx.folderKey();
  const before = ctx.layout();
  const viewed = ctx.activeSessionId() === s.id;
  const after = before ? closeTab(before, s.id) : null;
  // Reuse closeTab's own adjacency/fixFocus pick so the URL matches the tab the
  // layout will actually show (no URL/deck divergence). Empty layout → sibling.
  const destTab = after
    ? (allLeaves(after.root).find((l) => l.paneId === after.focusedPaneId)?.selectedTab ?? null)
    : null;
  const destHref = destTab ? `/s/${destTab}` : siblingOrHomeHref(s);
  batch(() => {
    scheduleClose(
      s.id,
      closeLabelsFor(s),
      killAfterUndo(s.id),
      () => { if (fk && before) commitLayout(fk, before); if (viewed) ctx.navigate(`/s/${s.id}`); },
    );
    if (fk && after) commitLayout(fk, after);
    if (viewed) ctx.navigate(destHref);
  });
  diag("close.click", {
    sid: s.id,
    sessions: Object.keys(rootStore.sessions).length,
    ms_commit: performance.now() - _t0,
  });
}
