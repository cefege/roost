// Shared mutations of a SessionRecord's chat cache. Both writers — the RPC
// child's live event stream and the transcript hydration that refills a cold
// thread at boot — go through here, so they agree on one rule: append-or-replace
// BY id, keeping the message's ORIGINAL chatMsgSeqs slot (getChatHistory pages
// by walking that array; a moved slot re-serves or skips a page).

import type { ChatMessage } from "@roost/shared/chat/wire";
import type { SessionRecord } from "../../session-record.ts";


/** Append-or-replace by id, keeping `chatMsgSeqs` MONOTONIC — getChatHistory
 *  pages by walking it. A new message is spliced in at its seq order, not
 *  pushed: rows can be produced out of thread order (a history reload
 *  interleaves with a live turn), so arrival order is not thread order.
 *
 *  Returns TRUE when the row did NOT land at the tail. The wire only knows
 *  "append these ids" and "reset to this list", so the caller must reseed on a
 *  true — otherwise the worker's order is right and the pane's is still wrong,
 *  which is worse than the bug, because now the two disagree silently. */
export function upsertChatMessage(rec: SessionRecord, msg: ChatMessage, seq: number): boolean {
	const msgs = (rec.chatMessages ??= []);
	const seqs = (rec.chatMsgSeqs ??= []);
	const i = msgs.findIndex((m) => m.id === msg.id);
	if (i >= 0) {
		msgs[i] = msg;
		seqs[i] = seq;
		return false;   // in place: the client already holds this row at this index
	}
	// First slot strictly newer than this message; equal seqs keep arrival order
	// (one transcript line can yield several rows, and they are already in order).
	let at = seqs.length;
	while (at > 0 && (seqs[at - 1] ?? 0) > seq) at--;
	msgs.splice(at, 0, msg);
	seqs.splice(at, 0, seq);
	return at !== msgs.length - 1;
}

/** Forget a streamed row the bridge never finished (it died mid-turn), so the
 *  transcript's later copy of that turn is the only one in the thread instead
 *  of a second row beside a frozen partial. Returns whether anything was
 *  dropped — the caller only has to reseed the client if so. */
export function dropChatMessage(rec: SessionRecord, id: string): boolean {
	const msgs = rec.chatMessages;
	if (!msgs) return false;
	const i = msgs.findIndex((m) => m.id === id);
	if (i < 0) return false;
	msgs.splice(i, 1);
	rec.chatMsgSeqs?.splice(i, 1);
	return true;
}
