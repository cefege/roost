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
): void {
	if (cols <= 0 || rows <= 0) {
		this.withdrawViewport(channelId, viewerFp);
		return;
	}
	if (!this.sessions.has(channelId)) return;
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
		const rec = this.sessions.get(channelId);
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
		if (intentMount) this.emitCellSnapshot(channelId as ChannelId);
		return;
	}
	claims.set(viewerFp, { cols, rows, lastMs: Date.now(), clientSeq: seq });
	const rec = this.sessions.get(channelId);
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
	this._recomputeViewport(channelId);
	// R11 — a claim is the worker's "viewer attached/resized" signal; emit a
	// full cell frame so a fresh cell-mode viewer paints the whole grid
	// immediately (live deltas follow on the next PTY chunk). No-op off-flag.
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
 *  reflows to the current PTY width. */
export function _recomputeViewport(this: SessionManager, channelId: number): void {
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
	this._scheduleWtermRebuild(channelId, cols, rows);
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
): void {
	const prior = this._wtermRebuildChain.get(channelId) ?? Promise.resolve();
	const next = prior
		.catch(() => {})
		.then(() => this._rebuildWtermCore(channelId, cols, rows));
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
): Promise<void> {
	const rec0 = this.sessions.get(channelId);
	if (!rec0) return;
	// Skip rebuild if the wtermCore is already at the target size — no reflow
	// needed, and the claim path (emitCellSnapshot) already sent a full frame.
	// Avoids a redundant full-frame emit that the leading-edge cell emit exposes
	// (the old 16ms timer masked it by clearing before the rebuild fired).
	if (rec0.wtermCore.getCols() === cols && rec0.wtermCore.getRows() === rows) return;
	const fresh = await _createWtermCore(cols, rows);
	const rec = this.sessions.get(channelId);
	if (!rec) return;
	// ALT-SCREEN (claude): do NOT replay the raw ring into the new-width core.
	// The ring holds alt-screen bytes (absolute cursor moves, line clears)
	// PAINTED FOR THE OLD WIDTH; replaying them into a different-width grid
	// re-stamps the same logical line across many rows with tail mangling —
	// the "claude history fucked up on resize / multi-device" corruption
	// (project_terminal_history_corruption_viewport_slaved_pty; screenshot
	// 2026-06-22). This is the WORKER-side counterpart to the client fix
	// d745b1e3 — the worker's authoritative core mangled the same way and
	// shipped it to cell viewers. Instead: start the fresh core EMPTY +
	// alt-primed; claude REPAINTS the whole screen at the new cols via the
	// SIGWINCH already fired in _recomputeViewport (winsize → live byte
	// stream). Brief stale/blank frame until the repaint, NEVER a persistent
	// mangle. Plain shells DO replay — text reflows cleanly and the raw ring
	// is their history source.
	// Kind-AGNOSTIC: any alt-screen session (claude OR vim/htop in a shell)
	// mangles if the alt ring is replayed at a new width. Gate on the
	// stream-driven alt_mode alone, not kind.
	const isAltScreen = rec.alt_mode;
	if (!isAltScreen && rec.scrollback.length > 0)
		fresh.writeRaw(rec.scrollback);
	// Prime alt-screen state so the rebuilt core's usingAltScreen() matches
	// rec.alt_mode (L11 "stale text wallpaper after worker restart").
	if (rec.alt_mode && !fresh.usingAltScreen()) {
		fresh.writeRaw(ALT_ENTER_SEQS[0]);
	}
	// Discard capability replies from the ring replay — same reason as resume():
	// these probes are historical, not live; re-answering claude would corrupt
	// stdin. The live chunk handler owns the real reply routing.
	fresh.getResponse();
	rec.wtermCore = fresh;
	// R11 — the core is a brand-new instance; emit a forced full cell frame
	// now so viewers reflect the resize without waiting for the next PTY
	// chunk. A delta is meaningless here — dirty bits + cursor on the fresh
	// core don't describe a change from the OLD core the client holds.
	this.emitCellSnapshot(channelId as ChannelId);
	// mode must report the ACTUAL branch: alt-screen claude builds an EMPTY +
	// alt-primed core (ring NOT replayed — the anti-mangle path); only shells
	// replay the ring. The old unconditional "rebuild_from_ring" label lied for
	// claude and falsely reads as the corruption path in worker logs.
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
