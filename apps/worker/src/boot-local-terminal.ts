// Boot-time assembly of the direct terminal path. It creates the one view,
// grant, input-work, route, and peer owners before CoordLink advertises their
// capabilities. SessionManager stays a lazy forward reference so local carriers
// can start before keeper/session construction without inventing a second owner.

import { createSpaResponder } from "@roost/shared/spa";
import { WEB_ASSETS } from "@roost/shared/web-embed";
import type { WorkerFp } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import type { CoordLinkRefs, LocalTerminalWiring } from "./coord-link-deps.ts";
import { AttachmentGrantStore } from "./attachment-grants.ts";
import { AttachmentDirectSockets } from "./attachment-direct-socket.ts";
import { AttachmentPeerOwner } from "./attachment-peer-owner.ts";
import { LocalTerminalGrantStore } from "./local-terminal-grants.ts";
import {
	LocalTerminalSockets,
	type LocalTerminalSocketTestFaults,
} from "./local-terminal-socket.ts";
import { startLocalUiServer } from "./local-ui-server.ts";
import type { SessionManager } from "./session-manager.ts";
import { TerminalInputRouteOwner } from "./terminal-input-route-owner.ts";
import { TerminalInputWorkBudget } from "./terminal-input-work-budget.ts";
import { TerminalPeerOwner } from "./terminal-peer-owner.ts";
import type { TerminalPeerTestFaultState } from "./terminal-peer-test-faults.ts";
import { TerminalViewOwner } from "./terminal-view-owner.ts";

export interface LocalTerminalDoorOptions {
	bind: string;
	coordinatorUrl: string;
	workerFp: WorkerFp;
	processEpoch: string;
	terminalPeerEnabled: boolean;
	terminalPeerBindAddress?: string;
	terminalPeerPortRange?: { readonly min: number; readonly max: number };
	allowedBrowserOrigins: readonly string[];
	webDistPath: string | undefined;
	refs: CoordLinkRefs;
	/** Disposable source-smoke-only direct carrier faults. Ordinary boot omits this. */
	testFaults?: LocalTerminalSocketTestFaults;
	/** Shared in-process peer fault state; only the smoke source entrypoint supplies it. */
	terminalPeerTestFaults?: TerminalPeerTestFaultState;
}

export interface LocalTerminalDoor {
	wiring: LocalTerminalWiring;
	close(): void;
}

export async function startLocalTerminalDoor(options: LocalTerminalDoorOptions): Promise<LocalTerminalDoor> {
	const { refs } = options;
	const sessions = (): SessionManager => {
		if (!refs.sessionMgr) throw new Error("local terminal door used before sessionMgr was bound");
		return refs.sessionMgr;
	};
	const viewOwner = new TerminalViewOwner({
		sessions,
		sendViewState: (socketId, frame) => { refs.link?.sendTerminalViewState(socketId, frame); },
		sendProjection: (projection) => { refs.link?.sendTerminalViewProjection(projection); },
	});
	const inputWorkBudget = new TerminalInputWorkBudget();
	const inputRouteOwner = new TerminalInputRouteOwner({
		workerEpoch: options.processEpoch,
		sessions,
		inputWorkBudget,
	});
	const terminalPeerTestFaults = options.terminalPeerTestFaults;
	const grants = new LocalTerminalGrantStore({
		workerEpoch: options.processEpoch,
		...(terminalPeerTestFaults ? { now: () => terminalPeerTestFaults.now() } : {}),
	});
	terminalPeerTestFaults?.attachGrantExpirySweep(() => grants._sweepExpiredForTest());
	terminalPeerTestFaults?.attachGrantScopeShrink((sessionId) => grants._shrinkSessionForTest(sessionId));
	const sockets = new LocalTerminalSockets({
		sessions,
		grants,
		viewOwner,
		inputWorkBudget,
		inputRouteOwner,
		workerFingerprint: options.workerFp,
		workerEpoch: options.processEpoch,
		testFaults: localSocketTestFaults(options.testFaults, terminalPeerTestFaults),
	});
	let coordinatorGeneration: string | null = null;
	let directDisposed = false;
	const peerOwner = new TerminalPeerOwner({
		processEpoch: options.processEpoch,
		enabled: options.terminalPeerEnabled,
		bindAddress: options.terminalPeerBindAddress,
		portRange: options.terminalPeerPortRange,
		isCurrentCoordinator: (generation) => coordinatorGeneration === generation,
		authorizeGrant: (request) => grants.authorizePeer(
			request.grantId,
			request.deviceFingerprint,
			request.tabId,
			request.workerEpoch,
		),
		openPeerPort: (port, expectedTuple) => sockets.openPeerPort(port, expectedTuple),
		testFaults: terminalPeerTestFaults,
		expireGrantForTest: (grantId) => { grants.remove(grantId, "expired"); },
	});
	terminalPeerTestFaults?.attachPeerOwner(peerOwner);
	const attachmentGrants = new AttachmentGrantStore({ workerEpoch: options.processEpoch });
	const attachmentSockets = new AttachmentDirectSockets({
		grants: attachmentGrants,
		workerFingerprint: options.workerFp,
		workerEpoch: options.processEpoch,
	});
	const attachmentPeerOwner = new AttachmentPeerOwner({
		processEpoch: options.processEpoch,
		enabled: options.terminalPeerEnabled,
		bindAddress: options.terminalPeerBindAddress,
		portRange: options.terminalPeerPortRange,
		isCurrentCoordinator: (generation) => coordinatorGeneration === generation,
		authorizeGrant: (request) => attachmentGrants.authorizePeer({
			grantId: request.grantId,
			deviceFingerprint: request.deviceFingerprint,
			tabId: request.tabId,
			workerEpoch: request.workerEpoch,
		}),
		openPeerPort: (port, expectedTuple) => attachmentSockets.openPeerPort(port, expectedTuple),
	});
	const server = startLocalUiServer({
		bind: options.bind,
		coordinatorUrl: options.coordinatorUrl,
		workerFingerprint: options.workerFp,
		allowedBrowserOrigins: options.allowedBrowserOrigins,
		spa: createSpaResponder(options.webDistPath, WEB_ASSETS),
		terminal: sockets,
		attachment: attachmentSockets,
	});
	const peerBootstrapState = await peerOwner.bootstrap();
	const attachmentPeerBootstrapState = await attachmentPeerOwner.bootstrap();

	function clearCoordinatorGeneration(): void {
		coordinatorGeneration = null;
	}

	function useCoordinatorGeneration(generation: string): boolean {
		if (!validCoordinatorGeneration(generation)) return false;
		if (coordinatorGeneration !== null && coordinatorGeneration !== generation) return false;
		coordinatorGeneration = generation;
		return true;
	}

	function disposeDirect(): void {
		if (directDisposed) return;
		directDisposed = true;
		clearCoordinatorGeneration();
		inputWorkBudget.dispose();
		terminalPeerTestFaults?.dispose();
		inputRouteOwner.dispose();
		attachmentSockets.dispose();
		attachmentPeerOwner.dispose();
		attachmentGrants.dispose();
		grants.dispose();
		sockets.dispose();
		peerOwner.dispose();
	}

	return {
		wiring: {
			viewOwner,
			grants,
			sockets,
			inputWorkBudget,
			inputRouteOwner,
			peerOwner,
			terminalPeerTestFaults,
			attachmentGrants,
			attachmentSockets,
			attachmentPeerOwner,
			workerEpoch: options.processEpoch,
			peerSupported: peerBootstrapState === "ready",
			attachmentPeerSupported: attachmentPeerBootstrapState === "ready",
			useCoordinatorGeneration,
			clearCoordinatorGeneration,
			revokeDevice: (deviceFingerprint) => {
				inputRouteOwner.revokeDevice(deviceFingerprint);
				grants.revokeDevice(deviceFingerprint);
				peerOwner.revokeDevice(deviceFingerprint);
			},
			revokeAttachmentDevice: (deviceFingerprint) => {
				attachmentSockets.revokeDevice(deviceFingerprint);
				attachmentGrants.revokeDevice(deviceFingerprint);
				attachmentPeerOwner.revokeDevice(deviceFingerprint);
			},
			retireDirect: (reason) => {
				log.info("terminal-peer", "direct_retired", { reason });
				disposeDirect();
			},
			disposeDirect,
		},
		close: () => {
			server.close();
			disposeDirect();
			viewOwner.dispose();
		},
	};
}

function localSocketTestFaults(
	base: LocalTerminalSocketTestFaults | undefined,
	terminalPeerTestFaults: TerminalPeerTestFaultState | undefined,
): LocalTerminalSocketTestFaults | undefined {
	if (!terminalPeerTestFaults) return base;
	return {
		...base,
		shouldSendPeerInputResult: (port, frame, result) =>
			base?.shouldSendPeerInputResult?.(port, frame, result) !== false
			&& (result.status !== "accepted" || !terminalPeerTestFaults.consumePeerInputResultDrop()),
		onPeerHistoryResponse: async (session, request, response) => {
			if (base?.onPeerHistoryResponse && !(await base.onPeerHistoryResponse(session, request, response))) {
				return false;
			}
			return await terminalPeerTestFaults.holdPeerHistoryResponse(request.sessionId);
		},
	};
}

function validCoordinatorGeneration(value: string): boolean {
	const encoded = Buffer.from(value, "utf8");
	if (encoded.byteLength === 0 || encoded.byteLength > 128) return false;
	for (const byte of encoded) {
		if (byte <= 0x1f || byte === 0x7f) return false;
	}
	return true;
}
