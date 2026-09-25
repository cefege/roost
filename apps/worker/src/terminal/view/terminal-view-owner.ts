// The worker owns terminal view membership, geometry aggregation and stream
// generations for its OWN sessions. This facade wires the shared
// TerminalViewRegistry to the worker's screen port (terminal-view-owner-screen)
// and stream authority (terminal-view-owner-streams), routes each decision back
// to the transport that declared the view — a local WebSocket or the
// coordinator relay — and publishes the per-session projection the coordinator
// answers presence and diagnostics from.
// Membership only ever starts from an authorization a capable coordinator
// issued — a relayed command or an installed local grant — so a coordinator too
// old for either leaves this registry empty and stays the only minimizer.

import type { DTerminalViewRelay } from "@roost/protocol/proto/worker_transport_pb";
import type {
	TerminalResyncCommand,
	TerminalViewCommand,
	TerminalViewStateFrame,
} from "@roost/protocol/proto/sync_pb";
import { log } from "@roost/observability/log";
import {
	TerminalViewRegistry,
	type TerminalViewStateSink,
} from "@roost/protocol/terminal-view";
import { TERMINAL_VIEW_SWEEP_MS } from "@roost/protocol/viewport";
import type { SessionManager } from "../../session/session-manager.ts";
import {
	TerminalViewOwnerScreen,
	type LocalViewTransport,
	type TerminalViewOwnerTransport,
} from "./terminal-view-owner-screen.ts";
import { TerminalViewStreams } from "./terminal-view-owner-streams.ts";
import type { TerminalViewProjectionFrame } from "../../transport/coord-link-types.ts";
import { monoNowMs } from "../../util/mono.ts";

export interface TerminalViewOwnerDeps {
	sessions(): SessionManager;
	/** One view decision addressed back to a coordinator-relayed browser socket. */
	sendViewState(socketId: string, frame: TerminalViewStateFrame): void;
	sendProjection(projection: TerminalViewProjectionFrame): void;
	/** Test seam: the registry's lease/park clock. Monotonic in production. */
	now?(): number;
}

export interface LocalViewRegistration {
	socketId: string;
	deviceFingerprint: string;
	tabId: string;
	/** The grant's session set. Nothing outside it becomes membership. */
	allowsSession(sessionId: string): boolean;
	transport: LocalViewTransport;
}

export class TerminalViewOwner {
	private readonly screen: TerminalViewOwnerScreen;
	private readonly streams: TerminalViewStreams;
	private readonly registry: TerminalViewRegistry;
	private readonly sweepTimer: NodeJS.Timeout;
	private readonly dirtyProjections = new Set<string>();
	private projectionFlushScheduled = false;
	private disposed = false;

	constructor(private readonly deps: TerminalViewOwnerDeps) {
		this.screen = new TerminalViewOwnerScreen(deps.sessions);
		this.streams = new TerminalViewStreams({
			sessions: deps.sessions,
			geometries: (sessionId) => this.registry.geometries(sessionId),
			broadcast: (sessionId, status, message) => {
				this.registry.broadcast(sessionId, status, message);
			},
			publish: (sessionId) => { this.markProjection(sessionId); },
		});
		this.registry = new TerminalViewRegistry({
			screen: this.screen,
			now: deps.now ?? monoNowMs,
			streamState: (sessionId) => this.streams.state(sessionId),
			recompute: (sessionId) => this.streams.recompute(sessionId),
			redrive: (sessionId) => { this.streams.redrive(sessionId); },
			onLiveViewExpired: (socketId, viewId, sessionId) => {
				this.liveViewExpired(socketId, viewId, sessionId);
			},
		});
		this.sweepTimer = setInterval(() => { this.registry.sweep(); }, TERMINAL_VIEW_SWEEP_MS);
		this.sweepTimer.unref?.();
	}

	/** A verified local terminal socket. `viewerKey` is `${fingerprint}:${tabId}`
	 * exactly as a coordinator-relayed socket's is, so one tab reclaiming its own
	 * view across transports stays the same identity. */
	registerLocalSocket(registration: LocalViewRegistration): void {
		const { socketId, transport } = registration;
		this.screen.attach(socketId, transport);
		this.registry.registerSocket({
			socketId,
			viewerKey: `${registration.deviceFingerprint}:${registration.tabId}`,
			callerFingerprint: registration.deviceFingerprint,
			allowsSession: registration.allowsSession,
			sink: stateSink(transport),
		});
		log.info("terminal-view", "local_socket_registered", {
			socket_id: socketId,
			device_fingerprint: registration.deviceFingerprint,
			tab_id: registration.tabId,
		});
	}

	/** One relayed command from a browser the coordinator authenticated. The
	 * membership decision is synchronous and the keeper work it triggers is
	 * governed by the worker's own control ceiling, so the frame's budget_ms has
	 * no waiter here to serve. */
	handleRelay(relay: DTerminalViewRelay): void {
		const command = relay.command;
		if (!command.case || !relay.socketId || !relay.viewerKey) return;
		this.ensureCoordinatorSocket(relay.socketId, relay.viewerKey, relay.deviceFingerprint);
		if (command.case === "view") {
			this.registry.handleViewCommand(relay.socketId, command.value);
			return;
		}
		this.registry.handleResync(relay.socketId, command.value);
	}

	handleViewCommand(socketId: string, command: TerminalViewCommand): void {
		this.registry.handleViewCommand(socketId, command);
	}

	handleResync(socketId: string, command: TerminalResyncCommand): void {
		this.registry.handleResync(socketId, command);
	}

	/** Park this socket's views and drop its delivery. The park grace and the
	 * lease sweep decide when they stop constraining the PTY. */
	closeSocket(socketId: string): void {
		this.registry.closeSocket(socketId);
	}

	/** A coordinator reconnect invalidates only the coordinator's own browser
	 * sockets: a local socket keeps its views, its lease and the live stream, so
	 * a coordinator bounce never blanks a local pane. Every projection is
	 * re-announced because the new coordinator generation has none. */
	dropCoordinatorSockets(): void {
		const socketIds = this.screen.coordinatorSocketIds();
		for (const socketId of socketIds) this.registry.closeSocket(socketId);
		for (const sessionId of this.streams.sessionIds()) this.markProjection(sessionId);
		log.info("terminal-view", "coordinator_sockets_dropped", { sockets: socketIds.length });
	}

	closeSession(sessionId: string): void {
		this.registry.closeSession(sessionId);
		this.streams.closeSession(sessionId);
	}

	/** The production sweep is an interval. Tests drive it explicitly so lease
	 * and park-grace behaviour is asserted against an injected clock. */
	_sweep(): void {
		this.registry.sweep();
	}

	dispose(): void {
		this.disposed = true;
		clearInterval(this.sweepTimer);
		this.registry.dispose();
		this.screen.dispose();
		this.streams.dispose();
		this.dirtyProjections.clear();
	}

	private ensureCoordinatorSocket(
		socketId: string,
		viewerKey: string,
		deviceFingerprint: string,
	): void {
		if (this.screen.has(socketId)) return;
		const transport: TerminalViewOwnerTransport = {
			kind: "coordinator",
			sendViewState: (frame) => { this.deps.sendViewState(socketId, frame); },
		};
		this.screen.attach(socketId, transport);
		this.registry.registerSocket({
			socketId,
			viewerKey,
			callerFingerprint: deviceFingerprint,
			// The coordinator authorized this browser for this session before
			// relaying; a session this worker does not host answers UNAVAILABLE
			// through the ordinary stream path instead of a second rule here.
			allowsSession: () => true,
			sink: stateSink(transport),
		});
		log.info("terminal-view", "coordinator_socket_registered", {
			socket_id: socketId,
			device_fingerprint: deviceFingerprint,
		});
	}

	private liveViewExpired(socketId: string, viewId: string, sessionId: string): void {
		const transport = this.screen.transportFor(socketId);
		log.warn("terminal-view", "live_view_expired", {
			socket_id: socketId,
			view_id: viewId,
			session_id: sessionId,
			transport: transport?.kind ?? "unknown",
		});
		// A live lease that ran out means the socket stopped heartbeating. The
		// coordinator owns its own sockets' lifetime; a local one is ours to end.
		if (transport?.kind === "local") transport.onViewExpired();
	}

	private markProjection(sessionId: string): void {
		if (this.disposed) return;
		this.dirtyProjections.add(sessionId);
		if (this.projectionFlushScheduled) return;
		this.projectionFlushScheduled = true;
		queueMicrotask(() => { this.flushProjections(); });
	}

	/** One frame per session per tick: a resize storm changes membership many
	 * times inside a single turn of the loop. */
	private flushProjections(): void {
		this.projectionFlushScheduled = false;
		const sessionIds = [...this.dirtyProjections];
		this.dirtyProjections.clear();
		if (this.disposed) return;
		for (const sessionId of sessionIds) {
			const stream = this.streams.state(sessionId);
			this.deps.sendProjection({
				sessionId,
				viewers: this.registry.viewerInputs(sessionId),
				effectiveCols: stream?.effective?.cols ?? 0,
				effectiveRows: stream?.effective?.rows ?? 0,
				streamId: stream?.streamId ?? "",
			});
		}
	}
}

function stateSink(transport: TerminalViewOwnerTransport): TerminalViewStateSink {
	return {
		enqueueTerminalState: (frame) => {
			if (frame.frame.case === "terminalViewState") transport.sendViewState(frame.frame.value);
		},
	};
}
