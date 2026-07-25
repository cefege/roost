// Pure omp-event → ChatMessage projection. ONE switch, both live engines.
//
// Two callers feed it the SAME event objects:
//   - rpc-chat.ts, from an `omp --mode rpc` child's stdout frames;
//   - live-watcher.ts, from the bridge extension's NDJSON sidecar — omp's
//     ExtensionAPI event objects are field-identical to the RPC frames
//     (MessageUpdateEvent.message, ToolExecution{Start,Update,End}Event's
//     {toolCallId,toolName,args,intent,partialResult,result,isError}).
// Keeping one switch is the point: a narration row must be WORDED the same
// whichever engine saw it, and the id/upsert discipline must match or one turn
// becomes two rows in the pane.
//
// PURE: no host, no SessionRecord, no timers, no I/O. The caller owns emission.
// `coalesce` marks a mid-stream repaint the caller MAY run through a trailing
// timer (rpc-chat does — every token would otherwise be a ChatFrame; the
// sidecar is already coalesced by the writer, so live-watcher emits directly).

import type { ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
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
		toolMsgIds: new Map(), fullText: new Map(), emitted: new Set(),
	};
}

/** Drop per-conversation state (a new omp said hello, or the bridge said bye)
 *  WITHOUT rewinding nextMsg: the previous conversation's rows are still in the
 *  record, and re-minting their ids would make the next turn overwrite them
 *  (both sides are upsert-by-id). Ids stay monotonic for the watcher's life. */
export function resetProjectState(st: ProjectState): void {
	st.curMsgId = null;
	st.lastMsgId = "";
	st.toolMsgIds.clear();
	st.fullText.clear();
	st.emitted.clear();
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

/** Map an omp AgentMessage → ChatMessage, stashing untruncated block text when
 *  the cap actually bit. The event stream carries no transcript entry id, so
 *  re-reading the transcript by id (the mirror engine's path) cannot work here. */
export function mapAndRecord(st: ProjectState, raw: unknown, id: string, parentId: string): ChatMessage | null {
	const ts = new Date().toISOString();
	const msg = mapAgentMessage(raw, id, parentId, ts);
	if (!msg) return null;
	const truncated = msg.blocks.some((b) => b.kind === "thinking" && b.truncated);
	if (!truncated) { st.fullText.delete(id); return msg; }
	const full = mapAgentMessageFull(raw, id, parentId, ts);
	if (full) st.fullText.set(id, full.blocks.map(blockFullText));
	return msg;
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

		case "auto_compaction_end":
			return { kind: "narrate", msg: narrate(st, "— context compacted —", "note") };

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

		default:
			return null;
	}
}
