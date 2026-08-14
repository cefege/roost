// Which sessions each browser TAB streams authoritative cell frames for.
// Driven by sessionsResize (handlers-sessions.ts) and read by the Sync fanout
// (handlers-streaming.ts::startSyncFeed), so a tab is no longer sent — and no
// longer re-serialized — every cell frame on the fleet. Raw PTY bytes never
// enter browser Sync; compact terminal-link mappings are intentionally global.
//
// Mirrors the worker's viewportClaims: positive claims subscribe, while current
// browsers send a real 0×0 WITHDRAW as soon as a pane becomes hidden/offscreen.
// Withdraw tombstones, session close, and the shared claim TTL prune state.
//
// Module-level singleton (one per coord), same shape and schedule as
// viewer-tracker.ts: importing this module starts the TTL reaper and the
// session-close subscription.

import { sessionBus } from "../buses.ts";
import { VIEWER_CLAIM_TTL_MS } from "@roost/shared/viewport";

const REAP_INTERVAL_MS = 10_000;

interface CellSubscription {
	readonly subscribed: boolean;
	// Highest effective intent sequence observed for this viewer/session. Keep
	// it across WITHDRAW as a tombstone so a delayed older claim cannot revive
	// it. Legacy sequence zero is translated into the next effective sequence.
	readonly clientSeq: bigint;
	readonly lastMs: number;
}

/** An accepted mutation. Its object identity makes rollback conditional: an
 * older failed send can restore its exact predecessor only while no newer
 * mutation has replaced the installed entry. */
export interface CellSubscriptionMutation {
	readonly effectiveClientSeq: bigint;
	rollback(): boolean;
}

// viewerKey (`${fingerprint}:${tabId}`) → sessionId → ordered membership.
const _sessionsByViewer = new Map<string, Map<string, CellSubscription>>();

export function mutateCellSubscription(
	viewerKey: string,
	sessionId: string,
	subscribed: boolean,
	clientSeq = 0n,
	refreshEqual = false,
): CellSubscriptionMutation | null {
	const prior = _sessionsByViewer.get(viewerKey)?.get(sessionId);
	if (clientSeq > 0n && prior) {
		if (clientSeq < prior.clientSeq) return null;
		if (clientSeq === prior.clientSeq) {
			// Heartbeats deliberately reuse the current intent sequence. They may
			// refresh a live claim (and be forwarded for held-cell repair), but
			// equality can never change membership.
			if (!refreshEqual || !subscribed || !prior.subscribed) return null;
		}
	}

	// Legacy claims remain arrival-ordered, but the coordinator owns the
	// synthesized watermark and forwards it to the worker. A legacy withdraw
	// preserves the prior watermark because the worker withdraw path removes the
	// claim before inspecting client_seq; advancing only here would suppress the
	// next ordered reclaim that the worker would accept.
	const effectiveClientSeq = clientSeq > 0n
		? clientSeq
		: subscribed
			? (prior?.clientSeq ?? -1n) + 1n
			: (prior?.clientSeq ?? 0n);
	let sessions = _sessionsByViewer.get(viewerKey);
	if (!sessions) {
		sessions = new Map();
		_sessionsByViewer.set(viewerKey, sessions);
	}
	const applied: CellSubscription = {
		subscribed,
		clientSeq: effectiveClientSeq,
		lastMs: Date.now(),
	};
	sessions.set(sessionId, applied);

	return {
		effectiveClientSeq,
		rollback(): boolean {
			const current = _sessionsByViewer.get(viewerKey);
			if (!current || current.get(sessionId) !== applied) return false;
			if (prior) {
				current.set(sessionId, prior);
			} else {
				current.delete(sessionId);
				if (current.size === 0) _sessionsByViewer.delete(viewerKey);
			}
			return true;
		},
	};
}

export function subscribeCells(
	viewerKey: string,
	sessionId: string,
	clientSeq = 0n,
	refreshEqual = false,
): boolean {
	return mutateCellSubscription(viewerKey, sessionId, true, clientSeq, refreshEqual) !== null;
}

export function unsubscribeCells(
	viewerKey: string,
	sessionId: string,
	clientSeq = 0n,
): boolean {
	return mutateCellSubscription(viewerKey, sessionId, false, clientSeq, false) !== null;
}

export function isSubscribed(viewerKey: string, sessionId: string): boolean {
	return _sessionsByViewer.get(viewerKey)?.get(sessionId)?.subscribed === true;
}

// A closed Sync socket deliberately does NOT clear the set. Subscriptions are
// per TAB, not per socket, and they mirror the worker's viewportClaims — which
// also survive a socket re-dial. Clearing here would leave a reconnecting tab
// unsubscribed until its next 30s claim heartbeat, i.e. a blank pane after every
// transient disconnect, and would desync coord from the worker in the meantime.

/** For diagSnapshot: viewerKey → the sessions that tab is streaming. Reading it
 *  is how a live investigation answers "why is this tab painting/not painting". */
export function _cellSubscriptionSnapshot(): Record<string, string[]> {
	const out: Record<string, string[]> = {};
	for (const [viewerKey, sessions] of _sessionsByViewer) {
		const subscribed = [...sessions]
			.filter(([, entry]) => entry.subscribed)
			.map(([sessionId]) => sessionId);
		if (subscribed.length > 0) out[viewerKey] = subscribed;
	}
	return out;
}

// A tab that crashed or slept past the shared claim TTL is no longer a viewer —
// the same TTL the worker and the viewer tracker use, so the three sides cannot
// disagree about who is watching.
export function _reapCellSubscriptions(now = Date.now()): void {
	for (const [viewerKey, sessions] of _sessionsByViewer) {
		for (const [sessionId, entry] of sessions) {
			if (now - entry.lastMs > VIEWER_CLAIM_TTL_MS) sessions.delete(sessionId);
		}
		if (sessions.size === 0) _sessionsByViewer.delete(viewerKey);
	}
}

setInterval(() => _reapCellSubscriptions(), REAP_INTERVAL_MS).unref?.();

// A closed session can never produce another frame; drop it from every tab now
// rather than leaving TTL-lived entries behind (same reasoning as the viewer
// tracker's close subscription).
sessionBus.subscribe((ev) => {
	if (ev.kind !== "closed") return;
	const sessionId = String(ev.session_id);
	for (const [viewerKey, sessions] of _sessionsByViewer) {
		if (!sessions.delete(sessionId)) continue;
		if (sessions.size === 0) _sessionsByViewer.delete(viewerKey);
	}
});
