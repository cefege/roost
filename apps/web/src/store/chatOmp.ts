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

/** Does this session render the chat pane? Web-UI mode is a SESSION KIND, not a
 *  property of what happens to be running in a PTY: `kind:"agent"` sessions own
 *  an `omp --mode rpc-ui` child and have no terminal at all.
 * Chat eligibility is immutable session kind, not a terminal observation.
 * A shell remains a terminal even if it happens to run omp. */
export function ompChatEnabled(sessionId: string): boolean {
	return rootStore.sessions[sessionId]?.kind === "agent";
}

/** Current chat state for a session (creates an empty slot lazily). */
export function ompChatForSession(sessionId: string): ChatOmpState {
	return rootStore.chat_omp[sessionId] ?? { messages: [], seq: 0, status: "idle", streaming: false, model: "", modelName: "", thinkingLevel: "", contextTokens: 0, contextWindow: 0, mode: "" };
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
		const messages = dedup(frame.append);
		// reconcile on the RESET path too, not just the append one. A reset is no
		// longer only a first seed: the worker reseeds mid-turn whenever a
		// transcript row lands ABOVE the tail-pinned live row (the wire has no
		// insert verb). A plain array set there would unmount and rebuild every
		// bubble in the middle of a stream — markdown re-parsed, selection and
		// scroll anchor lost — which is exactly the thrash the append path's
		// reconcile exists to avoid.
		//
		// Only when BOTH sides have rows: `reconcile` needs a previous value of
		// the same shape to diff against, and there is no identity to preserve
		// when either list is empty (a first seed, or the empty seed reset the
		// worker sends on bind).
		const patch = cur !== undefined && messages.length > 0 && cur.messages.length > 0;
		if (patch) setRootStore("chat_omp", sid, "messages", reconcile(messages, { key: "id" }));
		setRootStore("chat_omp", sid, {
			...(patch ? {} : { messages }),
			seq: frame.seq,
			// A seed reset (empty append, seq 0) is the worker saying "reseed",
			// not "no conversation". Demoting an already-resolved pane to
			// "loading" hid every row behind the skeleton until the next
			// non-empty frame — which, on an idle session, never comes.
			status: frame.append.length > 0 ? "resolved" : (cur?.status ?? "loading"),
			streaming: frame.streaming,
			model: frame.model,
			modelName: frame.modelName,
			thinkingLevel: frame.thinkingLevel,
			contextTokens: frame.contextTokens,
			contextWindow: frame.contextWindow,
			mode: frame.mode,
		});
		return;
	}
	// Turn state and session status ride EVERY frame, including the payload-less
	// ones the worker sends on agent_start/agent_end — apply before any early
	// return or the status line freezes at its first value.
	if (cur.streaming !== frame.streaming) setRootStore("chat_omp", sid, "streaming", frame.streaming);
	// Session status rides every frame, payload-less ones included.
	if (cur.model !== frame.model) setRootStore("chat_omp", sid, "model", frame.model);
	if (cur.modelName !== frame.modelName) setRootStore("chat_omp", sid, "modelName", frame.modelName);
	if (cur.thinkingLevel !== frame.thinkingLevel) setRootStore("chat_omp", sid, "thinkingLevel", frame.thinkingLevel);
	if (cur.contextTokens !== frame.contextTokens) setRootStore("chat_omp", sid, "contextTokens", frame.contextTokens);
	if (cur.contextWindow !== frame.contextWindow) setRootStore("chat_omp", sid, "contextWindow", frame.contextWindow);
	if (cur.mode !== frame.mode) setRootStore("chat_omp", sid, "mode", frame.mode);
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
			model: cur?.model ?? "", modelName: cur?.modelName ?? "", thinkingLevel: cur?.thinkingLevel ?? "",
			mode: cur?.mode ?? "",
			contextTokens: cur?.contextTokens ?? 0, contextWindow: cur?.contextWindow ?? 0,
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
			model: existing?.model ?? "", modelName: existing?.modelName ?? "", thinkingLevel: existing?.thinkingLevel ?? "",
			mode: existing?.mode ?? "",
			contextTokens: existing?.contextTokens ?? 0, contextWindow: existing?.contextWindow ?? 0,
		});
	} catch (e) {
		// "failed", not "resolved": a dead pipeline that renders as an empty
		// resolved pane is indistinguishable from a brand-new conversation, and
		// the pane paints a friendly welcome card over a broken chat. Live frames
		// still arrive and will flip it back to resolved on their own.
		setRootStore("chat_omp", sessionId, "status", "failed");
		diag("chat.parse_skip", { reason: "backfill_failed", msg: String(e) });
		signal("chat.backfill_failed", { sid: sessionId, msg: String(e) });
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
