// Pure omp-event → ChatMessage projection for the native RPC child.
//
// The child's stdout uses the same event shapes for messages, tools, plans and
// subagents. Keeping their projection in one switch makes each streaming
// update an upsert of the row it expands, rather than a second transcript row.
//
// PURE: no host, no SessionRecord, no timers, no I/O. The RPC controller owns
// emission and coalesces mid-stream updates so every token is not a ChatFrame.

import { TRUNC_CAP, type ChatMessage, type ContentBlock } from "@roost/shared/chat/wire";
import { mapAgentMessage, mapAgentMessageFull } from "./parse.ts";

/** Tail of a running tool's output kept on the wire. A `bash` tail -f would
 *  otherwise re-send its whole buffer on every update. */
const PARTIAL_CAP = 2000;

export type ToolPhase = "start" | "update" | "end";

export interface ProjectState {
	/** Minted-id prefix: `rpc-N` for the RPC child, `live-N` for the sidecar.
	 *  Distinct so a session both engines touched cannot collide on one id. */
	prefix: string;
	nextMsg: number;
	/** Chat message id the in-flight assistant message streams into; null
	 *  between turns. Doubles as the "a turn is mid-flight" signal the live
	 *  watcher needs to clean up after a bridge that died mid-message. */
	curMsgId: string | null;
	lastMsgId: string;
	/** toolCallId → chat message id, so start/end collapse into one message. */
	toolMsgIds: Map<string, string>;
	/** subagent id → the card being repainted for it.
	 *
	 *  One entry per subagent, kept for the life of the conversation. Two things
	 *  ride here, both learned the hard way against a live spawn:
	 *  - `id`, so a stream of progress ticks repaints ONE card instead of
	 *    stacking a row each;
	 *  - the last non-empty `title`/`detail`, because omp's lifecycle frames
	 *    carry no `progress` object at all. The final `completed` lifecycle
	 *    would otherwise blank a card that had just been showing the subagent's
	 *    tool and token counts. */
	subagentCards: Map<string, { id: string; title: string; detail: string }>;
	/** chat message id → per-block UNTRUNCATED text, for SessionsGetChatBlock.
	 *  Only populated for messages the cap actually bit. */
	fullText: Map<string, string[]>;
	/** Message ids this projector has actually PUT ON SCREEN. A turn whose
	 *  intermediate frames rendered but whose final frame renders as nothing
	 *  must retract the row (see ProjectResult["drop"]); without this record
	 *  there is no way to tell "never shown" from "shown and now stale". */
	emitted: Set<string>;
}

export function newProjectState(prefix = "rpc"): ProjectState {
	return {
		prefix, nextMsg: 1, curMsgId: null, lastMsgId: "",
		toolMsgIds: new Map(), subagentCards: new Map(), fullText: new Map(), emitted: new Set(),
	};
}

/** Mint the next synthetic message id. Exported: rpc-chat mints ids for rows
 *  that are not projections (approval cards, command output, exit notices) and
 *  they must share this counter or two rows collide on one id. */
export function nextId(st: ProjectState): string {
	return `${st.prefix}-${st.nextMsg++}`;
}

/** What one event projects to. null = nothing to emit. */
export type ProjectResult =
	| { kind: "message"; msg: ChatMessage; coalesce: boolean }
	| { kind: "tool"; msg: ChatMessage; callId: string; phase: ToolPhase }
	| { kind: "narrate"; msg: ChatMessage }
	| { kind: "streaming"; value: boolean }
	/** A row this projector already emitted has become unrenderable — remove it.
	 *  A turn can start with a renderable error and END silent: omp's
	 *  `message_start` for an aborted turn carries `errorMessage: "Request was
	 *  aborted"` (a red "Operation aborted" notice), while its `message_end`
	 *  carries `__omp.silent_abort__`, which the TUI paints as NOTHING. Without
	 *  this the pane keeps the intermediate error row forever and shows a red
	 *  line where the terminal shows silence. Measured against omp 17.1.3. */
	| { kind: "drop"; id: string }
	| null;

/** Untruncated text of one block, mirroring parse.ts::fullBlockText's switch.
 *  toolResult is absent on purpose: its payload rides whole in rawJson, and
 *  notice/fileMention/approval/toolEvent are never capped. */
function blockFullText(b: ContentBlock): string {
	switch (b.kind) {
		case "text":
		case "thinking":
		case "summary":
		case "custom":
			return b.text;
		case "toolCall":
			return b.argsJson;
		case "exec":
			return b.output;
		default:
			return "";
	}
}

/** Bound on `st.fullText`. Same 256 as session-store.ts's MAX_ENTRIES: a chat
 *  is long-lived but finite, and a capped block older than the last 256 that
 *  were capped degrades to the truncated text with no expand — the pane's
 *  pre-existing behavior, never an error. */
const MAX_FULL_TEXT = 256;

/** Map an omp AgentMessage → ChatMessage, stashing untruncated block text when
 *  the cap actually bit. The event stream carries no transcript entry id, so
 *  re-reading the transcript by id (the mirror engine's path) cannot work here
 *  — this store is the ONLY recovery path the rpc engine has, which is why it
 *  covers every capped block kind and not just `thinking`. */
export function mapAndRecord(st: ProjectState, raw: unknown, id: string, parentId: string): ChatMessage | null {
	const ts = new Date().toISOString();
	const msg = mapAgentMessage(raw, id, parentId, ts);
	if (!msg) return null;
	// Every capped kind blockFullText can actually serve — summary/custom/exec
	// and toolCall args, not thinking alone; rpcChatFullBlock could never serve
	// those before. toolResult is excluded on purpose: its `text` is ALWAYS ""
	// (the payload rides whole in rawJson — parse.ts:243), so its `truncated`
	// flag describes something that needs no recovery.
	const truncated = msg.blocks.some((b) => b.kind !== "toolResult" && "truncated" in b && b.truncated);
	if (!truncated) { st.fullText.delete(id); return msg; }
	const full = mapAgentMessageFull(raw, id, parentId, ts);
	if (full) recordFullText(st, id, full.blocks.map(blockFullText));
	return msg;
}

/** Stash untruncated per-block text under a message id, newest last, bounded.
 *  Shared by mapAndRecord and the compaction summary, which is minted here
 *  rather than mapped from an AgentMessage. */
function recordFullText(st: ProjectState, id: string, texts: string[]): void {
	// Re-insert last so a message that keeps growing stays the newest entry.
	st.fullText.delete(id);
	st.fullText.set(id, texts);
	while (st.fullText.size > MAX_FULL_TEXT) {
		const oldest = st.fullText.keys().next();
		if (oldest.done) break;
		st.fullText.delete(oldest.value);
	}
}

/** One narration row: a line the omp TUI paints for itself (retrying, model
 *  fallback, compacting…). A `notice` block, not prose — the pane renders it as
 *  its own dim/error row exactly as the terminal does. */
function narrate(st: ProjectState, text: string, level: "error" | "note"): ChatMessage {
	return {
		id: nextId(st), parentId: st.lastMsgId, ts: new Date().toISOString(),
		role: "developer", synthetic: false, blocks: [{ kind: "notice", text, level }],
	};
}

// CSI/OSC/SS3 escapes. omp writes slash-command output for a terminal, so
// `/context` arrives full of 24-bit colour runs that would render as literal
// garbage in a web bubble.
const ANSI_RE = /[\u001b\u009b](?:\][^\u0007\u001b]*(?:\u0007|\u001b\\)|[[(][0-?]*[ -/]*[@-~])/g;

/** Strip escapes AND resolve carriage-return overwrites. omp renders progress
 *  lines by rewriting one line with \r; kept verbatim inside a fence they
 *  stack up as duplicate rows, so only the final text of each line survives. */
export function stripAnsi(text: string): string {
	return text
		.replace(ANSI_RE, "")
		.replace(/\r\n/g, "\n")
		.split("\n")
		.map((line) => (line.includes("\r") ? line.slice(line.lastIndexOf("\r") + 1) : line))
		.join("\n");
}

/** Text of a tool's in-flight partial result. omp's AgentToolResult carries
 *  `content: [{type:"text", text}]`; some tools report progress as a bare
 *  string. Tail-capped: a running command's newest output is what matters,
 *  and this re-renders on every update. */
function partialResultText(raw: unknown): string {
	if (typeof raw === "string") return stripAnsi(raw).slice(-PARTIAL_CAP);
	if (raw === null || typeof raw !== "object") return "";
	const content = "content" in raw ? raw.content : undefined;
	if (!Array.isArray(content)) return "";
	let out = "";
	for (const b of content) {
		if (b !== null && typeof b === "object" && "type" in b && "text" in b) {
			if (b.type === "text" && typeof b.text === "string") out += b.text;
		}
	}
	return stripAnsi(out).slice(-PARTIAL_CAP);
}

/** Project one omp event. The caller decides what reaches the wire and when;
 *  everything id- and turn-shaped is decided HERE so both engines agree. */
export function projectEvent(st: ProjectState, ev: Record<string, unknown>): ProjectResult {
	switch (ev.type) {
		case "agent_start":
			return { kind: "streaming", value: true };

		case "agent_end":
			st.curMsgId = null;
			return { kind: "streaming", value: false };

		case "message_start": {
			const id = nextId(st);
			st.curMsgId = id;
			// A message_start usually carries empty content — nothing to render
			// yet, but the id is reserved so updates land on one row.
			const msg = mapAndRecord(st, ev.message, id, st.lastMsgId);
			if (!msg) return null;
			st.emitted.add(id);
			return { kind: "message", msg, coalesce: false };
		}

		case "message_update": {
			// ev.message is the FULL message so far, not a delta — remap it
			// wholesale and let the caller decide when it hits the wire.
			const id = st.curMsgId ?? (st.curMsgId = nextId(st));
			const msg = mapAndRecord(st, ev.message, id, st.lastMsgId);
			if (!msg) return null;
			st.emitted.add(id);
			return { kind: "message", msg, coalesce: true };
		}

		case "message_end": {
			const id = st.curMsgId ?? nextId(st);
			const emitted = st.emitted.has(id);
			st.curMsgId = null;
			st.emitted.delete(id);
			const msg = mapAndRecord(st, ev.message, id, st.lastMsgId);
			// Renders as nothing now. If an earlier frame of this same turn DID
			// render, that row is stale and must go, not linger.
			if (!msg) return emitted ? { kind: "drop", id } : null;
			st.lastMsgId = id;
			st.emitted.add(id);
			return { kind: "message", msg, coalesce: false };
		}

		case "tool_execution_start":
		case "tool_execution_update":
		case "tool_execution_end": {
			const callId = typeof ev.toolCallId === "string" ? ev.toolCallId : "";
			if (!callId) return null;
			let id = st.toolMsgIds.get(callId);
			if (id === undefined) { id = nextId(st); st.toolMsgIds.set(callId, id); }
			const phase: ToolPhase = ev.type === "tool_execution_start" ? "start"
				: ev.type === "tool_execution_update" ? "update" : "end";
			// The final result arrives separately as a toolResult message, so the
			// event's own output only has to carry the LIVE view while running —
			// which is what the TUI shows and the chat used to drop entirely.
			const output = phase === "update" ? partialResultText(ev.partialResult) : "";
			const msg: ChatMessage = {
				id, parentId: st.lastMsgId, ts: new Date().toISOString(), role: "assistant",
				synthetic: false,
				blocks: [{
					kind: "toolEvent", callId, phase, output,
					name: typeof ev.toolName === "string" ? ev.toolName : "",
					intent: typeof ev.intent === "string" ? ev.intent : "",
				}],
			};
			if (phase === "end") st.toolMsgIds.delete(callId);
			return { kind: "tool", msg, callId, phase };
		}

		// ── Narration: rows the TUI paints for itself. A chat that drops them
		//    is not an alternative to the terminal.
		case "notice": {
			const message = typeof ev.message === "string" ? ev.message : "";
			if (!message) return null;
			const level = typeof ev.level === "string" ? ev.level : "info";
			// omp's notice levels are info/warn/error; the block carries two, so
			// the original level stays in the text rather than being lost.
			return { kind: "narrate", msg: narrate(st, `${level}: ${message}`, level === "error" || level === "warn" ? "error" : "note") };
		}

		case "extension_error": {
			const err = typeof ev.error === "string" ? ev.error : "";
			const where = typeof ev.extensionPath === "string" ? ev.extensionPath : "extension";
			return { kind: "narrate", msg: narrate(st, `extension error (${where}): ${err}`, "error") };
		}

		case "auto_compaction_start":
			return { kind: "narrate", msg: narrate(st, "— compacting context… —", "note") };

		// The mirror engine gets its compaction card from the transcript's own
		// `compaction` entry (parse.ts:367); the rpc engine never reads the
		// transcript, so THIS event is the only place the card can come from.
		// Shape and cap match parse.ts exactly, or one engine's summary row
		// would differ from the other's for the same compaction.
		//
		// The sidecar bridge does not forward auto_compaction_end (it forwards
		// six narration types and this is not one), so nothing here reaches the
		// mirror pane and no card is doubled.
		case "auto_compaction_end": {
			const result = ev.result;
			// Aborted / skipped / errored: NOTHING was compacted. A summary card
			// would claim a rollup that does not exist, so those stay narration.
			if (ev.aborted === true || ev.skipped === true) {
				return { kind: "narrate", msg: narrate(st, "— compaction skipped —", "note") };
			}
			const errorMessage = typeof ev.errorMessage === "string" ? ev.errorMessage : "";
			if (errorMessage) {
				return { kind: "narrate", msg: narrate(st, `— compaction failed: ${errorMessage} —`, "error") };
			}
			const data = result !== null && typeof result === "object" ? result as Record<string, unknown> : undefined;
			const summary = typeof data?.summary === "string" ? data.summary : "";
			// omp's own compaction entry carries the summary; an absent one still
			// gets a card, because the context DID shrink and the pane must say so.
			const text = summary || "context compacted";
			const id = nextId(st);
			const capped = text.length > TRUNC_CAP;
			if (capped) recordFullText(st, id, [text]);
			return {
				kind: "narrate",
				msg: {
					id, parentId: st.lastMsgId, ts: new Date().toISOString(),
					role: "developer", synthetic: false,
					blocks: [{
						kind: "summary", variant: "compaction",
						text: capped ? text.slice(0, TRUNC_CAP) : text,
						tokensBefore: typeof data?.tokensBefore === "number" ? Math.max(0, Math.round(data.tokensBefore)) : 0,
						truncated: capped, fullLen: text.length,
					}],
				},
			};
		}

		case "auto_retry_start": {
			const why = typeof ev.error === "string" ? `: ${ev.error}` : "";
			return { kind: "narrate", msg: narrate(st, `— retrying${why} —`, "note") };
		}

		// The retry either produced a turn (its own rows follow) or escalated to
		// a fallback (which narrates itself) — a bare "done retrying" row would
		// be noise the TUI does not paint either.
		case "auto_retry_end":
			return null;

		case "retry_fallback_applied": {
			const from = typeof ev.from === "string" ? ev.from : "?";
			const to = typeof ev.to === "string" ? ev.to : "?";
			return { kind: "narrate", msg: narrate(st, `— model fallback: ${from} → ${to} —`, "note") };
		}

		// Sidecar-only: omp's RPC mode never emits these, the extension bus does.
		case "ttsr_triggered":
		case "todo_reminder": {
			const text = typeof ev.message === "string" && ev.message ? ev.message
				: typeof ev.reason === "string" ? ev.reason : "";
			if (!text) return null;
			return { kind: "narrate", msg: narrate(st, text, "note") };
		}

		// Subagent visibility. RPC-only: omp defaults the subscription to "off"
		// and rpc-chat.ts opts in at "progress" level, so only that engine ever
		// sees these. The card repaints in place under one id per subagent —
		// a progress stream is a status, not a conversation.
		//
		// `subagent_event` is deliberately unhandled: the "progress" level never
		// requests it, and replaying every subagent message would multiply frame
		// volume for nothing this card renders.
		case "subagent_lifecycle":
		case "subagent_progress": {
			const p = ev.payload !== null && typeof ev.payload === "object"
				? ev.payload as Record<string, unknown> : undefined;
			if (!p) return null;
			// Lifecycle carries `id`; progress nests the same id under `progress`.
			const prog = p.progress !== null && typeof p.progress === "object"
				? p.progress as Record<string, unknown> : undefined;
			const agent = typeof p.agent === "string" ? p.agent : "subagent";
			const index = typeof p.index === "number" ? p.index : 0;
			const key = typeof p.id === "string" && p.id ? p.id
				: typeof prog?.id === "string" && prog.id ? prog.id
				: `${agent}:${index}`;
			const status = typeof p.status === "string" ? p.status
				: typeof prog?.status === "string" ? prog.status : "running";
			const title = typeof p.description === "string" && p.description ? p.description
				: typeof prog?.description === "string" && prog.description ? prog.description
				: typeof p.task === "string" ? p.task
				: typeof prog?.task === "string" ? prog.task : "";
			// Third line only while there is live work to report. An empty one
			// would render as a stray blank row in the markdown body.
			const detail: string[] = [];
			const tool = typeof prog?.currentTool === "string" ? prog.currentTool : "";
			const intent = typeof prog?.lastIntent === "string" ? prog.lastIntent : "";
			if (tool) detail.push(intent ? `${tool} — ${intent}` : tool);
			else if (intent) detail.push(intent);
			if (typeof prog?.toolCount === "number" && prog.toolCount > 0) detail.push(`${prog.toolCount} tools`);
			if (typeof prog?.tokens === "number" && prog.tokens > 0) detail.push(`${prog.tokens} tokens`);

			// ONE card per subagent id, for the whole life of the conversation,
			// and it MERGES rather than overwrites.
			//
			// Releasing the slot on a terminal status looks tidy and is wrong:
			// omp reports completion TWICE — once as `subagent_lifecycle`
			// {status:"completed"} and once as a final `subagent_progress` — so
			// the first one freed the key and the second minted a SECOND card.
			// Observed live: two "scout · completed" rows for one spawn.
			//
			// Merging matters for the same reason: a lifecycle frame carries no
			// `progress` object, so the closing one would blank a card that had
			// just been showing the subagent's tool and token counts. Last
			// NON-EMPTY value wins per field.
			//
			// Nothing needs the slot back: omp mints a fresh subagent id per
			// spawn, and a new conversation gets a whole new ProjectState.
			// Bounded like fullText so a long session cannot grow it without limit.
			let card = st.subagentCards.get(key);
			if (card === undefined) {
				card = { id: nextId(st), title: "", detail: "" };
				st.subagentCards.set(key, card);
				while (st.subagentCards.size > MAX_FULL_TEXT) {
					const oldest = st.subagentCards.keys().next();
					if (oldest.done) break;
					st.subagentCards.delete(oldest.value);
				}
			}
			if (title) card.title = title;
			if (detail.length > 0) card.detail = detail.join(" · ");

			const lines = [`**${agent}** · ${status}`];
			if (card.title) lines.push(card.title);
			if (card.detail) lines.push(card.detail);
			return {
				kind: "narrate",
				msg: {
					id: card.id, parentId: st.lastMsgId, ts: new Date().toISOString(),
					role: "developer", synthetic: false,
					blocks: [{
						kind: "custom", customType: "subagent", text: lines.join("\n\n"),
						detailsJson: "", truncated: false, fullLen: 0,
					}],
				},
			};
		}

		default:
			return null;
	}
}
