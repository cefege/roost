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
//   notice                       → developer text row
//   everything else              → ignored (tool_execution_update carries partial
//                                  results ToolEventBlock cannot model; the final
//                                  result arrives as a toolResult message)

import type { ChatFrame, ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
import { diag, log } from "@roost/shared";
import type { SessionRecord } from "../../session-record.ts";
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
	set_model: true, cycle_model: true, get_available_models: true,
	set_thinking_level: true, cycle_thinking_level: true,
	compact: true, set_auto_compaction: true, new_session: true, set_session_name: true,
};

/** UI methods that ask the user a question. The rest (notify/setStatus/
 *  setWidget/setTitle/cancel/open_url/editor) carry no decision to render. */
const UI_DECISION_METHODS: Record<string, true> = { confirm: true, select: true, input: true };

/** Coalesce per-token message_update remaps. Without this every token becomes
 *  a ChatFrame on the coord bus. */
const STREAM_FLUSH_MS = 60;

/** omp caps a page at 256 messages; ask for the max so a reload is few round trips. */
const HISTORY_PAGE_LIMIT = 256;

interface RpcChatEntry {
	driver: OmpRpcDriver;
	sessionId: string;
	nextMsg: number;
	lastMsgId: string;
	/** Chat message id the in-flight assistant/user message streams into. */
	curMsgId: string | null;
	/** True between agent_start and agent_end — rides every ChatFrame. */
	streaming: boolean;
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
	/** chat message id → per-block UNTRUNCATED text, for SessionsGetChatBlock.
	 *  Only populated for messages that actually got truncated. */
	fullText: Map<string, string[]>;
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
	entry.pendingMsg = null;
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
	host.sendChatFrameUpstream?.(rec.channelId, {
		sessionId: "", append: [msg], seq: rec.chat_seq, reset: false, streaming: entry.streaming,
	});
}

/** Publish a turn-state change with no message payload (agent_start/end). */
function emitState(host: RpcChatHost, entry: RpcChatEntry): void {
	const rec = host.getBySessionId(entry.sessionId);
	if (!rec) return;
	host.sendChatFrameUpstream?.(rec.channelId, {
		sessionId: "", append: [], seq: rec.chat_seq, reset: false, streaming: entry.streaming,
	});
}

/** Untruncated text of one block, mirroring parse.ts::fullBlockText's switch. */
function blockFullText(b: ContentBlock): string {
	switch (b.kind) {
		case "text":
		case "thinking":
		case "toolResult":
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
	const truncated = msg.blocks.some((b) => (b.kind === "thinking" || b.kind === "toolResult") && b.truncated);
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

function onEvent(host: RpcChatHost, entry: RpcChatEntry, frame: RpcFrame): void {
	switch (frame.type) {
		case "agent_start":
			entry.streaming = true;
			emitState(host, entry);
			return;

		case "agent_end":
			flushPending(host, entry);
			entry.streaming = false;
			entry.curMsgId = null;
			emitState(host, entry);
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
		case "tool_execution_end": {
			const callId = typeof frame.toolCallId === "string" ? frame.toolCallId : "";
			if (!callId) return;
			flushPending(host, entry);
			let id = entry.toolMsgIds.get(callId);
			if (id === undefined) { id = `rpc-${entry.nextMsg++}`; entry.toolMsgIds.set(callId, id); }
			const phase = frame.type === "tool_execution_start" ? "start" : "end";
			if (phase === "end") entry.toolMsgIds.delete(callId);
			upsertMessage(host, entry, {
				id, parentId: entry.lastMsgId, ts: new Date().toISOString(), role: "assistant",
				blocks: [{
					kind: "toolEvent", callId,
					name: typeof frame.toolName === "string" ? frame.toolName : "",
					phase,
					intent: typeof frame.intent === "string" ? frame.intent : "",
				}],
			});
			return;
		}

		case "extension_ui_request": {
			const requestId = typeof frame.id === "string" ? frame.id : "";
			const method = typeof frame.method === "string" ? frame.method : "";
			if (!requestId || UI_DECISION_METHODS[method] !== true) return;
			flushPending(host, entry);
			const id = `rpc-${entry.nextMsg++}`;
			entry.pendingUi.set(requestId, id);
			upsertMessage(host, entry, {
				id, parentId: entry.lastMsgId, ts: new Date().toISOString(), role: "developer",
				blocks: [{
					kind: "approval", requestId, method,
					title: typeof frame.title === "string" ? frame.title : "",
					message: typeof frame.message === "string" ? frame.message
						: typeof frame.placeholder === "string" ? frame.placeholder : "",
					options: Array.isArray(frame.options) ? frame.options.filter((o): o is string => typeof o === "string") : [],
					resolved: false, answer: "",
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

		default:
			return;
	}
}

/** Resolve the child's session file (get_state) and, on a RESPAWN, resume the
 *  prior conversation: switch_session then re-seed history from the paged
 *  endpoint. Paging (not get_messages) because a v1 physical frame caps at
 *  1 MiB and this driver has no rpc_chunk reassembly. */
async function initSession(host: RpcChatHost, entry: RpcChatEntry, priorFile: string | null): Promise<void> {
	try {
		if (priorFile) {
			const res = await entry.driver.send({ type: "switch_session", sessionPath: priorFile });
			if (res.success === true) await reloadHistory(host, entry);
			else log.warn("omp-rpc", "switch_session_failed", { sid: entry.sessionId, error: String(res.error ?? "") });
		}
		const state = await entry.driver.send({ type: "get_state" });
		const data = state.data as { sessionFile?: unknown } | undefined;
		const file = data && typeof data.sessionFile === "string" ? data.sessionFile : null;
		if (!file) return;
		entry.sessionFile = file;
		// Durable, so a worker RESTART resumes this conversation too — the
		// in-memory entry only covers a child that died under a live worker.
		saveOmpSessionFile(entry.sessionId, file);
		const rec = host.getBySessionId(entry.sessionId);
		if (rec) rec.chatTranscriptPath = file;
	} catch (err) {
		log.warn("omp-rpc", "init_session_failed", { sid: entry.sessionId, error: String(err) });
	}
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
	host.sendChatFrameUpstream?.(rec.channelId, {
		sessionId: "", append: [], seq: rec.chat_seq, reset: true, streaming: entry.streaming,
	});
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
		sessionFile: priorFile, pendingUi: new Map(), toolMsgIds: new Map(), fullText: new Map(),
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
	host.sendChatFrameUpstream?.(rec.channelId, { sessionId: "", append: [], seq: rec.chat_seq, reset: true, streaming: false });
	entry.driver.start();
	// initSession never rejects (it logs); commands await this, they never fail on it.
	entry.ready = initSession(host, entry, priorFile);
	ensureReaper(host);
	diag("chat.rpc_start", { sid, cwd: rec.cwd, resumed: priorFile !== null });
	return entry;
}

/** Answer an omp extension_ui_request from the chat pane. Bypasses send():
 *  UI responses get no correlated `response` frame back. */
function answerUiRequest(
	host: RpcChatHost, rec: SessionRecord, cmd: Record<string, unknown>,
): { ok: true; response: RpcFrame } | { ok: false; error: string } {
	const requestId = typeof cmd.id === "string" ? cmd.id : "";
	const entry = entries.get(String(rec.sessionId));
	const msgId = requestId ? entry?.pendingUi.get(requestId) : undefined;
	if (!entry || msgId === undefined) return { ok: false, error: "unknown ui request" };
	try { entry.driver.post(cmd); }
	catch (err) { return { ok: false, error: err instanceof Error ? err.message : String(err) }; }
	entry.pendingUi.delete(requestId);

	const answer = cmd.cancelled === true ? "cancelled"
		: typeof cmd.value === "string" ? cmd.value
		: cmd.confirmed === true ? "approved"
		: "denied";
	const prev = host.getBySessionId(entry.sessionId)?.chatMessages?.find((m) => m.id === msgId);
	const block = prev?.blocks[0];
	if (prev && block?.kind === "approval") {
		upsertMessage(host, entry, { ...prev, blocks: [{ ...block, resolved: true, answer }] });
	}
	return { ok: true, response: { type: "ack" } };
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
