// Browser-command handlers: omp chat history backfill (get-chat-history /
// get-chat-block). Mirrors browser-command-terminal.ts::handleGetScrollbackCells.
// Serves the in-memory transcript cache (history) + a file re-read (full block).

import type { ClientControlFrame } from "@roost/shared/wire";
import type { CoordLink } from "./transport/CoordLink.ts";
import type { SessionManager } from "./session-manager.ts";
import { getChatHistory, getChatBlockText } from "./session-chat.ts";

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
}

/** Full untruncated text of one ContentBlock. rpc-ok data: { text }.
 *  Re-reads the transcript file (the cache holds truncated text). */
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
	const text = await getChatBlockText(rec, frame.message_id, frame.block_index);
	if (text === null) {
		coordLink.send({ kind: "rpc-error", request_id, message: "block not found" });
		return;
	}
	coordLink.send({ kind: "rpc-ok", request_id, data: { text } });
}
