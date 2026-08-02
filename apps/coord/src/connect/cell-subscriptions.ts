// Which sessions each browser TAB streams cell frames and PTY bytes for.
// Driven by sessionsResize (handlers-sessions.ts) and read by the Sync fanout
// (handlers-streaming.ts::startSyncFeed) so a tab is no longer sent — and no
// longer re-serialized — every frame of every session on the fleet.
//
// Mirrors the worker's viewportClaims exactly: a BACKGROUND claim (0x0, cause
// BACKGROUND) KEEPS the subscription, because that parked-pane keep-alive is
// what makes a reveal need no socket re-dial and no snapshot. Only an explicit
// WITHDRAW, a session close, or the shared claim TTL drops it.
//
// Module-level singleton (one per coord), same shape and schedule as
// viewer-tracker.ts: importing this module starts the TTL reaper and the
// session-close subscription.

import { sessionBus } from "../buses.ts";
import { VIEWER_CLAIM_TTL_MS } from "@roost/shared/viewport";

const REAP_INTERVAL_MS = 10_000;

// viewerKey (`${fingerprint}:${tabId}`) → sessionId → last claim wall-clock.
const _sessionsByViewer = new Map<string, Map<string, number>>();

export function subscribeCells(viewerKey: string, sessionId: string): void {
	let sessions = _sessionsByViewer.get(viewerKey);
	if (!sessions) {
		sessions = new Map();
		_sessionsByViewer.set(viewerKey, sessions);
	}
	sessions.set(sessionId, Date.now());
}

export function unsubscribeCells(viewerKey: string, sessionId: string): void {
	const sessions = _sessionsByViewer.get(viewerKey);
	if (!sessions) return;
	sessions.delete(sessionId);
	if (sessions.size === 0) _sessionsByViewer.delete(viewerKey);
}

export function isSubscribed(viewerKey: string, sessionId: string): boolean {
	return _sessionsByViewer.get(viewerKey)?.has(sessionId) === true;
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
	for (const [viewerKey, sessions] of _sessionsByViewer) out[viewerKey] = [...sessions.keys()];
	return out;
}

// A tab that crashed or slept past the shared claim TTL is no longer a viewer —
// the same TTL the worker and the viewer tracker use, so the three sides cannot
// disagree about who is watching.
setInterval(() => {
	const now = Date.now();
	for (const [viewerKey, sessions] of _sessionsByViewer) {
		for (const [sessionId, lastMs] of sessions) {
			if (now - lastMs > VIEWER_CLAIM_TTL_MS) sessions.delete(sessionId);
		}
		if (sessions.size === 0) _sessionsByViewer.delete(viewerKey);
	}
}, REAP_INTERVAL_MS).unref?.();

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
