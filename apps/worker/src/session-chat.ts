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
	resolveTranscriptPath, startTranscriptWatcher, emptyOmpStatus,
} from "./chat/omp/transcript-watcher.ts";
import { startLiveWatcher } from "./chat/omp/live-watcher.ts";
import { claimJoinKey, dropChatMessage, resolveLiveId, upsertChatMessage } from "./chat/omp/chat-record.ts";
import { parseOmpLine, fullBlockText, ompLineJoinKey, TRUNC_CAP } from "./chat/omp/parse.ts";
import { lookupOmpModel } from "./chat/omp/model-catalog.ts";
import { OMP_LIVE_DIR } from "./session-constants.ts";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

import { isOmpTitle, ompTitleRunState } from "@roost/shared/chat/omp-title";
export { isOmpTitle };

/** Cap on transcript-resolve attempts per session. Each attempt costs a global
 *  `lsof -c bun` and runs ~12 s, so this is ~1.5 min of trying: ample for a slow
 *  omp boot, longer than findActiveTranscriptByCwd's 60 s activeWindowMs (a
 *  just-exited sibling in the same cwd keeps the fallback ambiguous for that
 *  long), and short enough that a permanently-unresolvable session stops
 *  probing lsof for the rest of its life. */
const CHAT_RESOLVE_MAX_TRIES = 8;

/** Build + send a ChatFrame upstream. No-op when no sink (tests). */
function emitChatFrame(this: SessionManager, channelId: number, append: ChatMessage[], seq: number, reset: boolean): void {
	if (!this.sendChatFrameUpstream) return;
	const rec = this.sessions.get(channelId);
	// Run state: the bridge's agent_start/agent_end when a sidecar is attached
	// (it is omp's own turn state), else omp's OSC title separator — the only
	// signal the mirror engine has on its own. The status block is folded out of
	// the transcript by the tailer and rides EVERY frame — including the
	// payload-less run-state and reset ones — because the client applies status
	// off any frame carrying it.
	const streaming = rec?.chatLiveStreaming
		?? (ompTitleRunState(this.lastOscTitle.get(channelId)) === "working");
	const status = rec?.chatStatus ?? emptyOmpStatus();
	// The transcript gives tokens but not the window; omp's own model catalog
	// gives the window and the friendly name. Unknown model → 0 window, and the
	// client renders "<tokens> / ?" exactly as omp's formatContextUsage does.
	const info = status.model ? lookupOmpModel(status.model) : null;
	const frame: ChatFrame = {
		sessionId: "", append, seq, reset, streaming,
		engine: "mirror",
		model: status.model,
		modelName: info?.name ?? "",
		thinkingLevel: status.thinkingLevel,
		mode: status.mode,
		contextTokens: status.contextTokens,
		contextWindow: info?.contextWindow ?? 0,
	};
	this.sendChatFrameUpstream(channelId, frame);
}

/** Publish a payload-less frame when omp's run state flips with no new
 *  transcript line. The chat pane's working badge and Stop button read
 *  `streaming` off every frame, and a turn can run for minutes between
 *  appends. Change-gated: the title re-emits ~12.5×/s on the spinner, and
 *  every Braille frame is the same `working` state. */
export function _emitChatRunState(this: SessionManager, channelId: number): void {
	const rec = this.sessions.get(channelId);
	if (!rec || !rec.chatWatchDispose) return;
	const next = ompTitleRunState(this.lastOscTitle.get(channelId));
	if (rec.chatRunState === next) return;
	rec.chatRunState = next;
	emitChatFrame.call(this, channelId, [], rec.chat_seq, false);
}

/** Idempotently start the omp chat watcher for a session when its OSC title
 *  identifies it as omp. Safe to call on every title update — it short-circuits
 *  once a watcher is running, and only acts on the omp title. */
export function _ensureChatWatch(this: SessionManager, channelId: number): void {
	const rec = this.sessions.get(channelId);
	if (!rec || rec.chatWatchDispose || rec.chatWatchStarting) return;
	if (!isOmpTitle(this.lastOscTitle.get(channelId))) return;
	rec.chatWatchStarting = true;

	void resolveTranscriptPath(rec.childPid, rec.cwd).then((r) => {
		// Session may have closed during the async resolve.
		if (this.sessions.get(channelId) !== rec) return;
		if (!r) {
			rec.chatWatchTries = (rec.chatWatchTries ?? 0) + 1;
			diag("chat.resolve", { ok: false, reason: "no_path", sid: String(rec.sessionId), tries: rec.chatWatchTries });
			// Retryable, bounded. omp boots slower than its first OSC title, so
			// the first resolve routinely runs before the transcript exists —
			// latching here would kill the pane for the session's whole life.
			// Clearing does NOT bring back the 12 Hz storm: that came from having
			// no in-flight guard at all, so ~100 resolves overlapped. The guard is
			// set BEFORE the await, so the next attempt can only start once this
			// one finished — one resolve per ~12 s, and at most MAX_TRIES of them.
			if (rec.chatWatchTries < CHAT_RESOLVE_MAX_TRIES) rec.chatWatchStarting = false;
			return;
		}
		// One transcript, one session. Two Roost sessions in the same cwd both
		// resolve the single open transcript otherwise, and both mirror it.
		for (const other of this.sessions.values()) {
			if (other !== rec && other.chatTranscriptPath === r.path) {
				diag("chat.resolve", { ok: false, reason: "path_taken", sid: String(rec.sessionId) });
				return;   // chatWatchStarting stays true: one attempt, fails safe to the terminal
			}
		}
		rec.chatMessages = [];
		rec.chatMsgSeqs = [];
		// Reset first so the client drops any prior history and reseeds.
		emitChatFrame.call(this, channelId, [], 0, true);
		diag("chat.resolve", { ok: true, via: r.via, sid: String(rec.sessionId) });
		rec.chatTranscriptPath = r.path;
		const handle = startTranscriptWatcher(r.path, (msgs, seq, status, joinKeys) => {
			if (this.sessions.get(channelId) !== rec) return;
			for (const m of msgs) {
				// A turn the bridge already streamed keeps its `live-N` id, so this
				// canonical copy REPLACES that row instead of doubling it. When the
				// bridge has NOT been here yet, claim the key so a live frame that
				// arrives later recognises this row as the one already on screen.
				const key = joinKeys.get(m.id) ?? "";
				m.id = resolveLiveId(rec, key, m.id);
				claimJoinKey(rec, key, m.id);
				upsertChatMessage(rec, m, seq);
			}
			rec.chat_seq = seq;
			// Held by reference on purpose: the tailer mutates one snapshot in
			// place, so every later frame (including the payload-less run-state
			// ones that never reach this callback) reads the current values.
			rec.chatStatus = status;
			emitChatFrame.call(this, channelId, msgs, seq, false);
		});
		// The live sidecar: omp only persists a message once it is COMPLETE, so
		// the transcript can never stream. The file usually does not exist yet
		// (omp not started, or no bridge) — the tailer's poll loop picks it up if
		// it ever appears and costs one failed stat/s until then.
		const live = startLiveWatcher(join(OMP_LIVE_DIR, `${rec.sessionId}.ndjson`), (ev) => {
			if (this.sessions.get(channelId) !== rec) return;
			switch (ev.kind) {
				case "hello":
					rec.chatLiveAttached = true;
					diag("chat.live_hello", { sid: String(rec.sessionId), pid: ev.pid, file: ev.sessionFile });
					return;
				case "streaming":
					rec.chatLiveStreaming = ev.value;
					emitChatFrame.call(this, channelId, [], rec.chat_seq, false);
					return;
				case "message":
					// Live frames must NOT advance chat_seq: it is the transcript's
					// line count and getChatHistory pages by it.
					upsertChatMessage(rec, ev.msg, rec.chat_seq);
					emitChatFrame.call(this, channelId, [ev.msg], rec.chat_seq, false);
					return;
				case "join": {
					// Commutative: if the transcript already claimed this turn, the
					// streamed row is the duplicate — retract it and keep the
					// canonical copy. Otherwise the claim succeeds and the tailer
					// will rewrite the transcript copy onto this row instead.
					const held = claimJoinKey(rec, ev.key, ev.liveId);
					if (held === null || !dropChatMessage(rec, ev.liveId)) return;
					emitChatFrame.call(this, channelId, rec.chatMessages ?? [], rec.chat_seq, true);
					return;
				}
				case "retract": {
					// A turn that rendered mid-flight but ends as something the TUI
					// paints as nothing. Drop it rather than leave a red row where
					// the terminal is silent; reseed, since the wire has no delete.
					if (!dropChatMessage(rec, ev.liveId)) return;
					emitChatFrame.call(this, channelId, rec.chatMessages ?? [], rec.chat_seq, true);
					return;
				}
				case "abort": {
					// The bridge died or said goodbye. Its turn state is no longer
					// authoritative, and a row it left mid-stream would sit frozen
					// beside the transcript's later copy of the same turn.
					rec.chatLiveStreaming = undefined;
					const dropped = ev.liveId !== null && dropChatMessage(rec, ev.liveId);
					// A drop needs the full list: the wire has no delete verb, so
					// the client reseeds off a reset frame.
					emitChatFrame.call(this, channelId, dropped ? rec.chatMessages ?? [] : [], rec.chat_seq, dropped);
					return;
				}
			}
		});
		if (this.sessions.get(channelId) === rec) {
			rec.chatWatchDispose = handle.dispose;
			rec.chatLiveDispose = live.dispose;
		} else {
			handle.dispose();
			live.dispose();
		}
	}).catch((err) => {
		log.warn("session-manager", "chat_watch_start_failed", { sid: String(rec.sessionId), error: String(err) });
	});
}

/** Dispose the chat watcher (close / cwd-change / respawn). Idempotent. */
export function _disposeChatWatch(rec: SessionRecord): void {
	try { rec.chatWatchDispose?.(); } catch { /* best-effort */ }
	try { rec.chatLiveDispose?.(); } catch { /* best-effort */ }
	rec.chatWatchDispose = null;
	rec.chatLiveDispose = null;
	rec.chatLiveAttached = false;
	rec.chatLiveStreaming = undefined;
	rec.chatLiveIds = undefined;
	rec.chatWatchStarting = false;
	rec.chatWatchTries = 0;
	rec.chatRunState = undefined;
	rec.chatStatus = undefined;
	// Release the path so the S3 `path_taken` scan can't be tripped by a record
	// that no longer mirrors anything.
	rec.chatTranscriptPath = null;
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
		if (typeof raw !== "object" || raw === null || !("id" in raw) || typeof raw.id !== "string") continue;
		// Same rewrite the tailer applies, or "show full N chars" cannot resolve
		// a message whose row was streamed first and re-keyed to its live id.
		if (resolveLiveId(rec, ompLineJoinKey(line) ?? "", raw.id) !== messageId) continue;
		// Found the line — re-parse WITHOUT truncation and pull the block.
		return fullBlockText(line, blockIndex);
	}
	return null;
}

export { TRUNC_CAP };
