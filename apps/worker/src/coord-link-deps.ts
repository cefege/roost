// The startCoordLink() dependency object for the worker process. Every
// coordinator→worker callback the worker answers lives here: terminal input and
// viewport writes, PTY byte demux, attachment chunks, browser commands, the
// coordinator-move handshake and the Windows update broker. Extracted verbatim
// from main.ts (CLAUDE.md 400-line cap); main.ts keeps boot orchestration.
//
// Why forward refs instead of closures: the CoordLink, the SessionManager and
// the AgentStatusRegistry are all constructed FROM this object, so none of them
// exists when it is built. main.ts already solved that for two callbacks with
// `sessionMgrForResnapshot` / `agentRegistryForReconnect`; CoordLinkRefs is the
// same pattern generalised so the whole object can live in its own module.

import { randomUUID, createHash } from "node:crypto";
import { diag, isDiagEnabled, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import type { WorkerFp } from "@roost/shared/wire";
import {
	TerminalInputStatus,
	TerminalViewportStatus,
	TerminalWritePhase,
} from "@roost/shared/proto/worker_transport_pb";
import { handleAttachmentChunk } from "./attachment-upload.ts";
import { handleBrowserCommand } from "./browser-command-handler.ts";
import type { CoordTarget } from "./coord-target.ts";
import type { WorkerCoordRelocation } from "./coord-relocation.ts";
import type { SessionManager } from "./session-manager.ts";
import type { AgentStatusRegistry } from "./agent-status/registry.ts";
import type { CoordLink, CoordLinkDeps } from "./transport/coord-link.ts";

const _workerSha8 = (b: Uint8Array): string =>
	createHash("sha256").update(b).digest("hex").slice(0, 8);

async function replayDurableWindowsUpdateProgress(coordLink: CoordLink): Promise<void> {
	if (process.platform !== "win32") return;
	const {
		DurableWindowsUpdateJournalStore,
		readWindowsUpdateProgressFromJournal,
	} = await import("../../roost-cli/src/windows/windows-update-journal.ts");
	const journal = await new DurableWindowsUpdateJournalStore().load();
	if (!journal) return;
	const requestId = randomUUID();
	for (const entry of readWindowsUpdateProgressFromJournal(journal, 0)) {
		coordLink.send({
			kind: "update-progress",
			request_id: requestId,
			job_id: journal.jobId,
			sequence: entry.sequence,
			phase: entry.phase,
			message: entry.message,
			terminal: entry.terminal,
			success: entry.success,
			error: entry.error,
		});
	}
}

/** Forward refs to the three objects built FROM these deps. runWorker assigns
 * each one the instant it exists; every downstream frame arrives strictly after
 * that, so a null read here is a boot-wiring bug rather than a race. */
export interface CoordLinkRefs {
	link: CoordLink | null;
	sessionMgr: SessionManager | null;
	agentRegistry: AgentStatusRegistry | null;
}

export interface CoordLinkDepsCtx {
	coordHttpUrl: string;
	workerFp: WorkerFp;
	mintJwt: () => Promise<string>;
	coordTarget: CoordTarget;
	relocation: WorkerCoordRelocation;
	setCoordinatorEndpoint: (url: string) => void;
	/** Re-announces live sessions once the link settles on the new coordinator. */
	reannounceAfterRelocation: (targetUrl: string) => void;
	refs: CoordLinkRefs;
}

export function buildCoordLinkDeps(ctx: CoordLinkDepsCtx): CoordLinkDeps {
	const { refs, coordTarget, relocation, setCoordinatorEndpoint } = ctx;
	const mgr = (): SessionManager => {
		if (!refs.sessionMgr) throw new Error("coord-link deps used before sessionMgr was bound");
		return refs.sessionMgr;
	};
	const link = (): CoordLink => {
		if (!refs.link) throw new Error("coord-link deps used before the link was bound");
		return refs.link;
	};
	return {
		coordHttpUrl: ctx.coordHttpUrl,
		workerFp: ctx.workerFp,
		workerVersion: "v2",
		mintJwt: ctx.mintJwt,
		onHelloAck: ({ reconnected }) => {
			if (reconnected) refs.sessionMgr?.resnapshotClaimedSessions();
		},
		onWritable: () => {
			refs.sessionMgr?.flushPendingCellRepairs();
		},
		onOpen: (reconnected) => {
			if (reconnected) refs.agentRegistry?.resend();
			void replayDurableWindowsUpdateProgress(link()).catch((error) => {
				log.warn("windows-update", "progress_replay_failed", { error: String(error) });
			});
		},
		// The wire `phase` is derived from the session manager's status under a
		// single invariant: "rejected" is returned only from a stage that
		// provably never wrote (validation, queue refusal, pre-write expiry),
		// and every post-write or post-claim uncertainty is "ambiguous". The
		// coordinator unwinds provisional state only on PRE_WRITE, so a status
		// that overstates certainty can never license a duplicate write.
		onInputRequest: async (request, budget) => {
			const result = await mgr().writeTerminalInput(
				request.sessionId,
				request.inputSeq,
				request.data,
				budget,
			);
			link().send({
				kind: "input-result",
				request_id: request.requestId,
				session_id: request.sessionId,
				input_seq: request.inputSeq,
				status: result.status === "accepted"
					? TerminalInputStatus.ACCEPTED
					: result.status === "rejected"
						? TerminalInputStatus.REJECTED
						: TerminalInputStatus.AMBIGUOUS,
				written_bytes: result.writtenBytes,
				phase: result.status === "accepted"
					? TerminalWritePhase.WRITTEN
					: result.status === "rejected"
						? TerminalWritePhase.PRE_WRITE
						: TerminalWritePhase.UNKNOWN,
				reason: result.status === "accepted" ? undefined : result.reason,
			});
		},
		onViewportRequest: async (request, budget) => {
			const result = await mgr().applyTerminalViewport({
				sessionId: request.sessionId,
				viewerId: request.viewerId,
				clientSeq: request.clientSeq,
				cols: request.cols,
				rows: request.rows,
				cause: request.cause,
				heldCellSeq: request.heldCellSeq,
				budget,
			});
			switch (result.status) {
				case "committed":
					link().send({
						kind: "viewport-result",
						request_id: request.requestId,
						session_id: request.sessionId,
						client_seq: request.clientSeq,
						status: TerminalViewportStatus.COMMITTED,
						channel_resize_seq: BigInt(result.channelResizeSeq),
						cols: result.cols,
						rows: result.rows,
						resized: result.resized,
						phase: TerminalWritePhase.WRITTEN,
					});
					return;
				case "rejected":
					link().send({
						kind: "viewport-result",
						request_id: request.requestId,
						session_id: request.sessionId,
						client_seq: request.clientSeq,
						status: TerminalViewportStatus.REJECTED,
						channel_resize_seq: 0n,
						cols: 0,
						rows: 0,
						resized: false,
						phase: TerminalWritePhase.PRE_WRITE,
						reason: result.reason,
					});
					return;
				case "ambiguous":
					link().send({
						kind: "viewport-result",
						request_id: request.requestId,
						session_id: request.sessionId,
						client_seq: request.clientSeq,
						status: TerminalViewportStatus.AMBIGUOUS,
						channel_resize_seq: 0n,
						cols: 0,
						rows: 0,
						resized: false,
						phase: TerminalWritePhase.UNKNOWN,
						reason: result.reason,
					});
					return;
			}
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
					await relocation.stage(request);
					return;
				}
				if (request.action === "ACTIVATE") {
					await relocation.activate(request);
					setCoordinatorEndpoint(request.target_url);
					setTimeout(() => link().relocate(request.target_url), 0);
					return;
				}
				if (request.action === "COMMIT") {
					await relocation.commit(
						() => link().unackedEventCount(),
						(url, force) => link().relocate(url, force),
					);
					// No-op on every worker but the new host: only the move target
					// has a handoffs/<id>/ staging + rollback directory.
					await coordTarget.finalizeCommit(request.handoff_id);
					ctx.reannounceAfterRelocation(request.target_url);
					return;
				}
				if (request.action === "ABORT") {
					await coordTarget.abort(request.handoff_id);
					await relocation.abort(request.handoff_id, (url) => {
						setCoordinatorEndpoint(url);
						link().relocate(url);
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
		onUpdateBroker: async (command) => {
			switch (process.platform) {
				case "win32": {
					const { handleUpdateBrokerCommand } = await import("../../roost-cli/src/windows/windows-update-control.ts");
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
