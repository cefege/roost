// Native omp chat sessions — one `omp --mode rpc-ui` child per kind:"agent"
// session.
//
// The SPA's chat view drives its session process through
// SessionsChatCommand → coord → worker → this module → child stdin. The
// child's live event stream returns through the chat wire
// (rec.chatMessages + ChatFrame upstream → SPA store → pane). It has no PTY,
// terminal grid, OSC title, or sidecar.
//
// The wire is upsert-by-id: a streaming message is re-emitted under the SAME
// chat message id as it grows, and both rec.chatMessages and the SPA store
// replace in place. Ids are synthetic (`rpc-N`) because AgentEvent messages
// carry no transcript entry id — see event-project.ts's fullText for the
// consequence.
//
// Event mapping lives in event-project.ts — ONE switch shared with the live
// sidecar engine, so a narration row is worded the same whichever saw it. This
// module keeps only what is RPC-specific:
//   extension_ui_request  → an approval block the pane answers inline
//                           (select/confirm/input/editor); `cancel` retires a
//                           card omp gave up on
//   command_output        → local slash-command output, fenced
//   thinking_level_changed→ status refresh
// plus the trailing-timer coalescing the projector deliberately does not own.

import type { ChatFrame, ChatMessage } from "@roost/shared/chat/wire";
import { existsSync } from "node:fs";
import { diag, log } from "@roost/shared";
import type { AgentState } from "@roost/shared";
import type { SessionRecord } from "../../session-record.ts";
import {
	askQuestionMatches, buildAskChoices, parseAskSpec, splitSelectTitle, type AskQuestionSpec,
} from "./ask-spec.ts";
import { dropChatMessage, upsertChatMessage } from "./chat-record.ts";
import {
	mapAndRecord, newProjectState, nextId, projectEvent, stripAnsi, type ProjectState,
} from "./event-project.ts";
import { OmpRpcDriver, type RpcFrame } from "./rpc-driver.ts";
import { loadOmpSessionFile, saveOmpSessionFile, forgetOmpSession } from "./session-store.ts";

/** Narrow view of SessionManager — keeps this module decoupled/testable. */
export interface RpcChatHost {
	getBySessionId(sessionId: string): SessionRecord | undefined;
	sendChatFrameUpstream: ((channelId: number, frame: ChatFrame) => void) | null;
	/** Publish an AgentState delta (the sidebar chip). For kind:"agent" sessions
	 *  this child is the ONLY status source — they have no grid for detect/ to
	 *  scrape. `AgentState.kind` is deliberately absent from every patch: the
	 *  fold derives it from Session.kind (event.ts), so putting it on the wire
	 *  would be a second source of truth that can disagree. */
	applyAgentPatch(p: { sessionId: string; patch: Partial<AgentState> }): void;
	/** The RPC child of a kind:"agent" session exited. That session has no PTY
	 *  exit to piggyback on, so its `closed` SessionEvent has to come from here.
	 *  No-op for any other kind. */
	closeAgentSession(sessionId: string, exitCode: number | null): void;
}

/** Commands the SPA may tunnel to the child (omp RpcCommand grammar subset). */
const ALLOWED_COMMANDS: Record<string, true> = {
	prompt: true, steer: true, follow_up: true, abort: true, abort_and_prompt: true,
	get_state: true, get_messages: true, get_session_stats: true,
	set_model: true, cycle_model: true, get_available_models: true, get_available_commands: true,
	set_thinking_level: true, cycle_thinking_level: true,
	compact: true, set_auto_compaction: true, new_session: true, set_session_name: true,
	// Subagent surface. The worker subscribes itself (initSession) so cards
	// arrive unasked; these three let a drill-in UI land without a worker change.
	set_subagent_subscription: true, get_subagents: true, get_subagent_messages: true,
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

/** A turn whose child goes quiet mid-MESSAGE is reaped rather than left
 * spinning forever. */
const RPC_STALL_MS = 30_000;

/** omp caps a page at 256 messages; ask for the max so a reload is few round trips. */
const HISTORY_PAGE_LIMIT = 256;

/** Shared empty tick set for select frames with no toggle history. */
const EMPTY_CHECKED: ReadonlySet<string> = new Set();

interface RpcChatEntry {
	driver: OmpRpcDriver;
	sessionId: string;
	/** Id minting + per-turn projection state, shared with the live engine's
	 *  projector. Ids are `rpc-N` here. */
	st: ProjectState;
	/** True between agent_start and agent_end — rides every ChatFrame. */
	streaming: boolean;
	/** Session status the omp TUI keeps on screen. Refreshed from get_state at
	 *  boot and after every turn, and carried on every frame. */
	model: string;
	contextWindow: number;
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
	/** toolCallId → newest un-emitted live output message. Coalesced on a
	 *  trailing timer: tool_execution_update fires as fast as the tool writes,
	 *  and a leading-edge drop would strand the last line before a long quiet
	 *  stretch — exactly the compile-then-wait case live output is for. */
	toolPending: Map<string, ChatMessage>;
	toolTimer: ReturnType<typeof setTimeout> | null;
	/** Trailing-timer coalescing state for message_update. */
	pendingMsg: ChatMessage | null;
	flushTimer: ReturnType<typeof setTimeout> | null;
	/** Armed while a turn is in flight, rearmed by every inbound frame. Fires
	 *  when the child has gone silent mid-turn — see stallOut. */
	stallTimer: ReturnType<typeof setTimeout> | null;
	/** Newest `available_commands_update` payload. omp PUSHES the slash-command
	 *  catalog at boot and on every change (plugin reload, session switch); the
	 *  SPA only ever ASKS, on mount. Caching the push is what keeps the palette
	 *  from going stale without adding a frame the pane would have to handle. */
	commands: unknown[] | null;
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
	if (entry.stallTimer) { clearTimeout(entry.stallTimer); entry.stallTimer = null; }
	entry.pendingMsg = null;
	entry.toolPending.clear();
	entry.askSpecs.clear();
	entry.askChecked.clear();
	entry.askCardIds.clear();
	entry.activeAsk = null;
	entry.driver.dispose();
}

/** Append-or-replace by id, then publish the row. rec.chat_seq still advances
 *  so the SPA orders frames; the record's own slot bookkeeping is shared with
 *  the transcript and live engines (chat-record.ts). */
function upsertMessage(host: RpcChatHost, entry: RpcChatEntry, msg: ChatMessage): void {
	const rec = host.getBySessionId(entry.sessionId);
	if (!rec) return;
	rec.chat_seq += 1;
	upsertChatMessage(rec, msg, rec.chat_seq);
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
		contextTokens: entry.contextTokens,
		contextWindow: entry.contextWindow,
		// omp's RPC get_state carries no agent mode (verified against
		// rpc-mode.ts's RpcSessionState), so none is reported. The pane omits an
		// absent chip rather than faking one.
		mode: "",
	};
}

/** Publish a state change with no message payload (agent_start/end, status). */
function emitState(host: RpcChatHost, entry: RpcChatEntry): void {
	const rec = host.getBySessionId(entry.sessionId);
	if (!rec) return;
	host.sendChatFrameUpstream?.(rec.channelId, frameFor(entry, rec.chat_seq, [], false));
}

/** Publish an AgentState delta for a kind:"agent" session; no-op otherwise.
 *  Terminal-mode sessions take their status from the screen scrape (detect/),
 *  which cannot see a PTY-less child — and two writers on one chip flap. */
function patchAgent(host: RpcChatHost, entry: RpcChatEntry, patch: Partial<AgentState>): void {
	if (host.getBySessionId(entry.sessionId)?.kind !== "agent") return;
	host.applyAgentPatch({ sessionId: entry.sessionId, patch });
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

/** The child went silent while an assistant message was mid-stream. Only child
 * EXIT is insufficient: a wedged-but-alive omp would leave the streamed row
 * spinning forever with no way back short of reload.
 *
 * `curMsgId !== null` is load-bearing: it means a row is literally
 * half-painted. A turn that is merely OPEN is not enough — a quiet
 *  tool (a long build writing nothing) emits no frames for minutes, and
 *  reaping that would paint "agent stopped responding" over a healthy turn.
 *
 *  The child is deliberately NOT killed — mirror does not kill either, it may
 *  still recover, and the next command re-arms everything anyway. */
function stallOut(host: RpcChatHost, entry: RpcChatEntry): void {
	entry.stallTimer = null;
	if (!entry.streaming || entry.st.curMsgId === null) return;
	log.warn("omp-rpc", "turn_stalled", { sid: entry.sessionId });
	diag("chat.rpc_stall", { sid: entry.sessionId });
	flushPending(host, entry);
	flushToolUpdates(host, entry);
	entry.streaming = false;
	patchAgent(host, entry, { status: "idle", current_tool: null });
	// The half-painted row is finished as far as the pane is concerned. Leaving
	// curMsgId set would make a late frame from a recovered child stream into a
	// row that now sits ABOVE the stall notice.
	entry.st.curMsgId = null;
	upsertMessage(host, entry, {
		id: nextId(entry.st), parentId: entry.st.lastMsgId, ts: new Date().toISOString(),
		role: "developer", synthetic: false,
		blocks: [{ kind: "notice", text: "agent stopped responding", level: "error" }],
	});
}

/** Rearm the stall clock. Called on EVERY inbound frame: any traffic at all
 *  proves the child is alive.
 *
 *  The interval is read HERE, not captured at module load: a slow provider can
 *  legitimately outlast the default, and a 30 s invariant is otherwise
 *  untestable — an import is hoisted above any env assignment beside it. */
function armStall(host: RpcChatHost, entry: RpcChatEntry): void {
	const ms = Number(process.env.ROOST_RPC_STALL_MS ?? "") || RPC_STALL_MS;
	clearTimeout(entry.stallTimer ?? undefined);
	entry.stallTimer = setTimeout(() => { stallOut(host, entry); }, ms);
}

/** Stop the stall clock — the turn is over, or the entry is going away. */
function clearStall(entry: RpcChatEntry): void {
	clearTimeout(entry.stallTimer ?? undefined);
	entry.stallTimer = null;
}

/** RPC-only frames, then the shared projection. The two halves are disjoint:
 *  nothing below is in event-project.ts's switch, and nothing in it is here. */
function onEvent(host: RpcChatHost, entry: RpcChatEntry, frame: RpcFrame): void {
	// Any frame at all proves the child is alive; a long tool call is not a
	// stall. Armed unconditionally, disarmed by agent_end / dispose.
	armStall(host, entry);
	switch (frame.type) {
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
			const id = reuse ?? nextId(entry.st);
			if (matched && reuse === undefined) entry.askCardIds.set(key, id);
			entry.pendingUi.set(requestId, id);
			patchAgent(host, entry, { status: "needs-input" });
			upsertMessage(host, entry, {
				id, parentId: entry.st.lastMsgId, ts: new Date().toISOString(), role: "developer",
				synthetic: false,
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

		// Local slash commands (/model, /context, /cost …) answer HERE, not via
		// an agent turn — no message_* frames at all. Dropping this is why a
		// slash command in the chat pane looked like it did nothing. Prose, not
		// a notice row: it is the command's own output, fenced so the box-drawn
		// tables survive the markdown pass.
		case "command_output": {
			const text = typeof frame.text === "string" ? stripAnsi(frame.text) : "";
			if (text.trim().length === 0) return;
			flushPending(host, entry);
			upsertMessage(host, entry, developerRow(entry, preformatted(text)));
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

		// omp PUSHES the slash-command catalog: once at boot and again whenever
		// command metadata changes (plugin reload, session switch). The SPA only
		// PULLS, on ChatWelcome mount — so without this the palette shows
		// whatever the very first get_available_commands happened to return.
		// Cached, not pushed onward: the list is read on mount and on menu open,
		// and a new wire frame for that is more surface than the problem.
		case "available_commands_update":
			entry.commands = Array.isArray(frame.commands) ? frame.commands : null;
			return;

		default:
			break;
	}

	// The ask tool's rich question data — descriptions, header, multi — exists
	// ONLY on the tool frames: omp's RPC select frame flattens every option to a
	// bare label. Stash it BEFORE projecting, so the select frames that already
	// arrived can be correlated back into a real selection card.
	trackAskSpec(host, entry, frame);

	const out = projectEvent(entry.st, frame);
	if (!out) return;
	switch (out.kind) {
		case "streaming":
			if (!out.value) {
				// agent_end: land everything queued before the turn reads idle,
				// and stop the stall clock — an idle child is not a frozen one.
				flushPending(host, entry);
				flushToolUpdates(host, entry);
				clearStall(entry);
			}
			entry.streaming = out.value;
			patchAgent(host, entry, out.value
				? { status: "running", current_tool: null }
				: { status: "idle", current_tool: null });
			emitState(host, entry);
			// Context grew over the turn; the TUI's status line would already
			// show the new number. Failure is silent — status is not load-bearing.
			if (!out.value) void refreshStatus(host, entry).catch(() => { /* keep last known */ });
			return;

		case "message":
			if (out.coalesce) { entry.pendingMsg = out.msg; scheduleFlush(host, entry); return; }
			// A pending update under THIS id is superseded by the frame in hand
			// (message_end); one under any other id belongs to the previous
			// message and must land before this one appears beneath it.
			if (entry.pendingMsg?.id === out.msg.id) {
				if (entry.flushTimer) { clearTimeout(entry.flushTimer); entry.flushTimer = null; }
				entry.pendingMsg = null;
			} else {
				flushPending(host, entry);
			}
			upsertMessage(host, entry, out.msg);
			return;

		case "tool":
			flushPending(host, entry);
			if (out.phase === "update") { queueToolUpdate(host, entry, out.callId, out.msg); return; }
			// start/end are turn structure, not chatter — emit immediately, and
			// drop any queued partial the terminal state supersedes.
			entry.toolPending.delete(out.callId);
			patchAgent(host, entry, out.phase === "start"
				? { current_tool: { name: typeof frame.toolName === "string" && frame.toolName ? frame.toolName : "tool", input_summary: "" } }
				: { current_tool: null });
			upsertMessage(host, entry, out.msg);
			return;

		case "narrate":
			upsertMessage(host, entry, out.msg);
			return;

		case "drop": {
			// The turn rendered mid-flight but ends as nothing the TUI paints (a
			// silent abort). Kill any coalesced update for the SAME id first —
			// the trailing flush would otherwise resurrect the row we just
			// removed — then retract it and reseed, the wire having no delete.
			if (entry.pendingMsg?.id === out.id) {
				if (entry.flushTimer) { clearTimeout(entry.flushTimer); entry.flushTimer = null; }
				entry.pendingMsg = null;
			}
			const rec = host.getBySessionId(entry.sessionId);
			if (!rec || !dropChatMessage(rec, out.id)) return;
			host.sendChatFrameUpstream?.(rec.channelId, frameFor(entry, rec.chat_seq, rec.chatMessages ?? [], true));
			return;
		}
	}
}

/** Capture / release the ask tool's question spec off the tool lifecycle. */
function trackAskSpec(host: RpcChatHost, entry: RpcChatEntry, frame: RpcFrame): void {
	const callId = typeof frame.toolCallId === "string" ? frame.toolCallId : "";
	if (!callId) return;
	if (frame.type === "tool_execution_start" && frame.toolName === "ask") {
		const spec = parseAskSpec(frame.args);
		if (spec.length === 0) return;
		entry.askSpecs.set(callId, spec);
		entry.activeAsk = callId;
		enrichPendingAsk(host, entry);
		return;
	}
	if (frame.type !== "tool_execution_end" || !entry.askSpecs.delete(callId)) return;
	if (entry.activeAsk === callId) entry.activeAsk = null;
	const prefix = `${callId}:`;
	for (const key of [...entry.askChecked.keys()]) if (key.startsWith(prefix)) entry.askChecked.delete(key);
	for (const key of [...entry.askCardIds.keys()]) if (key.startsWith(prefix)) entry.askCardIds.delete(key);
}

/** A plain prose row from the worker itself (command output, process exit,
 *  incomplete reload) — NOT a projected omp narration row, which is a `notice`. */
function developerRow(entry: RpcChatEntry, text: string): ChatMessage {
	return {
		id: nextId(entry.st), parentId: entry.st.lastMsgId, ts: new Date().toISOString(),
		role: "developer", synthetic: false, blocks: [{ kind: "text", text }],
	};
}

/** Resolve the child's session file (get_state) and, on a RESPAWN, resume the
 *  prior conversation: switch_session then re-seed history from the paged
 *  endpoint. Paging (not get_messages) because a physical frame caps at 1 MiB;
 *  chunk reassembly covers a big RESPONSE, but paging keeps each one small. */
async function initSession(
	host: RpcChatHost, entry: RpcChatEntry, priorFile: string | null, launchedWithSession: boolean,
): Promise<void> {
	try {
		// Two resume routes, never both: `--session FILE` at launch (Step 7's
		// transcript picker) puts the history on the child before it speaks, so
		// only the pane needs seeding; switch_session is the respawn route for a
		// child that died under a live worker.
		let seedHistory = launchedWithSession;
		if (priorFile) {
			const res = await entry.driver.send({ type: "switch_session", sessionPath: priorFile });
			if (res.success === true) seedHistory = true;
			else log.warn("omp-rpc", "switch_session_failed", { sid: entry.sessionId, error: String(res.error ?? "") });
		}
		if (seedHistory) await reloadHistory(host, entry);
		// omp defaults subagent delivery to "off" (rpc-subagents.ts), so without
		// this opt-in the pane is BLIND to every subagent a turn spawns while the
		// terminal shows their work. "progress", not "events": progress carries
		// lifecycle + status, which is all a card renders; "events" replays every
		// subagent message and multiplies frame volume for nothing on screen.
		//
		// NOT awaited. Every command waits on `ready`, and a cold child answers
		// its first command ~16 s in — putting another round trip in front of the
		// user's first prompt pushes chat-command past its 35 s deadline. The
		// subscription only has to be in place before a subagent SPAWNS, which is
		// mid-turn at the earliest. Logged on refusal: a silent one is
		// indistinguishable from "no subagents ran".
		void entry.driver.send({ type: "set_subagent_subscription", level: "progress" })
			.then((sub) => {
				if (sub.success !== true) {
					log.warn("omp-rpc", "subagent_subscribe_failed", { sid: entry.sessionId, error: String(sub.error ?? "") });
				}
			})
			.catch((err: unknown) => {
				log.warn("omp-rpc", "subagent_subscribe_failed", { sid: entry.sessionId, error: String(err) });
			});
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
		contextUsage?: { tokens?: unknown; contextWindow?: unknown };
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
	// The percentage is derived client-side off tokens/window, so get_state's
	// own `percent` is redundant — the window is what the wire carries.
	entry.contextWindow = typeof usage?.contextWindow === "number" && usage.contextWindow > 0
		? Math.round(usage.contextWindow)
		: 0;
	// The chip's model + context numbers. Tokens are pull-only in omp — no event
	// carries them — so this is the single place they can come from.
	patchAgent(host, entry, {
		model: entry.modelName || entry.model,
		tokens: { in: entry.contextTokens, out: 0, cached: 0 },
	});
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
			const id = nextId(entry.st);
			const msg = mapAndRecord(entry.st, raw, id, entry.st.lastMsgId);
			if (!msg) continue;
			entry.st.lastMsgId = id;
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
		upsertMessage(host, entry, developerRow(entry, "— history reload incomplete —"));
	}
}

export interface EnsureRpcChatOpts {
	/** Absolute path to an existing omp *.jsonl to resume, applied as
	 *  `--session FILE` on a FRESH child. Ignored when this session already has
	 *  a conversation of its own (that resumes via switch_session instead).
	 *  A path that cannot be read starts a fresh conversation and says so. */
	resumeSessionFile?: string;
	/** omp model id; empty = omp's own default. argv-time only. */
	model?: string;
}

/** Start the RPC child for a session (idempotent). Emits the reset frame so the
 *  SPA reseeds. Called eagerly by spawnAgent for kind:"agent" sessions, and
 *  lazily on the first chat command for everything else. */
export function ensureRpcChat(host: RpcChatHost, rec: SessionRecord, opts?: EnsureRpcChatOpts): RpcChatEntry {
	const sid = String(rec.sessionId);
	const existing = entries.get(sid);
	if (existing?.driver.alive) return existing;
	// The session file is the only way back to a conversation. In-memory entry
	// = child died under a live worker; the durable store = the worker itself
	// restarted, so there is no entry at all.
	const priorFile = existing?.sessionFile ?? loadOmpSessionFile(sid);
	if (existing) disposeEntry(existing);

	// Resume-by-path is a one-shot read at spawn, not a live coupling: the file
	// is checked once, here, and a stale bookmark must never block making a
	// session — it degrades to a fresh conversation plus one notice row.
	const wantResume = !priorFile && (opts?.resumeSessionFile ?? "") !== "";
	const resumePath = wantResume ? opts!.resumeSessionFile! : "";
	const resumeReadable = resumePath !== "" && existsSync(resumePath);
	const args: string[] = [];
	if (opts?.model) args.push("--model", opts.model);
	if (resumeReadable) args.push("--session", resumePath);

	const entry: RpcChatEntry = {
		sessionId: sid, st: newProjectState("rpc"), streaming: false,
		model: "", modelName: "", thinkingLevel: "",
		contextWindow: 0, contextTokens: 0,
		sessionFile: priorFile, pendingUi: new Map(),
		askSpecs: new Map(), activeAsk: null, askChecked: new Map(), askCardIds: new Map(),
		toolPending: new Map(), toolTimer: null,
		pendingMsg: null, flushTimer: null, stallTimer: null, commands: null,
		ready: Promise.resolve(),
		driver: null as unknown as OmpRpcDriver,
	};
	entry.driver = new OmpRpcDriver({
		cwd: rec.cwd,
		args,
		onEvent: (frame) => onEvent(host, entry, frame),
		onExit: (code) => {
			diag("chat.rpc_exit", { sid });
			if (entry.flushTimer) { clearTimeout(entry.flushTimer); entry.flushTimer = null; }
			// The exit row IS the terminal state; a stall notice on top of it
			// would say the same thing twice, 30 s later.
			clearStall(entry);
			entry.streaming = false;
			upsertMessage(host, entry, developerRow(entry, "— agent process exited —"));
			patchAgent(host, entry, { status: "done", current_tool: null });
			// For an agent session the child IS the session, so its exit closes
			// the row. For a terminal-mode session the child is a side process:
			// keep the entry, its sessionFile resumes on the next command.
			host.closeAgentSession(sid, code);
		},
	});
	entries.set(sid, entry);
	rec.chatMessages ??= [];
	rec.chatMsgSeqs ??= [];
	host.sendChatFrameUpstream?.(rec.channelId, frameFor(entry, rec.chat_seq, [], true));
	entry.driver.start();
	if (resumePath !== "" && !resumeReadable) {
		upsertMessage(host, entry, developerRow(entry,
			`— could not read ${resumePath}; started a fresh conversation —`));
	}
	// initSession never rejects (it logs); commands await this, they never fail on it.
	entry.ready = initSession(host, entry, priorFile, resumeReadable);
	ensureReaper(host);
	diag("chat.rpc_start", { sid, cwd: rec.cwd, resumed: priorFile !== null || resumeReadable });
	return entry;
}

/** Mark a pending approval card answered and stop tracking its request id.
 *  Shared by the pane's own reply and by omp withdrawing the question. */
function retireUiRequest(host: RpcChatHost, entry: RpcChatEntry, requestId: string, answer: string): void {
	const msgId = requestId ? entry.pendingUi.get(requestId) : undefined;
	if (msgId === undefined) return;
	entry.pendingUi.delete(requestId);
	// The chip leaves needs-input only once NOTHING is still asking — omp can
	// have two questions open (a batched `ask`), and clearing on the first
	// answer would say "running" while the pane still shows a live card.
	if (entry.pendingUi.size === 0) {
		patchAgent(host, entry, { status: entry.streaming ? "running" : "idle" });
	}
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
		// omp pushed a newer catalog than any reply we could ask for, and a
		// refused/failed pull would otherwise blank the palette. The live answer
		// still wins — the push is the floor, not the ceiling.
		if (type === "get_available_commands" && response.success !== true && entry.commands !== null) {
			return { ok: true, response: { type: "response", success: true, data: { commands: entry.commands } } };
		}
		return { ok: true, response };
	} catch (err) {
		log.warn("omp-rpc", "command_failed", { sid: entry.sessionId, type, error: String(err) });
		return { ok: false, error: err instanceof Error ? err.message : String(err) };
	}
}

/** Untruncated text of a native-chat block, or null when this session has no
 *  RPC child / the block was never truncated.
 *
 *  An empty slot reads as ABSENT, matching parse.ts::fullBlockText: a block
 *  kind that carries nothing truncatable (toolEvent beside a capped thinking
 *  block, say) stores "" to keep block indices aligned, and answering an
 *  expand with blank text is worse than answering "block not found". */
export function rpcChatFullBlock(sessionId: string, messageId: string, blockIndex: number): string | null {
	const text = entries.get(sessionId)?.st.fullText.get(messageId)?.[blockIndex];
	return text ? text : null;
}

/** True when this session has an rpc entry — a live child, or a dead one whose
 *  sessionFile still resumes the conversation. The parity oracle needs it to
 *  tell "rpc engine, no terminal column" from "mirror engine, bridge missing",
 *  and `entries` is private to this module. */
export function rpcChatActive(sessionId: string): boolean {
	return entries.has(sessionId);
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
