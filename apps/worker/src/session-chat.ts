// Omp chat watcher orchestration — split out of session-manager.ts (400-line cap).
//
// _ensureChatWatch: idempotent. Fires when the omp OSC title (`π:`) is first seen
// on a session — resolves the transcript path, starts the tailer, and emits
// ChatFrames upstream (coord stamps session_id, fans to SPA). The reset frame
// (empty append, seq 0) precedes the first real batch so the client reseeds.
//
// History/block readers serve the browser-command RPCs (get-chat-history /
// get-chat-block) from the in-memory cache + a file re-read for full block text.
//
// Eligibility = OSC title `π:` (the same signal omp-manifest.ts anchors on).
// AgentState.kind is literally "claude" for every agent session, so the title is
// the only positive omp signal. A future omp that drops the title → chat silently
// off (fails safe to the terminal).

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import { diag, log } from "@roost/shared";
import type { ChatFrame, ChatMessage } from "@roost/shared/chat/wire";
import {
	resolveTranscriptPath, startTranscriptWatcher,
} from "./chat/omp/transcript-watcher.ts";
import { parseOmpLine, fullBlockText, TRUNC_CAP } from "./chat/omp/parse.ts";
import { readFile } from "node:fs/promises";

/** omp identity: OSC window title. omp emits "π > <breadcrumb>" (and older
 *  builds "π: <summary>"); match those two prefixes. Crucially EXCLUDE pi's
 *  "π - <dir>" so chat never steals a pi pane — a bare "π" check would. */
export function isOmpTitle(title: string | undefined): boolean {
	return !!title && (title.startsWith("\u03C0 >") || title.startsWith("\u03C0:"));
}

/** Build + send a ChatFrame upstream. No-op when no sink (tests). */
function emitChatFrame(this: SessionManager, channelId: number, append: ChatMessage[], seq: number, reset: boolean): void {
	if (!this.sendChatFrameUpstream) return;
	// streaming/model/context are native-RPC-only signals; the mirror engine
	// tails a file and has no session state to report.
	const frame: ChatFrame = { sessionId: "", append, seq, reset, streaming: false, model: "", contextPct: 0, contextTokens: 0 };
	this.sendChatFrameUpstream(channelId, frame);
}

/** Idempotently start the omp chat watcher for a session when its OSC title
 *  identifies it as omp. Safe to call on every title update — it short-circuits
 *  once a watcher is running, and only acts on the omp title. */
export function _ensureChatWatch(this: SessionManager, channelId: number): void {
	const rec = this.sessions.get(channelId);
	if (!rec || rec.chatWatchDispose) return;
	if (!isOmpTitle(this.lastOscTitle.get(channelId))) return;

	rec.chatMessages = [];
	rec.chatMsgSeqs = [];
	// Reset first so the client drops any prior history and reseeds.
	emitChatFrame.call(this, channelId, [], 0, true);

	void resolveTranscriptPath(rec.childPid, rec.cwd).then((r) => {
		// Session may have closed during the async resolve.
		if (this.sessions.get(channelId) !== rec) return;
		if (!r) {
			diag("chat.resolve", { ok: false, reason: "no_path", sid: String(rec.sessionId) });
			return;
		}
		diag("chat.resolve", { ok: true, via: r.via, sid: String(rec.sessionId) });
		rec.chatTranscriptPath = r.path;
		const handle = startTranscriptWatcher(r.path, (msgs, seq) => {
			if (this.sessions.get(channelId) !== rec) return;
			for (const m of msgs) {
				rec.chatMessages!.push(m);
				rec.chatMsgSeqs!.push(seq);
			}
			rec.chat_seq = seq;
			emitChatFrame.call(this, channelId, msgs, seq, false);
		});
		if (this.sessions.get(channelId) === rec) {
			rec.chatWatchDispose = handle.dispose;
		} else {
			handle.dispose();
		}
	}).catch((err) => {
		log.warn("session-manager", "chat_watch_start_failed", { sid: String(rec.sessionId), error: String(err) });
	});
}

/** Dispose the chat watcher (close / cwd-change / respawn). Idempotent. */
export function _disposeChatWatch(rec: SessionRecord): void {
	try { rec.chatWatchDispose?.(); } catch { /* best-effort */ }
	rec.chatWatchDispose = null;
}

export interface ChatHistoryPage {
	messages: ChatMessage[];
	nextSeq: number;     // next-older cursor (0 = no more)
	truncated: boolean;
}

/** Serve a slice of cached chat history for backfill. Messages with line-seq
 *  > afterSeq, newest-first capped at maxMessages. nextSeq = the oldest seq in
 *  this page (page older with after_seq = nextSeq - 1); 0 = exhausted. */
export function getChatHistory(rec: SessionRecord, afterSeq: number, maxMessages: number): ChatHistoryPage {
	const msgs = rec.chatMessages ?? [];
	const seqs = rec.chatMsgSeqs ?? [];
	// Walk newest→oldest, collect entries with seq <= afterSeq (older history).
	const out: ChatMessage[] = [];
	const outSeqs: number[] = [];
	for (let i = msgs.length - 1; i >= 0 && out.length < maxMessages; i--) {
		const s = seqs[i] ?? 0;
		if (s > afterSeq) continue;
		out.unshift(msgs[i]);
		outSeqs.push(s);
	}
	const truncated = (() => {
		// More older history exists iff any earlier entry has seq <= afterSeq.
		for (let i = msgs.length - 1 - out.length; i >= 0; i--) {
			if ((seqs[i] ?? 0) <= afterSeq) return true;
		}
		return false;
	})();
	const oldest = outSeqs.length > 0 ? Math.min(...outSeqs) : 0;
	return { messages: out, nextSeq: oldest, truncated };
}

/** Re-read the transcript file, find the line whose parsed id === messageId,
 *  and return the FULL (untruncated) text of the block at blockIndex.
 *  Returns null if the message or block can't be found. */
export async function getChatBlockText(rec: SessionRecord, messageId: string, blockIndex: number): Promise<string | null> {
	const path = rec.chatTranscriptPath;
	if (!path) return null;
	let text: string;
	try { text = await readFile(path, "utf8"); }
	catch { return null; }
	for (const line of text.split("\n")) {
		if (line.length === 0) continue;
		let raw: unknown;
		try { raw = JSON.parse(line); } catch { continue; }
		if (typeof raw !== "object" || raw === null) continue;
		const o = raw as { id?: unknown };
		if (o.id !== messageId) continue;
		// Found the line — re-parse WITHOUT truncation and pull the block.
		return fullBlockText(line, blockIndex);
	}
	return null;
}

export { TRUNC_CAP };
