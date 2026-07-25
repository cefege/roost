// Browser-command handlers: omp chat history backfill (get-chat-history /
// get-chat-block) and the chat-command tunnel to a session's RPC child.
// Mirrors browser-command-terminal.ts::handleGetScrollbackCells.
// Serves the in-memory transcript cache (history) + a file re-read (full block).

import { readFile } from "node:fs/promises";
import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/CoordLink.ts";
import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import { getChatHistory, getChatBlockText } from "./chat/omp/history.ts";
import { parseOmpLine } from "./chat/omp/parse.ts";
import { upsertChatMessage } from "./chat/omp/chat-record.ts";
import { ensureRpcChat, rpcChatActive, rpcChatCommand, rpcChatFullBlock, republishRpcChatState } from "./chat/omp/rpc-chat.ts";
import { loadOmpSessionFile } from "./chat/omp/session-store.ts";
import { listOmpSessions } from "./chat/omp/session-discovery.ts";
import { log } from "@roost/shared";



/** Refill `rec.chatMessages` for an rpc session straight from omp's session
 *  JSONL, WITHOUT spawning a child.
 *
 *  The mirror engine gets this for free: session-resume re-arms the transcript
 *  watcher, which re-reads from offset 0. The rpc engine had no equivalent, so
 *  after a worker restart its rows lived only inside a child that had not been
 *  started yet — the pane rendered empty and the parity oracle scored a real
 *  chat as `mismatch` (13 terminal rows vs 0). Measured in production.
 *
 *  A file parse, not a spawn: a cold `omp --mode rpc-ui` costs ~16 s, and
 *  paying that per session at boot to answer "what is in this thread" is
 *  absurd when the answer is already on disk in the format parse.ts reads.
 *  The child still starts lazily; when it does, reloadHistory resets and
 *  re-seeds under its own ids, replacing everything written here.
 *
 *  No-op unless the row list is EMPTY — a live child's rows always win. */
function hydrateRpcRows(rec: SessionRecord, path: string, lines: string[]): void {
	if ((rec.chatMessages?.length ?? 0) > 0) return;
	// These rows carry REAL transcript entry ids, so `getChatBlockText` can serve
	// their "show full" — but only if it knows the file. refreshStatus normally
	// sets this, and it has not run: no child has booted.
	rec.chatTranscriptPath ??= path;
	rec.chatMessages = [];
	rec.chatMsgSeqs = [];
	for (const line of lines) {
		const msg = parseOmpLine(line);
		if (!msg) continue;
		rec.chat_seq += 1;
		upsertChatMessage(rec, msg, rec.chat_seq);
	}
}

/** Serve a slice of cached chat history. rpc-ok data:
 *  { messages, next_seq, truncated }. Empty messages when no watcher is running
 *  (non-omp session / not yet resolved). */
export async function handleGetChatHistory(
	frame: Extract<ClientControlFrame, { kind: "get-chat-history" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session not found" });
		return;
	}
	// A worker restart leaves the durable sessionId→JSONL mapping but neither a
	// child nor any in-memory rows, so the pane rendered an empty thread until
	// the user typed. Two separate fixes, in order:
	//
	// 1. HYDRATE from the transcript, synchronously. It is a file parse, and it
	//    means this very response carries the thread instead of promising it.
	// 2. PRE-WARM the child, NOT awaited. A cold `omp --mode rpc-ui` takes ~16 s
	//    (extensions + MCP mounts) and this RPC's deadline is 8 s
	//    (handlers-sessions.ts:397), so awaiting turned every chat pane open
	//    into a guaranteed timeout. Warming it here is also what keeps the
	//    user's first prompt inside chat-command's 35 s deadline. When it
	//    finishes, reloadHistory resets and re-seeds under the child's own ids
	//    through the SAME push channel the pane renders from, replacing step 1.
	const rpcFile = loadOmpSessionFile(frame.session_id);
	if (rpcFile !== null) {
		// Skip when a child is already live — its rpc-N rows are authoritative,
		// and interleaving transcript ids underneath them would double the thread.
		if (!rpcChatActive(frame.session_id)) {
			try { hydrateRpcRows(rec, rpcFile, (await readFile(rpcFile, "utf8")).split("\n")); }
			catch (err) { log.warn("omp-rpc", "history_hydrate_failed", { sid: frame.session_id, err: String(err) }); }
		}
		try { ensureRpcChat(sessionMgr, rec); }
		catch (err) {
			// A dead omp binary must not fail the history RPC: the pane would
			// paint its status:"failed" state for a recoverable condition.
			log.warn("omp-rpc", "history_boot_failed", { sid: frame.session_id, err: String(err) });
		}
	}
	const afterSeq = frame.after_seq ?? rec.chat_seq;
	const page = getChatHistory(rec, afterSeq, frame.max_messages);
	coordLink.send({
		kind: "rpc-ok",
		request_id,
		data: {
			messages: page.messages,
			next_seq: page.nextSeq,
			truncated: page.truncated,
		},
	});
	// Session status (model, effort, context) is PUSHED on change, never part of
	// this page — so a pane that mounts after the child booted would show no
	// model chip until the next turn ended. Re-publish it on every reseed: the
	// mount is exactly the moment the client needs it.
	republishRpcChatState(sessionMgr, frame.session_id);
}

/** Full untruncated text of one ContentBlock. rpc-ok data: { text }.
 *
 *  Two id spaces, tried in order: the live child keeps untruncated text in
 *  memory under its synthetic `rpc-N` ids, while a cold thread refilled by
 *  `hydrateRpcRows` carries REAL omp entry ids and is re-read off disk.
 *  Whichever the row came from, one of the two serves it. */
export async function handleGetChatBlock(
	frame: Extract<ClientControlFrame, { kind: "get-chat-block" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session not found" });
		return;
	}
	const text = rpcChatFullBlock(frame.session_id, frame.message_id, frame.block_index)
		?? await getChatBlockText(rec, frame.message_id, frame.block_index);
	if (text === null) {
		coordLink.send({ kind: "rpc-error", request_id, message: "block not found" });
		return;
	}
	coordLink.send({ kind: "rpc-ok", request_id, data: { text } });
}

/** Native omp chat: tunnel one RpcCommand to the session's RPC child.
 *  rpc-ok data: { response_json }. Lazy-starts the child on first command. */
export async function handleChatCommand(
	frame: Extract<ClientControlFrame, { kind: "chat-command" }>,
	request_id: string,
	deps: { coordLink: CoordLink; sessionMgr: SessionManager },
): Promise<void> {
	const { coordLink, sessionMgr } = deps;
	const rec = sessionMgr.getBySessionId(frame.session_id);
	if (!rec) {
		coordLink.send({ kind: "rpc-error", request_id, message: "session not found" });
		return;
	}
	const result = await rpcChatCommand(sessionMgr, rec, frame.command_json);
	if (!result.ok) {
		coordLink.send({ kind: "rpc-error", request_id, message: result.error });
		return;
	}
	coordLink.send({ kind: "rpc-ok", request_id, data: { response_json: JSON.stringify(result.response) } });
}

/** Resumable omp transcripts on this machine, newest first. rpc-ok data:
 *  { sessions: OmpSessionEntry[] }. A read-only scan — it never touches a
 *  session, a child, or the terminal that wrote the file. */
export function handleListOmpSessions(
	frame: Extract<ClientControlFrame, { kind: "list-omp-sessions" }>,
	request_id: string,
	deps: { coordLink: CoordLink },
): void {
	const { coordLink } = deps;
	try {
		coordLink.send({ kind: "rpc-ok", request_id, data: { sessions: listOmpSessions(frame.limit) } });
	} catch (err) {
		coordLink.send({
			kind: "rpc-error", request_id,
			message: err instanceof Error ? err.message : String(err),
		});
	}
}
