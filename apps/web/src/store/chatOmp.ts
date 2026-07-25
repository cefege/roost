// Omp chat store slice — self-contained logic + selectors for the omp
// transcript-reader chat. State mounts on rootStore.chat_omp (so sync.ts's
// single reactive flush covers chat frames); this module owns the projector,
// the backfill RPC, and the eligibility selector.
//
// No shared chat components — a future Claude/pi chat is a sibling module that
// shares only the wire ChatMessage type. Mirrors the scrollback backfill model:
// reset → reseed, else splice append by seq (dedup by message id).

import { rootStore, setRootStore, type ChatOmpState } from "./root.ts";
import { reconcile } from "solid-js/store";
import { isChatFolder } from "../lib/quickChat.ts";
import { chatFrameFromProto, chatMessageFromProto, type ChatFrame, type ChatMessage } from "@roost/shared/chat/wire";
import { coordClient } from "../connect.ts";
import { asSessionId } from "@roost/shared/wire";
import type { ChatFrame as PbChatFrame, ChatMessage as PbChatMessage } from "@roost/shared/proto/sync_pb";
import { diag, signal } from "@roost/shared/diag";

// Cap in-memory transcript tail per session. Matches the 2000-row scrollback
// cap philosophy: the chat reader pages older history via backfillOmpChat, so
// trimming the in-memory tail is recoverable. Without this, chat_omp[sid].messages
// is append-only and grows unbounded on a long-lived π session.
const MAX_CHAT_MSGS = 2000;

/** omp identity on the SPA side (chat toggle gate). A PURE read: the latch is
 *  written once, where the title lands (sync.ts). Reading terminal_title here
 *  would re-run every consumer on all ~12.5 spinner frames a working pane
 *  emits per second, for a value that cannot change. */
export function ompChatEnabled(sessionId: string): boolean {
	// Native quick-chats (scratch folder under ~/.roost/chats) are chat-eligible
	// by construction — the engine is the worker's `omp --mode rpc` child, no
	// TUI/title involved. Terminal omp sessions latch off their OSC title.
	const cwd = rootStore.sessions[sessionId]?.cwd;
	if (cwd && isChatFolder(cwd)) return true;
	return rootStore.omp_eligible[sessionId] === true;
}

/** Current chat state for a session (creates an empty slot lazily). */
export function ompChatForSession(sessionId: string): ChatOmpState {
	return rootStore.chat_omp[sessionId] ?? { messages: [], seq: 0, status: "idle", streaming: false, model: "", modelName: "", thinkingLevel: "", contextPct: 0, contextTokens: 0 };
}

/** Apply an inbound ChatFrame. reset → replace; else UPSERT by message id:
 *  a streaming message is re-emitted under the same id as it grows, so a
 *  known id replaces in place and only unknown ids append. */
export function applyOmpChatFrame(pb: PbChatFrame): void {
	let frame: ChatFrame;
	try { frame = chatFrameFromProto(pb); }
	catch (e) {
		// signal(), not diag(): one unparseable message discards the WHOLE frame,
		// including any co-batched assistant text, and the pane just stops
		// growing. diag is a no-op without ROOST_DIAG=1, so that failure used to
		// leave no trace at all — same always-on channel the sessions/
		// sessionPresence cases in sync.ts use for corruption.
		signal("chat.frame_drop", { reason: "frame_zod", msg: String(e) });
		return;
	}
	const sid = frame.sessionId;
	if (!sid) return;
	const cur = rootStore.chat_omp[sid];
	if (frame.reset || !cur) {
		setRootStore("chat_omp", sid, {
			messages: dedup(frame.append),
			seq: frame.seq,
			status: frame.append.length > 0 ? "resolved" : "loading",
			streaming: frame.streaming,
			model: frame.model,
			modelName: frame.modelName,
			thinkingLevel: frame.thinkingLevel,
			contextPct: frame.contextPct,
			contextTokens: frame.contextTokens,
		});
		return;
	}
	// Turn state and session status ride EVERY frame, including the payload-less
	// ones the worker sends on agent_start/agent_end — apply before any early
	// return or the status line freezes at its first value.
	if (cur.streaming !== frame.streaming) setRootStore("chat_omp", sid, "streaming", frame.streaming);
	// `model` non-empty marks a frame from the native engine that has completed
	// its first get_state. Gate context on that rather than on a non-zero token
	// count, so a genuine 0 is representable and boot frames never clobber.
	if (frame.model) {
		if (cur.model !== frame.model) setRootStore("chat_omp", sid, "model", frame.model);
		if (cur.modelName !== frame.modelName) setRootStore("chat_omp", sid, "modelName", frame.modelName);
		if (cur.thinkingLevel !== frame.thinkingLevel) setRootStore("chat_omp", sid, "thinkingLevel", frame.thinkingLevel);
		if (cur.contextPct !== frame.contextPct) setRootStore("chat_omp", sid, "contextPct", frame.contextPct);
		if (cur.contextTokens !== frame.contextTokens) setRootStore("chat_omp", sid, "contextTokens", frame.contextTokens);
	}
	if (frame.append.length === 0 && cur.seq >= frame.seq) {
		// Already current — just bump status if we were loading.
		if (cur.status === "loading") setRootStore("chat_omp", sid, "status", "resolved");
		return;
	}
	const merged = cur.messages.slice();
	const at = new Map(merged.map((m, i) => [m.id, i]));
	for (const m of frame.append) {
		const i = at.get(m.id);
		if (i === undefined) { at.set(m.id, merged.length); merged.push(m); }
		else merged[i] = m;
	}
	const trimmed = merged.length > MAX_CHAT_MSGS ? merged.slice(-MAX_CHAT_MSGS) : merged;
	// reconcile, NOT a plain array set: a streaming message is re-emitted as a
	// NEW object under the SAME id every ~60ms, and `<For each={messages}>` keys
	// by reference — a bare set therefore unmounts and rebuilds the whole bubble
	// (markdown re-rendered from scratch, selection and scroll anchor lost) on
	// every token batch. That thrash is what made live output look like it
	// arrived in one lump at the end. Keyed reconcile patches the changed row's
	// fields in place, so only the text node updates.
	setRootStore("chat_omp", sid, "messages", reconcile(trimmed, { key: "id" }));
	setRootStore("chat_omp", sid, {
		seq: Math.max(cur.seq, frame.seq),
		status: "resolved",
	});
}

/** Keep the LAST occurrence of each id — a reset batch can carry the same
 *  message twice (growing), and the newest copy is the complete one. */
function dedup(msgs: ChatMessage[]): ChatMessage[] {
	const at = new Map<string, number>();
	const out: ChatMessage[] = [];
	for (const m of msgs) {
		const i = at.get(m.id);
		if (i === undefined) { at.set(m.id, out.length); out.push(m); }
		else out[i] = m;
	}
	return out;
}

/** Reap a closed session's chat state. No-op when absent (setStore undefined). */
export function pruneChatOmp(sid: string): void {
	setRootStore("chat_omp", sid, undefined as unknown as ChatOmpState);
}

/** Leak-watch accumulator sizes: live session count + total held messages. */
export function chatOmpStats(): { sessions: number; msgs: number } {
	const ids = Object.keys(rootStore.chat_omp);
	let msgs = 0;
	for (const id of ids) msgs += rootStore.chat_omp[id]?.messages.length ?? 0;
	return { sessions: ids.length, msgs };
}

/** Backfill chat history on first chat-view enter / firehose reconnect.
 *  No-op when already resolved unless `force`. Marks status "loading" only when
 *  there is nothing on screen yet — a reconnect resync must never blank a
 *  transcript the user is reading. Always pulls the NEWEST page: the RPC's
 *  `after_seq` is a BACKWARD cursor (worker keeps entries with seq <= it, to
 *  page OLDER history), so there is no forward "everything since X" catch-up
 *  to lean on — a resync refetches the tail and dedups by id. */
export async function backfillOmpChat(sessionId: string, force = false): Promise<void> {
	const cur = rootStore.chat_omp[sessionId];
	if (!force && cur?.status === "resolved") return;
	if (!cur || cur.messages.length === 0) {
		setRootStore("chat_omp", sessionId, {
			messages: cur?.messages ?? [],
			seq: cur?.seq ?? 0,
			status: "loading",
			streaming: cur?.streaming ?? false,
			model: cur?.model ?? "", contextPct: cur?.contextPct ?? 0, contextTokens: cur?.contextTokens ?? 0,
		});
	}
	try {
		const res = await coordClient.sessionsGetChatHistory({
			sessionId: asSessionId(sessionId),
			maxMessages: 500,
		});
		const messages = res.messages.map(protoMsgToWire);
		const existing = rootStore.chat_omp[sessionId];
		const liveMsgs = existing?.messages ?? [];
		// History is the worker's authoritative transcript, so it OWNS the order.
		// Live frames only contribute ids history does not have yet — frames that
		// landed while this fetch was in flight, which are by definition newest.
		const known = new Set(messages.map((m) => m.id));
		const merged = [...messages, ...liveMsgs.filter((m) => !known.has(m.id))];
		setRootStore("chat_omp", sessionId, {
			messages: merged,
			seq: Math.max(existing?.seq ?? 0, Number(res.nextSeq)),
			status: "resolved",
			streaming: existing?.streaming ?? false,
			model: existing?.model ?? "", contextPct: existing?.contextPct ?? 0, contextTokens: existing?.contextTokens ?? 0,
		});
	} catch (e) {
		// On failure, mark resolved with whatever we have so the pane renders
		// instead of spinning forever; live frames will keep arriving.
		setRootStore("chat_omp", sessionId, "status", "resolved");
		diag("chat.parse_skip", { reason: "backfill_failed", msg: String(e) });
	}
}

/** Re-pull every held chat transcript after the firehose reconnects.
 *  globalChatBus is a 64-frame in-memory ring with NO reconnect replay (unlike
 *  titleBus/claudeStatusBus, which coord re-seeds), so a frame published while
 *  the socket was silently stalled — Chrome throttles a backgrounded tab's WS
 *  without surfacing an error — is gone from the live feed for good, and the
 *  transcript just stops growing mid-reply. The worker still holds the whole
 *  thing, so ask it again. Scoped to sessions we actually hold chat state for,
 *  never every session. */
export function resyncOmpChats(): void {
	for (const sid of Object.keys(rootStore.chat_omp)) void backfillOmpChat(sid, true);
}

/** Fetch the full untruncated text of one ContentBlock (thinking/tool_result). */
export async function fetchChatBlock(sessionId: string, messageId: string, blockIndex: number): Promise<string | null> {
	try {
		const res = await coordClient.sessionsGetChatBlock({
			sessionId: asSessionId(sessionId),
			messageId,
			blockIndex,
		});
		return res.text;
	} catch {
		return null;
	}
}

// Connect returns proto-typed values; the wire ChatMessage is the in-app shape.
function protoMsgToWire(m: PbChatMessage): ChatMessage {
	return chatMessageFromProto(m);
}
