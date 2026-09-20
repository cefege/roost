// Lazy node-datachannel ownership for the worker peer transport.
// Source workers import the pinned package; compiled workers load the staged
// literal N-API addon. No other process imports native peer code eagerly.
// Logger output is fixed and rate-bounded because native text can contain SDP.

import { log } from "@roost/shared/log";
import type * as NodeDataChannel from "node-datachannel";
import { loadEmbeddedTerminalPeerNative } from "./terminal-peer-native.generated.ts";

export type TerminalPeerNative = Pick<
	typeof NodeDataChannel,
	"PeerConnection" | "preload" | "cleanup" | "initLogger" | "getLibraryVersion" | "setSctpSettings"
>;

declare const __ROOST_EMBEDDED_TERMINAL_PEER__: boolean | undefined;

const NATIVE_ERROR_LOG_INTERVAL_MS = 60_000;
let nativePromise: Promise<TerminalPeerNative> | null = null;
let lastNativeErrorLogAt = -Infinity;

export function loadTerminalPeerNative(): Promise<TerminalPeerNative> {
	nativePromise ??= loadAndConfigureTerminalPeerNative();
	return nativePromise;
}

async function loadAndConfigureTerminalPeerNative(): Promise<TerminalPeerNative> {
	// Source mode must stay lazy: importing the worker through another CLI
	// command must not load a platform addon that only peer bootstrap uses.
	const embeddedBuild = typeof __ROOST_EMBEDDED_TERMINAL_PEER__ === "boolean"
		&& __ROOST_EMBEDDED_TERMINAL_PEER__;
	const native = embeddedBuild
		? loadEmbeddedTerminalPeerNative()
		: await import("node-datachannel");
	if (!native) throw new Error("compiled terminal peer addon is unavailable");

	native.initLogger("Error", () => {
		const now = performance.now();
		if (now - lastNativeErrorLogAt < NATIVE_ERROR_LOG_INTERVAL_MS) return;
		lastNativeErrorLogAt = now;
		log.error("terminal-peer", "native_error", {
			diagnostic: "terminal-peer/native_error",
		});
	});
	native.setSctpSettings({
		sendBufferSize: 262_144,
		recvBufferSize: 524_288,
		maxChunksOnQueue: 2_048,
	});
	native.preload();
	return native;
}
