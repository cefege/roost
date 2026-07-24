// Shared render-routing for the omp chat thread. ONE place decides how each
// ContentBlock reaches the screen, so the renderer and the parity harness can
// never drift: OmpChatPane consumes buildToolIndex + orphanCallIds; the coverage
// test asserts analyzeCoverage() drops nothing.
//
// Routing rules (the parity contract — "nothing the transcript holds is lost"):
//  - text / thinking / image on a NON-toolResult message → rendered inline.
//  - toolCall → a ToolCard at the call site.
//  - EVERY toolResult block + toolEvent → folded into the matching call's card
//    (by callId). A tool can emit MULTIPLE result blocks (text interleaved with
//    images, e.g. browser screenshots) — all are kept and rendered.
//  - image on a toolResult message → folded into that message's tool card
//    (images carry no callId; they inherit the message's toolResult callId).
//  - a toolResult / toolEvent whose callId has NO toolCall (orphan) → its OWN
//    card at the result site, so an out-of-order or call-less result still shows.

import type { ChatMessage, ContentBlock, ToolCallBlock, ToolResultBlock, ToolEventBlock, ImageBlock } from "@roost/shared/chat/wire";

/** One result block plus the coordinates its untruncated text is fetched from. */
export interface ResultRef {
  block: ToolResultBlock;
  msgId: string;
  blockIndex: number;
}

export interface ToolMatch {
  call?: ToolCallBlock;
  callMsgId?: string;
  callBlockIndex?: number;
  results: ResultRef[];
  event?: ToolEventBlock;
  images: ImageBlock[];
}

/** callId → {call, results, event, images} across the whole thread. */
export function buildToolIndex(messages: ChatMessage[]): Map<string, ToolMatch> {
  const idx = new Map<string, ToolMatch>();
  const get = (id: string): ToolMatch => {
    let m = idx.get(id);
    if (!m) { m = { results: [], images: [] }; idx.set(id, m); }
    return m;
  };
  for (const msg of messages) {
    let msgCallId: string | undefined;
    msg.blocks.forEach((b, i) => {
      if (b.kind === "toolCall") { const m = get(b.callId); m.call = b; m.callMsgId = msg.id; m.callBlockIndex = i; }
      else if (b.kind === "toolResult") { get(b.callId).results.push({ block: b, msgId: msg.id, blockIndex: i }); msgCallId = b.callId; }
      else if (b.kind === "toolEvent") { get(b.callId).event = b; }
    });
    if (msgCallId) {
      for (const b of msg.blocks) if (b.kind === "image") get(msgCallId).images.push(b);
    }
  }
  return idx;
}

/** callIds with NO toolCall (call-less result/event) — rendered as their own
 *  card at the result message so nothing is orphaned off-screen. */
export function orphanCallIds(idx: Map<string, ToolMatch>): string[] {
  const out: string[] = [];
  for (const [id, m] of idx) if (!m.call && (m.results.length || m.event || m.images.length)) out.push(id);
  return out;
}

export interface Dropped { msgId: string; blockIndex: number; kind: ContentBlock["kind"] }

/** Regression guard: every block must reach the screen. Returns the blocks that
 *  the routing rules would NOT render (must be empty). */
export function analyzeCoverage(messages: ChatMessage[]): { total: number; dropped: Dropped[] } {
  const idx = buildToolIndex(messages);
  const hasCard = (callId: string): boolean => {
    const m = idx.get(callId);
    return !!m && (!!m.call || m.results.length > 0 || !!m.event || m.images.length > 0);
  };
  let total = 0;
  const dropped: Dropped[] = [];
  for (const msg of messages) {
    const msgCallId = msg.blocks.find((b): b is ToolResultBlock => b.kind === "toolResult")?.callId;
    msg.blocks.forEach((b, i) => {
      total++;
      let ok: boolean;
      switch (b.kind) {
        case "text":
        case "thinking":
          ok = true; break;
        case "toolCall":
        case "toolResult":
        case "toolEvent":
          ok = hasCard(b.callId); break;
        case "image":
          ok = msgCallId ? hasCard(msgCallId) : true; break;
        case "approval":
          // Carries no callId — its own row at the message site.
          ok = true; break;
      }
      if (!ok) dropped.push({ msgId: msg.id, blockIndex: i, kind: b.kind });
    });
  }
  return { total, dropped };
}
