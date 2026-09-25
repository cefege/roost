// Common terminal-packet port contract for loopback and WebRTC carriers.
// LocalTerminalSockets owns frame serialization and chooses the lane; carriers
// own packet admission, backpressure, and their own close lifecycle.

import type { TerminalPeerPacketLane } from "@roost/protocol/terminal-peer";

export type TerminalPacketSendResult = "accepted" | "backpressured" | "refused";

export interface TerminalPacketPort {
	readonly socketId: string;
	readonly kind: "loopback" | "webrtc";
	readonly open: boolean;
	bufferedBytes(): number;
	send(bytes: Uint8Array, lane: TerminalPeerPacketLane): TerminalPacketSendResult;
	close(code?: number, reason?: string): void;
}
