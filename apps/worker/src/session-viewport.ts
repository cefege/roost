// Multi-viewer SCD viewport claims + deterministic wtermCore rebuild-on-resize.
// Split out of session-manager.ts (400-line cap); bodies byte-for-byte
// unchanged, called with a SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import type { ChannelId } from "@roost/shared";
import { log, diag } from "@roost/shared";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { ALT_ENTER_SEQS } from "./terminal-stream-scan.ts";
import { _createWtermCore } from "./session-constants.ts";
import {
	VIEWER_WITHDRAW_GRACE_MS as VIEWPORT_WITHDRAW_GRACE_MS,
	VIEWER_CLAIM_TTL_MS as VIEWPORT_CLAIM_TTL_MS,
	VIEWER_CLAIM_FRESH_MS,
} from "@roost/shared/viewport";
import { SB_SNAPSHOT_TAIL_ROWS, SB_SNAPSHOT_MAX_CATCHUP_ROWS, initCellEmitState } from "@roost/shared/cell";

/** Scrollback rows the claim snapshot must carry so it EXTENDS what the
 *  returning viewer already painted. The viewer holds through its last held
 *  row, heldSbTotal-1; mergeFullFrame needs the tail to include that row, i.e.
 *  total - heldSbTotal + 1 rows. Floored at the standard tail, capped at
 *  SB_SNAPSHOT_MAX_CATCHUP_ROWS. Unknown/zero/ahead-of-us → the default.
 *  `total` is MONOTONIC (sbDropped + retained count) because heldSbTotal is —
 *  comparing it against the raw retained count would go negative on any
 *  session whose ring has evicted, silently falling back to the 250-row tail
 *  for exactly the long-lived sessions this sizing exists to serve. */
function _claimTailRows(mgr: SessionManager, channelId: number, heldSbTotal?: number): number {
	if (!heldSbTotal || heldSbTotal <= 0) return SB_SNAPSHOT_TAIL_ROWS;
	const rec = mgr.sessions.get(channelId);
	const total = (rec?.cell_emit.sbDropped ?? 0) + (rec?.wtermCore.getScrollbackCount() ?? 0);
	const need = total - heldSbTotal + 1;
	return Math.min(Math.max(need, SB_SNAPSHOT_TAIL_ROWS), SB_SNAPSHOT_MAX_CATCHUP_ROWS);
}

/** Register or refresh a viewer's viewport claim, then resize the
 *  PTY to the SCD across live claims. Each browser viewing the same
 *  session has its own claim keyed by its EdDSA fingerprint. Two
 *  browsers at different window sizes → PTY shrinks to the smaller,
 *  preventing the "scrambled redraw on the smaller viewer" symptom.
 *  cols=0 OR rows=0 → withdraw this viewer's claim.
 *  See _recomputeViewport for the SCD math + SIGWINCH gating. */
export function claimViewport(
	this: SessionManager,
	channelId: number,
	viewerFp: string,
	cols: number,
	rows: number,
	clientSeq?: number,
	// numeric roost.v1.ResizeCause — the browser event behind this claim
	// (1=INITIAL, 2=VIEWPORT, 3=TAB_VISIBLE, 4=WITHDRAW). Hint only: the claim
	// already force-emits a full cell frame, so a reattach (INITIAL/TAB_VISIBLE)
	// paints immediately. Recorded in diag for resize-pathology forensics.
	cause?: number,
	// rows this viewer already holds — sizes the snapshot tail
	heldSbTotal?: number,
): void {
	if (cols <= 0 || rows <= 0) {
		this.withdrawViewport(channelId, viewerFp);
		return;
	}
	// Ignore a viewport claim that races a teardown. A live session always
	// owns a terminal core.
	const rec = this.sessions.get(channelId);
	if (!rec) return;
	// A real claim cancels any in-flight deferred withdraw for this
	// viewer (refresh re-claimed within the grace) → no size flap.
	this._cancelPendingWithdraw(channelId, viewerFp);
	let claims = this.viewportClaims.get(channelId);
	if (!claims) {
		claims = new Map();
		this.viewportClaims.set(channelId, claims);
	}
	// clientSeq solves two problems at once:
	//   #1 network reorder: SPA stamps a monotonic counter on each
	//      intent-bearing claim (focus / resize / visibilitychange).
	//      A late-arriving packet has a stale seq → ignored for
	//      latest-pointer purposes (lastMs still refreshes so the TTL
	//      reaper doesn't drop it).
	//   #6 dual-focus heartbeat flap: heartbeat re-sends with the SAME
	//      seq as the last intent-bearing claim → worker sees seq
	//      unchanged → no bump. Two windows both heartbeating no
	//      longer flap the pointer.
	// Calls without clientSeq (legacy in-process tests, or pre-rollout
	// SPA builds) treat every claim as an intent — bump unconditionally.
	// That preserves the prior latest-wins semantics for those callers.
	const prior = claims.get(viewerFp);
	const priorSeq = prior?.clientSeq ?? -1;
	const seq = clientSeq ?? priorSeq + 1; // legacy: synthesize fresh
	const seqAdvanced = seq > priorSeq;
	if (prior && !seqAdvanced) {
		// Stale-seq packet (heartbeat or WAN reorder): refresh lastMs so the
		// TTL reaper sees the viewer is alive, but DON'T overwrite dims (a
		// reordered old packet must not regress the SCD min). clientSeq is
		// kept purely for this reorder guard now that latest-wins is gone.
		prior.lastMs = Date.now();
		// SEQ-EPOCH RESET on reload: a fresh page-load's per-mount claim counter
		// resets to 1, colliding with the prior page's last seq (same stable
		// viewer_key, kept stable for the withdraw-grace) → stale by seq. But an
		// INITIAL (1) / TAB_VISIBLE (3) cause means the viewer just MOUNTED and
		// has nothing painted — it MUST get a full frame or it stays blank
		// forever (deltas drop with no base). Emit the snapshot; skip the SCD
		// recompute (don't let a stale-seq packet regress the min size). A
		// heartbeat re-claim (VIEWPORT cause) stays a no-op → no full-frame spam.
		const intentMount = cause === 1 || cause === 3; // INITIAL | TAB_VISIBLE
		diag("viewport.claim", {
			sid: rec?.sessionId,
			viewer_key: viewerFp,
			channel_id: channelId,
			session_trace_id: rec?.session_trace_id,
			cols,
			rows,
			client_seq: seq,
			prev_seq: priorSeq,
			seq_advanced: false,
			was_stale_seq: true,
			cause: cause ?? 0,
			resnapshot: intentMount,
		});
		if (intentMount) this.emitCellSnapshot(channelId as ChannelId, _claimTailRows(this, channelId, heldSbTotal));
		return;
	}
	claims.set(viewerFp, { cols, rows, lastMs: Date.now(), clientSeq: seq });
	diag("viewport.claim", {
		sid: rec?.sessionId,
		viewer_key: viewerFp,
		channel_id: channelId,
		session_trace_id: rec?.session_trace_id,
		cols,
		rows,
		client_seq: seq,
		prev_seq: priorSeq,
		seq_advanced: true,
		cause: cause ?? 0,
	});
	this._recomputeViewport(channelId, heldSbTotal);
	// R11 — a claim is the worker's "viewer attached/resized" signal; emit a
	// full cell frame so a fresh cell-mode viewer paints the whole grid
	// immediately (live deltas follow on the next PTY chunk). No-op off-flag.
	this.emitCellSnapshot(channelId as ChannelId, _claimTailRows(this, channelId, heldSbTotal));
}

/** Drop a viewer's claim, DEFERRED by VIEWPORT_WITHDRAW_GRACE_MS so a
 *  refresh's re-claim cancels it (see _cancelPendingWithdraw, called
 *  from claimViewport). Fires on SPA visibility-hidden / pagehide.
 *  Triggers a PTY recompute only after the grace elapses without a
 *  re-claim — so a refresh no longer flaps the SCD size + scrollback. */
export function withdrawViewport(this: SessionManager, channelId: number, viewerFp: string): void {
	const claims = this.viewportClaims.get(channelId);
	if (!claims || !claims.has(viewerFp)) return;
	const key = `${channelId}:${viewerFp}`;
	if (this.pendingWithdraws.has(key)) return; // already scheduled
	const rec = this.sessions.get(channelId);
	diag("viewport.withdraw", {
		sid: rec?.sessionId,
		viewer_key: viewerFp,
		channel_id: channelId,
		session_trace_id: rec?.session_trace_id,
		claims_left: claims.size,
		deferred: true,
		grace_ms: VIEWPORT_WITHDRAW_GRACE_MS,
	});
	const timer = setTimeout(() => {
		this.pendingWithdraws.delete(key);
		const live = this.viewportClaims.get(channelId);
		if (!live || !live.delete(viewerFp)) return;
		this._recomputeViewport(channelId);
	}, VIEWPORT_WITHDRAW_GRACE_MS);
	this.pendingWithdraws.set(key, timer);
}

/** Cancel a pending deferred withdraw for this viewer (it re-claimed
 *  within the grace window — e.g. a refresh completed). No recompute:
 *  the size never changed. */
export function _cancelPendingWithdraw(this: SessionManager, channelId: number, viewerFp: string): void {
	const key = `${channelId}:${viewerFp}`;
	const timer = this.pendingWithdraws.get(key);
	if (timer) {
		clearTimeout(timer);
		this.pendingWithdraws.delete(key);
	}
}

/** Reaper: drops claims older than VIEWPORT_CLAIM_TTL_MS. Live
 *  viewers refresh every 30s; anything older is a stale browser.
 *  Also recomputes while ANY claim sits past VIEWER_CLAIM_FRESH_MS:
 *  crossing the freshness cutoff removes that claim from the SCD min
 *  (_recomputeViewport skips it) but arrives with NO event — the
 *  surviving viewer's same-seq heartbeats never recompute, so without
 *  this tick a dead viewer's smaller size pins the PTY for the full
 *  TTL instead of releasing at the freshness cutoff. */
export function _reapViewportClaims(this: SessionManager): void {
	const now = Date.now();
	for (const [channelId, claims] of this.viewportClaims) {
		let dropped = false;
		let anyStale = false;
		for (const [fp, claim] of claims) {
			const age = now - claim.lastMs;
			if (age > VIEWPORT_CLAIM_TTL_MS) {
				claims.delete(fp);
				dropped = true;
			} else if (age > VIEWER_CLAIM_FRESH_MS) {
				anyStale = true;
			}
		}
		if (dropped || anyStale) this._recomputeViewport(channelId);
	}
}

/** Recompute PTY size = SCD min(cols)×min(rows) across live claims.
 *  SIGWINCH only if changed from the last applied size. wtermCore
 *  tracks the same size so server-side scrollback serialization
 *  reflows to the current PTY width. `heldSbTotal` (claim path only) is
 *  forwarded to the rebuild so its snapshot reaches back to the claimant's
 *  boundary row instead of the 250-row default. */
export function _recomputeViewport(this: SessionManager, channelId: number, heldSbTotal?: number): void {
	const rec = this.sessions.get(channelId);
	if (!rec) {
		// Session gone — drop any leftover claims so the maps don't grow
		// unbounded across spawn/kill churn.
		this.viewportClaims.delete(channelId);
		this.lastAppliedSize.delete(channelId);
		return;
	}
	const claims = this.viewportClaims.get(channelId);
	if (!claims || claims.size === 0) {
		// No viewer is claiming — leave the PTY at whatever it last had.
		// Resizing to a default would thrash the running TUI for no gain.
		return;
	}
	// SCD — smallest-common-denominator. PTY = min(cols) × min(rows)
	// across ALL active claims (each dimension min'd independently, =
	// the intersection every viewer can fully render). NO viewer is
	// ever clipped. min is order-independent + idempotent → a stable
	// fixed point: a refresh/focus can never hijack the size and the
	// recompute can't oscillate (the WINDOW_SIZE_LATEST pathology, Author
	// 2026-06-18 "refresh gains priority over everything"). N==1 falls
	// out for free (min of one claim = that claim = perfect fit).
	// See memory feedback_viewport_scd_min_policy.
	// A3: liveness-weighted SCD. Exclude claims not refreshed within
	// VIEWER_CLAIM_FRESH_MS (~2× heartbeat) from the min — a dead viewer
	// (kill-9/WiFi/sleep, no graceful withdraw) otherwise pins everyone to
	// its tiny window for the full 120s TTL. If EVERY claim is stale, leave
	// the PTY where it is (don't thrash) and let the reaper remove them.
	const now = Date.now();
	let minCols = Infinity,
		minRows = Infinity;
	for (const c of claims.values()) {
		if (now - c.lastMs > VIEWER_CLAIM_FRESH_MS) continue;
		if (c.cols > 0 && c.cols < minCols) minCols = c.cols;
		if (c.rows > 0 && c.rows < minRows) minRows = c.rows;
	}
	if (minCols === Infinity || minRows === Infinity) return;
	const cols = minCols,
		rows = minRows;
	const chosen_key =
		claims.size === 1 ? (claims.keys().next().value as string) : "scd_min";
	const chose_via: "only" | "scd_min" =
		claims.size === 1 ? "only" : "scd_min";
	const last = this.lastAppliedSize.get(channelId);
	const will_signal = !(last && last.cols === cols && last.rows === rows);
	diag("viewport.recompute", {
		sid: rec.sessionId,
		channel_id: channelId,
		session_trace_id: rec.session_trace_id,
		chosen_key,
		claims_size: claims.size,
		chose_via,
		cols,
		rows,
		signaled: will_signal,
		prev_cols: last?.cols ?? null,
		prev_rows: last?.rows ?? null,
	});
	if (!will_signal) return;
	getMultiplexedPool().resize(channelId, cols, rows);
	diag("resize.pty_signal", {
		sid: rec.sessionId,
		channel_id: channelId,
		session_trace_id: rec.session_trace_id,
		cols,
		rows,
	});
	// OPT2-1: deterministic rebuild from the raw ring instead of the
	// path-dependent in-place wtermCore.resize (see _wtermRebuildChain).
	this._scheduleWtermRebuild(channelId, cols, rows, heldSbTotal);
	this.lastAppliedSize.set(channelId, { cols, rows });
	log.info("session-manager", "viewport_scd_resize", {
		channelId,
		cols,
		rows,
		claimCount: claims.size,
	});
}

/** OPT2-1: chain a deterministic wtermCore rebuild so concurrent resizes
 *  don't overlap (each would `await _createWtermCore` and race the swap).
 *  The chain is per-channel; a failed rebuild doesn't poison the next. */
export function _scheduleWtermRebuild(
	this: SessionManager,
	channelId: number,
	cols: number,
	rows: number,
	heldSbTotal?: number,
): void {
	const prior = this._wtermRebuildChain.get(channelId) ?? Promise.resolve();
	const next = prior
		.catch(() => {})
		.then(() => this._rebuildWtermCore(channelId, cols, rows, heldSbTotal));
	this._wtermRebuildChain.set(channelId, next);
}

/** Build a fresh wtermCore at cols×rows and replay the raw ring into it,
 *  then swap it in. The raw ring (rec.scrollback) is the single source of
 *  truth, so the rebuilt grid is a pure function of (ring, cols, rows) —
 *  no path-dependence, no asymmetric-resize drift. The swap tail is sync
 *  (no await between reading rec.scrollback and assigning rec.wtermCore),
 *  so a PTY chunk arriving mid-rebuild is either already in the ring we
 *  replay or lands on the new core via the next appendScrollback — never
 *  lost or duplicated.
 *  ponytail: replays the full ring (≤8 MB) on every deliberate resize.
 *  Resizes are rare (viewport hysteresis), so this is cheap in practice;
 *  if a deep-ring replay ever shows up on the resize hot path, switch to
 *  replay-from-last-applied-seqno or reuse the bridge via init(). */
export async function _rebuildWtermCore(
	this: SessionManager,
	channelId: number,
	cols: number,
	rows: number,
	heldSbTotal?: number,
): Promise<void> {
	const rec0 = this.sessions.get(channelId);
	// The chain is timer-driven: teardown can remove the terminal core before
	// this queued rebuild executes.
	if (!rec0?.wtermCore) return;
	// Skip rebuild if the wtermCore is already at the target size — no reflow
	// needed, and the claim path (emitCellSnapshot) already sent a full frame.
	// Avoids a redundant full-frame emit that the leading-edge cell emit exposes
	// (the old 16ms timer masked it by clearing before the rebuild fired).
	if (rec0.wtermCore.getCols() === cols && rec0.wtermCore.getRows() === rows) return;
	const fresh = await _createWtermCore(cols, rows);
	const rec = this.sessions.get(channelId);
	if (!rec) return;
	// Alt-screen: do NOT replay the raw ring into the new-width core. The ring
	// holds absolute cursor moves and line clears painted for the old width;
	// replaying it at a new width duplicates and mangles rows. Start an empty,
	// alt-primed core instead. The TUI repaints at the new size after SIGWINCH.
	// Main-screen sessions replay their ring because text reflows cleanly. This
	// is stream-driven: any alt-screen TUI takes the empty-core branch.
	const isAltScreen = rec.alt_mode;
	if (!isAltScreen && rec.scrollback.length > 0)
		fresh.writeRaw(rec.scrollback);
	// Prime alt-screen state so the rebuilt core's usingAltScreen() matches
	// rec.alt_mode (L11 "stale text wallpaper after worker restart").
	if (rec.alt_mode && !fresh.usingAltScreen()) {
		fresh.writeRaw(ALT_ENTER_SEQS[0]);
	}
	// Discard historical capability replies from the ring replay. The live
	// chunk handler owns the actual reply route back into the PTY.
	fresh.getResponse();
	rec.wtermCore = fresh;
	// Fresh core, fresh ring: the retained-index origin restarts at 0, so the
	// monotonic origin must absorb the whole difference or every index the SPA
	// holds re-aliases and scrollbackTotal REWINDS (which parks the backfill
	// controller and defeats _claimTailRows). The replay reproduces the same
	// raw ring, so the NEWEST line is the same line in both cores — pin that:
	// sbDropped = prevMonoTotal - freshCount. Reflow at the new width shifts
	// older rows by the reflow delta, which no bookkeeping can avoid; the newest
	// row, the one the reader is measured from, stays exact. Alt-screen rebuilds
	// replay nothing (freshCount 0) and correctly report their whole history as
	// dropped. seq is kept so the SPA's gap detector doesn't see a rewind; the
	// zeroed cols/rows/sentFull force the next emit to be a full frame.
	rec.cell_emit = {
		...initCellEmitState(),
		seq: rec.cell_emit.seq,
		sbDropped: Math.max(0, rec.cell_emit.lastSbTotal - fresh.getScrollbackCount()),
	};
	// R11 — the core is a brand-new instance; emit a forced full cell frame
	// now so viewers reflect the resize without waiting for the next PTY
	// chunk. A delta is meaningless here — dirty bits + cursor on the fresh
	// core don't describe a change from the OLD core the client holds.
	// Sized from the CLAIMANT's held total: _scheduleWtermRebuild defers onto a
	// promise chain, so this frame lands AFTER claimViewport's correctly-sized
	// one and is the frame the SPA ends on. At the 250-row default the returning
	// viewer's last held row falls below sbBase and the pane shows only a shallow
	// window. Evaluated here, after the swap, so _claimTailRows reads the
	// REBUILT core's scrollback count.
	this.emitCellSnapshot(channelId as ChannelId, _claimTailRows(this, channelId, heldSbTotal));
	// Report the actual branch: alt-screen rebuilds start empty and alt-primed;
	// main-screen rebuilds replay the retained ring.
	diag("resize.wterm_core", {
		sid: rec.sessionId,
		channel_id: channelId,
		session_trace_id: rec.session_trace_id,
		cols,
		rows,
		mode: isAltScreen ? "empty_alt_primed" : "rebuild_from_ring",
		replayed_ring: !isAltScreen,
		ring_bytes: rec.scrollback.length,
	});
}
