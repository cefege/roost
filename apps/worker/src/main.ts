// Worker entry point.
//
// Boot sequence: config, credentials, OMP bridge installation, outbound
// coordinator link, heartbeat, and live-session snapshot.
// Worker has no inbound port: all browser commands arrive through CoordLink.

import { loadWorkerConfig } from "./config.ts";
import { loadWorkerKey, mintJwt } from "./jwt.ts";
import { createCoordClient } from "./coord-client.ts";
import { runInstall } from "./install.ts";
import { startHeartbeat, type HeartbeatDisposer } from "./heartbeat.ts";
import { SessionManager } from "./session-manager.ts";
import { buildSnapshot } from "./snapshot.ts";
import { startCoordLink } from "./transport/coord-link.ts";
import { buildCoordLinkDeps, type CoordLinkRefs } from "./coord-link-deps.ts";
import { handleKeeperSurvivor } from "./boot-keeper.ts";
import { createWorkerTerminalCoreCapacity } from "./terminal-core-capacity.ts";
import { spendKeeperForceLiveRetireAuthorization } from "./service-definition-env.ts";
import {
	setupReconcile,
	type ReconcileAdmissionOutcome,
	type ReconcileAdmissionSuccess,
} from "./boot-reconcile.ts";
import { coordLinkSink, isFatalSessionEventError } from "./event-sink.ts";
import { openSessionEventStore } from "./transport/session-event-store.ts";
import { AgentScreenDetector } from "./agent-status/detector.ts";
import { AgentStatusRegistry } from "./agent-status/registry.ts";
import { installAgentIntegrations } from "./agent-status/install-integrations.ts";
import { startAgentReportServer, type AgentReportServer } from "./agent-status/report-server.ts";
import { AgentReferenceAdmissionGate } from "./agent-status/reference-admission.ts";
import {
	restoreAgentConversationAfterRespawn,
} from "./agent-conversation-restore.ts";
import { serveServiceHealth } from "@roost/shared/service-health";
import { asWorkerFp } from "@roost/shared/wire";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { ROOST_ARTIFACT_VERSION, ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import { workerDataDir } from "@roost/shared/paths";
import { prepareWtermCoreModule } from "@roost/shared/wterm-core-factory";

import { randomUUID } from "node:crypto";

// hook.sock lives in the same data dir as the worker key.
// install.sh always sets ROOST_WORKER_DATA_DIR; default is v2-isolated.
const SUPPORT = workerDataDir();

export async function completeWorkerBootAdmission(deps: {
	reconcile: () => Promise<ReconcileAdmissionOutcome>;
	activateSnapshotProvider: () => void;
	markReady: () => void;
}): Promise<ReconcileAdmissionSuccess> {
	const outcome = await deps.reconcile();
	if (!outcome.admitted) throw outcome.error;
	deps.activateSnapshotProvider();
	deps.markReady();
	return outcome;
}

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
	// Compile the patched WTerm module without touching the keeper. Survivor
	// admission waits until the coordinator's complete open set is reserved.
	await prepareWtermCoreModule();

	diag("worker.boot", { step: "config" });
	const cfg = loadWorkerConfig();
	log.info("worker", "config loaded", {
		coordinatorUrl: cfg.coordinatorUrl,
		label: cfg.label,
	});
	const terminalCoreCapacity = createWorkerTerminalCoreCapacity({
		terminalCoreCap: cfg.terminalCoreCap,
	});
	// The authorization the deploy installed is destructive, so it is spent by
	// this activation before any keeper work: a value left in the service
	// definition would re-authorize discarding live PTYs on every later restart.
	if (cfg.keeperForceLiveRetire) await spendKeeperForceLiveRetireAuthorization();
	const healthVersion = ROOST_ARTIFACT_VERSION === "dev" ? ROOST_BUILD_SHA : ROOST_ARTIFACT_VERSION;
	const healthBuild = ROOST_BUILD_SHA;
	const processEpoch = randomUUID();
	let workerReady = false;
	let keeperReconciledAtMs: number | null = null;
	let stopHeartbeat: HeartbeatDisposer = () => {};

	diag("worker.boot", { step: "key" });
	const key = await loadWorkerKey(cfg.workerKeyPath);
	const workerFp = asWorkerFp(key.fingerprint);
	const client = createCoordClient({ cfg, getJwt: () => mintJwt(key, "roost-coordinator") });

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
	try {
		await installAgentIntegrations();
	} catch (error) {
		log.warn("agent-status", "integration_install_failed", { error: String(error) });
	}

	// phase-24a-3: outbound CoordLink — dial coord bidir WSS. 24a-4
	// routes ALL non-snapshot SessionEvents through it via `sink` below.
	// 24a-5 will move snapshot here as well + retire `client.sessions.emit`.
	diag("worker.boot", { step: "link" });
	const refs: CoordLinkRefs = {
		link: null,
		sessionMgr: null,
		agentRegistry: null,
		agentDetector: null,
		acquireKeeperUpdateBoundary: null,
	};
	const sessionEventStore = openSessionEventStore();
	const coordLink = startCoordLink(buildCoordLinkDeps({
		coordHttpUrl: cfg.coordinatorUrl,
		workerFp,
		mintJwt: () => mintJwt(key, "roost-coordinator"),
		refs,
		sessionEventStore,
	}));
	// Bind the forward ref before yielding: startCoordLink's first dial awaits
	// mintJwt(), so no callback can observe a null link on this tick.
	refs.link = coordLink;
	// phase-25d: teeSink retired. Single emit boundary via CoordLink.
	// tRPC sessions.emit + the trpcSink branch deleted; CoordLink has
	// been proven through smoke + multi-restart cycles.
	const sink = coordLinkSink(coordLink, sessionEventStore);
	const referenceAdmission = new AgentReferenceAdmissionGate();

	// att1b — attachment TTL/LRU reaper. 1h sweep interval; 24h TTL;
	// 1 GB LRU cap on ~/.roost/attachments/.
	const { startAttachmentReaper } = await import("./attachment-reaper.ts");
	startAttachmentReaper();


	// Session manager. phase-24d-1: ALL PTY bytes flow upstream on
	// CoordLink — no inbound worker WSS exists anymore.
	const sessionMgr = new SessionManager({
		workerFp,
		sink,
		sendBinaryUpstream: (channelId, direction, endSeq, bytes) =>
			coordLink.sendBinary(channelId, direction, endSeq, bytes),
		sendCellGridUpstream: (channelId, frame) =>
			coordLink.sendCellGrid(channelId, frame),
		sendCellGridChunkUpstream: (channelId, chunk) =>
			coordLink.sendCellGridChunk(channelId, chunk),
		terminalCoreCapacity,
	});
	refs.sessionMgr = sessionMgr;
	const agentRegistry = new AgentStatusRegistry({
		publish: (status) => { coordLink.sendAgentStatus(status); },
	});
	refs.agentRegistry = agentRegistry;
	const agentDetector = new AgentScreenDetector(sessionMgr, agentRegistry, undefined, {
		referenceClear: { eventSink: sink, referenceAdmission },
	});
	refs.agentDetector = agentDetector;
	sessionMgr.setAgentStatusHooks({
		terminalChanged: (channelId) => agentDetector.schedule(channelId),
		sessionClosed: (sessionId) => agentDetector.closeSession(sessionId),
	});
	const serviceHealth = await serveServiceHealth("worker", () => {
		const targetLinkReady = coordLink.ready();
		return {
			role: "worker",
			version: healthVersion,
			build: healthBuild,
			processEpoch,
			ready: workerReady && targetLinkReady,
			targetLinkReady,
			coordinatorUrl: cfg.coordinatorUrl,
		};
	}, { dataDir: SUPPORT });


	// Worker has NO inbound port. Browser commands arrive as
	// `browser-command` frames on CoordLink downstream. PTY bytes flow
	// upstream via persistent KeeperClient per session.

	let agentReportServer: AgentReportServer | null = null;
	try {
		agentReportServer = await startAgentReportServer({
			detector: agentDetector,
			registry: agentRegistry,
			eventSink: sink,
			referenceAdmission,
		});
	} catch (error) {
		log.warn("agent-status", "report_server_start_failed", { error: String(error) });
	}

	diag("worker.boot", { step: "reconcile" });
	const {
		reconcileOpenSessions,
		acquireKeeperUpdateBoundary,
	} = setupReconcile({
		client: () => client,
		workerFp,
		sessionMgr,
		referenceAdmission,
		prepareKeeper: (coordinatorOpenSessionIds) =>
			handleKeeperSurvivor(
				coordinatorOpenSessionIds,
				cfg.keeperForceLiveRetire,
				terminalCoreCapacity,
			),
		restoreAgentConversation: (sessionId, reference, resumedReferenceKeys) =>
			restoreAgentConversationAfterRespawn({
				enabled: cfg.agentConversationRestore,
				sessionMgr,
				resumedReferenceKeys,
			}, sessionId, reference),
		beforeRecoveryRead: () =>
			coordLink.waitForDurableSessionEventReplay(),
		onReconcileStarted: () => {
			keeperReconciledAtMs = null;
		},
		onReconciled: (reconciledAtMs) => {
			keeperReconciledAtMs = reconciledAtMs;
		},
	});
	refs.acquireKeeperUpdateBoundary = acquireKeeperUpdateBoundary;

	await completeWorkerBootAdmission({
		reconcile: () => reconcileOpenSessions("boot"),
		activateSnapshotProvider: () => {
			// Snapshot ownership starts only after reconciliation produced the
			// complete local session set.
			diag("worker.boot", { step: "snapshot-provider" });
			coordLink.activateSnapshotProvider(() =>
				buildSnapshot(sessionMgr, workerFp)
			);
		},
		markReady: () => {
			workerReady = true;
		},
	});
	stopHeartbeat = await startHeartbeat({
		client: () => client,
		reconciledAtMs: () => keeperReconciledAtMs,
		readTerminalCoreCapacity: () => {
			const snapshot = terminalCoreCapacity.snapshot();
			return {
				used: snapshot.used,
				pending: snapshot.pending,
				capacity: snapshot.capacity,
				estimated_reserved_bytes: snapshot.estimatedReservedBytes,
				effective_memory_ceiling_bytes: snapshot.effectiveMemoryCeilingBytes,
				boot_rss_bytes: snapshot.bootRssBytes,
				overcommit_count: snapshot.overcommitCount,
				refusal_count: snapshot.refusalCount,
			};
		},
	});
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
	const shutdown = async (sig: string) => {
		if (_shuttingDown) return;
		_shuttingDown = true;
		workerReady = false;
		stopHeartbeat();
		try {
			await serviceHealth.close();
		} catch {
			/* best-effort */
		}
		if (agentReportServer) {
			try {
				await agentReportServer.close();
			} catch {
				/* best-effort */
			}
		}
		log.info("worker", "shutdown", { signal: sig });
		try {
			agentDetector.dispose();
			agentRegistry.dispose();
		} catch {
			/* best-effort */
		}
		diag("worker.shutdown", { step: "sessions" });
		try {
			sessionMgr.dispose();
		} catch (error) {
			if (isFatalSessionEventError(error)) throw error;
		}
		diag("worker.shutdown", { step: "coordlink" });
		try {
			coordLink.dispose();
		} catch {
			/* best-effort */
		}
		diag("worker.shutdown", { step: "session-event-store" });
		sessionEventStore.close();
		process.exit(0);
	};
	process.on("SIGTERM", () => { void shutdown("SIGTERM"); });
	process.on("SIGINT", () => { void shutdown("SIGINT"); });
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
