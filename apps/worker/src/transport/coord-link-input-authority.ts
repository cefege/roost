// Live Sync-input authority for the worker boundary. Coordinator authorization is
// represented by the current authenticated link budget; route ownership remains
// worker-owned and is checked again immediately before SessionManager begins PTY
// input. Old coordinator frames without actor fields retain empty-epoch behavior.

import type { DInputRequest } from "@roost/protocol/proto/worker_transport_pb";
import type { SessionManager } from "../session/session-manager.ts";
import type { TerminalWriteAuthority } from "../session/session-terminal-control.ts";
import type { LocalTerminalWiring, TerminalRequestBudget } from "./coord-link-types.ts";

export function coordLinkInputAuthority(
	localTerminal: LocalTerminalWiring | undefined,
	request: DInputRequest,
	budget: TerminalRequestBudget,
	sessions: SessionManager,
): TerminalWriteAuthority | undefined {
	if (!localTerminal) return undefined;
	const sessionAuthorized = () =>
		budget.isCurrentConnection() && sessions.getBySessionId(request.sessionId) !== undefined;
	if (
		request.deviceFingerprint === ""
		|| request.tabId === ""
		|| request.browserConnectionId === ""
	) {
		return {
			isSessionAuthorized: sessionAuthorized,
			isCurrentInputRoute: () => request.inputRouteEpoch === "",
		};
	}
	const actor = {
		deviceFingerprint: request.deviceFingerprint,
		tabId: request.tabId,
		connectionId: request.browserConnectionId,
	};
	return {
		isSessionAuthorized: sessionAuthorized,
		isCurrentInputRoute: () => request.inputRouteEpoch === ""
			? localTerminal.inputRouteOwner.allowsLegacyInput(actor, request.sessionId)
			: localTerminal.inputRouteOwner.isCurrent(actor, request.sessionId, request.inputRouteEpoch),
	};
}
