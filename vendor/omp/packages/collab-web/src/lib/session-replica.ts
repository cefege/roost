/**
 * Transport-neutral browser replica for an OMP session.
 *
 * {@link SessionReplica} folds canonical `HostFrame`s in arrival order and
 * exposes an immutable, `useSyncExternalStore`-compatible snapshot. It owns no
 * link, socket, request timers, or reconnect policy; transports drive those
 * concerns through {@link SessionReplica.setPhase} and consume effects returned
 * by {@link SessionReplica.applyFrame}.
 */
import {
	type AgentSnapshot,
	type AssistantMessage,
	applyAssistantDelta,
	type CollabUiRequest,
	type HostFrame,
	type SessionEntry,
	type SessionHeader,
	type SessionState,
	type SubagentLifecyclePayload,
	type SubagentProgressPayload,
} from "@oh-my-pi/pi-wire";

export type ConnectionPhase = "connecting" | "waiting" | "live" | "reconnecting" | "ended";

export interface ActiveTool {
	toolCallId: string;
	toolName: string;
	args: unknown;
	intent?: string;
	partialResult?: unknown;
	startedAt: number;
}

export interface Notice {
	id: number;
	level: "info" | "warning" | "error";
	message: string;
	at: number;
}

export type SessionTodoStatus = "pending" | "in_progress" | "completed" | "abandoned";

export interface SessionTodo {
	content: string;
	status: SessionTodoStatus;
	details?: string;
	notes?: readonly string[];
}

export type SessionGoalStatus = "active" | "paused" | "budget-limited" | "complete" | "dropped";

/** Presentation-safe projection of the goal and its optional mode state. */
export interface SessionGoal {
	id: string;
	objective: string;
	status: SessionGoalStatus;
	tokenBudget?: number;
	tokensUsed: number;
	timeUsedSeconds: number;
	createdAt: number;
	updatedAt: number;
	enabled?: boolean;
	mode?: "active" | "exiting";
	reason?: "completed";
}

export interface SessionRuleSummary {
	name: string;
	path?: string;
	description?: string;
}

interface ActivityBase {
	id: number;
	at: number;
}

/** Bounded, typed rows for lifecycle events that do not become transcript entries. */
export type SessionActivity =
	| (ActivityBase & {
			kind: "retry";
			phase: "started";
			attempt: number;
			maxAttempts: number;
			delayMs: number;
			message: string;
	  })
	| (ActivityBase & {
			kind: "retry";
			phase: "succeeded" | "failed";
			attempt: number;
			message?: string;
	  })
	| (ActivityBase & {
			kind: "fallback";
			phase: "applied";
			from: string;
			to: string;
			role: string;
	  })
	| (ActivityBase & {
			kind: "fallback";
			phase: "succeeded";
			model: string;
			role: string;
	  })
	| (ActivityBase & {
			kind: "ttsr";
			rules: readonly SessionRuleSummary[];
	  })
	| (ActivityBase & {
			kind: "todo";
			phase: "reminder";
			attempt: number;
			maxAttempts: number;
			todoCount: number;
	  })
	| (ActivityBase & {
			kind: "todo";
			phase: "cleared";
	  })
	| (ActivityBase & {
			kind: "irc";
			customType: string;
			text: string;
			from?: string;
			to?: string;
			messageAt?: number;
	  })
	| (ActivityBase & {
			kind: "goal";
			goal: SessionGoal | null;
	  });

type SessionActivityPayload = SessionActivity extends infer Activity
	? Activity extends ActivityBase
		? Omit<Activity, keyof ActivityBase>
		: never
	: never;

export interface GuestSnapshot {
	phase: ConnectionPhase;
	endedReason: string | null;
	header: SessionHeader | null;
	entries: readonly SessionEntry[];
	state: SessionState | null;
	agents: readonly AgentSnapshot[];
	/** Keyed by `payload.progress.id`. */
	progress: ReadonlyMap<string, SubagentProgressPayload>;
	/** Keyed by `payload.id`. */
	lifecycle: ReadonlyMap<string, SubagentLifecyclePayload>;
	/** Streaming assistant ghost; held until the matching entry lands. */
	stream: AssistantMessage | null;
	streamDone: boolean;
	activeTools: ReadonlyMap<string, ActiveTool>;
	/** agent_start..agent_end, reconciled by state.isStreaming. */
	working: boolean;
	/** True when this guest joined through a read-only (view) link. */
	readOnly: boolean;
	/** Pending host-side UI request (`ask` select/editor) this guest can answer. */
	uiRequest: CollabUiRequest | null;
	/** Latest todo state supplied by todo reminder/clear events. */
	todos: readonly SessionTodo[];
	/** Latest goal state supplied by goal updates. */
	goal: SessionGoal | null;
	/** Capped lifecycle activity, oldest first. */
	activity: readonly SessionActivity[];
	/** Capped at 50, newest last. */
	notices: readonly Notice[];
}

/**
 * One fetch-transcript round trip.
 * - `rows`: decoded JSONL from `fromByte`; `newSize` is the next offset base.
 * - `error`: terminal read failure reported by the host (unchanged cursor);
 *   callers must surface it and stop polling instead of hot retrying.
 * Transient transport failures resolve `null` in the protocol client and are
 * not represented by a host frame.
 */
export type TranscriptResult = { kind: "rows"; text: string; newSize: number } | { kind: "error"; message: string };

/** Non-snapshot output produced while folding a targeted host reply. */
export type SessionReplicaEffect = {
	kind: "transcript";
	reqId: number;
	result: TranscriptResult;
};

const MAX_NOTICES = 50;
const MAX_ACTIVITY = 50;
const TODO_STATUSES: ReadonlySet<string> = new Set(["pending", "in_progress", "completed", "abandoned"]);
const GOAL_STATUSES: ReadonlySet<string> = new Set(["active", "paused", "budget-limited", "complete", "dropped"]);

function record(value: unknown): Record<string, unknown> | null {
	return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

function optionalString(value: unknown): string | undefined {
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

function optionalNumber(value: unknown): number | undefined {
	return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function contentText(value: unknown): string {
	if (typeof value === "string") return value;
	if (!Array.isArray(value)) return "";
	const parts: string[] = [];
	for (const item of value) {
		const block = record(item);
		if (!block) continue;
		if (typeof block.text === "string") parts.push(block.text);
		else if (typeof block.thinking === "string") parts.push(block.thinking);
	}
	return parts.join("\n");
}

function normalizeTodos(value: unknown): readonly SessionTodo[] {
	if (!Array.isArray(value)) return [];
	const todos: SessionTodo[] = [];
	for (const candidate of value) {
		const todo = record(candidate);
		if (!todo || typeof todo.content !== "string" || !TODO_STATUSES.has(String(todo.status))) continue;
		const normalized: SessionTodo = {
			content: todo.content,
			status: todo.status as SessionTodoStatus,
		};
		if (typeof todo.details === "string") normalized.details = todo.details;
		if (Array.isArray(todo.notes)) normalized.notes = todo.notes.filter((note): note is string => typeof note === "string");
		todos.push(normalized);
	}
	return todos;
}

function normalizeGoal(value: unknown, modeValue: unknown): SessionGoal | null {
	const goal = record(value);
	if (
		!goal ||
		typeof goal.id !== "string" ||
		typeof goal.objective !== "string" ||
		!GOAL_STATUSES.has(String(goal.status)) ||
		typeof goal.tokensUsed !== "number" ||
		typeof goal.timeUsedSeconds !== "number" ||
		typeof goal.createdAt !== "number" ||
		typeof goal.updatedAt !== "number"
	) {
		return null;
	}
	const normalized: SessionGoal = {
		id: goal.id,
		objective: goal.objective,
		status: goal.status as SessionGoalStatus,
		tokensUsed: goal.tokensUsed,
		timeUsedSeconds: goal.timeUsedSeconds,
		createdAt: goal.createdAt,
		updatedAt: goal.updatedAt,
	};
	const tokenBudget = optionalNumber(goal.tokenBudget);
	if (tokenBudget !== undefined) normalized.tokenBudget = tokenBudget;
	const mode = record(modeValue);
	if (typeof mode?.enabled === "boolean") normalized.enabled = mode.enabled;
	if (mode?.mode === "active" || mode?.mode === "exiting") normalized.mode = mode.mode;
	if (mode?.reason === "completed") normalized.reason = mode.reason;
	return normalized;
}

function normalizeRules(value: unknown): readonly SessionRuleSummary[] {
	if (!Array.isArray(value)) return [];
	const rules: SessionRuleSummary[] = [];
	for (const candidate of value) {
		const rule = record(candidate);
		if (!rule) continue;
		const name = optionalString(rule.name);
		const path = optionalString(rule.path);
		if (!name && !path) continue;
		const normalized: SessionRuleSummary = { name: name ?? path ?? "rule" };
		if (path) normalized.path = path;
		const description = optionalString(rule.description);
		if (description) normalized.description = description;
		rules.push(normalized);
	}
	return rules;
}

function normalizeIrc(value: unknown): Omit<Extract<SessionActivityPayload, { kind: "irc" }>, "kind"> {
	const message = record(value);
	const details = record(message?.details);
	const text =
		optionalString(details?.message) ?? optionalString(details?.body) ?? contentText(message?.content) ?? "";
	const normalized: Omit<Extract<SessionActivityPayload, { kind: "irc" }>, "kind"> = {
		customType: optionalString(message?.customType) ?? "irc",
		text,
	};
	const from = optionalString(details?.from);
	if (from) normalized.from = from;
	const to = optionalString(details?.to);
	if (to) normalized.to = to;
	const messageAt = optionalNumber(message?.timestamp);
	if (messageAt !== undefined) normalized.messageAt = messageAt;
	return normalized;
}

export class SessionReplica {
	readonly #listeners = new Set<() => void>();
	#noticeSeq = 0;
	#activitySeq = 0;
	#welcomed = false;

	#phase: ConnectionPhase = "connecting";
	#endedReason: string | null = null;
	#header: SessionHeader | null = null;
	#entries: readonly SessionEntry[] = [];
	#state: SessionState | null = null;
	#agents: readonly AgentSnapshot[] = [];
	#progress: ReadonlyMap<string, SubagentProgressPayload> = new Map();
	#lifecycle: ReadonlyMap<string, SubagentLifecyclePayload> = new Map();
	#stream: AssistantMessage | null = null;
	#streamDone = false;
	#activeTools: ReadonlyMap<string, ActiveTool> = new Map();
	#working = false;
	#readOnly = false;
	#uiRequest: CollabUiRequest | null = null;
	#uiRequestQueue: CollabUiRequest[] = [];
	#todos: readonly SessionTodo[] = [];
	#goal: SessionGoal | null = null;
	#activity: readonly SessionActivity[] = [];
	#notices: readonly Notice[] = [];
	#snapshot: GuestSnapshot;

	constructor() {
		this.#snapshot = this.#buildSnapshot();
	}

	/** Bound callback; safe to pass directly to `useSyncExternalStore`. */
	readonly subscribe = (listener: () => void): (() => void) => {
		this.#listeners.add(listener);
		return () => {
			this.#listeners.delete(listener);
		};
	};

	/** Cached stable reference; safe to pass directly to `useSyncExternalStore`. */
	readonly getSnapshot = (): GuestSnapshot => this.#snapshot;

	/**
	 * Set a transport lifecycle phase without coupling the replica to a socket.
	 * Non-ended phases clear any prior terminal reason. Ending also dismisses
	 * host UI requests, matching a terminal `bye` or pre-welcome `error` frame.
	 */
	setPhase(phase: ConnectionPhase, endedReason: string | null = null): void {
		if (phase === "ended") {
			if (this.#phase === "ended" && this.#endedReason === endedReason) return;
			this.#phase = phase;
			this.#endedReason = endedReason;
			this.#clearUiRequests();
		} else {
			if (this.#phase === phase && this.#endedReason === null) return;
			this.#phase = phase;
			this.#endedReason = null;
		}
		this.#commit();
	}

	/** Advance the host UI-request queue after the active request is answered locally. */
	resolveUiRequest(reqId: number): void {
		if (this.#uiRequest?.reqId !== reqId) return;
		this.#showNextUiRequest();
		this.#commit();
	}

	/**
	 * Fold one canonical host frame. Snapshot changes notify subscribers once.
	 * Targeted transcript replies are returned as effects because request
	 * correlation and timeout policy belong to the transport, not the replica.
	 */
	applyFrame(frame: HostFrame): SessionReplicaEffect | null {
		try {
			return this.#reduceFrame(frame);
		} catch (err) {
			console.warn("collab: failed to apply frame", frame.t, err);
			if (frame.t === "welcome" && !this.#welcomed) {
				this.setPhase("ended", `failed to apply session snapshot: ${err instanceof Error ? err.message : String(err)}`);
				return null;
			}
			this.#pushNotice("error", `failed to apply ${frame.t} frame`);
			this.#commit();
			return null;
		}
	}

	#reduceFrame(frame: HostFrame): SessionReplicaEffect | null {
		switch (frame.t) {
			case "welcome":
				// Reset accumulator: a fresh welcome arriving mid-load (reconnect)
				// supersedes any partially-streamed snapshot from the prior session.
				this.#header = frame.header;
				this.#entries = [];
				this.#state = frame.state;
				this.#agents = [...frame.agents];
				this.#stream = null;
				this.#streamDone = false;
				this.#activeTools = new Map();
				this.#progress = new Map();
				this.#lifecycle = new Map();
				this.#working = frame.state.isStreaming;
				this.#readOnly = frame.readOnly === true;
				this.#clearUiRequests();
				this.#welcomed = true;
				if (frame.entryCount === 0) this.#phase = "live";
				this.#endedReason = null;
				break;
			case "snapshot-chunk":
				// Stream transcript fragments into the live snapshot. The host
				// always closes the train with `final: true`; that flip is what
				// moves the replica from its loading phase to "live".
				this.#entries = [...this.#entries, ...frame.entries];
				if (frame.final) this.#phase = "live";
				break;
			case "entry":
				this.#entries = [...this.#entries, frame.entry];
				if (this.#streamDone && frame.entry.type === "message" && frame.entry.message.role === "assistant") {
					this.#stream = null;
					this.#streamDone = false;
				}
				break;
			case "event":
				this.#applyEvent(frame.event);
				break;
			case "state":
				this.#state = frame.state;
				if (!frame.state.isStreaming) {
					this.#working = false;
					if (this.#streamDone) {
						this.#stream = null;
						this.#streamDone = false;
					}
				}
				break;
			case "agents":
				this.#agents = [...frame.agents];
				break;
			case "bus":
				if (frame.channel === "task:subagent:progress") {
					const payload = frame.data as SubagentProgressPayload;
					this.#progress = new Map(this.#progress).set(payload.progress.id, payload);
				} else if (frame.channel === "task:subagent:lifecycle") {
					const payload = frame.data as SubagentLifecyclePayload;
					this.#lifecycle = new Map(this.#lifecycle).set(payload.id, payload);
				}
				break;
			case "ui-request":
				if (this.#uiRequest) this.#uiRequestQueue = [...this.#uiRequestQueue, frame.request];
				else this.#uiRequest = frame.request;
				break;
			case "ui-request-end":
				if (this.#uiRequest?.reqId === frame.reqId) this.#showNextUiRequest();
				else this.#uiRequestQueue = this.#uiRequestQueue.filter(request => request.reqId !== frame.reqId);
				break;
			case "transcript": {
				const effect: SessionReplicaEffect = {
					kind: "transcript",
					reqId: frame.reqId,
					result:
						frame.error !== undefined
							? { kind: "error", message: frame.error }
							: { kind: "rows", text: frame.text, newSize: frame.newSize },
				};
				// Preserve GuestClient's one fresh snapshot/notification per
				// successfully applied relay frame, even for targeted replies.
				this.#commit();
				return effect;
			}
			case "bye":
				if (this.#phase === "ended") return null;
				this.#phase = "ended";
				this.#endedReason = frame.reason;
				this.#clearUiRequests();
				break;
			case "error":
				if (!this.#welcomed) {
					// Pre-welcome errors are the host's targeted reply to hello (for
					// example protocol mismatch): no welcome will follow.
					if (this.#phase === "ended") return null;
					this.#phase = "ended";
					this.#endedReason = frame.message;
					this.#clearUiRequests();
				} else {
					this.#pushNotice("error", frame.message);
				}
				break;
			default:
				// Unknown frame type from a newer host — ignore.
				break;
		}
		this.#commit();
		return null;
	}

	#applyEvent(event: Extract<HostFrame, { t: "event" }>["event"]): void {
		switch (event.type) {
			case "message_start":
				if (event.message.role === "assistant") {
					this.#stream = event.message;
					this.#streamDone = false;
				}
				break;
			case "message_update":
				// No base means the subscriber joined between the host's mid-turn
				// replay and this delta; the message_end snapshot fills the row.
				if (this.#stream !== null) {
					this.#stream = applyAssistantDelta(this.#stream, event.delta);
					this.#streamDone = false;
				}
				break;
			case "message_end":
				if (event.message.role === "assistant") {
					this.#stream = event.message;
					this.#streamDone = true;
				}
				break;
			case "tool_execution_start": {
				const tool: ActiveTool = {
					toolCallId: event.toolCallId,
					toolName: event.toolName,
					args: event.args,
					intent: event.intent,
					startedAt: Date.now(),
				};
				this.#activeTools = new Map(this.#activeTools).set(event.toolCallId, tool);
				break;
			}
			case "tool_execution_update": {
				const existing = this.#activeTools.get(event.toolCallId);
				const tool: ActiveTool = existing
					? { ...existing, partialResult: event.partialResult }
					: {
							toolCallId: event.toolCallId,
							toolName: event.toolName,
							args: event.args,
							partialResult: event.partialResult,
							startedAt: Date.now(),
						};
				this.#activeTools = new Map(this.#activeTools).set(event.toolCallId, tool);
				break;
			}
			case "tool_execution_end": {
				const next = new Map(this.#activeTools);
				next.delete(event.toolCallId);
				this.#activeTools = next;
				break;
			}
			case "agent_start":
				this.#working = true;
				break;
			case "agent_end":
				this.#working = false;
				break;
			case "notice":
				this.#pushNotice(event.level, event.message);
				break;
			case "auto_retry_start":
				this.#pushActivity({
					kind: "retry",
					phase: "started",
					attempt: event.attempt,
					maxAttempts: event.maxAttempts,
					delayMs: event.delayMs,
					message: event.errorMessage,
				});
				this.#pushNotice("info", `retry ${event.attempt}/${event.maxAttempts}: ${event.errorMessage}`);
				break;
			case "auto_retry_end":
				this.#pushActivity({
					kind: "retry",
					phase: event.success ? "succeeded" : "failed",
					attempt: event.attempt,
					...(event.finalError ? { message: event.finalError } : {}),
				});
				this.#pushNotice(
					event.success ? "info" : "error",
					event.success
						? `retry succeeded on attempt ${event.attempt}`
						: `retry failed on attempt ${event.attempt}${event.finalError ? `: ${event.finalError}` : ""}`,
				);
				break;
			case "retry_fallback_applied":
				this.#pushActivity({
					kind: "fallback",
					phase: "applied",
					from: event.from,
					to: event.to,
					role: event.role,
				});
				this.#pushNotice("warning", `fallback ${event.from} → ${event.to}`);
				break;
			case "retry_fallback_succeeded":
				this.#pushActivity({
					kind: "fallback",
					phase: "succeeded",
					model: event.model,
					role: event.role,
				});
				this.#pushNotice("info", `fallback succeeded on ${event.model}`);
				break;
			case "ttsr_triggered": {
				const rules = normalizeRules(event.rules);
				this.#pushActivity({ kind: "ttsr", rules });
				const names = rules.map(rule => rule.name).join(", ");
				this.#pushNotice("warning", names ? `tool-time reminder: ${names}` : "tool-time reminder triggered");
				break;
			}
			case "todo_reminder":
				this.#todos = normalizeTodos(event.todos);
				this.#pushActivity({
					kind: "todo",
					phase: "reminder",
					attempt: event.attempt,
					maxAttempts: event.maxAttempts,
					todoCount: this.#todos.length,
				});
				this.#pushNotice("warning", `todo reminder ${event.attempt}/${event.maxAttempts}`);
				break;
			case "todo_auto_clear":
				this.#todos = [];
				this.#pushActivity({ kind: "todo", phase: "cleared" });
				this.#pushNotice("info", "completed todos cleared");
				break;
			case "irc_message": {
				const message = normalizeIrc(event.message);
				this.#pushActivity({ kind: "irc", ...message });
				this.#pushNotice("info", `IRC${message.from ? ` ${message.from}` : ""}: ${message.text}`);
				break;
			}
			case "goal_updated": {
				if (event.goal === null) {
					this.#goal = null;
				} else {
					const goal = normalizeGoal(event.goal, event.state);
					if (goal) this.#goal = goal;
				}
				this.#pushActivity({ kind: "goal", goal: this.#goal });
				this.#pushNotice("info", this.#goal ? `goal ${this.#goal.status}: ${this.#goal.objective}` : "goal cleared");
				break;
			}
			case "auto_compaction_start":
				this.#pushNotice("info", `compacting context (${event.reason})`);
				break;
			case "auto_compaction_end":
				if (!event.skipped) {
					this.#pushNotice(
						"info",
						event.aborted
							? "compaction aborted"
							: event.errorMessage
								? `compaction failed: ${event.errorMessage}`
								: "context compacted",
					);
				}
				break;
			default:
				// turn_start/turn_end/thinking_level_changed/unknown — ignore
				break;
		}
	}

	#pushNotice(level: Notice["level"], message: string): void {
		const notice: Notice = { id: ++this.#noticeSeq, level, message, at: Date.now() };
		const next = [...this.#notices, notice];
		if (next.length > MAX_NOTICES) next.splice(0, next.length - MAX_NOTICES);
		this.#notices = next;
	}

	#pushActivity(activity: SessionActivityPayload): void {
		const row = { ...activity, id: ++this.#activitySeq, at: Date.now() } as SessionActivity;
		const next = [...this.#activity, row];
		if (next.length > MAX_ACTIVITY) next.splice(0, next.length - MAX_ACTIVITY);
		this.#activity = next;
	}

	#clearUiRequests(): void {
		this.#uiRequest = null;
		this.#uiRequestQueue = [];
	}

	#showNextUiRequest(): void {
		const [next, ...rest] = this.#uiRequestQueue;
		this.#uiRequest = next ?? null;
		this.#uiRequestQueue = rest;
	}

	#buildSnapshot(): GuestSnapshot {
		return {
			phase: this.#phase,
			endedReason: this.#endedReason,
			header: this.#header,
			entries: this.#entries,
			state: this.#state,
			agents: this.#agents,
			progress: this.#progress,
			lifecycle: this.#lifecycle,
			stream: this.#stream,
			streamDone: this.#streamDone,
			activeTools: this.#activeTools,
			working: this.#working,
			readOnly: this.#readOnly,
			uiRequest: this.#uiRequest,
			todos: this.#todos,
			goal: this.#goal,
			activity: this.#activity,
			notices: this.#notices,
		};
	}

	#commit(): void {
		this.#snapshot = this.#buildSnapshot();
		for (const listener of this.#listeners) listener();
	}
}
