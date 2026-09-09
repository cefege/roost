// Fresh shell spawn implementation split out of SessionManager.
// The manager reserves durable open and future-close capacity before calling it.
// SessionManager holds its shared channel-creation lease across this promise.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { SessionEventReservation } from "./event-sink.ts";
import type { ChannelId, SessionId } from "@roost/shared/wire";
import type { TerminalCoreLease } from "./terminal-core-capacity.ts";
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { asSessionId } from "@roost/shared/wire";
import { randomUUID } from "node:crypto";
import { newTraceId } from "@roost/shared/trace";
import { initCellEmitState } from "@roost/shared/cell";
import { isTerminalGeometry } from "@roost/shared/viewport";
import { FsmChannel } from "./fsm.ts";
import { canonicalSessionCwd } from "./util/path.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { createSbRing } from "./session-scrollback-ring.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";
import { initAgentOscState } from "./terminal-stream-scan.ts";
import { resolveShellSpec } from "./shell-spec.ts";

/** Spawn a plain shell session. Returns the admitted record.
 *  An optional targetSessionId lets a caller-minted logical session ID cross
 *  the async terminal-core and keeper-spawn boundary without being replaced. */
export async function spawnShell(
	this: SessionManager,
	cwd: string,
	cols: number | undefined,
	rows: number | undefined,
	targetSessionId: SessionId | undefined,
	openedReservation: SessionEventReservation,
	closeReservation: SessionEventReservation,
): Promise<SessionRecord> {
	let openedOwned = true;
	let closeOwned = true;
	let record: SessionRecord | null = null;
	let spawnAttempted = false;
	let channelId: ChannelId | null = null;
	let terminalCoreLease: TerminalCoreLease | null = null;
	try {
		if (targetSessionId && this.getBySessionId(targetSessionId)) {
			throw new Error(`session ${targetSessionId} is already live`);
		}
		const spawnCols = cols ?? 80;
		const spawnRows = rows ?? 24;
		if (!isTerminalGeometry({ cols: spawnCols, rows: spawnRows })) {
			throw new Error("spawn geometry must be within 1..256 on both axes");
		}
		channelId = this.nextChannelId();
		const sessionId = targetSessionId ?? asSessionId(randomUUID());
		const resolvedCwd = canonicalSessionCwd(cwd);
		const shellSpec = resolveShellSpec({
			cwd: resolvedCwd,
			sessionId: String(sessionId),
			envOverlay: withAgentStatusEnvironment({}, String(sessionId)),
		});
		const fsm = new FsmChannel((from, to, event) =>
			this._onTransition(sessionId, channelId!, from, to, event),
		);

		const socketPath = `mux:${channelId}`;
		// Register before keeper spawn so prompt bytes emitted immediately after
		// SpawnAck already have terminal state to receive them.
		const allocatedCore = await this.allocateTerminalCore(
			"fresh",
			spawnCols,
			spawnRows,
		);
		terminalCoreLease = allocatedCore.lease;
		const wtermCore = allocatedCore.core;
		if (wtermCore.getCols() !== spawnCols || wtermCore.getRows() !== spawnRows) {
			throw new Error("terminal core did not retain validated spawn geometry");
		}
		record = {
			sessionId,
			channelId,
			socketPath,
			kind: "shell",
			cwd: resolvedCwd,
			shellSpec,
			fsm,
			scrollback: createSbRing(),
			head_seq: 0,
			alt_mode: false,
			mode_carry: new Uint8Array(0),
			osc7_carry: new Uint8Array(0),
			query_carry: new Uint8Array(0),
			...initAgentOscState(),
			wtermCore,
			terminalCoreLease: allocatedCore.lease,
			session_trace_id: newTraceId(),
			cell_emit: initCellEmitState(newTraceId(), randomUUID()),
			lastPtyOutMs: 0,
			sb_origin_pin: null,
			spawnedAtMs: Date.now(),
			closeReservation,
		};
		this.sessions.set(channelId, record);
		allocatedCore.lease.activate();
		this.channelResizeSeq.set(channelId, 0);
		diag("session.spawn", {
			sid: sessionId,
			channel_id: channelId,
			session_trace_id: record.session_trace_id,
			kind: "shell",
			cwd: resolvedCwd,
			cols: spawnCols,
			rows: spawnRows,
		});

		spawnAttempted = true;
		record.childPid = await getMultiplexedPool().spawn({
			channelId,
			shellSpec,
			cols: spawnCols,
			rows: spawnRows,
			callbacks: this.muxCallbacks(channelId),
		});
		this.lastAppliedSize.set(channelId, { cols: spawnCols, rows: spawnRows });

		this.emitEvent({
			kind: "opened",
			session_id: sessionId,
			worker_fp: this.workerFp,
			channel: channelId,
			session_kind: "shell",
			cwd: resolvedCwd,
			ts: Date.now(),
		}, openedReservation);
		openedOwned = false;
		this.holdSessionEvent(closeReservation);
		closeOwned = false;
	} catch (error) {
		if (channelId !== null && spawnAttempted) {
			getMultiplexedPool().kill(channelId);
		}
		if (channelId !== null && record && this.sessions.get(channelId) === record) {
			this._dropChannelState(channelId);
		}
		terminalCoreLease?.release();
		if (openedOwned) this.releaseSessionEvent(openedReservation);
		if (closeOwned) this.releaseSessionEvent(closeReservation);
		throw error;
	}
	const admittedRecord = record as SessionRecord;

	admittedRecord.fsm.send({ kind: "attach" });
	this._startGitBranch(admittedRecord);
	this._startPorts(admittedRecord);
	log.info("session-manager", "shell spawned", {
		sessionId: admittedRecord.sessionId,
		channelId: admittedRecord.channelId,
		cwd: admittedRecord.cwd,
	});
	return admittedRecord;
}
