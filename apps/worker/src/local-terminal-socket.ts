// Direct terminal frame owner for loopback and authenticated WebRTC packet ports.
// It verifies grants and expected peer tuples, keeps session authorization live
// through asynchronous input/history work, and selects the three carrier lanes.
// TerminalPeerPacketPort owns WebRTC framing; this file owns LocalTerminal protobufs.

import { create, fromBinary } from "@bufbuild/protobuf";
import { log } from "@roost/observability/log";
import {
	LocalTerminalClientFrameSchema,
	LocalTerminalClosedSchema,
	LocalTerminalReadySchema,
	type LocalTerminalHello,
	type LocalTerminalServerFrame,
} from "@roost/protocol/proto/local_terminal_pb";
import type { LocalTerminalGrantChange, LocalTerminalGrantStore } from "./local-terminal-grants.ts";
import {
	LocalTerminalPortControls,
	type LocalTerminalPeerHistoryFault,
} from "./local-terminal-socket-controls.ts";
import {
	directPortActor,
	directPortRequestBudget,
	isDirectPortSessionAuthorized,
	type LocalTerminalAuthorizationDeps,
	type LocalTerminalPortSession,
} from "./local-terminal-socket-authority.ts";
import {
	localTerminalCellDelivery,
	sendLocalTerminalFrame,
	sendLocalTerminalInputResult,
} from "./local-terminal-socket-delivery.ts";
import {
	writeLocalTerminalInput,
	type LocalTerminalPeerInputFault,
	type LocalTerminalPeerInputResultFault,
} from "./local-terminal-socket-input.ts";
import { LocalTerminalPreHelloOwner } from "./local-terminal-prehello.ts";
import type { SessionManager } from "./session-manager.ts";
import type { TerminalInputRouteOwner } from "./terminal-input-route-owner.ts";
import type { TerminalInputWorkBudget } from "./terminal-input-work-budget.ts";
import type { TerminalPacketPort } from "./terminal-packet-port.ts";
import type { TerminalPeerExpectedTuple } from "./terminal-peer-connection.ts";
import type { TerminalViewOwner } from "./terminal-view-owner.ts";
import type { LocalViewTransport } from "./terminal-view-owner-screen.ts";

interface PeerTerminalPacketPort extends TerminalPacketPort {
	markAuthenticated(): void;
}

/** Source-smoke-only faults injected through an alternate worker entrypoint. */
export interface LocalTerminalSocketTestFaults {
	readonly onAuthenticatedPeerInput?: LocalTerminalPeerInputFault;
	readonly shouldSendPeerInputResult?: LocalTerminalPeerInputResultFault;
	readonly onPeerHistoryResponse?: LocalTerminalPeerHistoryFault;
}

export interface LocalTerminalSocketDeps {
	sessions(): SessionManager;
	grants: LocalTerminalGrantStore;
	viewOwner: TerminalViewOwner;
	inputWorkBudget: TerminalInputWorkBudget;
	inputRouteOwner: TerminalInputRouteOwner;
	workerFingerprint: string;
	workerEpoch: string;
	testFaults?: LocalTerminalSocketTestFaults;
}

/** One direct carrier registry. Loopback calls onOpen; TerminalPeerOwner calls
 * openPeerPort before it applies remote SDP, so neither can race an ingress frame. */
export class LocalTerminalSockets {
	private readonly ports = new Map<string, LocalTerminalPortSession>();
	private readonly preHello: LocalTerminalPreHelloOwner;
	private readonly authorizationDeps: LocalTerminalAuthorizationDeps;
	private readonly controls: LocalTerminalPortControls;
	private readonly unsubscribeGrantChanges: () => void;
	private generations = 0n;
	private disposed = false;
	constructor(private readonly deps: LocalTerminalSocketDeps) {
		this.preHello = new LocalTerminalPreHelloOwner((socketId) => {
			const session = this.ports.get(socketId);
			if (session && session.grantId === null) this.close(session, "local terminal hello timed out");
		});
		this.authorizationDeps = {
			sessions: deps.sessions,
			grants: deps.grants,
			inputRouteOwner: deps.inputRouteOwner,
			workerEpoch: deps.workerEpoch,
			isCurrentPort: (session) => this.ports.get(session.port.socketId) === session,
		};
		this.controls = new LocalTerminalPortControls({
			sessions: deps.sessions,
			inputRouteOwner: deps.inputRouteOwner,
			workerFingerprint: deps.workerFingerprint,
			workerEpoch: deps.workerEpoch,
			actor: directPortActor,
			requestBudget: (session) =>
				directPortRequestBudget(this.authorizationDeps.isCurrentPort, session),
			isSessionAuthorized: (session, sessionId) => this.isSessionAuthorized(session, sessionId),
			sendControl: (session, frame) => { this.sendControl(session, frame); },
			sendHistory: (session, response, closeOnRefusal) =>
				this.sendFrame(session, { case: "scrollback", value: response }, "history", closeOnRefusal),
			close: (session, reason) => { this.close(session, reason); },
			testFaults: {
				onPeerHistoryResponse: deps.testFaults?.onPeerHistoryResponse,
			},
		});
		this.unsubscribeGrantChanges = deps.grants.subscribe((change) => {
			this.onGrantChange(change);
		});
	}

	onOpen(port: TerminalPacketPort): void {
		this.registerPort(port, null);
	}

	openPeerPort(
		port: PeerTerminalPacketPort,
		expectedPeer: TerminalPeerExpectedTuple,
	): { onMessage(bytes: Uint8Array): void; onClose(): void } {
		this.registerPort(port, expectedPeer);
		return {
			onMessage: (bytes) => { this.onMessage(port, bytes); },
			onClose: () => { this.onClose(port); },
		};
	}

	onMessage(port: TerminalPacketPort, data: Uint8Array): void {
		const session = this.ports.get(port.socketId);
		if (!session || this.disposed) return;
		let frame;
		try {
			frame = fromBinary(LocalTerminalClientFrameSchema, data);
		} catch {
			this.close(session, "undecodable frame");
			return;
		}
		const client = frame.frame;
		if (client.case === "hello") {
			this.accept(session, client.value);
			return;
		}
		if (!session.grantId) {
			this.close(session, "hello required");
			return;
		}
		switch (client.case) {
			case "terminalView":
				this.deps.viewOwner.handleViewCommand(port.socketId, client.value);
				return;
			case "terminalResync":
				this.deps.viewOwner.handleResync(port.socketId, client.value);
				return;
			case "input":
				void writeLocalTerminalInput({
					session,
					command: client.value,
					sessions: this.deps.sessions,
					inputWorkBudget: this.deps.inputWorkBudget,
					authorizationDeps: this.authorizationDeps,
					isSessionAuthorized: (candidate, sessionId) => this.isSessionAuthorized(candidate, sessionId),
					sendResult: (command, result) => {
						sendLocalTerminalInputResult(
							(frame) => { this.sendControl(session, frame); },
							session.generation,
							command,
							result,
						);
					},
					onAuthenticatedPeerInput: this.deps.testFaults?.onAuthenticatedPeerInput,
					shouldSendPeerInputResult: this.deps.testFaults?.shouldSendPeerInputResult,
				});
				return;
			case "scrollback":
				void this.controls.scrollback(session, client.value);
				return;
			case "inputRouteClaim":
				void this.controls.claim(session, client.value);
				return;
			case "transportProbe":
				this.controls.probe(session, client.value);
				return;
			case undefined:
				return;
		}
	}

	onClose(port: TerminalPacketPort): void {
		const session = this.ports.get(port.socketId);
		if (!session) return;
		this.preHello.retire(port.socketId);
		this.ports.delete(port.socketId);
		this.controls.retirePort(port.socketId);
		this.deps.inputRouteOwner.retireConnection(port.socketId);
		if (session.grantId) this.deps.viewOwner.closeSocket(port.socketId);
	}

	revokeDevice(deviceFingerprint: string): void {
		this.deps.inputRouteOwner.revokeDevice(deviceFingerprint);
		this.deps.grants.revokeDevice(deviceFingerprint);
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.unsubscribeGrantChanges();
		for (const session of [...this.ports.values()]) {
			this.close(session, "local terminal worker is stopping");
		}
		this.preHello.dispose();
	}

	private registerPort(port: TerminalPacketPort, expectedPeer: TerminalPeerExpectedTuple | null): void {
		if (this.disposed || this.ports.has(port.socketId)) {
			port.close(1008, "local terminal port unavailable");
			return;
		}
		if (!expectedPeer && !this.preHello.admit(port.socketId)) {
			port.close(1008, "local terminal pre-hello capacity reached");
			return;
		}
		this.ports.set(port.socketId, {
			port,
			expectedPeer,
			generation: 0n,
			grantId: null,
			deviceFingerprint: null,
			tabId: null,
			closing: false,
		});
	}

	private accept(session: LocalTerminalPortSession, hello: LocalTerminalHello): void {
		if (session.grantId) {
			this.close(session, "duplicate hello");
			return;
		}
		if (!this.matchesExpectedPeer(session, hello)) {
			this.close(session, session.expectedPeer
				? "peer hello does not match offer"
				: "loopback hello must not include peer identity");
			return;
		}
		const verdict = this.deps.grants.verify({
			grantId: hello.grantId,
			secret: hello.secret,
			tabId: hello.tabId,
			deviceFingerprint: hello.deviceFingerprint,
		});
		if (!verdict.ok) {
			log.warn("local-terminal", "hello_refused", {
				socket_id: session.port.socketId,
				grant_id: hello.grantId,
				reason: verdict.reason,
			});
			this.close(session, verdict.reason);
			return;
		}
		const grant = verdict.grant;
		if (session.expectedPeer && grant.workerEpoch !== session.expectedPeer.workerEpoch) {
			this.close(session, "peer grant is unavailable");
			return;
		}
		if (!session.expectedPeer && grant.workerEpoch !== "" && grant.workerEpoch !== this.deps.workerEpoch) {
			this.close(session, "local terminal worker epoch changed");
			return;
		}
		if (!session.expectedPeer) {
			const admission = this.preHello.authenticate(grant.grantId, session.port.socketId);
			if (!admission.admitted) {
				this.close(session, "local terminal authenticated capacity reached");
				return;
			}
			const replaced = admission.replacedSocketId
				? this.ports.get(admission.replacedSocketId)
				: null;
			if (replaced) this.close(replaced, "local terminal grant connection replaced");
		}
		this.generations += 1n;
		session.generation = this.generations;
		session.grantId = grant.grantId;
		session.deviceFingerprint = grant.deviceFingerprint;
		session.tabId = grant.tabId;
		if (session.expectedPeer) (session.port as PeerTerminalPacketPort).markAuthenticated();
		if (!this.sendControl(session, {
			case: "ready",
			value: create(LocalTerminalReadySchema, {
				workerFingerprint: this.deps.workerFingerprint,
				sessionIds: [...grant.sessionIds],
				socketGeneration: session.generation,
				workerEpoch: this.deps.workerEpoch,
				socketId: session.port.socketId,
				peerId: session.expectedPeer?.peerId ?? "",
			}),
		})) return;
		this.deps.viewOwner.registerLocalSocket({
			socketId: session.port.socketId,
			deviceFingerprint: grant.deviceFingerprint,
			tabId: grant.tabId,
			allowsSession: (sessionId) => this.isSessionAuthorized(session, sessionId),
			transport: this.transport(session),
		});
		log.info("local-terminal", "hello_accepted", {
			socket_id: session.port.socketId,
			grant_id: grant.grantId,
			device_fingerprint: grant.deviceFingerprint,
			tab_id: grant.tabId,
			socket_generation: session.generation.toString(),
			sessions: grant.sessionIds.length,
			kind: session.port.kind,
		});
	}

	private matchesExpectedPeer(session: LocalTerminalPortSession, hello: LocalTerminalHello): boolean {
		const expectedPeer = session.expectedPeer;
		if (!expectedPeer) return hello.peerId === "" && hello.workerEpoch === "";
		return hello.peerId === expectedPeer.peerId
			&& hello.grantId === expectedPeer.grantId
			&& hello.deviceFingerprint === expectedPeer.deviceFingerprint
			&& hello.tabId === expectedPeer.tabId
			&& hello.workerEpoch === expectedPeer.workerEpoch;
	}

	private transport(session: LocalTerminalPortSession): LocalViewTransport {
		return {
			kind: "local",
			sendViewState: (frame) => {
				this.sendFrame(session, { case: "terminalViewState", value: frame }, "terminal", true);
			},
			sendCellFrame: (frame) => this.sendCells(session, { case: "cellGrid", value: frame }),
			sendCellChunk: (chunk) => this.sendCells(session, { case: "cellGridChunk", value: chunk }),
			onOverflow: () => { this.close(session, "local delivery overflow"); },
			onViewExpired: () => { this.close(session, "terminal view lease expired"); },
		};
	}
	private isSessionAuthorized(session: LocalTerminalPortSession, sessionId: string): boolean {
		return isDirectPortSessionAuthorized(this.authorizationDeps, session, sessionId);
	}

	private sendControl(session: LocalTerminalPortSession, frame: LocalTerminalServerFrame["frame"]): boolean {
		return this.sendFrame(session, frame, "control", true) !== "refused";
	}

	private sendCells(
		session: LocalTerminalPortSession,
		frame: LocalTerminalServerFrame["frame"],
	): "sent" | "overflow" {
		return localTerminalCellDelivery(
			this.sendFrame(session, frame, "terminal", false),
			() => { this.close(session, "local delivery overflow"); },
		);
	}

	private sendFrame(
		session: LocalTerminalPortSession,
		frame: LocalTerminalServerFrame["frame"],
		lane: "control" | "terminal" | "history",
		closeOnRefusal: boolean,
	) {
		return sendLocalTerminalFrame(
			session.port,
			frame,
			lane,
			closeOnRefusal ? (reason) => { this.close(session, reason); } : undefined,
		);
	}

	private close(session: LocalTerminalPortSession, reason: string): void {
		if (session.closing) return;
		session.closing = true;
		log.info("local-terminal", "socket_closing", {
			socket_id: session.port.socketId,
			reason,
		});
		this.sendFrame(session, {
			case: "closed",
			value: create(LocalTerminalClosedSchema, { reason }),
		}, "control", false);
		this.onClose(session.port);
		session.port.close(1000, reason);
	}

	private onGrantChange(change: LocalTerminalGrantChange): void {
		if (change.kind === "removed") {
			const reason = change.reason === "expired"
				? "local terminal grant expired"
				: "local terminal grant revoked";
			for (const session of [...this.ports.values()]) {
				if (session.grantId === change.grant.grantId) this.close(session, reason);
			}
			return;
		}
		if (change.kind === "installed" || change.removedSessionIds.length === 0) return;
		for (const session of [...this.ports.values()]) {
			if (session.grantId === change.grant.grantId) {
				this.close(session, "local terminal grant scope reduced");
			}
		}
	}
}
