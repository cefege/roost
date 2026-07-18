// Viewer-presence tracker — source of truth for "which browsers are looking
// at which session". Driven by sessionsResize claims (each claim is a
// heartbeat; cols=0||rows=0 is a withdraw). Fans out on globalPresenceBus
// {kind:"viewers", fps:[...]}; the SPA folds it into rootStore.session_viewers
// and SessionRow renders one colored dot per fp. VIEWER_TTL_MS +
// VIEWER_WITHDRAW_GRACE_MS are shared with the worker (@roost/shared/viewport)
// so the two sides' viewer sets can't desync — coord drives the SPA's
// session_viewers projection, the worker drives the PTY, both on one schedule.
//
// Module-level singleton state (one tracker per coord). Importing this module
// starts the TTL reaper + session-close subscription. Consumers: router.ts
// (buildConnectRouter wires the DB; sessions resize bumps; diagSnapshot reads
// the map) and the auth/pair handlers (invalidate the label cache on relabel).
// Split out of router.ts so those handler files can share it without a cycle.

import type { KyselyDB } from "../db/connection.ts";
import { globalPresenceBus, sessionBus } from "../buses.ts";
import { resolveHostname } from "../tailnet-resolver.ts";
import { VIEWER_WITHDRAW_GRACE_MS, VIEWER_CLAIM_TTL_MS as VIEWER_TTL_MS } from "@roost/shared/viewport";

const VIEWER_REAP_INTERVAL_MS = 10_000;
const _pendingViewerWithdraws = new Map<string, ReturnType<typeof setTimeout>>();
interface ViewerEntry { cols: number; rows: number; lastMs: number; clientSeq: number; lastIp?: string }
export const _viewersBySession = new Map<string, Map<string, ViewerEntry>>();
// fp → authorized_keys.label cache. Populated lazily on first publish.
// Invalidated whenever a label changes via _invalidateLabel (called from
// authAuthorizeBrowser + pairApprove). _viewerTrackerDb is wired once from
// buildConnectRouter — keeps callsites grep-able.
const _labelByFp = new Map<string, string>();
let _viewerTrackerDb: KyselyDB | null = null;
export function _setViewerTrackerDb(db: KyselyDB): void { _viewerTrackerDb = db; }
export function _invalidateLabel(fp: string): void { _labelByFp.delete(fp); }
async function _labelFor(fp: string): Promise<string | undefined> {
  const cached = _labelByFp.get(fp);
  if (cached !== undefined) return cached;
  if (!_viewerTrackerDb) return undefined;
  const row = await _viewerTrackerDb.selectFrom("authorized_keys").select("label")
    .where("fingerprint", "=", fp).executeTakeFirst();
  if (row?.label) _labelByFp.set(fp, row.label);
  return row?.label ?? undefined;
}
// Combine browser self-label + tailnet hostname (reverse-resolved from
// the client's tailnet IP via apps/coord/src/tailnet-resolver.ts). User
// chose AUGMENT (not replace): "Chrome — macOS on <tailnet-host>". Either
// piece may be missing → graceful fallback to whichever is present, or
// to the 8-char fp prefix (SPA-side fallback in ViewersChip).
function _composeViewerLabel(self: string | undefined, host: string | null): string | undefined {
  if (self && host) return `${self} on ${host}`;
  return self ?? host ?? undefined;
}
async function _publishViewers(sessionId: string): Promise<void> {
  const m = _viewersBySession.get(sessionId);
  const entries = m
    ? await Promise.all(Array.from(m.entries()).map(async ([fp, e]) => ({
        fp,
        cols: e.cols,
        rows: e.rows,
        lastMs: e.lastMs,
        label: _composeViewerLabel(await _labelFor(fp), resolveHostname(e.lastIp)),
      })))
    : [];
  globalPresenceBus.publish({
    session_id: sessionId,
    data: {
      kind: "viewers",
      fps: entries.map((e) => e.fp),
      entries,
    },
  });
}
export function _bumpViewer(
  sessionId: string,
  viewerFp: string,
  cols: number,
  rows: number,
  clientSeq?: number,
  clientIp?: string,
): void {
  let m = _viewersBySession.get(sessionId);
  const pendKey = `${sessionId}:${viewerFp}`;
  if (cols <= 0 || rows <= 0) {
    // Deferred withdraw (hysteresis): a refresh re-claims within the
    // grace and cancels this → no session_viewers flap → no scrollback
    // re-serialize on peer SPAs. A genuine close just removes the viewer
    // VIEWER_WITHDRAW_GRACE_MS later.
    if (!m || !m.has(viewerFp)) return;
    if (_pendingViewerWithdraws.has(pendKey)) return;
    const timer = setTimeout(() => {
      _pendingViewerWithdraws.delete(pendKey);
      const live = _viewersBySession.get(sessionId);
      if (!live || !live.delete(viewerFp)) return;
      if (live.size === 0) _viewersBySession.delete(sessionId);
      void _publishViewers(sessionId);
    }, VIEWER_WITHDRAW_GRACE_MS);
    _pendingViewerWithdraws.set(pendKey, timer);
    return;
  }
  // Real claim → cancel any pending deferred withdraw for this viewer.
  const pend = _pendingViewerWithdraws.get(pendKey);
  if (pend) { clearTimeout(pend); _pendingViewerWithdraws.delete(pendKey); }
  if (!m) { m = new Map(); _viewersBySession.set(sessionId, m); }
  const prev = m.get(viewerFp);
  const priorSeq = prev?.clientSeq ?? -1;
  const seq = clientSeq ?? priorSeq + 1;
  const seqAdvanced = seq > priorSeq;
  if (prev && !seqAdvanced) {
    // Stale-seq packet (heartbeat or WAN reorder): refresh liveness only,
    // DON'T overwrite dims (a reordered old packet must not regress the
    // SCD min). clientSeq is kept purely for this reorder guard now that
    // the latest-pointer is gone (SCD min is order-independent).
    prev.lastMs = Date.now();
    if (clientIp) prev.lastIp = clientIp;
    return;
  }
  m.set(viewerFp, { cols, rows, lastMs: Date.now(), clientSeq: seq, lastIp: clientIp });
  // Republish on membership/dims change so peers recompute the SCD min.
  void _publishViewers(sessionId);
}
setInterval(() => {
  const now = Date.now();
  for (const [sid, m] of _viewersBySession) {
    let dropped = false;
    for (const [fp, e] of m) {
      if (now - e.lastMs > VIEWER_TTL_MS) {
        m.delete(fp);
        dropped = true;
      }
    }
    if (dropped) {
      if (m.size === 0) _viewersBySession.delete(sid);
      void _publishViewers(sid);
    }
  }
}, VIEWER_REAP_INTERVAL_MS).unref?.();

// Session close fans through sessionBus as a `closed` SessionEvent.
// Drop the viewer entry immediately + broadcast empty viewers so the
// SPA can clear sidebar dots without waiting on TTL. Without this the
// map keeps dead sessionId entries until reaper drops the last stale
// claim (up to 120s) — and any stale heartbeat from a tab that didn't
// yet learn the session died would re-add the entry.
sessionBus.subscribe((ev) => {
  if (ev.kind !== "closed") return;
  const sid = String(ev.session_id);
  if (!_viewersBySession.has(sid)) return;
  _viewersBySession.delete(sid);
  void _publishViewers(sid);
});
