// The parity oracle: what omp's TERMINAL paints, as structure.
//
// Pure port of @oh-my-pi/pi-coding-agent@17.1.3:
//   src/modes/components/chat-transcript-builder.ts
//     — #appendChatMessage (the exhaustive role dispatch), #appendAssistantMessage,
//       #appendCustomMessage, userMessageText
//   src/modes/utils/transcript-render-helpers.ts
//     — assistantHasVisibleContent, splitAssistantMessageToolTimeline
//   src/utils/thinking-display.ts — canonicalizeMessage
// Re-derive from those files when omp changes; a drifted port surfaces as a
// failing corpus test (tests/chat/omp/tui-parity.test.ts), which is the alarm.
//
// `tuiRows(lines)` projects raw transcript JSONL; `roostRows(messages)` projects
// Roost's own ChatMessages. Parity IS the two being deep-equal. Rows carry only
// STRUCTURAL identity, never prose: text equality is the browser oracle's job,
// and keeping prose out keeps a corpus diff readable when it fails.
//
// Two deliberate, measured-zero divergences from a literal omp port, both on
// Roost's side (parse.ts drops the row, omp would paint an empty one):
//   - a `fileMention` whose every entry lacks a path string (corpus: 0 of 1),
//   - a `custom`/`custom_message` with display:true, empty text AND no details
//     (corpus: 0 of 4468 visible custom messages).
// They are left as real mismatches rather than papered over — an oracle that
// bends to its subject proves nothing.

import { resolveAssistantNotice } from "./parse.ts";
import type { ChatMessage, ContentBlock } from "@roost/shared/chat/wire";

export type TuiRow =
  | { kind: "user"; synthetic: boolean }
  | { kind: "assistant" }
  | { kind: "tool"; callId: string; name: string }
  | { kind: "notice"; level: "error" | "note" }
  | { kind: "summary"; variant: "compaction" | "branch" }
  | { kind: "custom"; customType: string }
  | { kind: "exec"; lang: "bash" | "python" }
  | { kind: "fileMention"; count: number };

type Rec = Record<string, unknown>;
const asRec = (x: unknown): Rec | undefined => (typeof x === "object" && x !== null ? (x as Rec) : undefined);
const asStr = (x: unknown): string | undefined => (typeof x === "string" ? x : undefined);

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

// ─── raw JSONL → rows ─────────────────────────────────────────────────────

/** omp's userMessageText: the whole string, or the joined text blocks. Note it
 *  does NOT canonicalize — `#appendChatMessage` gates on bare truthiness. */
function userMessageText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  let out = "";
  for (const b of content) {
    const br = asRec(b);
    if (br && br.type === "text") out += asStr(br.text) ?? "";
  }
  return out;
}

/** omp's assistantHasVisibleContent, over raw content blocks. */
function rawVisible(blocks: readonly Rec[]): boolean {
  for (const b of blocks) {
    if (b.type === "image") return true;
    if (b.type === "text" && canonical(asStr(b.text))) return true;
    if (b.type === "thinking" && canonical(asStr(b.thinking))) return true;
  }
  return false;
}

/** #appendAssistantMessage: the pre-tool segment, then each tool card followed
 *  by its post-tool segment (splitAssistantMessageToolTimeline), then the
 *  turn-ending notice. */
function rawAssistantRows(m: Rec, out: TuiRow[]): void {
  const content = Array.isArray(m.content) ? m.content : [];
  let segment: Rec[] = [];
  const flush = (): void => {
    if (rawVisible(segment)) out.push({ kind: "assistant" });
    segment = [];
  };
  for (const raw of content) {
    const b = asRec(raw);
    if (!b) continue;
    if (b.type === "toolCall") {
      flush();
      // Same id/name alias tolerance parse.ts applies, so the two projections
      // agree on a transcript that used the toolCallId/toolName spelling.
      out.push({
        kind: "tool",
        callId: asStr(b.id) ?? asStr(b.toolCallId) ?? "",
        name: asStr(b.name) ?? asStr(b.toolName) ?? "",
      });
      continue;
    }
    segment.push(b);
  }
  flush();
  const notice = resolveAssistantNotice(m);
  if (notice) out.push({ kind: "notice", level: notice.level });
}

/** #appendChatMessage's role dispatch. `developer` and `toolResult` paint no
 *  row of their own (developer's textContent is "" by construction; a tool
 *  result folds into its call's card). */
function rawMessageRows(m: Rec, out: TuiRow[]): void {
  switch (m.role) {
    case "user":
      if (userMessageText(m.content).length > 0) out.push({ kind: "user", synthetic: m.synthetic === true });
      return;
    case "assistant":
      rawAssistantRows(m, out);
      return;
    case "bashExecution":
      out.push({ kind: "exec", lang: "bash" });
      return;
    case "pythonExecution":
      out.push({ kind: "exec", lang: "python" });
      return;
    case "fileMention": {
      // buildFileMentionBlock adds one child per file; the builder keeps the
      // block only when it has children.
      const count = Array.isArray(m.files) ? m.files.length : 0;
      if (count > 0) out.push({ kind: "fileMention", count });
      return;
    }
    case "custom":
    case "hookMessage":
      if (m.display === true) out.push({ kind: "custom", customType: asStr(m.customType) ?? "" });
      return;
    default:
      return;
  }
}

/** Rows omp 17.1.3's transcript builder paints for these raw JSONL lines. */
export function tuiRows(lines: readonly string[]): TuiRow[] {
  const out: TuiRow[] = [];
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed.length === 0) continue;
    let parsed: unknown;
    try { parsed = JSON.parse(trimmed); } catch { continue; }
    const e = asRec(parsed);
    if (!e) continue;
    switch (e.type) {
      case "message": {
        const m = asRec(e.message);
        if (m) rawMessageRows(m, out);
        break;
      }
      case "compaction":
        out.push({ kind: "summary", variant: "compaction" });
        break;
      case "branch_summary":
        out.push({ kind: "summary", variant: "branch" });
        break;
      case "custom_message":
        if (e.display === true) out.push({ kind: "custom", customType: asStr(e.customType) ?? "" });
        break;
      // session header, custom (entry renderers Roost cannot execute), and every
      // footer/status entry paint no transcript row.
      default:
        break;
    }
  }
  return out;
}

// ─── ChatMessage[] → rows ─────────────────────────────────────────────────

/** assistantHasVisibleContent over Roost's mapped blocks. */
function blocksVisible(blocks: readonly ContentBlock[]): boolean {
  for (const b of blocks) {
    if (b.kind === "image") return true;
    if ((b.kind === "text" || b.kind === "thinking") && canonical(b.text)) return true;
  }
  return false;
}

/** The same projection over Roost's ChatMessages. Roost emits ONE message per
 *  assistant entry holding all its blocks in source order, so the pre-tool /
 *  per-tool segmentation is re-derived by walking that block list. */
export function roostRows(messages: readonly ChatMessage[]): TuiRow[] {
  const out: TuiRow[] = [];
  for (const msg of messages) {
    switch (msg.role) {
      case "user": {
        let text = "";
        for (const b of msg.blocks) if (b.kind === "text") text += b.text;
        if (text.length > 0) out.push({ kind: "user", synthetic: msg.synthetic });
        break;
      }
      case "assistant": {
        let segment: ContentBlock[] = [];
        const flush = (): void => {
          if (blocksVisible(segment)) out.push({ kind: "assistant" });
          segment = [];
        };
        for (const b of msg.blocks) {
          if (b.kind === "toolCall") {
            flush();
            out.push({ kind: "tool", callId: b.callId, name: b.name });
            continue;
          }
          // The notice is appended last by parse.ts and painted last by omp;
          // toolEvent is Roost's own tool-lifecycle value-add with no TUI row.
          if (b.kind === "notice") continue;
          segment.push(b);
        }
        flush();
        for (const b of msg.blocks) if (b.kind === "notice") out.push({ kind: "notice", level: b.level });
        break;
      }
      case "developer":
        for (const b of msg.blocks) {
          if (b.kind === "summary") out.push({ kind: "summary", variant: b.variant });
          else if (b.kind === "custom") out.push({ kind: "custom", customType: b.customType });
          else if (b.kind === "exec") out.push({ kind: "exec", lang: b.lang });
          else if (b.kind === "fileMention") out.push({ kind: "fileMention", count: b.paths.length });
        }
        break;
      // toolResult folds into its call's card — no row of its own.
      default:
        break;
    }
  }
  return out;
}
