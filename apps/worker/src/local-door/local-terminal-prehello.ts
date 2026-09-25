// Bounded loopback sockets before and after authenticated terminal Hello.
// This owner expires unauthenticated local FDs, caps live grant bindings, and
// ensures replaying one grant replaces its prior socket rather than multiplying sinks.

import {
	TERMINAL_PEER_HELLO_DEADLINE_MS,
	TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER,
} from "@roost/protocol/terminal-peer";

export interface LocalTerminalAuthenticatedAdmission {
	readonly admitted: boolean;
	readonly replacedSocketId: string | null;
}

export class LocalTerminalPreHelloOwner {
	private readonly timers = new Map<string, NodeJS.Timeout>();
	private readonly authenticatedByGrant = new Map<string, string>();
	private readonly grantBySocket = new Map<string, string>();

	constructor(private readonly onTimeout: (socketId: string) => void) {}

	admit(socketId: string): boolean {
		if (this.timers.has(socketId) || this.timers.size >= TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER) {
			return false;
		}
		const timer = setTimeout(() => {
			this.timers.delete(socketId);
			this.onTimeout(socketId);
		}, TERMINAL_PEER_HELLO_DEADLINE_MS);
		timer.unref?.();
		this.timers.set(socketId, timer);
		return true;
	}

	authenticate(grantId: string, socketId: string): LocalTerminalAuthenticatedAdmission {
		const previous = this.authenticatedByGrant.get(grantId) ?? null;
		if (previous === null && this.authenticatedByGrant.size >= TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER) {
			return { admitted: false, replacedSocketId: null };
		}
		this.clear(socketId);
		this.authenticatedByGrant.set(grantId, socketId);
		this.grantBySocket.set(socketId, grantId);
		return { admitted: true, replacedSocketId: previous === socketId ? null : previous };
	}

	clear(socketId: string): void {
		const timer = this.timers.get(socketId);
		if (!timer) return;
		clearTimeout(timer);
		this.timers.delete(socketId);
	}

	retire(socketId: string): void {
		this.clear(socketId);
		const grantId = this.grantBySocket.get(socketId);
		if (!grantId) return;
		this.grantBySocket.delete(socketId);
		if (this.authenticatedByGrant.get(grantId) === socketId) {
			this.authenticatedByGrant.delete(grantId);
		}
	}

	dispose(): void {
		for (const timer of this.timers.values()) clearTimeout(timer);
		this.timers.clear();
		this.authenticatedByGrant.clear();
		this.grantBySocket.clear();
	}
}