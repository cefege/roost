// Omp chat store slice — self-contained logic + selectors for the omp
// transcript-reader chat. State mounts on rootStore.chat_omp (so sync.ts's
// single reactive flush covers chat frames); this module owns the projector,
// the backfill RPC, and the eligibility selector.
//
// No shared chat components — a future Claude/pi chat is a sibling module that
// shares only the wire ChatMessage type. Mirrors the scrollback backfill model:
// reset → reseed, else splice append by seq (dedup by message id).

import { rootStore, setRootStore, type ChatOmpState } from "./root.ts";
import { isChatFolder } from "../lib/quickChat.ts";
import { chatFrameFromProto, chatMessageFromProto, type ChatFrame, type ChatMessage } from "@roost/shared/chat/wire";
import { coordClient } from "../connect.ts";
import { asSessionId } from "@roost/shared/wire";
import type { ChatFrame as PbChatFrame, ChatMessage as PbChatMessage } from "@roost/shared/proto/sync_pb";
import { diag } from "@roost/shared/diag";

// Cap in-memory transcript tail per session. Matches the 2000-row scrollback
// cap philosophy: the chat reader pages older history via backfillOmpChat, so
// trimming the in-memory tail is recoverable. Without this, chat_omp[sid].messages
// is append-only and grows unbounded on a long-lived π session.
const MAX_CHAT_MSGS = 2000;

/** omp identity on the SPA side (chat toggle gate). omp emits "π > <breadcrumb>"
 *  (older builds "π: <summary>"); match those two. EXCLUDE pi's "π - <dir>" so
 *  the toggle never appears on a pi pane. Absent/other → toggle hidden. */
export function ompChatEnabled(sessionId: string): boolean {
	// Native quick-chats (scratch folder under ~/.roost/chats) are chat-eligible
	// by construction — the engine is the worker's `omp --mode rpc` child, no
	// TUI/title involved. Terminal omp sessions stay title-gated.
	const cwd = rootStore.sessions[sessionId]?.cwd;
	if (cwd && isChatFolder(cwd)) return true;
	const title = rootStore.terminal_title[sessionId];
	return !!title && (title.startsWith("\u03C0 >") || title.startsWith("\u03C0:"));
}

/** Current chat state for a session (creates an empty slot lazily). */
export function ompChatForSession(sessionId: string): ChatOmpState {
	return rootStore.chat_omp[sessionId] ?? { messages: [], seq: 0, status: "idle", streaming: false };
}

/** Apply an inbound ChatFrame. reset → replace; else UPSERT by message id:
 *  a streaming message is re-emitted under the same id as it grows, so a
 *  known id replaces in place and only unknown ids append. */
export function applyOmpChatFrame(pb: PbChatFrame): void {
	let frame: ChatFrame;
	try { frame = chatFrameFromProto(pb); }
	catch (e) {
		diag("chat.parse_skip", { reason: "frame_zod", msg: String(e) });
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
		});
		return;
	}
	// Turn state rides every frame, including the payload-less ones the worker
	// sends on agent_start/agent_end — apply it before any early return.
	if (cur.streaming !== frame.streaming) setRootStore("chat_omp", sid, "streaming", frame.streaming);
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
	setRootStore("chat_omp", sid, {
		messages: merged.length > MAX_CHAT_MSGS ? merged.slice(-MAX_CHAT_MSGS) : merged,
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

/** Backfill chat history on first chat-view enter / browser reconnect.
 *  Pages older history via after_seq (lowest held seq − 1). No-op when already
 *  resolved. Marks status "loading" while in flight so the pane shows a skeleton. */
export async function backfillOmpChat(sessionId: string): Promise<void> {
	const cur = rootStore.chat_omp[sessionId];
	if (cur?.status === "resolved") return;
	setRootStore("chat_omp", sessionId, {
		messages: cur?.messages ?? [],
		seq: cur?.seq ?? 0,
		status: "loading",
		streaming: cur?.streaming ?? false,
	});
	try {
		const res = await coordClient.sessionsGetChatHistory({
			sessionId: asSessionId(sessionId),
			maxMessages: 500,
		});
		const messages = res.messages.map((m) => {
			// Connect returns proto-typed ChatMessage; adapt via the wire boundary.
			return protoMsgToWire(m);
		});
		const existing = rootStore.chat_omp[sessionId];
		const liveMsgs = existing?.messages ?? [];
		const liveIds = new Set(liveMsgs.map((m) => m.id));
		// Merge: history (oldest) ++ live frames not yet in history.
		const merged = [...messages.filter((m) => !liveIds.has(m.id)), ...liveMsgs];
		setRootStore("chat_omp", sessionId, {
			messages: merged,
			seq: Math.max(existing?.seq ?? 0, Number(res.nextSeq)),
			status: "resolved",
			streaming: existing?.streaming ?? false,
		});
	} catch (e) {
		// On failure, mark resolved with whatever we have so the pane renders
		// instead of spinning forever; live frames will keep arriving.
		setRootStore("chat_omp", sessionId, "status", "resolved");
		diag("chat.parse_skip", { reason: "backfill_failed", msg: String(e) });
	}
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
