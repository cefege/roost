// Shared close-session helpers. Every close path — the tab ✕ in TerminalDeck,
// the sidebar ✕/swipe in SessionRow, and the right-click "Close terminal" in
// TerminalContextMenu — converges here so the snackbar labels, the landing
// destination, and the deferred kill are computed identically (repo L11: one
// solution, every callsite). Pairs with pendingClose.ts's 5s soft-close: these
// build the inputs to scheduleClose(). The kill RPC fires in the background
// AFTER the undo window; failure surfaces a toast + force-tombstones the row.

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { newestOpenSessionForFolderKey } from "../store/selectors.ts";
import { coordClient } from "../connect.ts";
import { sessionTitle } from "./sessionTitle.ts";
import { folderDisplayName, folderKeyOf } from "./folderKey.ts";
import { shortServerLabel } from "./sidebarFormat.ts";
import { addToast } from "./toastStore.ts";
import { log } from "@roost/shared/log";

/** The three snackbar labels for a closing session — byte-identical to the
 *  triple TerminalDeck.doClose and SessionRow.serverLabel produced inline. */
export function closeLabelsFor(s: Session): { terminalName: string; folder: string; server: string } {
  return {
    terminalName: sessionTitle(s),
    folder: folderDisplayName(s),
    server: shortServerLabel(rootStore.workers[s.worker_fp]?.label ?? s.worker_fp.slice(0, 6)),
  };
}

/** Where to land the view when closing `s`: the newest still-open sibling in
 *  the same folder, else Home. Same target policy as MainPane's
 *  deadRouteSafetyNet bounceTarget — keep them in lockstep. A sibling in the
 *  undo window is excluded by newestOpenSessionForFolderKey's pending-close
 *  guard, so we never land back on a tab that's also being closed. */
export function siblingOrHomeHref(s: Session): string {
  const sib = newestOpenSessionForFolderKey(folderKeyOf(s), s.id);
  return sib ? `/s/${sib.id}` : "/";
}

/** The deferred-kill thunk handed to scheduleClose: after the undo window it
 *  fires sessionsKill in the background. accepted:false (owning worker offline)
 *  → force-tombstone so the row can't zombie back; handlers-sessions.ts honors
 *  force in its offline branch. A throw surfaces one "Close failed" toast.
 *  (Absorbs the deleted terminalActions.killSession — reconcile-on-failure.) */
export function killAfterUndo(sessionId: string): () => void {
  return () => { void (async () => {
    try {
      const res = await coordClient.sessionsKill({ sessionId });
      if (!res.accepted) await coordClient.sessionsKill({ sessionId, force: true });
    } catch (e) {
      log.warn("closeSession", "close_failed", { sid: sessionId, msg: String(e) });
      addToast(`Close failed: ${e instanceof Error ? e.message : String(e)}`, "err");
    }
  })(); };
}
