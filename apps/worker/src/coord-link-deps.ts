// Builds the coordinator-link dependency object for the worker process.
// Forward refs preserve construction order for the link, SessionManager, and
// agent registry; focused handlers own stateful downstream protocols.

import { createHash } from "node:crypto";
import { diag, isDiagEnabled } from "@roost/observability/diag";
import { log } from "@roost/observability/log";
import type { WorkerFp } from "@roost/protocol/wire";
import { TERMINAL_METADATA_CAPABILITY } from "@roost/protocol/terminal-metadata";
import {
	TerminalStreamStatus,
	TerminalWritePhase,
} from "@roost/protocol/proto/worker_transport_pb";
import { writeAgentPrompt } from "./agent-prompt-control.ts";
import {
	COORD_CELL_SINK_ID,
	resumeCellSink,
	suspendCellSink,
} from "./session-cell-sinks.ts";
import { createKeeperUpdatePrepareHandler } from "./coord-link-keeper-update.ts";
import { handleAttachmentChunk } from "./attachment-upload.ts";
import { handleBrowserCommand } from "./browser-command-handler.ts";
import type { SessionManager } from "./session-manager.ts";
import type { AgentScreenDetector } from "./agent-status/detector.ts";
import type { AgentStatusRegistry } from "./agent-status/registry.ts";
import { makeCoordLinkDirectTerminalHandlers } from "./coord-link-direct-deps.ts";
import { coordLinkInputAuthority } from "./coord-link-input-authority.ts";
import {
	boundedTerminalReason,
	sendTerminalInputResult,
	terminalStreamFailureKind,
} from "./coord-link-terminal-results.ts";
import { replayDurableWindowsUpdateProgress } from "./coord-link-windows-update.ts";
import type { CoordLink, CoordLinkDeps } from "./transport/coord-link.ts";
import type { LocalTerminalWiring } from "./transport/coord-link-types.ts";
import type { SessionEventStore } from "./transport/session-event-store.ts";
import { terminalPipelineSnapshot } from "./terminal-pipeline-snapshot.ts";
import {
	flushTerminalMetadata,
	replayTerminalMetadata,
	setTerminalMetadataNegotiated,
} from "./session-terminal-metadata.ts";
import { TERMINAL_VIEW_OWNER_CAPABILITY } from "./transport/coord-link-constants.ts";

const _workerSha8 = (bytes: Uint8Array): string =>
	createHash("sha256").update(bytes).digest("hex").slice(0, 8);


/** Forward refs to the objects built FROM these deps. runWorker assigns each
 * one the instant it exists; downstream frames arrive only after binding. */
export interface CoordLinkRefs {
	link: CoordLink | null;
	sessionMgr: SessionManager | null;
	agentRegistry: AgentStatusRegistry | null;
	agentDetector: Pick<AgentScreenDetector, "reportingAgentForSession"> | null;
	acquireKeeperUpdateBoundary: (() => Promise<() => void>) | null;
}

export type { LocalTerminalWiring } from "./transport/coord-link-types.ts";

export interface CoordLinkDepsCtx {
	coordHttpUrl: string;
	workerFp: WorkerFp;
	/** Fresh worker-process UUID advertised on every coordinator connection. */
	processEpoch: string;
	mintJwt: () => Promise<string>;
	sessionEventStore: SessionEventStore;
	refs: CoordLinkRefs;
	/** Extends the terminal base capabilities without duplicating hello assembly. */
	additionalCapabilities?: ReadonlySet<string>;
	/** Absent in focused link tests: the worker then answers no view relay, no
	 * grant install and no revocation. */
	localTerminal?: LocalTerminalWiring;
}

export function buildCoordLinkDeps(ctx: CoordLinkDepsCtx): CoordLinkDeps {
	const { refs } = ctx;
	const mgr = (): SessionManager => {
		if (!refs.sessionMgr) throw new Error("coord-link deps used before sessionMgr was bound");
		return refs.sessionMgr;
	};
	const link = (): CoordLink => {
		if (!refs.link) throw new Error("coord-link deps used before the link was bound");
		return refs.link;
	};
	const onKeeperUpdatePrepare = createKeeperUpdatePrepareHandler({
		sessionManager: mgr,
		acquireKeeperUpdateBoundary: () => refs.acquireKeeperUpdateBoundary,
	});
	const local = ctx.localTerminal;
	const directTerminalHandlers = makeCoordLinkDirectTerminalHandlers({
		localTerminal: local,
		processEpoch: ctx.processEpoch,
		sessions: mgr,
	});
	const capabilities = [...new Set([
		TERMINAL_METADATA_CAPABILITY,
		TERMINAL_VIEW_OWNER_CAPABILITY,
		...(ctx.additionalCapabilities ?? []),
	])].sort();
	return {
		coordHttpUrl: ctx.coordHttpUrl,
		workerFp: ctx.workerFp,
		processEpoch: ctx.processEpoch,
		workerVersion: "v2",
		capabilities,
		sessionEventStore: ctx.sessionEventStore,
		mintJwt: ctx.mintJwt,
		...directTerminalHandlers,
		onHelloAck: ({ terminalMetadataNegotiated }) => {
			// The coordinator's own browser sockets are gone with its previous
			// generation; a LOCAL viewer keeps its views, its lease and the live
			// stream, so a coordinator bounce never blanks a local pane.
			local?.viewOwner.dropCoordinatorSockets();
			const sessionMgr = refs.sessionMgr;
			if (!sessionMgr) return;
			setTerminalMetadataNegotiated(sessionMgr, terminalMetadataNegotiated);
			// A coordinator reconnect re-baselines the coord sink ONLY: stream
			// identity, geometry and a local viewer's live delivery are never
			// disturbed by the coordinator's socket churn.
			resumeCellSink(sessionMgr, COORD_CELL_SINK_ID);
		},
		onTerminalViewRelay: (request) => { local?.viewOwner.handleRelay(request); },
		onTerminalViewSocketClosed: (request) => {
			local?.inputRouteOwner.retireConnection(request.socketId);
			local?.viewOwner.closeSocket(request.socketId);
		},
		// A throw here answers the coordinator's acknowledged install with
		// WRpcError, so no browser is ever told a fast path it cannot use.
		onLocalTerminalGrant: local
			? (request) => { local.grants.install(request); }
			: undefined,
		onLocalTerminalGrantRevoke: (request) => {
			local?.revokeDevice(request.deviceFingerprint);
		},
		onLocalAttachmentGrant: local
			? (request) => { local.attachmentGrants.install(request); }
			: undefined,
		onLocalAttachmentGrantRevoke: (request) => {
			local?.revokeAttachmentDevice(request.deviceFingerprint);
		},
		onOpen: () => {
			const sessionMgr = refs.sessionMgr;
			if (!sessionMgr) return;
			setTerminalMetadataNegotiated(sessionMgr, false);
			// A freshly attached socket cannot carry cells until the replay and
			// snapshot barrier clears, so the sink stays suspended until hello-ack.
			suspendCellSink(sessionMgr, COORD_CELL_SINK_ID);
		},
		onDetach: () => {
			// Only unsettled signaling is fenced by coordinator loss. Established
			// authorized peers retain their grant/expiry lifetime while cells keep
			// flowing directly.
			local?.clearCoordinatorGeneration();
			local?.peerOwner.cancelPendingForCoordinator("coordinator_detached");
			local?.attachmentPeerOwner.cancelPendingForCoordinator();
			const sessionMgr = refs.sessionMgr;
			if (!sessionMgr) return;
			setTerminalMetadataNegotiated(sessionMgr, false);
			suspendCellSink(sessionMgr, COORD_CELL_SINK_ID);
		},
		onWritable: () => {
			const sessionMgr = refs.sessionMgr;
			if (!sessionMgr) return;
			resumeCellSink(sessionMgr, COORD_CELL_SINK_ID);
			flushTerminalMetadata(sessionMgr);
		},
		onSnapshotReady: ({ reconnected }) => {
			const sessionMgr = refs.sessionMgr;
			if (sessionMgr) {
				replayTerminalMetadata(sessionMgr);
				// The barrier has cleared, so a baseline parked at hello-ack ships
				// now instead of waiting for a backpressure notification.
				resumeCellSink(sessionMgr, COORD_CELL_SINK_ID);
			}
			refs.agentRegistry?.resend();
			void replayDurableWindowsUpdateProgress(link()).catch((error) => {
				log.warn("windows-update", "progress_replay_failed", { error: String(error) });
			});
			log.info("coord-link", "snapshot_ready", { reconnected });
		},
		// The wire `phase` is derived from the session manager's status under a
		// single invariant: "rejected" is returned only from a stage that
		// provably never wrote (validation, queue refusal, pre-write expiry),
		// and every post-write or post-claim uncertainty is "ambiguous". The
		// coordinator unwinds provisional state only on PRE_WRITE, so a status
		// that overstates certainty can never license a duplicate write.
		onInputRequest: async (request, budget) => {
			const workAdmission = local?.inputWorkBudget.reserveInput({
				origin: "sync",
				byteLength: request.data.byteLength,
			});
			if (workAdmission && !workAdmission.admitted) {
				sendTerminalInputResult(link().send, request, {
					status: "rejected",
					writtenBytes: 0,
					reason: workAdmission.reason,
				}, false);
				return;
			}
			const reservation = workAdmission && workAdmission.admitted
				? workAdmission.reservation
				: undefined;
			try {
				const sessionMgr = mgr();
				const result = await sessionMgr.writeTerminalInput(
					request.sessionId,
					request.inputSeq,
					request.data,
					budget,
					coordLinkInputAuthority(local, request, budget, sessionMgr),
				);
				sendTerminalInputResult(link().send, request, result, false);
			} finally {
				reservation?.release();
			}
		},
		onAgentPrompt: async (request, budget) => {
			const registry = refs.agentRegistry;
			const detector = refs.agentDetector;
			if (!registry || !detector) {
				throw new Error("agent prompt control used before status detection was bound");
			}
			const workAdmission = local?.inputWorkBudget.reserveInput({
				origin: "sync",
				byteLength: Buffer.byteLength(request.text, "utf8") + 13,
			});
			if (workAdmission && !workAdmission.admitted) {
				sendTerminalInputResult(link().send, request, {
					status: "rejected",
					writtenBytes: 0,
					reason: workAdmission.reason,
				}, true);
				return;
			}
			const reservation = workAdmission && workAdmission.admitted
				? workAdmission.reservation
				: undefined;
			try {
				const result = await writeAgentPrompt(request, budget, {
					sessions: mgr(),
					registry,
					detector,
				});
				sendTerminalInputResult(link().send, request, result, true);
			} finally {
				reservation?.release();
			}
		},
		onTerminalStreamState: async (request, budget) => {
			const result = await mgr().applyTerminalStreamState({
				requestId: request.requestId,
				sessionId: request.sessionId,
				streamId: request.streamId,
				enabled: request.enabled,
				cols: request.cols,
				rows: request.rows,
				budget,
			});
			link().send({
				kind: "terminal-stream-result",
				request_id: request.requestId,
				session_id: request.sessionId,
				stream_id: result.streamId,
				enabled: result.enabled,
				status: result.status === "committed"
					? TerminalStreamStatus.COMMITTED
					: result.status === "rejected"
						? TerminalStreamStatus.REJECTED
						: TerminalStreamStatus.AMBIGUOUS,
				channel_resize_seq: BigInt(result.channelResizeSeq),
				effective_cols: result.cols,
				effective_rows: result.rows,
				resized: result.status === "committed" ? result.resized : false,
				phase: result.phase === "written"
					? TerminalWritePhase.WRITTEN
					: result.phase === "pre_write"
						? TerminalWritePhase.PRE_WRITE
						: TerminalWritePhase.UNKNOWN,
				failure_kind: terminalStreamFailureKind(
					result.status === "committed" ? undefined : result.failure,
				),
				reason: boundedTerminalReason(
					result.status === "committed" ? undefined : result.reason,
				),
			});
		},
		onKeeperUpdatePrepare,
		onTerminalPipelineSnapshot: (request) => {
			const snapshot = terminalPipelineSnapshot(
				mgr(),
				request,
				link().pipelineState(),
			);
			link().send({
				kind: "terminal-pipeline-snapshot",
				snapshot,
			});
		},
		onTerminalSnapshotRequest: (request) => {
			mgr().requestTerminalSnapshot(request.sessionId, request.streamId);
		},
		// phase-24c-1: PTY input routed via sessions.input mutation arrives
		// here as a downstream binary frame. Demux by channel_id, only
		// accept DIR_TO_PTY (1), forward to keeper.
		onBinary: (channelId, dir, bytes) => {
			const sessionMgr = mgr();
			// Per-keystroke: one JSON line per keypress at info level was the
			// noisiest thing in the worker log. debug is level-gated in log.ts, so
			// nothing is serialized on the default path.
			log.debug("worker", "onBinary", {
				channelId,
				dir,
				len: bytes.length,
				hasSession: sessionMgr.hasChannel(channelId),
			});
			if (dir !== 1) return;
			const rec = sessionMgr.getByChannel(channelId);
			// Guarded because _workerSha8 is a real sha256 and diag()'s arguments
			// evaluate even when the firehose is off — once per keystroke.
			if (isDiagEnabled()) {
				diag("bytes.up_recv", {
					sid: rec?.sessionId,
					channel_id: channelId,
					session_trace_id: rec?.session_trace_id,
					dir: "up",
					len: bytes.length,
					sha8: _workerSha8(bytes),
				});
			}
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
					link().send({
						kind: "rpc-ok",
						request_id: chunk.request_id,
						data: { abs_path: absPath },
					}),
				err: (message) =>
					link().send({
						kind: "rpc-error",
						request_id: chunk.request_id,
						message,
					}),
			});
		},
		onUpdateBroker: async (command) => {
			switch (process.platform) {
				case "win32": {
					const { handleUpdateBrokerCommand } = await import("@roost/host/windows/windows-update-control");
					const progress = await handleUpdateBrokerCommand({
						requestId: command.request_id,
						jobId: command.job_id,
						action: command.action,
						manifestUrl: command.manifest_url,
						signatureUrl: command.signature_url,
						manifestSha256: command.manifest_sha256,
						publisherSha256: command.publisher_sha256,
					});
					return progress.map((frame) => ({
						request_id: frame.requestId,
						job_id: frame.jobId,
						sequence: frame.sequence,
						phase: frame.phase,
						message: frame.message,
						terminal: frame.terminal,
						success: frame.success,
						error: frame.error,
					}));
				}
				case "darwin":
				case "linux":
					throw new Error("Windows update broker command received on a POSIX worker");
				default:
					throw new Error(`unsupported worker platform: ${process.platform}`);
			}
		},
		onBrowserCommand: (msg) => handleBrowserCommand(msg, { coordLink: link(), sessionMgr: mgr() }),
	};
}
