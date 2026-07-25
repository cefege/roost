// Chat history readers for the get-chat-history / get-chat-block RPCs.
//
// Both serve a session's transcript WITHOUT touching its RPC child: the page
// comes out of the in-memory row cache, and full block text out of omp's own
// session JSONL. That matters at worker boot — a cold `omp --mode rpc-ui` costs
// ~16 s, far past the 8 s RPC deadline, and the answer is already on disk.
// Durable transcript reads live separately from the RPC controller, so reload
// and full-block expansion cannot affect the live child.
import { readFile } from "node:fs/promises";
import type { ChatMessage } from "@roost/shared/chat/wire";
import type { SessionRecord } from "../../session-record.ts";
import { fullBlockText } from "./parse.ts";

export interface ChatHistoryPage {
	messages: ChatMessage[];
	nextSeq: number;     // next-older cursor (0 = no more)
	truncated: boolean;
}

/** Serve a slice of cached chat history for backfill. Messages with line-seq
 *  <= afterSeq, newest-first capped at maxMessages. nextSeq = the oldest seq in
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
		out.unshift(msgs[i]!);
		outSeqs.push(s);
	}
	// More older history exists iff any earlier entry has seq <= afterSeq.
	let truncated = false;
	for (let i = msgs.length - 1 - out.length; i >= 0; i--) {
		if ((seqs[i] ?? 0) <= afterSeq) { truncated = true; break; }
	}
	const oldest = outSeqs.length > 0 ? Math.min(...outSeqs) : 0;
	return { messages: out, nextSeq: oldest, truncated };
}

/** Re-read the transcript file, find the line whose entry id === messageId, and
 *  return the FULL (untruncated) text of the block at blockIndex. Null if the
 *  message or block can't be found.
 *
 *  Only rows hydrated straight from the transcript (browser-command-chat.ts's
 *  hydrateRpcRows) carry real entry ids; a live child's rows are synthetic
 *  `rpc-N` and are served from its own in-memory store instead. */
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
		if (typeof raw !== "object" || raw === null || !("id" in raw) || raw.id !== messageId) continue;
		// Found the line — re-parse WITHOUT truncation and pull the block.
		return fullBlockText(line, blockIndex);
	}
	return null;
}
