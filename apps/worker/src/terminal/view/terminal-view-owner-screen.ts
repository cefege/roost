// The TerminalViewScreenPort the worker's view owner hands the shared registry.
// It owns one CellSink per LOCAL socket (`local:${socketId}`) plus the watch set
// that decides which sessions that sink forwards. Coordinator-relayed sockets
// get no sink: their cells ride the single "coord" sink and the coordinator owns
// its own remote replica, so only their view-state frames come back through
// here. Frame construction stays in session-emit.ts.

import type { PbCellGridChunk, PbCellGridFrame } from "@roost/protocol/proto/cell_pb";
import type { TerminalViewStateFrame } from "@roost/protocol/proto/sync_pb";
import type { TerminalViewScreenPort } from "@roost/protocol/terminal-view";
import {
	registerCellSink,
	unregisterCellSink,
	type CellSinkResult,
} from "../../session/session-cell-sinks.ts";
import type { SessionManager } from "../../session/session-manager.ts";

/** Where one registry socket's view decisions land. */
interface ViewStateTransport {
	sendViewState(frame: TerminalViewStateFrame): void;
}

/** A browser on this machine: it takes both the decision and the cells. */
export interface LocalViewTransport extends ViewStateTransport {
	readonly kind: "local";
	sendCellFrame(frame: PbCellGridFrame): CellSinkResult;
	sendCellChunk(chunk: PbCellGridChunk): CellSinkResult;
	/** The sink registry dropped this socket for a delivery overflow. */
	onOverflow(): void;
	/** A live view's lease ran out, so this socket stopped heartbeating. */
	onViewExpired(): void;
}

/** A browser on another machine, reached by relaying the decision upstream. */
export interface CoordinatorViewTransport extends ViewStateTransport {
	readonly kind: "coordinator";
}

export type TerminalViewOwnerTransport = LocalViewTransport | CoordinatorViewTransport;

export function localCellSinkId(socketId: string): string {
	return `local:${socketId}`;
}

interface OwnedSocket {
	readonly transport: TerminalViewOwnerTransport;
	/** Sessions this socket paints. Its sink forwards nothing else. */
	readonly watching: Set<string>;
	/** Last stream this socket was seeded on, per session, so entering a fresh
	 * stream is distinguishable from renewing inside the current one. */
	readonly streams: Map<string, string>;
}

export class TerminalViewOwnerScreen implements TerminalViewScreenPort {
	private readonly sockets = new Map<string, OwnedSocket>();

	constructor(private readonly sessions: () => SessionManager) {}

	/** Bind the transport before the registry admits the socket, so the sink the
	 * registry's registerSocket installs already knows where frames go. */
	attach(socketId: string, transport: TerminalViewOwnerTransport): void {
		this.sockets.set(socketId, {
			transport,
			watching: new Set(),
			streams: new Map(),
		});
	}

	has(socketId: string): boolean {
		return this.sockets.has(socketId);
	}

	transportFor(socketId: string): TerminalViewOwnerTransport | null {
		return this.sockets.get(socketId)?.transport ?? null;
	}

	coordinatorSocketIds(): readonly string[] {
		const ids: string[] = [];
		for (const [socketId, socket] of this.sockets) {
			if (socket.transport.kind === "coordinator") ids.push(socketId);
		}
		return ids;
	}

	registerSocket(socketId: string, _sink: unknown): void {
		const socket = this.sockets.get(socketId);
		if (socket?.transport.kind !== "local") return;
		const transport = socket.transport;
		registerCellSink(this.sessions(), {
			id: localCellSinkId(socketId),
			// A channel this socket does not watch owes it nothing, so the frame
			// is reported delivered: answering "dropped" would latch a permanent
			// repair loop on every session the browser never opened.
			sendFrame: (channelId, frame) =>
				this.watches(socket, channelId) ? transport.sendCellFrame(frame) : "sent",
			sendChunk: (channelId, chunk) =>
				this.watches(socket, channelId) ? transport.sendCellChunk(chunk) : "sent",
			onOverflow: () => { transport.onOverflow(); },
		});
	}

	unregisterSocket(socketId: string): void {
		const socket = this.sockets.get(socketId);
		if (!socket) return;
		this.sockets.delete(socketId);
		if (socket.transport.kind === "local") {
			unregisterCellSink(this.sessions(), localCellSinkId(socketId));
		}
	}

	setWatching(socketId: string, sessionId: string, watching: boolean): void {
		const socket = this.sockets.get(socketId);
		if (!socket) return;
		if (watching) {
			socket.watching.add(sessionId);
			return;
		}
		socket.watching.delete(sessionId);
		socket.streams.delete(sessionId);
	}

	seedSocket(socketId: string, sessionId: string): boolean {
		const socket = this.sockets.get(socketId);
		if (!socket) return false;
		// A coordinator-relayed socket is seeded from the coordinator's OWN screen
		// replica, which already holds the current grid. Requesting a full here
		// would re-baseline the single shared coord sink, so one viewer
		// re-attaching would cost every coordinator viewer a second baseline.
		// A local socket owns a dedicated sink, so its seed is its own.
		if (socket.transport.kind !== "local") return false;
		const streamId = this.requestFull(sessionId);
		if (!streamId) return false;
		socket.streams.set(sessionId, streamId);
		return true;
	}

	ensureSocketStream(socketId: string, sessionId: string): boolean {
		const socket = this.sockets.get(socketId);
		if (socket?.transport.kind !== "local") return false;
		const streamId = this.currentStreamId(sessionId);
		return streamId !== null && socket.streams.get(sessionId) !== streamId;
	}

	/** The worker keeps no record of what it already shipped to one socket, so a
	 * checkpoint can never be proven reachable here: every repair is a fresh
	 * authoritative full, which also carries a relayed socket's repair to the
	 * coordinator replica that owns it. */
	resyncSocket(socketId: string, sessionId: string, _at: { gridEpoch: string; seq: bigint }): void {
		const socket = this.sockets.get(socketId);
		if (!socket) return;
		const streamId = this.requestFull(sessionId);
		if (streamId) socket.streams.set(sessionId, streamId);
	}

	dispose(): void {
		for (const socketId of [...this.sockets.keys()]) this.unregisterSocket(socketId);
	}

	private watches(socket: OwnedSocket, channelId: number): boolean {
		const sessionId = this.sessions().sessions.get(channelId)?.sessionId;
		return sessionId !== undefined && socket.watching.has(String(sessionId));
	}

	private currentStreamId(sessionId: string): string | null {
		const manager = this.sessions();
		const record = manager.getBySessionId(sessionId);
		if (!record) return null;
		const state = manager.terminalStreams.get(record.channelId);
		return state?.enabled && state.coreValid ? state.streamId : null;
	}

	/** Retire every sink's cursor and rebuild one authoritative baseline. Going
	 * through the snapshot request rather than the raw baseline install is what
	 * keeps a forced full from being swallowed by another sink's pending
	 * cursor. */
	private requestFull(sessionId: string): string | null {
		const streamId = this.currentStreamId(sessionId);
		if (streamId) this.sessions().requestTerminalSnapshot(sessionId, streamId);
		return streamId;
	}
}
