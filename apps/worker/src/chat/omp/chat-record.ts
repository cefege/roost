// Shared mutations of a SessionRecord's chat cache. THREE producers write it —
// the transcript tailer (durable, canonical), the live sidecar (streaming), and
// the native RPC child — and they must agree on two rules or the pane doubles
// rows:
//   1. append-or-replace BY id, keeping the message's ORIGINAL chatMsgSeqs slot
//      (getChatHistory pages by walking that array; a moved slot re-serves or
//      skips a page);
//   2. the live→transcript join: a message streamed under a provisional `live-N`
//      id is re-parsed later from the transcript under its real omp entry id, so
//      the tailer rewrites the id back before upserting and the canonical copy
//      REPLACES the streamed one in place. The join key is omp's OWN
//      `sessionMessagePersistenceKey` (see parse.ts::assistantPersistenceKey),
//      NOT the entry id: `message_end` fires BEFORE the entry is appended, so
//      `getLeafId()` at that moment names the PREVIOUS leaf (measured: a
//      title_change / toolResult / developer row), which would both miss the
//      real row and corrupt the innocent one it named.

import type { ChatMessage } from "@roost/shared/chat/wire";
import type { SessionRecord } from "../../session-record.ts";

/** Join-key → live-id pairs retained per session. A week-long session produces
 *  thousands of turns; only recent ones can still be streaming, and an evicted
 *  pair just means the transcript row keeps its own id (one row, no duplicate —
 *  the streamed copy it would have replaced was already superseded). */
const LIVE_ID_CAP = 4096;

/** Append-or-replace by id. On replace the message's ORIGINAL chatMsgSeqs slot
 *  is kept: getChatHistory pages by walking that array and it must stay
 *  monotonic. `seq` is the slot a NEW message lands in. */
export function upsertChatMessage(rec: SessionRecord, msg: ChatMessage, seq: number): void {
	rec.chatMessages ??= [];
	rec.chatMsgSeqs ??= [];
	const i = rec.chatMessages.findIndex((m) => m.id === msg.id);
	if (i >= 0) rec.chatMessages[i] = msg;
	else { rec.chatMessages.push(msg); rec.chatMsgSeqs.push(seq); }
}

/** Claim persistence key `key` for row `id`, returning the id that ALREADY held
 *  it when someone else got there first (else null).
 *
 *  The join must be COMMUTATIVE: the live sidecar and the transcript are tailed
 *  by two independent poll loops, both of which reseed from offset 0 on attach,
 *  so either can observe a given turn first. Live-first is the common case
 *  (omp streams before it persists); transcript-first happens on every attach
 *  to an omp that was already running, and used to leave TWO permanent rows for
 *  one turn. Whoever claims the key first owns the row; the loser is a
 *  duplicate and its caller drops it.
 *
 *  Oldest-first eviction (Map keeps insertion order) so this cannot grow
 *  without bound. */
export function claimJoinKey(rec: SessionRecord, key: string, id: string): string | null {
	if (!key || !id) return null;
	const map = (rec.chatLiveIds ??= new Map());
	const held = map.get(key);
	if (held !== undefined) return held === id ? null : held;
	map.set(key, id);
	while (map.size > LIVE_ID_CAP) {
		const oldest = map.keys().next();
		if (oldest.done === true) break;
		map.delete(oldest.value);
	}
	return null;
}

/** The id a transcript message must carry: the row already on screen for this
 *  turn when the bridge streamed it first, else its own. `key` is the message's
 *  persistence key, or "" for a role that never streams. */
export function resolveLiveId(rec: SessionRecord, key: string, id: string): string {
	if (!key) return id;
	return rec.chatLiveIds?.get(key) ?? id;
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
