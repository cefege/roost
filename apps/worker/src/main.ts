// Worker entry point. Boot sequence (post phase-24):
//   1. loadWorkerConfig
//   2. loadWorkerKey + mintJwt factory
//   3. runInstall (ensure keypair + redeem bootstrap token)
//   4. startCoordLink (outbound WSS dial to coord, bidir)
//   5. startHeartbeat (30s loop)
//   6. startHookListener (UDS for claude --settings hooks)
//   7. emitSnapshot (re-announce live sessions on reconnect)
//
// Worker has NO inbound port — every command arrives via CoordLink's
// outbound WebSocket. LaunchAgent: com.roost.worker-v2.

import { loadWorkerConfig } from "./config.ts";
import { loadWorkerKey, mintJwt } from "./jwt.ts";
import { createCoordClient } from "./coord-client.ts";
import { runInstall } from "./install.ts";
import { startHeartbeat } from "./heartbeat.ts";
import { startHookListener } from "./claude/hooks.ts";
import { SessionManager } from "./session-manager.ts";
import { emitSnapshot } from "./snapshot.ts";
import { startCoordLink } from "./transport/CoordLink.ts";
import { handleAttachmentChunk } from "./attachment-upload.ts";
import { handleBrowserCommand } from "./browser-command-handler.ts";
import { handleKeeperSurvivor } from "./boot-keeper.ts";
import { setupReconcile } from "./boot-reconcile.ts";
import { coordLinkSink } from "./event-sink.ts";
import { asWorkerFp } from "@roost/shared";
import { log, diag, signal } from "@roost/shared";
import { createHash } from "node:crypto";
const _workerSha8 = (b: Uint8Array): string =>
	createHash("sha256").update(b).digest("hex").slice(0, 8);
import { join } from "node:path";
import { homedir } from "node:os";

// hook.sock lives in the same data dir as the worker key/coord-verifying-key.
// install.sh always sets ROOST_WORKER_DATA_DIR; default is v2-isolated.
const SUPPORT =
	process.env.ROOST_WORKER_DATA_DIR ??
	join(homedir(), "Library", "Application Support", "RoostWorkerV2");

export async function runWorker() {
	// Worker-scoped global handlers — installed when the worker RUNS (source
	// `bun run main.ts` or compiled `roost worker`), NOT on mere import into the
	// CLI, so other subcommands never inherit the worker's exit-on-error.
	process.on("unhandledRejection", (err) => {
		const stack = err instanceof Error ? err.stack : undefined;
		const msg = err instanceof Error ? err.message : String(err);
		signal("worker.uncaught", { kind: "rejection", msg, stack8: stack?.slice(0, 240) ?? null, cooldownKey: "worker" });
		console.error(
			JSON.stringify({
				ts: Date.now(),
				level: "error",
				msg: `unhandled: ${err instanceof Error ? (err.stack ?? err.message) : String(err)}`,
			}),
		);
		process.exit(1);
	});
	process.on("uncaughtException", (err) => {
		const stack = err instanceof Error ? err.stack : undefined;
		const msg = err instanceof Error ? err.message : String(err);
		signal("worker.uncaught", { kind: "error", msg, stack8: stack?.slice(0, 240) ?? null, cooldownKey: "worker" });
		console.error(
			JSON.stringify({
				ts: Date.now(),
				level: "error",
				msg: `uncaught: ${err instanceof Error ? (err.stack ?? err.message) : String(err)}`,
			}),
		);
		process.exit(1);
	});
	log.info("worker", "starting");
	await handleKeeperSurvivor(join(SUPPORT, "mux-keeper.sock"));

	diag("worker.boot", { step: "config" });
	const cfg = loadWorkerConfig();
	log.info("worker", "config loaded", {
		coordinatorUrl: cfg.coordinatorUrl,
		label: cfg.label,
	});

	diag("worker.boot", { step: "key" });
	const key = await loadWorkerKey(cfg.workerKeyPath);
	const workerFp = asWorkerFp(key.fingerprint);

	const client = createCoordClient({
		cfg,
		getJwt: () => mintJwt(key, "roost-coordinator"),
	});

	// Install: redeem one-shot bootstrap token (first boot only) + register
	// (idempotent, retried by heartbeat). Redeem MUST precede CoordLink so coord
	// trusts the worker's JWT — block on it ONLY when a token is present (first
	// boot; coord freshly deployed + reachable). On reboot there's no token and
	// register is idempotent, so run fire-and-forget: a wedged boot-path call
	// (e.g. a slow `tailscale status`) must NEVER gate CoordLink/heartbeat. This
	// is why coord boots reliably and the worker used to not — nothing external
	// may block the worker's core loops.
	diag("worker.boot", { step: "install" });
	if (cfg.bootstrapToken) {
		await runInstall({ cfg, client });
	} else {
		void runInstall({ cfg, client }).catch((err) =>
			log.warn("worker", "background_install_failed", { error: String(err) }),
		);
	}

	// phase-24a-3: outbound CoordLink — dial coord bidir WSS. 24a-4
	// routes ALL non-snapshot SessionEvents through it via `sink` below.
	// 24a-5 will move snapshot here as well + retire `client.sessions.emit`.
	diag("worker.boot", { step: "link" });
	const coordLink = startCoordLink({
		coordHttpUrl: cfg.coordinatorUrl,
		workerFp,
		workerVersion: "v2",
		mintJwt: () => mintJwt(key, "roost-coordinator"),
		onHelloAck: ({ coord_pubkey_kid }) => {
			log.info("worker", "coord_link_identity", {
				coord_kid: coord_pubkey_kid,
			});
			// Coord's claude_status cache is in-memory (empty after a coord restart)
			// and detection only emits on change — re-announce current statuses so
			// idle claudes don't show as plain terminals until their next transition.
			sessionMgr.resendClaudeStatuses();
		},
		// phase-24c-1: PTY input routed via sessions.input mutation arrives
		// here as a downstream binary frame. Demux by channel_id, only
		// accept DIR_TO_PTY (1), forward to keeper.
		onBinary: (channelId, dir, bytes) => {
			log.info("worker", "onBinary", {
				channelId,
				dir,
				len: bytes.length,
				hasSession: sessionMgr.hasChannel(channelId),
			});
			if (dir !== 1) return;
			const rec = sessionMgr.getByChannel(channelId);
			diag("bytes.up_recv", {
				sid: rec?.sessionId,
				channel_id: channelId,
				session_trace_id: rec?.session_trace_id,
				dir: "up",
				len: bytes.length,
				sha8: _workerSha8(bytes),
			});
			void sessionMgr.input(channelId, bytes);
		},
		// att1-stream: chunked upload assembled to a temp file; reply via the
		// same rpc-ok path save-attachment uses. Logic lives in attachment-upload.ts.
		// SYNCHRONOUS (static import, fs.writeSync) — chunks MUST assemble in the
		// order they arrive; a dynamic import().then() here would let a later
		// chunk's cached import win the microtask race and corrupt the file.
		onAttachmentChunk: (chunk) => {
			// chunk already carries seq from CoordLink; handleAttachmentChunk uses it.
			handleAttachmentChunk(chunk, {
				ok: (absPath) =>
					coordLink.send({
						kind: "rpc-ok",
						request_id: chunk.request_id,
						data: { abs_path: absPath },
					}),
				err: (message) =>
					coordLink.send({
						kind: "rpc-error",
						request_id: chunk.request_id,
						message,
					}),
			});
		},
		// phase-24b-2: browser commands routed via coord WorkerHub. Per
		// CoordWorkerDownstream.browser-command, the inner frame is a
		// ClientControlFrame. Handle the variants that map cleanly to
		// existing SessionManager methods; the rest land in subsequent
		// sub-commits (spawn/input/attach/detach/presence).
		onBrowserCommand: (msg) =>
			handleBrowserCommand(msg, { coordLink, sessionMgr }),
	});
	// phase-25d: teeSink retired. Single emit boundary via CoordLink.
	// tRPC sessions.emit + the trpcSink branch deleted; CoordLink has
	// been proven through smoke + multi-restart cycles.
	const sink = coordLinkSink(coordLink);

	// att1b — attachment TTL/LRU reaper. 1h sweep interval; 24h TTL;
	// 1 GB LRU cap on ~/.roost/attachments/.
	const { startAttachmentReaper } = await import("./attachment-reaper.ts");
	startAttachmentReaper();

	// Hook listener UDS.
	const hookSocketPath = join(SUPPORT, "hook.sock");
	startHookListener(hookSocketPath, (patch) => {
		if (!patch.sessionId || !patch.agentPatch) return;
		const rec = sessionMgr.getBySessionId(patch.sessionId);
		if (!rec) return;
		// applyAgentPatch = emit `agent` SessionEvent + advance the channel FSM.
		sessionMgr.applyAgentPatch({
			sessionId: rec.sessionId,
			patch: patch.agentPatch,
		});
	});

	// Session manager. phase-24d-1: ALL PTY bytes flow upstream on
	// CoordLink — no inbound worker WSS exists anymore.
	const sessionMgr = new SessionManager({
		workerFp,
		sink,
		hookSocketPath,
		sendBinaryUpstream: (bytes) => coordLink.sendBinary(bytes),
		sendCellGridUpstream: (channelId, frame) =>
			coordLink.sendCellGrid(channelId, frame),
		sendClaudeStatusUpstream: (channelId, status) =>
			coordLink.sendClaudeStatus(channelId, status),
	});

	// Worker has NO inbound port. Browser commands arrive as
	// `browser-command` frames on CoordLink downstream. PTY bytes flow
	// upstream via persistent KeeperClient per session.

	// Start heartbeat (first beat registers/updates worker row).
	diag("worker.boot", { step: "heartbeat" });
	await startHeartbeat({ client });

	diag("worker.boot", { step: "reconcile" });
	const { reconcileOpenSessions } = setupReconcile({
		client,
		workerFp,
		sessionMgr,
		sink,
	});

	await reconcileOpenSessions("boot");

	// Snapshot: re-announce live sessions (relevant after restart).
	// 24a-5: routed through SessionEventSink (CoordLink-backed). The
	// CoordLink pending queue is FIFO + drains in order on open, so
	// snapshot orders correctly w.r.t. any earlier `opened` events
	// queued before this point.
	diag("worker.boot", { step: "snapshot" });
	await emitSnapshot({ mgr: sessionMgr, sink, workerFp });

	log.info("worker", "ready", {
		fingerprint: workerFp,
		coordLinkState: coordLink.state().kind,
	});

	// Graceful shutdown: launchd sends SIGTERM on `launchctl kickstart -k` /
	// unload; Ctrl-C in dev sends SIGINT. Tear down the long-lived owners so
	// the process exits clean instead of leaving reaper/sweep intervals and the
	// CoordLink stream dangling. Idempotent + guarded so a double-signal (TERM
	// then an impatient second TERM) runs teardown once. The keeper subprocess
	// self-suicides when its UDS is unlinked, so we don't kill it here.
	let _shuttingDown = false;
	const shutdown = (sig: string) => {
		if (_shuttingDown) return;
		_shuttingDown = true;
		log.info("worker", "shutdown", { signal: sig });
		diag("worker.shutdown", { step: "sessions" });
		try {
			sessionMgr.dispose();
		} catch {
			/* best-effort */
		}
		diag("worker.shutdown", { step: "coordlink" });
		try {
			coordLink.dispose();
		} catch {
			/* best-effort */
		}
		process.exit(0);
	};
	process.on("SIGTERM", () => shutdown("SIGTERM"));
	process.on("SIGINT", () => shutdown("SIGINT"));
}


if (import.meta.main) {
	runWorker().catch((err) => {
		console.error(
			JSON.stringify({
				ts: Date.now(),
				level: "error",
				msg: `main: ${err instanceof Error ? (err.stack ?? err.message) : String(err)}`,
			}),
		);
		process.exit(1);
	});
}
