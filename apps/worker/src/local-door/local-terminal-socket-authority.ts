// Live authorization predicates for one direct terminal port. LocalTerminalSockets
// creates these immediately before input, claims, and history reads; the session
// control owner calls them again after keeper admission. No grant scope is copied
// across an await, and a closed or replaced port fails every predicate.

import type { LocalTerminalGrantStore } from "./local-terminal-grants.ts";
import type { SessionManager } from "../session/session-manager.ts";
import type { TerminalWriteAuthority } from "../session/session-terminal-control.ts";
import type { TerminalInputRouteActor, TerminalInputRouteOwner } from "../terminal/terminal-input-route-owner.ts";
import type { TerminalPacketPort } from "../terminal/peer/terminal-packet-port.ts";
import type { TerminalPeerExpectedTuple } from "../terminal/peer/terminal-peer-connection.ts";
import { TERMINAL_REQUEST_BUDGET_CAP_MS } from "../transport/coord-link-constants.ts";
import type { TerminalRequestBudget } from "../transport/coord-link-types.ts";
import { monoNowMs } from "../util/mono.ts";

export interface LocalTerminalPortSession {
	readonly port: TerminalPacketPort;
	readonly expectedPeer: TerminalPeerExpectedTuple | null;
	generation: bigint;
	grantId: string | null;
	deviceFingerprint: string | null;
	tabId: string | null;
	closing: boolean;
}

export interface LocalTerminalAuthorizationDeps {
	readonly sessions: () => SessionManager;
	readonly grants: LocalTerminalGrantStore;
	readonly inputRouteOwner: TerminalInputRouteOwner;
	readonly workerEpoch: string;
	readonly isCurrentPort: (session: LocalTerminalPortSession) => boolean;
}

export function directPortActor(session: LocalTerminalPortSession): TerminalInputRouteActor | null {
	if (!session.deviceFingerprint || !session.tabId) return null;
	return {
		deviceFingerprint: session.deviceFingerprint,
		tabId: session.tabId,
		connectionId: session.port.socketId,
	};
}

export function isDirectPortSessionAuthorized(
	deps: LocalTerminalAuthorizationDeps,
	session: LocalTerminalPortSession,
	sessionId: string,
): boolean {
	if (
		session.closing
		|| !session.grantId
		|| !session.deviceFingerprint
		|| !session.tabId
		|| !session.port.open
		|| !deps.isCurrentPort(session)
	) return false;
	const grant = deps.grants.current(session.grantId);
	if (!grant || grant.deviceFingerprint !== session.deviceFingerprint || grant.tabId !== session.tabId) {
		return false;
	}
	if (session.expectedPeer) {
		if (grant.workerEpoch !== session.expectedPeer.workerEpoch) return false;
	} else if (grant.workerEpoch !== "" && grant.workerEpoch !== deps.workerEpoch) {
		return false;
	}
	return grant.sessionIds.includes(sessionId)
		&& deps.sessions().getBySessionId(sessionId) !== undefined;
}

export function directPortInputAuthority(
	deps: LocalTerminalAuthorizationDeps,
	session: LocalTerminalPortSession,
	sessionId: string,
	inputRouteEpoch: string,
): TerminalWriteAuthority {
	const actor = directPortActor(session);
	return {
		isSessionAuthorized: () => isDirectPortSessionAuthorized(deps, session, sessionId),
		isCurrentInputRoute: () => {
			if (!actor) return false;
			if (session.expectedPeer) {
				return inputRouteEpoch !== ""
					&& deps.inputRouteOwner.isCurrent(actor, sessionId, inputRouteEpoch);
			}
			return inputRouteEpoch === ""
				? deps.inputRouteOwner.allowsLegacyInput(actor, sessionId)
				: deps.inputRouteOwner.isCurrent(actor, sessionId, inputRouteEpoch);
		},
	};
}

export function directPortRequestBudget(
	isCurrentPort: (session: LocalTerminalPortSession) => boolean,
	session: LocalTerminalPortSession,
): TerminalRequestBudget {
	const startedAtMono = monoNowMs();
	return {
		remainingMs: () => TERMINAL_REQUEST_BUDGET_CAP_MS - (monoNowMs() - startedAtMono),
		isCurrentConnection: () => !session.closing && session.port.open && isCurrentPort(session),
	};
}
