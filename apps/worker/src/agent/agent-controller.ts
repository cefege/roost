// One controller per agent session: owns the omp child, the seq counter, the
// pending-dialog map, and the status/usage projection into the existing `agent`
// SessionEvent. The transcript itself lives in AgentEntryRing.
//
// Two invariants drive the shape:
//   1. Entries are RE-EMITTED under the same seq as they grow, so the stream is
//      idempotent and a reconnect that re-delivers a frame is harmless.
//   2. An unanswered prompt hangs the agent FOREVER (omp's approval call site
//      passes neither timeout nor signal). Every answerable dialog must reach
//      the UI, and teardown must cancel the ones that never got answered.

import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
	AgentUiFrameSchema,
	type AgentEntriesFrame as PbAgentEntriesFrame,
	type AgentUiFrame as PbAgentUiFrame,
} from "@roost/shared/proto/sync_pb";
import { AGENT_ENTRY_CAPS, clampText } from "@roost/shared/wire/agent-entry";
import type { AgentUiRpcCommand } from "@roost/shared/wire";
import type { AgentState, AgentStatus, SessionEvent, SessionId } from "@roost/shared";
import { log, diag } from "@roost/shared";
import { isRpcRecord, type RpcFrame } from "./rpc-frame.ts";
import {
	newProjectionState,
	projectRpcFrame,
	type ProjectionOp,
	type ProjectionState,
} from "./entry-projection.ts";
import { projectSessionMessage } from "./history-projection.ts";
import { AgentEntryRing } from "./entry-ring.ts";
import { OmpRpcError, type OmpRpcHandle } from "./rpc-process.ts";


const STATE_TIMEOUT_MS = 10_000;
const AGENT_CONTROL_TIMEOUT_MS = 300_000;
const SEED_PAGE_LIMIT = 128;
// Ceiling used only when coord has no durable rows for this resumed session
// (pre-migration or transcript aged out) and omp history must seed it.
const SEED_MAX_MESSAGES = 500;
// current_tool.input_summary is a sidebar chip, not a payload.
const TOOL_SUMMARY_CHARS = 200;

export interface AgentControllerDeps {
	sessionId: SessionId;
	rpc: OmpRpcHandle;
	sendEntries: (frame: PbAgentEntriesFrame) => void;
	sendUiFrame: (frame: PbAgentUiFrame) => void;
	emitEvent: (event: SessionEvent) => void;
	nextSeq?: number;
}

export class AgentController {
	readonly sessionId: SessionId;
	readonly rpc: OmpRpcHandle;
	readonly #ring: AgentEntryRing;
	readonly #emitEvent: (event: SessionEvent) => void;
	readonly #sendUiFrame: (frame: PbAgentUiFrame) => void;
	// Seq counter plus the open-block / tool / prompt indices. The projector
	// advances them; this class only reads them.
	#proj: ProjectionState = newProjectionState();
	#costUsd = 0;
	#tokens = { in: 0, out: 0, cached: 0 };
	#todoSeq: number | null = null;
	#hasDurableHistory = false;
	#closed = false;
	#uiSnapshotId = "";
	#uiReady = false;

	constructor(deps: AgentControllerDeps) {
		this.sessionId = deps.sessionId;
		this.rpc = deps.rpc;
		this.#ring = new AgentEntryRing(deps.sessionId, deps.sendEntries);
		this.#emitEvent = deps.emitEvent;
		this.#sendUiFrame = deps.sendUiFrame;
		if (deps.nextSeq !== undefined && deps.nextSeq > 1) {
			this.#proj.nextSeq = deps.nextSeq;
			this.#hasDurableHistory = true;
		}
		this.rpc.on((frame) => this.#onFrame(frame));
		void this.rpc.exited.then((code) => this.#onExit(code));
	}

	/** Post-construction async work, deliberately off the spawn-reply path: wait
	 *  out the omp handshake, seed history when resuming, then publish a first
	 *  state patch so the sidebar has model + status.
	 *
	 *  A handshake that never lands (bad --resume path, auth failure, a child
	 *  that dies at boot) becomes a transcript notice plus a stale patch rather
	 *  than a failed spawn — the session row exists either way, so the user needs
	 *  to be able to SEE the reason and kill it. */
	async start(opts: { resumed: boolean }): Promise<void> {
		try {
			await this.rpc.ready;
		} catch (err) {
			if (this.#closed) return;
			this.#closed = true;
			this.#appendNotice(
				"error",
				`omp failed to start — ${err instanceof Error ? err.message : String(err)}`,
			);
			this.#ring.flushNow();
			this.#emitEvent({
				kind: "agent",
				session_id: this.sessionId,
				patch: { status: "done", stale: true },
				ts: Date.now(),
			});
			return;
		}
		if (this.#closed) return;
		this.#uiReady = true;
		this.subscribeUi();
		this.rpc.send({ type: "set_subagent_subscription", level: "progress" });
		if (opts.resumed) {
			if (this.#hasDurableHistory) this.#appendNotice("info", "resumed");
			else await this.#seedHistory();
		}
		await this.#refreshState();
	}

	// ─── inbound: omp → transcript ──────────────────────────────────────

	#onFrame(frame: RpcFrame): void {
		if (this.#closed) return;
		if (frame.type === "ui_frame") {
			this.#forwardUiFrame(frame);
			return;
		}
		const promptChanged = this.#applyOps(projectRpcFrame(frame, this.#proj));
		if (frame.type === "message_end") this.#accumulateUsage(frame);
		// get_state is the ONLY source of model / thinkingLevel / sessionFile, and
		// the only way to observe isStreaming. Poll it exactly where the answer can
		// have changed: both ends of a turn and every dialog transition. Without
		// agent_start the session would read `idle` for the whole streaming turn
		// and only flip once it was already over.
		if (
			promptChanged ||
			frame.type === "agent_start" ||
			frame.type === "agent_end" ||
			frame.type === "todo_reminder" ||
			frame.type === "todo_auto_clear"
		) {
			void this.#refreshState();
		}
	}

	/** Forward one canonical browser HostFrame. OMP emits the initial snapshot
	 *  synchronously as welcome + snapshot-chunk train; stdout order is the
	 *  reconciliation boundary, so preserve it without projection or batching. */
	#forwardUiFrame(frame: RpcFrame): void {
		const hostFrame = frame.frame;
		if (!isRpcRecord(hostFrame)) {
			log.warn("agent-controller", "invalid_ui_frame", { sid: this.sessionId });
			return;
		}
		const frameJson = JSON.stringify(hostFrame);
		if (frameJson === undefined) return;
		if (hostFrame.t === "welcome") this.#uiSnapshotId = randomUUID();
		const snapshotId = this.#uiSnapshotId;
		if (hostFrame.t === "snapshot-chunk" && hostFrame.final === true) {
			this.#uiSnapshotId = "";
		}
		this.#sendUiFrame(create(AgentUiFrameSchema, {
			sessionId: this.sessionId,
			frameJson,
			snapshotId,
			coordRevision: 0n,
		}));
	}

	/** Returns whether a prompt entry appeared or changed — that is what flips
	 *  the session between `needs-input` and the rest of the vocabulary. */
	#applyOps(ops: ProjectionOp[]): boolean {
		let promptChanged = false;
		for (const op of ops) {
			if (op.op === "append") {
				this.#ring.append(op.entry);
				if (op.entry.kind === "prompt") promptChanged = true;
				continue;
			}
			if (this.#ring.patch(op.seq, op.patch)?.kind === "prompt") promptChanged = true;
		}
		return promptChanged;
	}

	#accumulateUsage(frame: RpcFrame): void {
		const message = frame.message;
		if (!isRpcRecord(message)) return;
		const usage = message.usage;
		if (!isRpcRecord(usage)) return;
		if (typeof usage.input === "number") this.#tokens.in += usage.input;
		if (typeof usage.output === "number") this.#tokens.out += usage.output;
		if (typeof usage.cacheRead === "number") this.#tokens.cached += usage.cacheRead;
		// Per-message cost is the billed total for that one request, so summing
		// them across the turn is the session spend.
		const cost = usage.cost;
		if (isRpcRecord(cost) && typeof cost.total === "number") this.#costUsd += cost.total;
	}

	async #refreshState(): Promise<void> {
		if (this.#closed) return;
		let data: unknown;
		try {
			data = await this.rpc.request<unknown>({ type: "get_state" }, STATE_TIMEOUT_MS);
		} catch (err) {
			log.warn("agent-controller", "get_state_failed", {
				sid: this.sessionId,
				error: err instanceof Error ? err.message : String(err),
			});
			return;
		}
		if (!isRpcRecord(data)) return;
		if (Array.isArray(data.todoPhases)) {
			const encoded = JSON.stringify(data.todoPhases);
			const phasesJson =
				encoded.length <= AGENT_ENTRY_CAPS.text
					? encoded
					: JSON.stringify([{
							id: "roost-truncated",
							name: "Todo list omitted",
							tasks: [{
								id: "roost-truncated",
								content: `Todo state exceeded ${AGENT_ENTRY_CAPS.text} characters`,
								status: "abandoned",
							}],
						}]);
			if (this.#todoSeq === null && data.todoPhases.length > 0) {
				this.#todoSeq = this.#proj.nextSeq++;
				this.#ring.append({
					kind: "todo",
					seq: this.#todoSeq,
					ts: Date.now(),
					phases_json: phasesJson,
				});
			} else if (this.#todoSeq !== null) {
				this.#ring.patch(this.#todoSeq, { phases_json: phasesJson });
			}
		}
		// The four AgentStatus values every existing SPA surface already reads
		// (sidebar glyph, attention list, push). A live dialog outranks streaming:
		// either way the turn is blocked on the human.
		const status: AgentStatus =
			this.#proj.promptById.size > 0
				? "needs-input"
				: data.isStreaming === true
					? "running"
					: "idle";
		const patch: Partial<AgentState> = {
			status,
			tokens: { ...this.#tokens },
			cost_usd: this.#costUsd,
			current_tool: this.#currentTool(),
		};
		const model = isRpcRecord(data.model) ? data.model.id : undefined;
		if (typeof model === "string") patch.model = model;
		if (typeof data.thinkingLevel === "string") patch.mode = data.thinkingLevel;
		// The absolute .jsonl path is what lets a worker-restart respawn resume
		// the SAME conversation instead of starting empty.
		if (typeof data.sessionFile === "string") patch.session_file = data.sessionFile;
		this.#emitEvent({ kind: "agent", session_id: this.sessionId, patch, ts: Date.now() });
	}

	/** Newest still-running tool, for the sidebar's "what is it doing" chip. */
	#currentTool(): AgentState["current_tool"] {
		let best: AgentState["current_tool"] = null;
		let bestSeq = 0;
		for (const seq of this.#proj.toolSeqByCallId.values()) {
			const entry = this.#ring.get(seq);
			if (entry?.kind !== "tool" || seq <= bestSeq) continue;
			bestSeq = seq;
			best = {
				name: entry.name,
				input_summary: clampText(entry.intent || entry.args_json, TOOL_SUMMARY_CHARS),
			};
		}
		return best;
	}

	// ─── commands from the SPA ──────────────────────────────────────────

	/** Request a fresh canonical browser snapshot. Called after initial ready and
	 *  again when CoordLink reconnects while this OMP child survives. */
	subscribeUi(): void {
		if (this.#closed || !this.#uiReady) return;
		this.rpc.send({ type: "subscribe_ui" });
	}

	userMessage(text: string): void {
		if (this.#closed) return;
		this.#ring.append({
			kind: "user",
			seq: this.#proj.nextSeq++,
			ts: Date.now(),
			text: clampText(text, AGENT_ENTRY_CAPS.text),
			done: true,
		});
		// streamingBehavior is REQUIRED while a turn is in flight or the prompt is
		// rejected outright; followUp is the chat-composer semantic (queue behind
		// the running turn rather than interrupt it).
		this.rpc.send({ type: "prompt", message: text, streamingBehavior: "followUp" });
		// Optimistic: the composer's Send must become Stop now, not one omp
		// round-trip later. agent_start re-asserts this from authoritative state.
		if (this.#proj.promptById.size === 0)
			this.#emitEvent({
				kind: "agent",
				session_id: this.sessionId,
				patch: { status: "running" },
				ts: Date.now(),
			});
	}

	/** Forward one validated SessionSurface command through OMP's correlated
	 * request path so failures and response data reach the coordinator. */
	async uiCommand(command: AgentUiRpcCommand): Promise<unknown> {
		if (this.#closed) throw new Error("agent session is closed");
		const data = await this.rpc.request<unknown>(
			{ ...command },
			command.type === "subagent_command" ? AGENT_CONTROL_TIMEOUT_MS : undefined,
		);
		if (command.type === "browser_ui_response") {
			const extensionRequestId =
				isRpcRecord(data) && typeof data.extensionRequestId === "string"
					? data.extensionRequestId
					: undefined;
			const ref = extensionRequestId === undefined
				? undefined
				: this.#proj.promptById.get(extensionRequestId);
			if (extensionRequestId === undefined) {
				log.warn("agent-controller", "browser_ui_response_missing_extension_id", {
					sid: this.sessionId,
					req_id: command.reqId,
				});
			} else if (ref) {
				this.#proj.promptById.delete(extensionRequestId);
				this.#ring.patch(ref.seq, {
					state: command.cancelled === true || command.timedOut === true ? "cancelled" : "answered",
					answer: command.cancelled === true || command.timedOut === true ? "" : command.value ?? "",
				});
				void this.#refreshState();
			}
		}
		return data;
	}

	async abort(): Promise<void> {
		if (this.#closed) return;
		await this.rpc.request<unknown>({ type: "abort" });
		await this.#refreshState();
	}

	/** Answer a dialog. Returns false for an unknown or already-answered
	 *  prompt_id — a user can double-tap a button, and that must never throw. */
	respond(promptId: string, value: string, cancelled: boolean): boolean {
		if (this.#closed) return false;
		const ref = this.#proj.promptById.get(promptId);
		if (!ref) return false;
		this.#proj.promptById.delete(promptId);
		// `confirm` is the one method answered with a boolean rather than the
		// chosen label; sending `value` there would be read as cancelled.
		const reply: RpcFrame = cancelled
			? { type: "extension_ui_response", id: promptId, cancelled: true }
			: ref.method === "confirm"
				? { type: "extension_ui_response", id: promptId, confirmed: value === "Yes" }
				: { type: "extension_ui_response", id: promptId, value };
		this.rpc.send(reply);
		this.#ring.patch(ref.seq, {
			state: cancelled ? "cancelled" : "answered",
			answer: cancelled ? "" : value,
		});
		void this.#refreshState();
		return true;
	}


	// ─── lifecycle ──────────────────────────────────────────────────────

	/** Rebuild the transcript from omp's own .jsonl after `--resume`. */
	async #seedHistory(): Promise<void> {
		let cursor: string | undefined;
		let seeded = 0;
		while (seeded < SEED_MAX_MESSAGES) {
			let data: unknown;
			try {
				data = await this.rpc.request<unknown>(
					cursor === undefined
						? { type: "get_messages_page", limit: SEED_PAGE_LIMIT }
						: { type: "get_messages_page", limit: SEED_PAGE_LIMIT, cursor },
				);
			} catch (err) {
				// session_busy / stale_cursor are races, not failures: the child was
				// already mid-turn, or history moved under the cursor. Say so in the
				// transcript instead of pretending the session is empty.
				const code = err instanceof OmpRpcError ? err.code : undefined;
				this.#appendNotice(
					"info",
					code === "session_busy" || code === "stale_cursor"
						? "history unavailable — session was busy"
						: `history unavailable — ${err instanceof Error ? err.message : String(err)}`,
				);
				return;
			}
			if (!isRpcRecord(data) || !Array.isArray(data.messages)) return;
			for (const message of data.messages) {
				this.#applyOps(projectSessionMessage(message, this.#proj));
				seeded++;
			}
			if (typeof data.nextCursor !== "string" || data.messages.length === 0) break;
			cursor = data.nextCursor;
		}
		this.#appendNotice("info", "resumed");
		diag("agent.resume_seeded", { sid: this.sessionId, messages: seeded });
	}

	#appendNotice(level: "info" | "error", text: string): void {
		this.#ring.append({
			kind: "notice",
			seq: this.#proj.nextSeq++,
			ts: Date.now(),
			level,
			text: clampText(text, AGENT_ENTRY_CAPS.text),
			details_json: "",
		});
	}

	/** Child died on its own. The Roost session row stays OPEN so coord's
	 *  respawn path can revive it with `--resume` and its history intact. */
	#onExit(code: number): void {
		if (this.#closed) return;
		this.#closed = true;
		const tail = this.rpc.stderrTail().trim();
		this.#appendNotice("error", `omp exited (code ${code})${tail ? `\n${tail}` : ""}`);
		this.#ring.flushNow();
		this.#emitEvent({
			kind: "agent",
			session_id: this.sessionId,
			patch: { status: "done", stale: true },
			ts: Date.now(),
		});
		log.warn("agent-controller", "child_exited", { sid: this.sessionId, code, pid: this.rpc.pid });
	}

	/** Deliberate teardown. Cancels every outstanding dialog FIRST: the child
	 *  blocks forever on an unanswered one, so a kill that races the reply would
	 *  otherwise leave a wedged process behind. */
	dispose(): void {
		if (this.#closed) return;
		this.#closed = true;
		for (const [promptId, ref] of this.#proj.promptById) {
			this.rpc.send({ type: "extension_ui_response", id: promptId, cancelled: true });
			this.#ring.patch(ref.seq, { state: "cancelled" });
		}
		this.#proj.promptById.clear();
		this.#ring.flushNow();
		this.rpc.kill();
	}
}
