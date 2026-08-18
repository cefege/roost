// Multi-viewer SCD viewport claims. The claim map is the browser-facing intent;
// every SIZE decision it implies is executed by the one terminal-control owner
// (session-terminal-control.ts::reconcileTerminalViewport → the transaction in
// session-terminal-txn.ts), which also owns the keeper resize, the resize
// capture, and the single core rebuild. Nothing here writes to the keeper.

import type { SessionManager } from "./session-manager.ts";
import type { ChannelId } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import {
	VIEWER_WITHDRAW_GRACE_MS as VIEWPORT_WITHDRAW_GRACE_MS,
	VIEWER_CLAIM_TTL_MS as VIEWPORT_CLAIM_TTL_MS,
	VIEWER_CLAIM_FRESH_MS,
} from "@roost/shared/viewport";

/** Does this claim have to carry a full frame, or is the claimant provably
 *  holding the current grid? Two conditions must both hold to skip it:
 *  the claimant reports the seq of the last frame this channel emitted, AND
 *  emission never stopped while it was away (`wasStreaming` — some claim was
 *  live). The second is load-bearing: with no claim, _hasActiveViewer gates
 *  emission off (session-emit.ts), so the grid keeps mutating while
 *  cell_emit.seq freezes and a seq match would prove nothing. Unknown seq (0 /
 *  undefined) or behind → emit. A mismatch caused by a frame still in flight
 *  costs one redundant snapshot, so this errs toward emitting. */
export function needsClaimSnapshot(
	mgr: SessionManager, channelId: number, heldCellSeq: number | undefined, wasStreaming: boolean,
): boolean {
	if (!heldCellSeq || !wasStreaming) return true;
	return mgr.shellByChannel(channelId)?.cell_emit.seq !== heldCellSeq;
}

/** Current smallest-common dimensions across fresh, sizing claims. Background
 * (0×0) claims keep output live without constraining the PTY. */
export function desiredViewportSize(
	this: SessionManager,
	channelId: number,
): { cols: number; rows: number } | null {
	const claims = this.viewportClaims.get(channelId);
	if (!claims || claims.size === 0) return null;
	const now = Date.now();
	let minCols = Infinity;
	let minRows = Infinity;
	for (const claim of claims.values()) {
		if (now - claim.lastMs > VIEWER_CLAIM_FRESH_MS) continue;
		if (claim.cols > 0 && claim.cols < minCols) minCols = claim.cols;
		if (claim.rows > 0 && claim.rows < minRows) minRows = claim.rows;
	}
	return minCols === Infinity || minRows === Infinity
		? null
		: { cols: minCols, rows: minRows };
}

/** Register or refresh a viewer's viewport claim, then resize the
 *  PTY to the SCD across live claims. Each browser viewing the same
 *  session has its own claim keyed by its EdDSA fingerprint. Two
 *  browsers at different window sizes → PTY shrinks to the smaller,
 *  preventing the "scrambled redraw on the smaller viewer" symptom.
 *  cols=0 OR rows=0 → withdraw this viewer's claim.
 *  The SCD math lives in desiredViewportSize; the resize it implies is executed
 *  by reconcileTerminalViewport on the terminal-control lane. */
export function claimViewport(
	this: SessionManager,
	channelId: number,
	viewerFp: string,
	cols: number,
	rows: number,
	clientSeq?: number | bigint,
	// numeric roost.v1.ResizeCause — the browser event behind this claim
	// (1=INITIAL, 2=VIEWPORT, 3=TAB_VISIBLE, 4=WITHDRAW, 5=BACKGROUND,
	// 6=HEARTBEAT). Hint only; cell snapshots remain state-based.
	cause?: number,
	// cell-frame seq this viewer has already applied — a claimant that already
	// holds the last emitted frame gets no snapshot (_needsClaimSnapshot)
	heldCellSeq?: number,
): void {
	// BACKGROUND (5): a parked deck pane. It sends 0×0 so it is excluded from the
	// SCD min, but it stays in viewportClaims so _hasActiveViewer keeps deltas
	// flowing — the pane is already current when it is revealed.
	const isBackground = cause === 5;
	if (!isBackground && (cols <= 0 || rows <= 0)) {
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
	// Was the worker still emitting to SOMEONE when this claim arrived? A live
	// claim (this viewer's own, or another viewer's) means every PTY chunk since
	// the claimant's held frame was emitted, which is what makes a matching
	// held_cell_seq proof that it is current. Read BEFORE this claim is written.
	const wasStreaming = claims.size > 0;
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
	const priorSeq = prior?.clientSeq ?? -1n;
	const seq = clientSeq === undefined ? priorSeq + 1n : BigInt(clientSeq);
	const seqAdvanced = seq > priorSeq;
	if (prior && !seqAdvanced) {
		// Stale-seq packet (heartbeat or WAN reorder): refresh lastMs so the
		// TTL reaper sees the viewer is alive, but DON'T overwrite dims or
		// recompute SCD. INITIAL/TAB_VISIBLE repair a seq-epoch reset after reload;
		// HEARTBEAT compares the viewer's applied watermark so a dropped final
		// frame self-heals even when no later delta exposes the gap.
		prior.lastMs = Date.now();
		const snapshotCheckCause = cause === 1 || cause === 3 || cause === 6;
		const resnapshot = snapshotCheckCause
			&& needsClaimSnapshot(this, channelId, heldCellSeq, wasStreaming);
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
			resnapshot,
		});
		if (resnapshot)
			this.emitCellSnapshot(channelId as ChannelId);
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
	this.reconcileTerminalViewport(channelId);
	// R11 — a claim is the worker's "viewer attached/resized" signal. It emits a
	// full cell frame only when the claimant is not provably current: a fresh or
	// fallen-behind viewer paints the whole grid immediately (live deltas follow
	// on the next PTY chunk), while a pane that never stopped streaming reveals
	// with no repaint at all. A background claim is NOT categorically excluded —
	// a parked pane re-subscribing after a withdraw needs its catch-up frame —
	// but it still must not size another viewer's rebuild tail (above).
	if (needsClaimSnapshot(this, channelId, heldCellSeq, wasStreaming))
		this.emitCellSnapshot(channelId as ChannelId);
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
		this.reconcileTerminalViewport(channelId);
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
		if (dropped || anyStale) this.reconcileTerminalViewport(channelId);
	}
}
