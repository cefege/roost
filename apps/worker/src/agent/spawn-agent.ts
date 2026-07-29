// Agent-session spawn. Structurally mirrors spawnShell (session-spawn.ts) —
// channel allocation, record registration, `opened` event, one attach, git/ports
// kickoff — but creates NO wterm core, never calls getMultiplexedPool(), and
// never touches the keeper. The omp child IS the attachment.
//
// The channel id is still allocated: `channel` is a NOT NULL column and a
// required field on OpenedEvt, and an integer costs nothing. An agent channel
// simply never carries bytes, so byte-hub's channel map needs no change.

import type { SessionManager } from "../session-manager.ts";
import type { SessionRecord } from "../session-record.ts";
import type { SessionId } from "@roost/shared";
import { asSessionId, log, diag } from "@roost/shared";
import { randomUUID } from "node:crypto";
import { newTraceId } from "@roost/shared/trace";
import { FsmChannel } from "../fsm.ts";
import { expandTilde } from "../util/path.ts";
import { AgentController } from "./agent-controller.ts";
import { startOmpRpc } from "./rpc-process.ts";

export interface SpawnAgentOptions {
	/** Explicit session id — the respawn path reuses the DB row's sid so SPA
	 *  URLs keep working. */
	targetSessionId?: SessionId;
	/** Absolute omp session .jsonl to `--resume`, from AgentState.session_file. */
	resumeFile?: string;
}

export async function spawnAgent(
	this: SessionManager,
	cwd: string,
	opts: SpawnAgentOptions = {},
): Promise<SessionRecord> {
	const channelId = this.nextChannelId();
	const sessionId = opts.targetSessionId ?? asSessionId(randomUUID());
	const resolvedCwd = expandTilde(cwd);
	const fsm = new FsmChannel((from, to, event) =>
		this._onTransition(sessionId, channelId, from, to, event),
	);

	// Spawn BEFORE registering. Unlike a PTY there is no first-output race to
	// lose bytes to, and a missing `omp` binary must fail the spawn RPC outright
	// rather than leave a half-registered session behind.
	const rpc = await startOmpRpc({ cwd: resolvedCwd, resumeFile: opts.resumeFile });

	const sendEntries = this.sendAgentEntriesUpstream;
	const agent = new AgentController({
		sessionId,
		rpc,
		// Dropping frames while the link is down is correct: the SPA backfills
		// through SessionsGetAgentEntries, which reads this same ring.
		sendEntries: (frame) => sendEntries?.(frame),
		emitEvent: (event) => this.emitEvent(event),
	});
	const record: SessionRecord = {
		sessionId,
		channelId,
		socketPath: `omp:${channelId}`,
		kind: "agent",
		cwd: resolvedCwd,
		fsm,
		session_trace_id: newTraceId(),
		spawnedAtMs: Date.now(),
		childPid: rpc.pid,
		agent,
	};
	this.sessions.set(channelId, record);
	diag("session.spawn", {
		sid: sessionId,
		channel_id: channelId,
		session_trace_id: record.session_trace_id,
		kind: "agent",
		cwd: resolvedCwd,
		pid: rpc.pid,
		resumed: opts.resumeFile !== undefined,
	});

	this.emitEvent({
		kind: "opened",
		session_id: sessionId,
		worker_fp: this.workerFp,
		channel: channelId,
		session_kind: "agent",
		cwd: resolvedCwd,
		ts: Date.now(),
	});

	// The child process is the initial attachment, so every natural close still
	// satisfies the one-attach-per-session lifecycle invariant.
	fsm.send({ kind: "attach" });
	this._startGitBranch(record);
	this._startPorts(record);
	// History seeding + the first state patch are async and must not block the
	// spawn reply; the SPA renders an empty transcript and fills in.
	void agent.start({ resumed: opts.resumeFile !== undefined }).catch((err) =>
		log.warn("session-manager", "agent_start_failed", {
			sessionId,
			error: err instanceof Error ? err.message : String(err),
		}),
	);
	log.info("session-manager", "agent spawned", {
		sessionId,
		channelId,
		cwd: resolvedCwd,
		pid: rpc.pid,
	});
	return record;
}
