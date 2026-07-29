// Worker entry point.
//
// Boot sequence: config, credentials, OMP bridge installation, outbound
// coordinator link, heartbeat, and live-session snapshot.
// Worker has no inbound port: all browser commands arrive through CoordLink.

import { loadWorkerConfig } from "./config.ts";
import { loadWorkerKey, mintJwt } from "./jwt.ts";
import { createCoordClient } from "./coord-client.ts";
import { runInstall } from "./install.ts";
import { startHeartbeat } from "./heartbeat.ts";
import { SessionManager } from "./session-manager.ts";
import { emitSnapshot } from "./snapshot.ts";
import { startCoordLink } from "./transport/CoordLink.ts";
import { handleAttachmentChunk } from "./attachment-upload.ts";
import { handleBrowserCommand } from "./browser-command-handler.ts";
import { handleKeeperSurvivor } from "./boot-keeper.ts";
import { setupReconcile } from "./boot-reconcile.ts";
import { coordLinkSink } from "./event-sink.ts";
import { CoordTarget } from "./coord-target.ts";
import { WorkerCoordRelocation } from "./coord-relocation.ts";
import { createCoordRelocationRecovery } from "./coord-relocation-recovery.ts";
import { asWorkerFp } from "@roost/shared";
import { log, diag, signal, workerDataDir } from "@roost/shared";
import { coordDataDir, coordServicePath, workerServicePath } from "@roost/shared/paths";
import { createHash } from "node:crypto";
const _workerSha8 = (b: Uint8Array): string =>
	createHash("sha256").update(b).digest("hex").slice(0, 8);
import { join } from "node:path";
import { homedir } from "node:os";

// hook.sock lives in the same data dir as the worker key/coord-verifying-key.
// install.sh always sets ROOST_WORKER_DATA_DIR; default is v2-isolated.
const SUPPORT = workerDataDir();

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
	const coordDataRoot = coordDataDir();
	const coordTarget = new CoordTarget({
		dataDir: coordDataRoot,
		dbPath: process.env.ROOST_COORDINATOR_DB ?? join(coordDataRoot, "coordinator_v2.db"),
		keyPath: process.env.ROOST_COORDINATOR_KEY_PATH ?? join(coordDataRoot, "ssh_ed25519.key"),
		authorizedKeysPath: process.env.ROOST_COORDINATOR_AUTHORIZED_KEYS ?? join(coordDataRoot, "authorized_keys.roost"),
		handoffPath: process.env.ROOST_COORDINATOR_HANDOFF_PATH ?? join(coordDataRoot, "coord-handoff.json"),
		servicePath: coordServicePath(),
	});
	const relocation = new WorkerCoordRelocation(
		join(SUPPORT, "coord-relocation.json"),
		workerServicePath(),
	);
	const recoveredRelocation = relocation.load();
	// COMMITTED counts too: commit() now keeps the journal so a service restart
	// before the next full login still finds the new endpoint.
	if (recoveredRelocation && recoveredRelocation.state !== "STAGED") cfg.coordinatorUrl = recoveredRelocation.target_url;
	let client = createCoordClient({ cfg, getJwt: () => mintJwt(key, "roost-coordinator") });
	const setCoordinatorEndpoint = (url: string): void => {
		cfg.coordinatorUrl = url;
		client = createCoordClient({ cfg, getJwt: () => mintJwt(key, "roost-coordinator") });
	};

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
		onCoordMovePrepare: (request) => coordTarget.prepare(request),
		onCoordMoveSnapshotStart: (request) => coordTarget.startSnapshot(request),
		onCoordMoveSnapshotChunk: (chunk) => coordTarget.appendSnapshot(chunk),
		onCoordRelocate: async (request) => {
			// No logging here at all meant `roost logs worker` showed nothing
			// during a failed move — errors only ever surfaced if the
			// coordinator happened to persist them.
			try {
				if (request.action === "STAGE") {
					relocation.stage(request);
					return;
				}
				if (request.action === "ACTIVATE") {
					relocation.activate(request);
					setCoordinatorEndpoint(request.target_url);
					setTimeout(() => coordLink.relocate(request.target_url), 0);
					return;
				}
				if (request.action === "COMMIT") {
					await relocation.commit(
						() => coordLink.unackedEventCount(),
						(url, force) => coordLink.relocate(url, force),
					);
					// No-op on every worker but the new host: only the move target
					// has a handoffs/<id>/ staging + rollback directory.
					coordTarget.finalizeCommit(request.handoff_id);
					reannounceAfterRelocation(request.target_url);
					return;
				}
				if (request.action === "ABORT") {
					await coordTarget.abort(request.handoff_id);
					await relocation.abort(request.handoff_id, (url) => {
						setCoordinatorEndpoint(url);
						coordLink.relocate(url);
					});
				}
			} catch (error) {
				log.error("worker", "coord_relocate_failed", {
					action: request.action, handoff_id: request.handoff_id, error: String(error),
				});
				signal("worker.coord_relocate_failed", {
					action: request.action, handoff_id: request.handoff_id,
					error: String(error), cooldownKey: request.handoff_id,
				});
				throw error;
			}
		},
		onBrowserCommand: (msg) => handleBrowserCommand(msg, { coordLink, sessionMgr }),
	});
	// The unacked replay that carries events across the cutover is in-memory and
	// capped, and emitSnapshot/reconcileOpenSessions run only at boot — nothing
	// re-announces after a relocation. emitSnapshot is a pure re-announce the
	// coordinator's projection folds idempotently, so the happy path pays one
	// extra message and the lossy paths get a repair pass.
	function reannounceAfterRelocation(targetUrl: string): void {
		void (async () => {
			const deadline = Date.now() + 60_000;
			while (Date.now() < deadline) {
				if (coordLink.state().kind === "open" && cfg.coordinatorUrl === targetUrl) {
					await emitSnapshot({ mgr: sessionMgr, sink, workerFp });
					log.info("worker", "coord_relocate_reannounced", { target_url: targetUrl });
					return;
				}
				await Bun.sleep(500);
			}
			log.warn("worker", "coord_relocate_reannounce_timeout", { target_url: targetUrl });
		})().catch((error) => log.warn("worker", "coord_relocate_reannounce_failed", { error: String(error) }));
	}
	const recoverRelocation = createCoordRelocationRecovery({
		relocation,
		link: coordLink,
		statusAt: (url, handoffId) =>
			createCoordClient({
				cfg: { ...cfg, coordinatorUrl: url },
				getJwt: () => mintJwt(key, "roost-coordinator"),
			}).coordinatorMoveStatus({ handoffId }, { timeoutMs: 5_000 }),
		setCoordinatorEndpoint,
		reannounce: reannounceAfterRelocation,
		abortTarget: (handoffId) => coordTarget.abort(handoffId),
		currentCoordinatorUrl: () => cfg.coordinatorUrl,
	});
	// A crashed source may miss the ACTIVATE frame. Query both public move
	// statuses after a sustained outage; recovery never blocks boot or the
	// connection retry loop.
	const triggerRelocationRecovery = (): void => {
		void recoverRelocation().catch((error) =>
			log.warn("worker", "coord_relocation_recovery_failed", { error: String(error) }),
		);
	};
	triggerRelocationRecovery();
	setInterval(triggerRelocationRecovery, 5_000);
	// phase-25d: teeSink retired. Single emit boundary via CoordLink.
	// tRPC sessions.emit + the trpcSink branch deleted; CoordLink has
	// been proven through smoke + multi-restart cycles.
	const sink = coordLinkSink(coordLink);

	// att1b — attachment TTL/LRU reaper. 1h sweep interval; 24h TTL;
	// 1 GB LRU cap on ~/.roost/attachments/.
	const { startAttachmentReaper } = await import("./attachment-reaper.ts");
	startAttachmentReaper();


	// Session manager. phase-24d-1: ALL PTY bytes flow upstream on
	// CoordLink — no inbound worker WSS exists anymore.
	const sessionMgr = new SessionManager({
		workerFp,
		sink,
		sendBinaryUpstream: (bytes) => coordLink.sendBinary(bytes),
		sendCellGridUpstream: (channelId, frame) =>
			coordLink.sendCellGrid(channelId, frame),
		sendAgentEntriesUpstream: (frame) => coordLink.sendAgentEntries(frame),
	});


	// Worker has NO inbound port. Browser commands arrive as
	// `browser-command` frames on CoordLink downstream. PTY bytes flow
	// upstream via persistent KeeperClient per session.

	// Start heartbeat (first beat registers/updates worker row).
	diag("worker.boot", { step: "heartbeat" });
	await startHeartbeat({ client: () => client });

	diag("worker.boot", { step: "reconcile" });
	const { reconcileOpenSessions } = setupReconcile({
		client: () => client,
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
