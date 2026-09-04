// Session respawn replaces a missing keeper PTY without replacing its logical session.
// SessionManager delegates here after reserving durable respawn and close events.
// Admission remains atomic so failed replacement cannot discard the prior live record.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { LifecycleReservation } from "./event-sink.ts";
import type { SessionId, ChannelId } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { newTraceId } from "@roost/shared/trace";
import { initCellEmitState } from "@roost/shared/cell";
import { isTerminalGeometry } from "@roost/shared/viewport";
import { randomUUID } from "node:crypto";
import { FsmChannel } from "./fsm.ts";
import { canonicalSessionCwd } from "./util/path.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { initAgentOscState } from "./terminal-stream-scan.ts";
import { _createWtermCore } from "./session-constants.ts";
import { createSbRing } from "./session-scrollback-ring.ts";
import { withAgentStatusEnvironment } from "./agent-status/environment.ts";
import { resolveShellSpec, type ShellSpec } from "./shell-spec.ts";
/** Rebind an existing session_id to a fresh keeper PTY: the boot-time
 *  auto-respawn loop after `resume()` returned false, plus future manual
 *  restarts. Coord's row keeps its identity (workspace + sidebar); only the PTY channel is new.
 *  Emits `respawned`, not `opened`/`closed` — logically continuous, not recreated. */
export async function respawn(
	this: SessionManager,
	opts: {
		oldSessionId: SessionId;
		cwd: string;
		kind: "shell";
		cols?: number;
		rows?: number;
		shellSpec?: ShellSpec;
	},
	eventReservation: LifecycleReservation,
	closeReservation: LifecycleReservation,
	releaseReservationsOnFailure: boolean,
): Promise<void> {
	let eventOwned = true;
	let closeOwned = true;
	let spawnAttempted = false;
	let channelId: ChannelId | null = null;
	let record: SessionRecord | null = null;
	const existing = this.getBySessionId(opts.oldSessionId);
	try {
		const existingSize = existing
			? {
				cols: existing.wtermCore.getCols(),
				rows: existing.wtermCore.getRows(),
			}
			: null;
		const requestedCwd = canonicalSessionCwd(opts.cwd);
		const shellSpec =
			existing?.shellSpec
			?? opts.shellSpec
			?? resolveShellSpec({
				cwd: requestedCwd,
				sessionId: String(opts.oldSessionId),
				envOverlay: withAgentStatusEnvironment({}, String(opts.oldSessionId)),
			});
		const resolvedCwd = shellSpec.cwd;
		const cols = opts.cols ?? existingSize?.cols ?? 80;
		const rows = opts.rows ?? existingSize?.rows ?? 24;
		if (!isTerminalGeometry({ cols, rows })) {
			throw new Error("respawn geometry must be within 1..256 on both axes");
		}

		channelId = this.nextChannelId();
		const fsm = new FsmChannel((from, to, event) =>
			this._onTransition(opts.oldSessionId, channelId!, from, to, event),
		);
		const wtermCore = await _createWtermCore(cols, rows);
		if (wtermCore.getCols() !== cols || wtermCore.getRows() !== rows) {
			throw new Error("terminal core did not retain validated respawn geometry");
		}

		record = {
			sessionId: opts.oldSessionId,
			channelId,
			socketPath: `mux:${channelId}`,
			kind: opts.kind,
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
			session_trace_id: newTraceId(),
			cell_emit: initCellEmitState(newTraceId(), randomUUID()),
			lastPtyOutMs: 0,
			sb_origin_pin: null,
			spawnedAtMs: Date.now(),
			closeReservation,
		};
		this.sessions.set(channelId, record);
		diag("session.spawn", {
			sid: opts.oldSessionId,
			channel_id: channelId,
			session_trace_id: record.session_trace_id,
			kind: opts.kind,
			cwd: resolvedCwd,
			cols,
			rows,
		});

		spawnAttempted = true;
		record.childPid = await getMultiplexedPool().spawn({
			channelId,
			shellSpec,
			cols,
			rows,
			callbacks: this.muxCallbacks(channelId),
		});
		if (this.sessions.get(channelId) !== record) {
			closeOwned = false;
			throw new Error("respawned channel exited before lifecycle admission");
		}
		this.channelResizeSeq.set(channelId, 0);
		this.lastAppliedSize.set(channelId, { cols, rows });

		this.emitEvent({
			kind: "respawned",
			session_id: opts.oldSessionId,
			new_channel: channelId,
			ts: Date.now(),
		}, eventReservation);
		eventOwned = false;
	} catch (error) {
		if (channelId !== null && spawnAttempted) {
			getMultiplexedPool().kill(channelId);
		}
		if (channelId !== null && record && this.sessions.get(channelId) === record) {
			this._dropChannelState(channelId);
		}
		if (releaseReservationsOnFailure) {
			if (eventOwned) this.releaseLifecycleEvent(eventReservation);
			if (closeOwned) this.releaseLifecycleEvent(closeReservation);
		}
		throw error;
	}
	const admittedRecord = record as SessionRecord;

	// Only the durable respawn event makes replacement authoritative. Until
	// that commit, the prior record and its eventual-close capacity stay intact.
	if (existing && this.sessions.get(existing.channelId) === existing) {
		this.releaseLifecycleEvent(existing.closeReservation);
		this._dropChannelState(existing.channelId);
		getMultiplexedPool().kill(existing.channelId);
	}
	this.holdLifecycleEvent(closeReservation);
	closeOwned = false;
	this._startGitBranch(admittedRecord);
	this._startPorts(admittedRecord);
	admittedRecord.fsm.send({ kind: "attach" });
	log.info("session-manager", "respawned", {
		sessionId: opts.oldSessionId,
		channelId: admittedRecord.channelId,
		cwd: admittedRecord.cwd,
		kind: opts.kind,
	});
}
