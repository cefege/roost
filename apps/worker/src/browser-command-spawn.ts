// Browser-command handlers: session lifecycle (kill / spawn-shell / attach /
// respawn-if-missing). Extracted from browser-command-handler.ts (CLAUDE.md
// 400-line cap).

import { log } from "@roost/shared/log";
import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/coord-link.ts";
import type { SessionManager } from "./session-manager.ts";
import {
	isLifecycleOutboxFullError,
	isSessionLifecycleDurabilityError,
} from "./session-manager.ts";

export function handleKill(
	frame: Extract<ClientControlFrame, { kind: "kill" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): void {
	const { coordLink, sessionMgr } = deps;
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		log.warn("worker", "browser_command_kill_unknown_tombstoning", {
			session_id: frame.session_id,
			request_id,
		});
		try {
			sessionMgr.emitClosedTombstone(frame.session_id);
		} catch (error) {
			if (isSessionLifecycleDurabilityError(error)) throw error;
			if (!isLifecycleOutboxFullError(error)) throw error;
			coordLink.send({
				kind: "rpc-error",
				request_id,
				message: "session lifecycle outbox full",
			});
		}
		return;
	}
	sessionMgr.kill(rec.channelId);
}

export function handleSpawnShell(
	frame: Extract<ClientControlFrame, { kind: "spawn-shell" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): void {
	const { coordLink, sessionMgr } = deps;
	void (async () => {
		try {
			const rec = await sessionMgr.spawnShell(
				frame.folder,
				frame.cols,
				frame.rows,
				frame.session_id,
			);
			coordLink.send({
				kind: "rpc-ok",
				request_id,
				data: {
					session_id: rec.sessionId,
					channel_id: rec.channelId,
				},
			});
		} catch (err) {
			if (isSessionLifecycleDurabilityError(err)) throw err;
			coordLink.send({
				kind: "rpc-error",
				request_id,
				message: err instanceof Error ? err.message : String(err),
			});
		}
	})();
}



export function handleAttach(
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	// No persistent KeeperClient yet for coord-routed attach
	// (24c-1 wires that). For now, reply with replay_offset=0;
	// bytes still flow via the legacy browser-direct WS during
	// the parallel-run window.
	coordLink.send({
		kind: "rpc-ok",
		request_id,
		data: { replay_offset: 0 },
	});
	return;
}

export function handleRespawnIfMissing(
	frame: Extract<ClientControlFrame, { kind: "respawn-if-missing" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): void {
	const { coordLink, sessionMgr } = deps;
	void (async () => {
		try {
			const rec = await sessionMgr.respawnIfMissing(
				frame.session_id,
				frame.cwd,
				frame.cols,
				frame.rows,
			);
			coordLink.send({
				kind: "rpc-ok",
				request_id,
				data: {
					session_id: rec.sessionId,
					channel_id: rec.channelId,
					already_live:
						rec.sessionId === frame.session_id
						&& rec.cwd === frame.cwd,
				},
			});
		} catch (err) {
			if (isSessionLifecycleDurabilityError(err)) throw err;
			coordLink.send({
				kind: "rpc-error",
				request_id,
				message: err instanceof Error ? err.message : String(err),
			});
		}
	})();
	return;
}
