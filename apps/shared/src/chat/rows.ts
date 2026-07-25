// What Roost's chat pane paints, as structure.
//
// One projection: `roostMessageRows(msg)` maps a ChatMessage onto the rows a
// rendered transcript shows, each anchored to the block that carries it. The
// pane stamps `data-tui-row` from exactly this, so "the Nth painted element is
// the Nth row" holds without a second, driftable walk of the DOM.
//
// Row shapes are a port of @oh-my-pi/pi-coding-agent@17.1.3's transcript
// builder (src/modes/components/chat-transcript-builder.ts,
// src/modes/utils/transcript-render-helpers.ts) so the pane segments a turn the
// way omp's own TUI does.
//
// There used to be a second projection here — `tuiRows(lines)`, over raw omp
// JSONL — plus `rowKey`, feeding a terminal-vs-web parity oracle. That oracle
// existed to arbitrate between two engines rendering one conversation. There is
// one engine now, so it and its transcript projection are gone.
//
// Rows carry structural identity plus a `label`: the first non-empty line of
// the row's own text, capped at LABEL_CAP.

import { resolveAssistantNotice } from "./assistant-notice.ts";
import type { ChatMessage } from "./wire.ts";

export type TuiRow =
  | { kind: "user"; synthetic: boolean; label: string }
  | { kind: "assistant"; label: string }
  | { kind: "tool"; callId: string; name: string; label: string }
  | { kind: "notice"; level: "error" | "note"; label: string }
  | { kind: "summary"; variant: "compaction" | "branch"; label: string }
  | { kind: "custom"; customType: string; label: string }
  | { kind: "exec"; lang: "bash" | "python"; label: string }
  | { kind: "fileMention"; count: number; label: string };

/** Chars of prose kept on a row. Long enough to identify the turn in a
 *  side-by-side column, short enough that a 200-row diff stays a page. */
export const LABEL_CAP = 80;

type Rec = Record<string, unknown>;
const asRec = (x: unknown): Rec | undefined => (typeof x === "object" && x !== null ? (x as Rec) : undefined);
const asStr = (x: unknown): string | undefined => (typeof x === "string" ? x : undefined);

/** First non-empty line, trimmed, capped. Scans line by line and stops at the
 *  first hit — never splits a whole assistant turn to read its opening words. */
function label(text: string | undefined): string {
  if (!text) return "";
  const n = text.length;
  let i = 0;
  while (i < n) {
    let j = text.indexOf("\n", i);
    if (j < 0) j = n;
    const line = text.slice(i, j).trim();
    if (line.length > 0) return line.length > LABEL_CAP ? line.slice(0, LABEL_CAP) : line;
    i = j + 1;
  }
  return "";
}

/** omp's canonicalizeMessage: trim, then treat a run of only `.`, `…` and
 *  whitespace as empty (streaming placeholder blocks paint nothing). */
function canonical(text: string | undefined): string {
  if (!text) return "";
  const trimmed = text.trim();
  for (let i = 0; i < trimmed.length; i++) {
    const code = trimmed.charCodeAt(i);
    if (code !== 0x2e && code !== 0x2026 && code !== 0x20 && code !== 0x09 && code !== 0x0a && code !== 0x0d) {
      return trimmed;
    }
  }
  return "";
}

// ─── ChatMessage[] → rows ─────────────────────────────────────────────────

/** A row plus the block that ANCHORS it in a rendered transcript: the index of
 *  the ContentBlock whose element carries the row, or -1 when the row belongs
 *  to the message itself (a user row spans all its text blocks). The browser
 *  oracle stamps `data-tui-row` using exactly this, so "the Nth painted element
 *  is the Nth row" holds without a second, driftable walk of the DOM. */
export interface RowAnchor { row: TuiRow; blockIndex: number }

/** Rows ONE message projects to, each anchored to the block that renders it.
 *  Roost emits one message per assistant entry holding all its blocks in source
 *  order, so the pre-tool / per-tool segmentation is re-derived by walking that
 *  block list. */
export function roostMessageRows(msg: ChatMessage): RowAnchor[] {
  const out: RowAnchor[] = [];
  switch (msg.role) {
    case "user": {
      let text = "";
      for (const b of msg.blocks) if (b.kind === "text") text += b.text;
      if (text.length > 0) out.push({ row: { kind: "user", synthetic: msg.synthetic, label: label(text) }, blockIndex: -1 });
      break;
    }
    case "assistant": {
      // assistantHasVisibleContent, inlined so the anchor (the FIRST block that
      // makes the segment visible) and the label come out of one pass.
      let at = -1;
      let text = "";
      const flush = (): void => {
        if (at >= 0) out.push({ row: { kind: "assistant", label: label(text) }, blockIndex: at });
        at = -1;
        text = "";
      };
      for (let i = 0; i < msg.blocks.length; i++) {
        const b = msg.blocks[i]!;
        if (b.kind === "toolCall") {
          flush();
          out.push({ row: { kind: "tool", callId: b.callId, name: b.name, label: b.name }, blockIndex: i });
          continue;
        }
        // The notice is appended last by parse.ts and painted last by omp;
        // toolEvent is Roost's own tool-lifecycle value-add with no TUI row.
        if (b.kind === "notice") continue;
        if (b.kind === "image") { if (at < 0) at = i; continue; }
        if (b.kind !== "text" && b.kind !== "thinking") continue;
        const t = canonical(b.text);
        if (!t) continue;
        if (at < 0) at = i;
        if (!text) text = t;
      }
      flush();
      for (let i = 0; i < msg.blocks.length; i++) {
        const b = msg.blocks[i]!;
        if (b.kind === "notice") out.push({ row: { kind: "notice", level: b.level, label: label(b.text) }, blockIndex: i });
      }
      break;
    }
    case "developer":
      for (let i = 0; i < msg.blocks.length; i++) {
        const b = msg.blocks[i]!;
        if (b.kind === "summary") out.push({ row: { kind: "summary", variant: b.variant, label: label(b.text) }, blockIndex: i });
        // `subagent` is Roost's own card, same status as toolEvent above: omp's
        // TUI paints subagent work in a live HUD, not as a transcript row, so
        // counting it here would report a permanent phantom mismatch on every
        // rpc chat that spawns one. Every OTHER customType is a real omp
        // custom_message and does have a TUI row.
        else if (b.kind === "custom" && b.customType !== "subagent") out.push({ row: { kind: "custom", customType: b.customType, label: label(b.text) }, blockIndex: i });
        else if (b.kind === "exec") out.push({ row: { kind: "exec", lang: b.lang, label: label(b.command) }, blockIndex: i });
        else if (b.kind === "fileMention") out.push({ row: { kind: "fileMention", count: b.paths.length, label: label(b.paths[0]) }, blockIndex: i });
      }
      break;
    // toolResult folds into its call's card — no row of its own.
    default:
      break;
  }
  return out;
}

/** The projection over Roost's ChatMessages — the flattened `roostMessageRows`,
 *  so the pane's per-message tagging and this list can never disagree. */
export function roostRows(messages: readonly ChatMessage[]): TuiRow[] {
  const out: TuiRow[] = [];
  for (const msg of messages) for (const a of roostMessageRows(msg)) out.push(a.row);
  return out;
}
