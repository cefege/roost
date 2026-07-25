// Omp transcript parser — pure line → ChatMessage.
//
// Reads omp's append-only JSONL transcript (one JSON object per line) and maps
// each conversational line to a ChatMessage. The transcript SCHEMA is owned by
// the format's author, not by Roost: we delegate JSONL decoding to the official
// SDK `@earendil-works/pi-coding-agent` (`parseSessionEntries`, a PURE function —
// no fs, no agent engine, no TUI). Roost keeps only this thin entry→ChatMessage
// adapter, mapped onto @roost/shared's ContentBlock union. When the format
// evolves, the SDK owns the migration; unknown/future entry types degrade to
// `null` here (chat.parse_skip diag), never a crash.
//
// The adapter mirrors agegr/pi-web's lib/session-reader.ts + lib/normalize.ts
// (MIT), extended with Roost's tool-lifecycle / image / truncation behavior.
//
// NOT delegated: `SessionManager.open` (fs + async blob resolution + full-file
// re-reads) — wrong fit for Roost's resumable byte-offset tailer. And NOT
// `migrateSessionEntries`: it is a whole-FILE op (reads `session.version` from
// the header, rebuilds the id/parentId tree) and per single line it mis-detects
// v1 and RANDOMIZES entry.id — which would break id-keyed block fetch. The live
// corpus is uniformly session-version 3 (== CURRENT_SESSION_VERSION), so no
// migration is needed on the tailer's per-line path.
//
// Tolerant by design: unknown/ill-formed lines → null (never throw).

import { parseSessionEntries, type FileEntry } from "@earendil-works/pi-coding-agent";
import {
  RAW_CAP, TRUNC_CAP,
  type ChatMessage, type ContentBlock,
} from "@roost/shared/chat/wire";
import { diag } from "@roost/shared";

const HOME = process.env.HOME ?? "";

type Rec = Record<string, unknown>;
const isRec = (x: unknown): x is Rec => typeof x === "object" && x !== null;
const asStr = (x: unknown): string | undefined => (typeof x === "string" ? x : undefined);
const asRec = (x: unknown): Rec | undefined => (isRec(x) ? x : undefined);
const asNum = (x: unknown): number | undefined => (typeof x === "number" && Number.isFinite(x) ? x : undefined);

/** A transcript entry as it actually appears on disk. The SDK's `SessionEntry`
 *  union is NOT used: `@earendil-works/pi-coding-agent` 0.82 is a different,
 *  older package than the agent that writes these files (omp 17), and its union
 *  omits `mode_change`, `session_init`, `ttsr_injection`, `title`,
 *  `mcp_tool_selection`, `service_tier_change` and more. Only the four fields
 *  every entry carries are typed; everything else is read through the `as*`
 *  helpers, so a future entry type degrades to `null` instead of a type error. */
type OmpEntry = { type: string; id: string; parentId: string | null; timestamp: string } & Rec;

// ─── Assistant turn-ending notice ─────────────────────────────────────────
// Lives in @roost/shared/chat/assistant-notice: the parity oracle's row
// projection needs the same decision, and both the worker and the browser
// project rows with that one copy.
import { resolveAssistantNotice } from "@roost/shared/chat/assistant-notice";
export { resolveAssistantNotice };

/** Resolve an omp image block → absolute blob path (or data URL) + mime.
 *  omp writes images as:
 *    - { data: "blob:sha256:<hex>", mimeType }           (file-backed blob)
 *    - { source: { type: "base64", data, media_type } }  (inline base64)
 *    - { data: "<base64>", mimeType }                     (legacy inline)
 *  Returns null when the shape is unrecognized. */
function resolveImage(b: Rec): { blobPath: string; mime: string } | null {
  const mime = asStr(b.mimeType) ?? asStr(b.mediaType) ?? asStr(b.mime) ?? "image/png";
  const data = b.data;
  if (typeof data === "string" && data.startsWith("blob:sha256:")) {
    const hash = data.slice("blob:sha256:".length);
    return { blobPath: `${HOME}/.omp/agent/blobs/${hash}`, mime };
  }
  const src = asRec(b.source);
  if (src && src.type === "base64" && typeof src.data === "string") {
    const m = asStr(src.media_type) ?? mime;
    return { blobPath: `data:${m};base64,${src.data}`, mime: m };
  }
  if (typeof data === "string" && data.length > 64 && /^[A-Za-z0-9+/=\r\n]+$/.test(data)) {
    return { blobPath: `data:${mime};base64,${data}`, mime };
  }
  return null;
}

type Capped = { text: string; truncated: boolean; fullLen: number };
type Cap = (text: string) => Capped;

/** Truncate text to TRUNC_CAP, returning {text, truncated, fullLen}. */
function capText(text: string): Capped {
  const fullLen = text.length;
  if (fullLen <= TRUNC_CAP) return { text, truncated: false, fullLen };
  return { text: text.slice(0, TRUNC_CAP), truncated: true, fullLen };
}

/** Identity cap — full text, used by fullBlockText for the untruncated fetch. */
const fullCap: Cap = (text) => ({ text, truncated: false, fullLen: text.length });

/** Map an omp content block (untrusted) → ContentBlock, or null to skip. */
function mapContent(b: unknown, cap: Cap): ContentBlock | null {
  const blk = asRec(b);
  if (!blk) return null;
  const t = blk.type;
  if (t === "text") {
    const text = asStr(blk.text);
    if (text === undefined) return null;
    // assistant/user text is sent in full (bare string in proto, no truncation flag).
    return { kind: "text", text };
  }
  if (t === "thinking") {
    const thinking = asStr(blk.thinking);
    if (thinking === undefined) return null;
    return { kind: "thinking", ...cap(thinking) };
  }
  if (t === "toolCall") {
    // normalizeToolCalls tolerance (pi-web): accept toolCallId/toolName aliases.
    const id = asStr(blk.id) ?? asStr(blk.toolCallId);
    const name = asStr(blk.name) ?? asStr(blk.toolName);
    if (id === undefined || name === undefined) return null;
    // omp `arguments` is a JSON object; stringify for the wire. The renderer
    // prettifies, so key order is cosmetic.
    let argsJson = "";
    const args = blk.arguments;
    if (args !== undefined && args !== null) {
      try { argsJson = typeof args === "string" ? args : JSON.stringify(args); }
      catch { argsJson = String(args); }
    }
    return { kind: "toolCall", callId: id, name, argsJson };
  }
  if (t === "image") {
    const img = resolveImage(blk);
    if (!img) return null;
    return { kind: "image", blobPath: img.blobPath, mime: img.mime };
  }
  return null;
}

/** Map an omp AgentMessage (transcript `message` entry OR RPC message_end
 *  payload — same shape) → ChatMessage by role. Exported for the native RPC
 *  chat path (rpc-chat.ts). */
export function mapAgentMessage(message: unknown, id: string, parentId: string, ts: string): ChatMessage | null {
  return mapMessage(message, id, parentId, ts, capText);
}

/** Same mapping WITHOUT truncation. The native RPC path has no transcript
 *  entry id to re-read by (AgentEvent messages carry none), so it keeps the
 *  full text alongside the capped message to serve SessionsGetChatBlock. */
export function mapAgentMessageFull(message: unknown, id: string, parentId: string, ts: string): ChatMessage | null {
  return mapMessage(message, id, parentId, ts, fullCap);
}

/** The omp ToolResultMessage envelope <omp-tool-view> renders, as JSON.
 *  `details` is the per-tool structured payload (diff hunks, grep hits, todo
 *  lists) every rich renderer reads — it is the reason this exists at all.
 *  Over RAW_CAP the envelope is rebuilt without `details` and with the text
 *  capped, so the output is ALWAYS valid JSON, never a sliced string. */
function toolResultRawJson(
  callId: string, name: string, isError: boolean,
  texts: readonly Rec[], details: unknown, joined: string,
): string {
  let raw = "";
  try { raw = JSON.stringify({ toolCallId: callId, toolName: name, isError, content: texts, details }); }
  catch { raw = ""; }
  if (raw && raw.length <= RAW_CAP) return raw;
  // An oversized `details` alone can blow the cap while the text is short, so
  // only claim truncation when the text is actually the part being cut.
  const capped = joined.length > TRUNC_CAP
    ? `${joined.slice(0, TRUNC_CAP)}\n… (truncated, ${joined.length - TRUNC_CAP} more characters)`
    : joined;
  return JSON.stringify({
    toolCallId: callId, toolName: name, isError,
    content: [{ type: "text", text: capped }],
  });
}

/** Map an omp message `content` → ContentBlocks, preserving SOURCE ORDER.
 *  Order is load-bearing: text/thinking that follows a toolCall must render
 *  after that tool's card (omp's splitAssistantMessageToolTimeline), and both
 *  the pane and @roost/shared/chat/rows.ts's oracle derive that split by walking this array.
 *  Array-only by design: omp types assistant content as a block array, and a
 *  bare string is legal for USER turns alone (userMessageText) — see mapMessage. */
function mapContentList(content: unknown, cap: Cap): ContentBlock[] {
  const blocks: ContentBlock[] = [];
  if (Array.isArray(content)) {
    for (const b of content) {
      const mapped = mapContent(b, cap);
      if (mapped) blocks.push(mapped);
    }
  }
  return blocks;
}

/** Extension-injected card (advisor / irc / async-result / hook / skill …).
 *  omp's own fallback renderer is label + markdown body, so that IS parity;
 *  `details` rides along untruncated-or-absent for per-type rich cards. */
function customCardBlock(customType: string, content: unknown, details: unknown, cap: Cap): ContentBlock | null {
  const body = cap(customMessageText(content));
  let detailsJson = "";
  if (details !== undefined) {
    try {
      const raw = JSON.stringify(details);
      if (raw !== undefined && raw.length <= RAW_CAP) detailsJson = raw;
    } catch { detailsJson = ""; }
  }
  if (body.text.length === 0 && detailsJson.length === 0) return null;
  return {
    kind: "custom", customType, text: body.text, detailsJson,
    truncated: body.truncated, fullLen: body.fullLen,
  };
}

/** Map a `message` entry → ChatMessage by role (or null).
 *  Covers every role omp's ChatTranscriptBuilder.#appendChatMessage paints. */
function mapMessage(message: unknown, id: string, parentId: string, ts: string, cap: Cap): ChatMessage | null {
  const m = asRec(message);
  if (!m) return null;

  const role = m.role;

  // toolResult message: omp carries toolCallId/toolName/isError/details at the
  // MESSAGE level (not per content block). The whole envelope becomes ONE
  // toolResult block's rawJson; images stay separate blocks Roost renders itself.
  if (role === "toolResult") {
    const callId = asStr(m.toolCallId) ?? "";
    const name = asStr(m.toolName) ?? "";
    const isError = m.isError === true;
    const images: ContentBlock[] = [];
    const texts: Rec[] = [];
    let joined = "";
    const content = m.content;
    if (Array.isArray(content)) {
      for (const b of content) {
        const br = asRec(b);
        if (!br) continue;
        if (br.type === "text") {
          const text = asStr(br.text);
          if (text === undefined) continue;
          texts.push(br);
          joined += joined ? `\n${text}` : text;
        } else if (br.type === "image") {
          const img = resolveImage(br);
          if (img) images.push({ kind: "image", blobPath: img.blobPath, mime: img.mime });
        }
      }
    }
    const { truncated, fullLen } = cap(joined);
    // `text` stays empty: the payload rides in rawJson, and shipping both would
    // double the frame. truncated/fullLen still describe the joined tool text.
    const result: ContentBlock = {
      kind: "toolResult", callId, name, isError, text: "", truncated, fullLen,
      rawJson: toolResultRawJson(callId, name, isError, texts, m.details, joined),
    };
    return { id, parentId, ts, role: "toolResult", synthetic: false, blocks: [result, ...images] };
  }

  if (role === "user" || role === "assistant") {
    // A user turn may carry a bare string instead of blocks (omp's
    // userMessageText handles both); make it one text block rather than an
    // empty — and therefore dropped — message.
    const content = m.content;
    const blocks: ContentBlock[] = role === "user" && typeof content === "string"
      ? (content.length > 0 ? [{ kind: "text", text: content }] : [])
      : mapContentList(content, cap);
    // Turn-ending abort/error/recovered-retry line, appended LAST — omp paints
    // it below the turn's content (AssistantMessageComponent.updateContent).
    // Unlike omp we keep the standalone row even when the turn carried tool
    // calls (omp folds the text into each tool card instead): synthesizing a
    // toolResult block would risk duplicating the call's real result, and the
    // shape fires on 6 of 39_155 assistant messages in the live corpus.
    if (role === "assistant") {
      const notice = resolveAssistantNotice(m);
      if (notice) blocks.push({ kind: "notice", text: notice.text, level: notice.level });
    }
    if (blocks.length === 0) return null; // empty message — nothing to render
    // Agent-attributed input (advisor "Session update" replays). omp collapses
    // these behind CollapsedSyntheticMessageComponent; the pane does the same.
    return { id, parentId, ts, role, blocks, synthetic: role === "user" && m.synthetic === true };
  }

  // `!cmd` / `!py` composer runs — their own transcript rows in omp
  // (BashExecutionComponent / EvalExecutionComponent).
  if (role === "bashExecution" || role === "pythonExecution") {
    const bash = role === "bashExecution";
    const out = cap(asStr(m.output) ?? "");
    return {
      id, parentId, ts, role: "developer", synthetic: false,
      blocks: [{
        kind: "exec",
        lang: bash ? "bash" : "python",
        command: (bash ? asStr(m.command) : asStr(m.code)) ?? "",
        output: out.text,
        exitCode: asNum(m.exitCode) ?? -1,
        cancelled: m.cancelled === true,
        excluded: m.excludeFromContext === true,
        truncated: out.truncated,
        fullLen: out.fullLen,
      }],
    };
  }

  // `@path` mentions attached to a prompt → omp's "Read <path>" rows.
  if (role === "fileMention") {
    const paths: string[] = [];
    const files = m.files;
    if (Array.isArray(files)) {
      for (const f of files) {
        const p = typeof f === "string" ? f : asStr(asRec(f)?.path) ?? "";
        if (p) paths.push(p);
      }
    }
    if (paths.length === 0) return null;
    return { id, parentId, ts, role: "developer", synthetic: false, blocks: [{ kind: "fileMention", paths }] };
  }

  if (role === "custom" || role === "hookMessage") {
    if (m.display !== true) return null;
    const card = customCardBlock(asStr(m.customType) ?? "", m.content, m.details, cap);
    return card ? { id, parentId, ts, role: "developer", synthetic: false, blocks: [card] } : null;
  }

  // developer: parity, not a gap — omp computes textContent = "" for it and
  // paints nothing.
  diag("chat.parse_skip", {
    reason: role === "developer" ? "developer_suppressed" : "unknown_role",
    role: String(role),
  });
  return null;
}

/** Extract the joined text of a custom_message `content` (string or block array). */
function customMessageText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  let out = "";
  for (const b of content) {
    const br = asRec(b);
    if (br && br.type === "text") out += asStr(br.text) ?? "";
  }
  return out;
}

/** Entry → ChatMessage, or null to skip. `cap` controls text truncation
 *  (capText for the live stream, fullCap for the untruncated block fetch). */
function entryToChatMessage(entry: OmpEntry, cap: Cap = capText): ChatMessage | null {
  const id = entry.id;
  const parentId = entry.parentId ?? "";
  const ts = entry.timestamp;

  switch (entry.type) {
    case "message":
      return mapMessage(entry.message, id, parentId, ts, cap);

    // custom: Roost's tool-lifecycle value-add. Only tool_execution_start is
    // ever emitted (tool_execution_end: 0/corpus). Everything else → skip.
    // That is parity: omp renders a CustomEntry only when an extension
    // registered an entry renderer, and Roost cannot execute those.
    case "custom": {
      const ct = asStr(entry.customType);
      if (ct === "tool_execution_start") {
        const d = asRec(entry.data) ?? {};
        const callId = asStr(d.toolCallId);
        if (!callId) return null;
        const name = asStr(d.toolName) ?? "";
        const intent = asStr(d.intent) ?? "";
        return { id, parentId, ts, role: "assistant", synthetic: false, blocks: [{ kind: "toolEvent", callId, name, phase: "start", intent, output: "" }] };
      }
      diag("chat.parse_skip", { reason: "unknown_custom", type: String(ct) });
      return null;
    }

    // compaction: top-level entry (NOT custom). omp paints a collapsible
    // CompactionSummaryMessageComponent headed by the pre-compaction token count.
    case "compaction": {
      const body = cap(asStr(entry.summary) ?? "");
      return {
        id, parentId, ts, role: "developer", synthetic: false,
        blocks: [{
          kind: "summary", variant: "compaction", text: body.text,
          tokensBefore: asNum(entry.tokensBefore) ?? 0,
          truncated: body.truncated, fullLen: body.fullLen,
        }],
      };
    }

    // branch_summary: the returned-from-subagent digest (BranchSummaryMessageComponent).
    case "branch_summary": {
      const body = cap(asStr(entry.summary) ?? "");
      return {
        id, parentId, ts, role: "developer", synthetic: false,
        blocks: [{
          kind: "summary", variant: "branch", text: body.text, tokensBefore: 0,
          truncated: body.truncated, fullLen: body.fullLen,
        }],
      };
    }

    // custom_message: extension-injected context (advisor, irc, async-result…).
    // Surface only when omp marks it shown (display === true).
    case "custom_message": {
      if (entry.display !== true) return null;
      const card = customCardBlock(asStr(entry.customType) ?? "", entry.content, entry.details, cap);
      return card ? { id, parentId, ts, role: "developer", synthetic: false, blocks: [card] } : null;
    }

    // thinking_level_change / model_change / service_tier_change / label /
    // title_change / mode_change / ttsr_injection / session_init /
    // mcp_tool_selection / … → footer state, not transcript rows, in both
    // systems. parseOmpStatusDelta folds the ones that matter into the chips.
    default:
      return null;
  }
}

/** Take the first non-header entry from a parsed line (drops SessionHeader). */
function firstEntry(line: string): OmpEntry | null {
  let entries: FileEntry[];
  try { entries = parseSessionEntries(line); }
  catch { diag("chat.parse_skip", { reason: "json_error" }); return null; }
  const entry = entries.find((e) => e.type !== "session");
  return (entry as OmpEntry | undefined) ?? null;
}

/** Parse one JSONL line → ChatMessage, or null if the line is not conversational.
 *  Never throws. Session/title/metadata lines → null (not part of the thread). */
export function parseOmpLine(line: string): ChatMessage | null {
  const entry = firstEntry(line);
  return entry ? entryToChatMessage(entry) : null;
}

/** Parse a transcript line fully (no truncation) and return the text of the
 *  block at blockIndex, using the SAME entry mapping as parseOmpLine so block
 *  indices stay aligned with the streamed message. Returns null if absent.
 *  toolResult blocks carry no text — their payload rides in rawJson, whole. */
export function fullBlockText(line: string, blockIndex: number): string | null {
  const entry = firstEntry(line);
  if (!entry) return null;
  const msg = entryToChatMessage(entry, fullCap);
  const blk = msg?.blocks[blockIndex];
  if (!blk) return null;
  switch (blk.kind) {
    case "text":
    case "thinking":
    case "summary":
    case "custom":
      return blk.text;
    case "exec":
      return blk.output;
    case "toolCall":
      return blk.argsJson;
    // notice / fileMention / image / toolResult / toolEvent / approval carry
    // nothing truncatable — toolResult's payload rides whole in rawJson.
    default:
      return null;
  }
}

export { TRUNC_CAP };
