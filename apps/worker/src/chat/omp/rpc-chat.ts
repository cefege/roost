// Native omp chat sessions — per-session `omp --mode rpc` children.
//
// The proper alternative-frontend integration: the SPA's chat view drives omp
// through Roost natively (SessionsChatCommand → coord → worker → this module →
// RPC child stdin), and the child's live event stream flows back through the
// EXISTING chat wire (rec.chatMessages + ChatFrame upstream → SPA store → pane).
// The session's PTY stays a plain shell (the terminal view); the RPC child is a
// side process in the same cwd. Lazy: the child starts on the first chat
// command for a session — no spawn-path changes.
//
// The wire is upsert-by-id: a streaming message is re-emitted under the SAME
// chat message id as it grows, and both rec.chatMessages and the SPA store
// replace in place. Ids are synthetic (`rpc-N`) because AgentEvent messages
// carry no transcript entry id — see fullText below for the consequence.
//
// Event mapping (tolerant, structural — omp types are deliberately NOT imported):
//   agent_start/agent_end        → entry.streaming (rides every ChatFrame)
//   message_start/update/end     → one upserted ChatMessage per assistant turn
//   tool_execution_start/end     → one upserted toolEvent message per toolCallId
//   extension_ui_request         → an approval block the pane answers inline
//                                  (select/confirm/input/editor); `cancel`
//                                  retires a card omp gave up on
//   notice                       → developer text row
//   everything else              → ignored (tool_execution_update carries partial
//                                  results ToolEventBlock cannot model; the final
//                                  result arrives as a toolResult message)

import type { ChatFrame, ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
import { diag, log } from "@roost/shared";
import type { SessionRecord } from "../../session-record.ts";
import {
	askQuestionMatches, buildAskChoices, parseAskSpec, splitSelectTitle, type AskQuestionSpec,
} from "./ask-spec.ts";
import { mapAgentMessage, mapAgentMessageFull } from "./parse.ts";
import { OmpRpcDriver, type RpcFrame } from "./rpc-driver.ts";
import { loadOmpSessionFile, saveOmpSessionFile, forgetOmpSession } from "./session-store.ts";

/** Narrow view of SessionManager — keeps this module decoupled/testable. */
export interface RpcChatHost {
	getBySessionId(sessionId: string): SessionRecord | undefined;
	sendChatFrameUpstream: ((channelId: number, frame: ChatFrame) => void) | null;
}

/** Commands the SPA may tunnel to the child (omp RpcCommand grammar subset). */
const ALLOWED_COMMANDS: Record<string, true> = {
	prompt: true, steer: true, follow_up: true, abort: true, abort_and_prompt: true,
	get_state: true, get_messages: true, get_session_stats: true,
	set_model: true, cycle_model: true, get_available_models: true, get_available_commands: true,
	set_thinking_level: true, cycle_thinking_level: true,
	compact: true, set_auto_compaction: true, new_session: true, set_session_name: true,
};

/** Commands that change the model with NO corresponding omp event — the chip
 *  only stays honest if the worker re-reads state off their response. (Effort
 *  changes DO push `thinking_level_changed`, so they are absent here.) */
const MODEL_CHANGING_COMMANDS: Record<string, true> = { set_model: true, cycle_model: true };

/** UI methods that ask the user a question and BLOCK the turn until answered
 *  (omp awaits every one of these — see rpc-mode's #createDialogPromise).
 *  `editor` is the `ask` tool's "Other (type your own)" branch: free text,
 *  same {value}/{cancelled} reply shape as `input`, so the pane reuses that
 *  card. Its optional prefill rides in the block's `message`. */
const UI_DECISION_METHODS: Record<string, true> = { confirm: true, select: true, input: true, editor: true };

/** UI methods omp fires and forgets — no response is awaited, so dropping one
 *  costs nothing. Anything OUTSIDE this set and outside UI_DECISION_METHODS is
 *  presumed awaited and gets an immediate decline: a request we neither render
 *  nor answer wedges the turn forever, which is the worst failure this pane has. */
const UI_FIRE_AND_FORGET_METHODS: Record<string, true> = {
	notify: true, setStatus: true, setWidget: true, setTitle: true,
	set_editor_text: true, open_url: true,
};

/** Coalesce per-token message_update remaps. Without this every token becomes
 *  a ChatFrame on the coord bus. */
const STREAM_FLUSH_MS = 60;

/** Tail of a running tool's output kept on the wire. A `bash` tail -f would
 *  otherwise re-send its whole buffer on every update. */
const PARTIAL_CAP = 2000;

/** omp caps a page at 256 messages; ask for the max so a reload is few round trips. */
const HISTORY_PAGE_LIMIT = 256;

/** Shared empty tick set for select frames with no toggle history. */
const EMPTY_CHECKED: ReadonlySet<string> = new Set();

interface RpcChatEntry {
	driver: OmpRpcDriver;
	sessionId: string;
	nextMsg: number;
	lastMsgId: string;
	/** Chat message id the in-flight assistant/user message streams into. */
	curMsgId: string | null;
	/** True between agent_start and agent_end — rides every ChatFrame. */
	streaming: boolean;
	/** Session status the omp TUI keeps on screen. Refreshed from get_state at
	 *  boot and after every turn, and carried on every frame. */
	model: string;
	contextPct: number;
	contextTokens: number;
	/** Friendly display name for `model`, from get_state. "" until resolved. */
	modelName: string;
	/** omp ThinkingLevel string from get_state/config_update. "" until resolved. */
	thinkingLevel: string;
	/** omp session JSONL path, from get_state. Survives a child restart so the
	 *  respawned child resumes the same conversation via switch_session. */
	sessionFile: string | null;
	/** Resolves once session resume + get_state finished. Every command awaits
	 *  it: a prompt written before switch_session lands in the WRONG (fresh)
	 *  omp session and the resume then swaps the conversation out mid-turn. */
	ready: Promise<void>;
	/** omp extension UI request id → chat message id holding its approval block. */
	pendingUi: Map<string, string>;
	/** toolCallId → chat message id, so start/end collapse into one message. */
	toolMsgIds: Map<string, string>;
	/** toolCallId → the ask tool's parsed questions, captured from
	 *  tool_execution_start. omp's RPC select frame carries labels only, so the
	 *  descriptions/multi/header metadata exists nowhere else on the wire. */
	askSpecs: Map<string, AskQuestionSpec[]>;
	/** toolCallId of the newest in-flight `ask` call; null when none. */
	activeAsk: string | null;
	/** `${toolCallId}:${questionIndex}` → labels ticked so far. omp never echoes
	 *  checked state back, so it is reconstructed from the answers WE post. */
	askChecked: Map<string, Set<string>>;
	/** `${toolCallId}:${questionIndex}` → chat message id, so a multi-select's
	 *  toggle chain repaints ONE card instead of stacking a dead one per tick. */
	askCardIds: Map<string, string>;
	/** chat message id → per-block UNTRUNCATED text, for SessionsGetChatBlock.
	 *  Only populated for messages that actually got truncated. */
	fullText: Map<string, string[]>;
	/** toolCallId → newest un-emitted live output message. Coalesced on a
	 *  trailing timer: tool_execution_update fires as fast as the tool writes,
	 *  and a leading-edge drop would strand the last line before a long quiet
	 *  stretch — exactly the compile-then-wait case live output is for. */
	toolPending: Map<string, ChatMessage>;
	toolTimer: ReturnType<typeof setTimeout> | null;
	/** Trailing-timer coalescing state for message_update. */
	pendingMsg: ChatMessage | null;
	flushTimer: ReturnType<typeof setTimeout> | null;
}

const entries = new Map<string, RpcChatEntry>();
let reaper: ReturnType<typeof setInterval> | null = null;

/** Kill children whose session is gone (covers close paths without hooks). */
function ensureReaper(host: RpcChatHost): void {
	if (reaper) return;
	reaper = setInterval(() => {
		for (const [sid, e] of entries) {
			if (!host.getBySessionId(sid)) {
				log.info("omp-rpc", "reap_orphan_child", { sid });
				disposeEntry(e);
				entries.delete(sid);
			}
		}
		if (entries.size === 0 && reaper) { clearInterval(reaper); reaper = null; }
	}, 30_000);
}

function disposeEntry(entry: RpcChatEntry): void {
	if (entry.flushTimer) { clearTimeout(entry.flushTimer); entry.flushTimer = null; }
	if (entry.toolTimer) { clearTimeout(entry.toolTimer); entry.toolTimer = null; }
	entry.pendingMsg = null;
	entry.toolPending.clear();
	entry.askSpecs.clear();
	entry.askChecked.clear();
	entry.askCardIds.clear();
	entry.activeAsk = null;
	entry.driver.dispose();
}

/** Append-or-replace by id. On replace the message's ORIGINAL chatMsgSeqs slot
 *  is kept: getChatHistory pages by walking that array and it must stay
 *  monotonic. rec.chat_seq still advances so the SPA orders frames. */
function upsertMessage(host: RpcChatHost, entry: RpcChatEntry, msg: ChatMessage): void {
	const rec = host.getBySessionId(entry.sessionId);
	if (!rec) return;
	rec.chatMessages ??= [];
	rec.chatMsgSeqs ??= [];
	rec.chat_seq += 1;
	const i = rec.chatMessages.findIndex((m) => m.id === msg.id);
	if (i >= 0) rec.chatMessages[i] = msg;
	else { rec.chatMessages.push(msg); rec.chatMsgSeqs.push(rec.chat_seq); }
	host.sendChatFrameUpstream?.(rec.channelId, frameFor(entry, rec.chat_seq, [msg], false));
}

/** Every frame carries the session status the omp TUI keeps on screen, so the
 *  pane can show it without a second channel. */
function frameFor(entry: RpcChatEntry, seq: number, append: ChatMessage[], reset: boolean): ChatFrame {
	return {
		sessionId: "", append, seq, reset,
		streaming: entry.streaming,
		model: entry.model,
		modelName: entry.modelName,
		thinkingLevel: entry.thinkingLevel,
		contextPct: entry.contextPct,
		contextTokens: entry.contextTokens,
	};
}

/** Publish a state change with no message payload (agent_start/end, status). */
function emitState(host: RpcChatHost, entry: RpcChatEntry): void {
	const rec = host.getBySessionId(entry.sessionId);
	if (!rec) return;
	host.sendChatFrameUpstream?.(rec.channelId, frameFor(entry, rec.chat_seq, [], false));
}

/** Untruncated text of one block, mirroring parse.ts::fullBlockText's switch.
 *  toolResult is absent on purpose: its payload rides whole in rawJson. */
function blockFullText(b: ContentBlock): string {
	switch (b.kind) {
		case "text":
		case "thinking":
			return b.text;
		case "toolCall":
			return b.argsJson;
		default:
			return "";
	}
}

/** Map an omp AgentMessage → ChatMessage, stashing untruncated block text when
 *  the cap actually bit. The RPC stream carries no entry id, so re-reading the
 *  transcript by id (the mirror engine's path) cannot work here. */
function mapAndRecord(entry: RpcChatEntry, raw: unknown, id: string, parentId: string): ChatMessage | null {
	const ts = new Date().toISOString();
	const msg = mapAgentMessage(raw, id, parentId, ts);
	if (!msg) return null;
	const truncated = msg.blocks.some((b) => b.kind === "thinking" && b.truncated);
	if (!truncated) { entry.fullText.delete(id); return msg; }
	const full = mapAgentMessageFull(raw, id, parentId, ts);
	if (full) entry.fullText.set(id, full.blocks.map(blockFullText));
	return msg;
}

/** One developer narration row. Prose — it goes through the renderer's
 *  markdown pass, same as any assistant text. */
function narrate(host: RpcChatHost, entry: RpcChatEntry, text: string): void {
	upsertMessage(host, entry, {
		id: `rpc-${entry.nextMsg++}`, parentId: entry.lastMsgId, ts: new Date().toISOString(),
		role: "developer", blocks: [{ kind: "text", text }],
	});
}

/** Text of a tool's in-flight partial result. omp's AgentToolResult carries
 *  `content: [{type:"text", text}]`; some tools report progress as a bare
 *  string. Tail-capped: a running command's newest output is what matters,
 *  and this re-renders on every update. */
function partialResultText(raw: unknown): string {
	if (typeof raw === "string") return stripAnsi(raw).slice(-PARTIAL_CAP);
	if (raw === null || typeof raw !== "object") return "";
	const content = (raw as { content?: unknown }).content;
	if (!Array.isArray(content)) return "";
	let out = "";
	for (const b of content) {
		if (b !== null && typeof b === "object") {
			const blk = b as { type?: unknown; text?: unknown };
			if (blk.type === "text" && typeof blk.text === "string") out += blk.text;
		}
	}
	return stripAnsi(out).slice(-PARTIAL_CAP);
}

// CSI/OSC/SS3 escapes. omp writes slash-command output for a terminal, so
// `/context` arrives full of 24-bit colour runs that would render as literal
// garbage in a web bubble.
const ANSI_RE = /[\u001b\u009b](?:\][^\u0007\u001b]*(?:\u0007|\u001b\\)|[[(][0-?]*[ -/]*[@-~])/g;

/** Strip escapes AND resolve carriage-return overwrites. omp renders progress
 *  lines by rewriting one line with \r; kept verbatim inside a fence they
 *  stack up as duplicate rows, so only the final text of each line survives. */
function stripAnsi(text: string): string {
	return text
		.replace(ANSI_RE, "")
		.replace(/\r\n/g, "\n")
		.split("\n")
		.map((line) => (line.includes("\r") ? line.slice(line.lastIndexOf("\r") + 1) : line))
		.join("\n");
}

/** Preformatted terminal output → a fenced block. `/context` and friends are
 *  box-drawn, column-aligned text; markdown would collapse the whitespace and
 *  shred the table. */
function preformatted(text: string): string {
	return `\`\`\`\n${text.replace(/```/g, "``\u200b`")}\n\`\`\``;
}

/** Flush a coalesced message_update remap now (message_end / tool / agent_end). */
function flushPending(host: RpcChatHost, entry: RpcChatEntry): void {
	if (entry.flushTimer) { clearTimeout(entry.flushTimer); entry.flushTimer = null; }
	const msg = entry.pendingMsg;
	entry.pendingMsg = null;
	if (msg) upsertMessage(host, entry, msg);
}

function scheduleFlush(host: RpcChatHost, entry: RpcChatEntry): void {
	if (entry.flushTimer) return;
	entry.flushTimer = setTimeout(() => {
		entry.flushTimer = null;
		const msg = entry.pendingMsg;
		entry.pendingMsg = null;
		if (msg) upsertMessage(host, entry, msg);
	}, STREAM_FLUSH_MS);
}

/** Queue a live tool-output frame, newest-wins per call, emitted on a trailing
 *  timer so a chatty tool cannot turn every write into a ChatFrame. */
function queueToolUpdate(host: RpcChatHost, entry: RpcChatEntry, callId: string, msg: ChatMessage): void {
	entry.toolPending.set(callId, msg);
	if (entry.toolTimer) return;
	entry.toolTimer = setTimeout(() => {
		entry.toolTimer = null;
		flushToolUpdates(host, entry);
	}, STREAM_FLUSH_MS);
}

function flushToolUpdates(host: RpcChatHost, entry: RpcChatEntry): void {
	if (entry.toolTimer) { clearTimeout(entry.toolTimer); entry.toolTimer = null; }
	if (entry.toolPending.size === 0) return;
	const pending = [...entry.toolPending.values()];
	entry.toolPending.clear();
	for (const msg of pending) upsertMessage(host, entry, msg);
}

function onEvent(host: RpcChatHost, entry: RpcChatEntry, frame: RpcFrame): void {
	switch (frame.type) {
		case "agent_start":
			entry.streaming = true;
			emitState(host, entry);
			return;

		case "agent_end":
			flushPending(host, entry);
			flushToolUpdates(host, entry);
			entry.streaming = false;
			entry.curMsgId = null;
			emitState(host, entry);
			// Context grew over the turn; the TUI's status line would already
			// show the new number. Failure is silent — status is not load-bearing.
			void refreshStatus(host, entry).catch(() => { /* keep last known */ });
			return;

		case "message_start": {
			flushPending(host, entry);
			const id = `rpc-${entry.nextMsg++}`;
			entry.curMsgId = id;
			const msg = mapAndRecord(entry, frame.message, id, entry.lastMsgId);
			// A message_start usually carries empty content — nothing to render
			// yet, but the id is reserved so updates land on one row.
			if (msg) upsertMessage(host, entry, msg);
			return;
		}

		case "message_update": {
			// frame.message is the FULL message so far, not a delta — remap it
			// wholesale and let the trailing timer decide when it hits the wire.
			const id = entry.curMsgId ?? (entry.curMsgId = `rpc-${entry.nextMsg++}`);
			const msg = mapAndRecord(entry, frame.message, id, entry.lastMsgId);
			if (!msg) return;
			entry.pendingMsg = msg;
			scheduleFlush(host, entry);
			return;
		}

		case "message_end": {
			// Drop the coalesced partial: this frame supersedes it under the same id.
			if (entry.flushTimer) { clearTimeout(entry.flushTimer); entry.flushTimer = null; }
			entry.pendingMsg = null;
			const id = entry.curMsgId ?? `rpc-${entry.nextMsg++}`;
			entry.curMsgId = null;
			const msg = mapAndRecord(entry, frame.message, id, entry.lastMsgId);
			if (!msg) return;
			entry.lastMsgId = id;
			upsertMessage(host, entry, msg);
			return;
		}

		case "tool_execution_start":
		case "tool_execution_update":
		case "tool_execution_end": {
			const callId = typeof frame.toolCallId === "string" ? frame.toolCallId : "";
			if (!callId) return;
			// The ask tool's rich question data — descriptions, header, multi —
			// exists ONLY here: omp's RPC select frame flattens every option to a
			// bare label. Stash it so the select frames can be correlated back
			// into a real selection card.
			if (frame.type === "tool_execution_start" && frame.toolName === "ask") {
				const spec = parseAskSpec(frame.args);
				if (spec.length > 0) {
					entry.askSpecs.set(callId, spec);
					entry.activeAsk = callId;
					enrichPendingAsk(host, entry);
				}
			} else if (frame.type === "tool_execution_end" && entry.askSpecs.delete(callId)) {
				if (entry.activeAsk === callId) entry.activeAsk = null;
				const prefix = `${callId}:`;
				for (const key of [...entry.askChecked.keys()]) if (key.startsWith(prefix)) entry.askChecked.delete(key);
				for (const key of [...entry.askCardIds.keys()]) if (key.startsWith(prefix)) entry.askCardIds.delete(key);
			}
			flushPending(host, entry);
			let id = entry.toolMsgIds.get(callId);
			if (id === undefined) { id = `rpc-${entry.nextMsg++}`; entry.toolMsgIds.set(callId, id); }
			const phase = frame.type === "tool_execution_start" ? "start"
				: frame.type === "tool_execution_update" ? "update" : "end";
			// The final result arrives separately as a toolResult message, so the
			// event's own output only has to carry the LIVE view while running —
			// which is what the TUI shows and the chat used to drop entirely.
			const output = phase === "update" ? partialResultText(frame.partialResult) : "";
			const msg: ChatMessage = {
				id, parentId: entry.lastMsgId, ts: new Date().toISOString(), role: "assistant",
				blocks: [{
					kind: "toolEvent", callId,
					name: typeof frame.toolName === "string" ? frame.toolName : "",
					phase,
					intent: typeof frame.intent === "string" ? frame.intent : "",
					output,
				}],
			};
			if (phase === "update") { queueToolUpdate(host, entry, callId, msg); return; }
			// start/end are turn structure, not chatter — emit immediately, and
			// drop any queued partial the terminal state supersedes.
			entry.toolPending.delete(callId);
			if (phase === "end") entry.toolMsgIds.delete(callId);
			upsertMessage(host, entry, msg);
			return;
		}

		case "extension_ui_request": {
			const requestId = typeof frame.id === "string" ? frame.id : "";
			const method = typeof frame.method === "string" ? frame.method : "";
			if (!requestId) return;
			// omp withdrew a question (turn aborted, timeout): retire the card so
			// the pane stops offering buttons that answer nothing.
			if (method === "cancel") {
				const targetId = typeof frame.targetId === "string" ? frame.targetId : "";
				retireUiRequest(host, entry, targetId, "cancelled");
				return;
			}
			if (UI_DECISION_METHODS[method] !== true) {
				// Unknown AND not known-fire-and-forget ⇒ omp is awaiting a reply we
				// will never render. Decline it now; a silent drop hangs the turn.
				if (UI_FIRE_AND_FORGET_METHODS[method] !== true) {
					log.warn("omp-rpc", "ui_request_declined", { sid: entry.sessionId, method });
					entry.driver.post({ type: "extension_ui_response", id: requestId, cancelled: true });
				}
				return;
			}
			flushPending(host, entry);
			// A select frame is the only place an ask question reaches the pane,
			// and omp mangles the title on the way: `(N selected) ` on a
			// multi-select re-prompt, ` (i/total)` inside a batch. Split it back
			// apart so the card shows the question the model actually wrote.
			const rawTitle = typeof frame.title === "string" ? frame.title : "";
			const rawOptions = Array.isArray(frame.options)
				? frame.options.filter((o): o is string => typeof o === "string")
				: [];
			const isSelect = method === "select";
			const { question, progress, index } = isSelect
				? splitSelectTitle(rawTitle)
				: { question: rawTitle, progress: "", index: 0 };
			const spec = isSelect && entry.activeAsk ? entry.askSpecs.get(entry.activeAsk) ?? [] : [];
			const matched = askQuestionMatches(spec, index, question);
			const key = `${entry.activeAsk ?? ""}:${index}`;
			const checked = entry.askChecked.get(key) ?? EMPTY_CHECKED;
			// Reuse the card of an earlier frame for the SAME ask question so a
			// multi-select's toggle chain repaints in place instead of stacking a
			// dead card per tick. Safe because answerUiRequest posts and retires
			// the previous requestId synchronously, before omp can re-prompt.
			const reuse = matched ? entry.askCardIds.get(key) : undefined;
			const id = reuse ?? `rpc-${entry.nextMsg++}`;
			if (matched && reuse === undefined) entry.askCardIds.set(key, id);
			entry.pendingUi.set(requestId, id);
			upsertMessage(host, entry, {
				id, parentId: entry.lastMsgId, ts: new Date().toISOString(), role: "developer",
				blocks: [{
					kind: "approval", requestId, method,
					title: question,
					// input → placeholder (hint text); editor → prefill (seed text).
					message: typeof frame.message === "string" ? frame.message
						: typeof frame.placeholder === "string" ? frame.placeholder
						: typeof frame.prefill === "string" ? frame.prefill : "",
					options: rawOptions,
					resolved: false, answer: "",
					richOptions: isSelect ? buildAskChoices(spec, index, rawOptions, checked, question) : [],
					header: matched ? spec[index]!.header : "",
					progress,
					multi: matched ? spec[index]!.multi : false,
				}],
			});
			return;
		}

		// Everything below is a plain narration row. The TUI shows all of it; a
		// chat that drops it is not an alternative to the terminal.
		case "notice": {
			const message = typeof frame.message === "string" ? frame.message : "";
			if (!message) return;
			const level = typeof frame.level === "string" ? frame.level : "info";
			narrate(host, entry, `${level}: ${message}`);
			return;
		}

		// Local slash commands (/model, /context, /cost …) answer HERE, not via
		// an agent turn — no message_* frames at all. Dropping this is why a
		// slash command in the chat pane looked like it did nothing.
		case "command_output": {
			const text = typeof frame.text === "string" ? stripAnsi(frame.text) : "";
			if (text.trim().length === 0) return;
			flushPending(host, entry);
			narrate(host, entry, preformatted(text));
			return;
		}

		case "extension_error": {
			const err = typeof frame.error === "string" ? frame.error : "";
			const where = typeof frame.extensionPath === "string" ? frame.extensionPath : "extension";
			narrate(host, entry, `extension error (${where}): ${err}`);
			return;
		}

		case "auto_compaction_start":
			narrate(host, entry, "— compacting context… —");
			return;

		case "auto_compaction_end":
			narrate(host, entry, "— context compacted —");
			return;

		case "auto_retry_start": {
			const why = typeof frame.error === "string" ? `: ${frame.error}` : "";
			narrate(host, entry, `— retrying${why} —`);
			return;
		}

		case "retry_fallback_applied": {
			const from = typeof frame.from === "string" ? frame.from : "?";
			const to = typeof frame.to === "string" ? frame.to : "?";
			narrate(host, entry, `— model fallback: ${from} → ${to} —`);
			return;
		}

		case "thinking_level_changed":
			// The ONLY unsolicited config event omp 17.1.2 pushes (verified against
			// a live child: set_model answers with a bare response and no event,
			// so THAT path refreshes off its own command result instead). Without
			// this the effort half of the chip goes stale until a turn ends —
			// including when the user changes it inside omp itself.
			void refreshStatus(host, entry).catch(() => { /* keep last known */ });
			return;

		default:
			return;
	}
}

/** Resolve the child's session file (get_state) and, on a RESPAWN, resume the
 *  prior conversation: switch_session then re-seed history from the paged
 *  endpoint. Paging (not get_messages) because a physical frame caps at 1 MiB;
 *  chunk reassembly covers a big RESPONSE, but paging keeps each one small. */
async function initSession(host: RpcChatHost, entry: RpcChatEntry, priorFile: string | null): Promise<void> {
	try {
		if (priorFile) {
			const res = await entry.driver.send({ type: "switch_session", sessionPath: priorFile });
			if (res.success === true) await reloadHistory(host, entry);
			else log.warn("omp-rpc", "switch_session_failed", { sid: entry.sessionId, error: String(res.error ?? "") });
		}
		await refreshStatus(host, entry);
	} catch (err) {
		log.warn("omp-rpc", "init_session_failed", { sid: entry.sessionId, error: String(err) });
	}
}

/** Pull the status the omp TUI shows permanently — model + context usage — and
 *  publish it. Cheap and only on boot / turn end, so no polling loop. */
async function refreshStatus(host: RpcChatHost, entry: RpcChatEntry): Promise<void> {
	const state = await entry.driver.send({ type: "get_state" });
	if (state.success !== true) return;
	const data = state.data as {
		sessionFile?: unknown;
		model?: { provider?: unknown; id?: unknown; name?: unknown };
		contextUsage?: { tokens?: unknown; percent?: unknown };
		thinkingLevel?: unknown;
	} | undefined;
	if (!data) return;

	const file = typeof data.sessionFile === "string" ? data.sessionFile : null;
	if (file && file !== entry.sessionFile) {
		entry.sessionFile = file;
		// Durable, so a worker RESTART resumes this conversation too — the
		// in-memory entry only covers a child that died under a live worker.
		saveOmpSessionFile(entry.sessionId, file);
		const rec = host.getBySessionId(entry.sessionId);
		if (rec) rec.chatTranscriptPath = file;
	}

	const provider = typeof data.model?.provider === "string" ? data.model.provider : "";
	const id = typeof data.model?.id === "string" ? data.model.id : "";
	entry.model = provider && id ? `${provider}/${id}` : id || provider;
	entry.thinkingLevel = typeof data.thinkingLevel === "string" ? data.thinkingLevel : "";
	// get_state's model object already carries the friendly name, so the chip
	// costs no extra round trip. Falling back to the id half keeps a build that
	// omits `name` readable rather than blank.
	entry.modelName = typeof data.model?.name === "string" && data.model.name
		? data.model.name
		: (entry.model.split("/").pop() ?? "");
	const usage = data.contextUsage;
	entry.contextTokens = typeof usage?.tokens === "number" ? Math.max(0, Math.round(usage.tokens)) : 0;
	// omp reports `percent` already scaled 0-100 (e.g. 0.55 = 0.55%).
	entry.contextPct = typeof usage?.percent === "number" ? Math.max(0, Math.min(100, Math.round(usage.percent))) : 0;
	emitState(host, entry);
}

/** Drain get_messages_page into a fresh transcript. A busy/stale session
 *  abandons the drain and keeps what it has — never retried in a loop. */
async function reloadHistory(host: RpcChatHost, entry: RpcChatEntry): Promise<void> {
	const collected: ChatMessage[] = [];
	let cursor: string | undefined;
	let incomplete = false;
	for (;;) {
		const cmd: Record<string, unknown> = { type: "get_messages_page", limit: HISTORY_PAGE_LIMIT };
		if (cursor !== undefined) cmd.cursor = cursor;
		const res = await entry.driver.send(cmd);
		if (res.success !== true) {
			log.warn("omp-rpc", "history_page_failed", {
				sid: entry.sessionId, code: String(res.code ?? ""), error: String(res.error ?? ""),
			});
			incomplete = true;
			break;
		}
		const data = res.data as { messages?: unknown; nextCursor?: unknown } | undefined;
		const page = Array.isArray(data?.messages) ? data.messages : [];
		for (const raw of page) {
			const id = `rpc-${entry.nextMsg++}`;
			const msg = mapAndRecord(entry, raw, id, entry.lastMsgId);
			if (!msg) continue;
			entry.lastMsgId = id;
			collected.push(msg);
		}
		cursor = typeof data?.nextCursor === "string" ? data.nextCursor : undefined;
		if (cursor === undefined) break;
	}

	const rec = host.getBySessionId(entry.sessionId);
	if (!rec) return;
	rec.chatMessages = [];
	rec.chatMsgSeqs = [];
	host.sendChatFrameUpstream?.(rec.channelId, frameFor(entry, rec.chat_seq, [], true));
	for (const msg of collected) upsertMessage(host, entry, msg);
	if (incomplete) {
		upsertMessage(host, entry, {
			id: `rpc-${entry.nextMsg++}`, parentId: entry.lastMsgId, ts: new Date().toISOString(),
			role: "developer", blocks: [{ kind: "text", text: "— history reload incomplete —" }],
		});
	}
}

/** Lazy-start the RPC child for a session (idempotent). Emits the reset frame
 *  so the SPA reseeds, mirroring _ensureChatWatch. */
export function ensureRpcChat(host: RpcChatHost, rec: SessionRecord): RpcChatEntry {
	const sid = String(rec.sessionId);
	const existing = entries.get(sid);
	if (existing?.driver.alive) return existing;
	// The session file is the only way back to a conversation. In-memory entry
	// = child died under a live worker; the durable store = the worker itself
	// restarted, so there is no entry at all.
	const priorFile = existing?.sessionFile ?? loadOmpSessionFile(sid);
	if (existing) disposeEntry(existing);

	const entry: RpcChatEntry = {
		sessionId: sid, nextMsg: 1, lastMsgId: "", curMsgId: null, streaming: false,
		model: "", modelName: "", thinkingLevel: "",
		contextPct: 0, contextTokens: 0,
		sessionFile: priorFile, pendingUi: new Map(), toolMsgIds: new Map(), fullText: new Map(),
		askSpecs: new Map(), activeAsk: null, askChecked: new Map(), askCardIds: new Map(),
		toolPending: new Map(), toolTimer: null,
		pendingMsg: null, flushTimer: null, ready: Promise.resolve(),
		driver: null as unknown as OmpRpcDriver,
	};
	entry.driver = new OmpRpcDriver({
		cwd: rec.cwd,
		onEvent: (frame) => onEvent(host, entry, frame),
		onExit: () => {
			diag("chat.rpc_exit", { sid });
			if (entry.flushTimer) { clearTimeout(entry.flushTimer); entry.flushTimer = null; }
			entry.streaming = false;
			upsertMessage(host, entry, {
				id: `rpc-${entry.nextMsg++}`, parentId: entry.lastMsgId, ts: new Date().toISOString(),
				role: "developer", blocks: [{ kind: "text", text: "— agent process exited —" }],
			});
			// Keep the entry: its sessionFile is what resumes the conversation on
			// the next command. The driver is dead, so ensureRpcChat respawns.
		},
	});
	entries.set(sid, entry);
	rec.chatMessages ??= [];
	rec.chatMsgSeqs ??= [];
	host.sendChatFrameUpstream?.(rec.channelId, frameFor(entry, rec.chat_seq, [], true));
	entry.driver.start();
	// initSession never rejects (it logs); commands await this, they never fail on it.
	entry.ready = initSession(host, entry, priorFile);
	ensureReaper(host);
	diag("chat.rpc_start", { sid, cwd: rec.cwd, resumed: priorFile !== null });
	return entry;
}

/** Mark a pending approval card answered and stop tracking its request id.
 *  Shared by the pane's own reply and by omp withdrawing the question. */
function retireUiRequest(host: RpcChatHost, entry: RpcChatEntry, requestId: string, answer: string): void {
	const msgId = requestId ? entry.pendingUi.get(requestId) : undefined;
	if (msgId === undefined) return;
	entry.pendingUi.delete(requestId);
	const prev = host.getBySessionId(entry.sessionId)?.chatMessages?.find((m) => m.id === msgId);
	const block = prev?.blocks[0];
	if (prev && block?.kind === "approval") {
		upsertMessage(host, entry, { ...prev, blocks: [{ ...block, resolved: true, answer }] });
	}
}

/** Re-render approval cards that reached the pane BEFORE their ask spec did.
 *  omp writes extension_ui_request straight to stdout, while
 *  tool_execution_start rides the session event bus a tick behind — so the
 *  FIRST question of an ask reliably arrives descriptionless. Observed against
 *  omp from source: select("Which auth method? (1/2)") lands before the
 *  tool_execution_start that carries the very args describing it. */
function enrichPendingAsk(host: RpcChatHost, entry: RpcChatEntry): void {
	const callId = entry.activeAsk;
	const spec = callId ? entry.askSpecs.get(callId) : undefined;
	const messages = host.getBySessionId(entry.sessionId)?.chatMessages;
	if (!callId || !spec || !messages) return;
	for (const msgId of entry.pendingUi.values()) {
		const prev = messages.find((m) => m.id === msgId);
		const block = prev?.blocks[0];
		if (!prev || block?.kind !== "approval" || block.method !== "select") continue;
		const index = block.progress ? Number(block.progress.split("/")[0]) - 1 : 0;
		if (!askQuestionMatches(spec, index, block.title)) continue;
		const key = `${callId}:${index}`;
		// Claim the card for the toggle chain too, or the first tick would mint
		// a second card beside this one.
		entry.askCardIds.set(key, msgId);
		upsertMessage(host, entry, { ...prev, blocks: [{
			...block,
			richOptions: buildAskChoices(spec, index, block.options, entry.askChecked.get(key) ?? EMPTY_CHECKED, block.title),
			header: spec[index]!.header,
			multi: spec[index]!.multi,
		}] });
	}
}

/** Answer an omp extension_ui_request from the chat pane. Bypasses send():
 *  UI responses get no correlated `response` frame back. */
function answerUiRequest(
	host: RpcChatHost, rec: SessionRecord, cmd: Record<string, unknown>,
): { ok: true; response: RpcFrame } | { ok: false; error: string } {
	const requestId = typeof cmd.id === "string" ? cmd.id : "";
	const entry = entries.get(String(rec.sessionId));
	if (!entry || (requestId ? entry.pendingUi.get(requestId) : undefined) === undefined) {
		return { ok: false, error: "unknown ui request" };
	}
	// Resolve the answer BEFORE posting: a multi-select tick has to update the
	// tracked set so the card omp re-prompts with paints the box as ticked.
	// omp never echoes checked state back — this is the only record of it.
	const answer = resolveAnswerText(host, entry, requestId, cmd);
	try { entry.driver.post(cmd); }
	catch (err) { return { ok: false, error: err instanceof Error ? err.message : String(err) }; }

	retireUiRequest(host, entry, requestId, answer);
	return { ok: true, response: { type: "ack" } };
}

/** What the resolved card should say it answered, and — for a multi-select —
 *  the tick bookkeeping that makes the next re-prompt render correctly.
 *  A bare `"Next →"` or a glyph-prefixed done label is navigation, not an
 *  answer: the card must show what was actually selected. */
function resolveAnswerText(
	host: RpcChatHost, entry: RpcChatEntry, requestId: string, cmd: Record<string, unknown>,
): string {
	if (cmd.cancelled === true) return "cancelled";
	const value = typeof cmd.value === "string" ? cmd.value : undefined;
	if (value === undefined) return cmd.confirmed === true ? "approved" : "denied";

	const msgId = entry.pendingUi.get(requestId);
	const block = host.getBySessionId(entry.sessionId)?.chatMessages
		?.find((m) => m.id === msgId)?.blocks[0];
	if (block?.kind !== "approval" || block.method !== "select") return value;
	const choice = block.richOptions.find((c) => c.value === value);
	if (!block.multi) return choice?.label ?? value;

	// The card's own progress suffix is what keyed the tick set at build time.
	const index = block.progress ? Number(block.progress.split("/")[0]) - 1 : 0;
	const key = `${entry.activeAsk ?? ""}:${Number.isFinite(index) && index >= 0 ? index : 0}`;
	let set = entry.askChecked.get(key);
	if (!set) { set = new Set(); entry.askChecked.set(key, set); }
	if (choice?.role === "option") {
		if (set.has(choice.label)) set.delete(choice.label); else set.add(choice.label);
	}
	return set.size > 0 ? [...set].join(", ") : "(none)";
}

/** omp's model catalog is ~1.1 MB of provider metadata (costs, context windows,
 *  capability matrices) and only arrives at all because the driver negotiates
 *  protocol v2 chunking. The picker needs five fields per model, so project
 *  here rather than pushing a megabyte through coord to the browser. */
function trimCatalog(response: RpcFrame): RpcFrame {
	if (response.success !== true) return response;
	const data = response.data as { models?: unknown } | undefined;
	if (!Array.isArray(data?.models)) return response;
	const models: { provider: string; id: string; name: string; reasoning: boolean; efforts: string[] }[] = [];
	for (const m of data.models as Record<string, unknown>[]) {
		if (!m || typeof m.provider !== "string" || typeof m.id !== "string") continue;
		const thinking = m.thinking as { efforts?: unknown } | undefined;
		models.push({
			provider: m.provider,
			id: m.id,
			name: typeof m.name === "string" && m.name ? m.name : m.id,
			reasoning: m.reasoning !== false,
			efforts: Array.isArray(thinking?.efforts)
				? thinking.efforts.filter((e): e is string => typeof e === "string")
				: [],
		});
	}
	return { ...response, data: { models } };
}

/** Tunnel one allow-listed RpcCommand to the session's child (lazy start). */
export async function rpcChatCommand(
	host: RpcChatHost,
	rec: SessionRecord,
	commandJson: string,
): Promise<{ ok: true; response: RpcFrame } | { ok: false; error: string }> {
	let cmd: Record<string, unknown>;
	try {
		const parsed: unknown = JSON.parse(commandJson);
		if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return { ok: false, error: "command must be an object" };
		cmd = parsed as Record<string, unknown>;
	} catch {
		return { ok: false, error: "invalid command JSON" };
	}
	const type = typeof cmd.type === "string" ? cmd.type : "";
	// Not an RpcCommand — a UI sub-protocol reply, answered against a live child only.
	if (type === "extension_ui_response") return answerUiRequest(host, rec, cmd);
	if (ALLOWED_COMMANDS[type] !== true) return { ok: false, error: `command not allowed: ${type}` };
	const entry = ensureRpcChat(host, rec);
	// A freshly (re)spawned child is still resuming its session; writing a prompt
	// now would run it in the wrong conversation.
	await entry.ready;
	// omp REJECTS a bare prompt mid-turn: streamingBehavior is required while
	// streaming. The worker owns the turn state, so the SPA never has to know.
	if (type === "prompt" && entry.streaming && cmd.streamingBehavior === undefined) cmd.streamingBehavior = "followUp";
	try {
		const response = await entry.driver.send(cmd);
		// A rejected command is invisible otherwise: the SPA toasts and the
		// worker log shows a successful round trip. Chief offender is a prompt
		// omp refuses because the turn state disagreed with entry.streaming.
		if (response.success === false) {
			log.warn("omp-rpc", "command_rejected", {
				sid: entry.sessionId, type,
				streaming: entry.streaming,
				behavior: String(cmd.streamingBehavior ?? ""),
				error: String(response.error ?? ""),
			});
		}
		// A model switch emits NO event (unlike thinking level) — its own response
		// is the only signal that the chip is now stale, and no agent turn runs to
		// trigger the agent_end refresh. Fire and forget: the caller gets the
		// command's own result, the frame follows a round trip later.
		if (response.success === true && MODEL_CHANGING_COMMANDS[type] === true) {
			void refreshStatus(host, entry).catch(() => { /* keep last known */ });
		}
		if (type === "get_available_models") return { ok: true, response: trimCatalog(response) };
		return { ok: true, response };
	} catch (err) {
		log.warn("omp-rpc", "command_failed", { sid: entry.sessionId, type, error: String(err) });
		return { ok: false, error: err instanceof Error ? err.message : String(err) };
	}
}

/** Untruncated text of a native-chat block, or null when this session has no
 *  RPC child / the block was never truncated. */
export function rpcChatFullBlock(sessionId: string, messageId: string, blockIndex: number): string | null {
	return entries.get(sessionId)?.fullText.get(messageId)?.[blockIndex] ?? null;
}

/** Re-emit the current session status for a live RPC child (no-op otherwise).
 *  Status rides pushed frames only, so a pane that mounts between two pushes
 *  would otherwise sit with a blank model chip until the next turn ended. */
export function republishRpcChatState(host: RpcChatHost, sessionId: string): void {
	const entry = entries.get(sessionId);
	if (entry) emitState(host, entry);
}

/** Kill a session's child. Called from _dropChannelState — the kill /
 *  closedByKeeper / spawn-failure / RESPAWN funnel — so a torn-down channel
 *  never leaves an `omp --mode rpc` child waiting on the 30 s reaper sweep.
 *
 *  Deliberately does NOT forget the durable mapping: _dropChannelState also
 *  runs on respawn (session-resume.ts:173), which re-creates the SAME
 *  sessionId — forgetting there would erase the conversation during boot
 *  reconcile. The true-close site (closedByKeeper) calls forgetOmpSession.
 *  Idempotent. */
export function disposeRpcChat(sessionId: string): void {
	const e = entries.get(sessionId);
	if (!e) return;
	disposeEntry(e);
	entries.delete(sessionId);
}

/** Worker shutdown: kill every child, keep every durable mapping.
 *
 *  omp exits on stdin EOF, so a hard crash self-heals — but SIGTERM runs
 *  `process.exit(0)` while the pipes are still open, and these children are
 *  plain Bun.spawn subprocesses, NOT keeper PTY channels, so nothing else
 *  reaps them. Three lines beats reasoning about the race. */
export function disposeAllRpcChats(): void {
	for (const [, e] of entries) disposeEntry(e);
	entries.clear();
}
