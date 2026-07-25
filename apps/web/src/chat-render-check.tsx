// Standalone render check — mounts OmpChatPane with a realistic synthetic
// conversation to prove the Solid UI render path (bubbles, ToolCard inline
// result, ThinkingBlock, Composer) with the REAL components + store. No coord
// auth needed: backfill fails → status "resolved" → injected data renders.
// Served by vite at /chat-render-check.html.

import { render } from "solid-js/web";
// Load the SAME tokens + globals main.tsx does. Without these the harness
// renders in Times on a transparent background and every design judgement
// made against it is wrong.
import "./styles/theme-vars.css";
import "./styles/syntax-vars.css";
// sidebar.css carries the @font-face for "Material Symbols Rounded" (see
// Settings/md/icon.css header) — without it every Icon paints its ligature
// name as literal text.
import "./styles/sidebar.css";
import "./styles/voice-input.css";
// Registers <omp-tool-view>, exactly as main.tsx does — without it every tool
// card is an empty custom element.
import "./vendor/omp-tool-views.js";
import { applyTheme, loadTheme } from "./lib/theme.ts";
import { rootStore, setRootStore } from "./store/root.ts";
import { OmpChatPane } from "./components/chat/omp/OmpChatPane.tsx";
import type { ChatMessage } from "@roost/shared/chat/wire";

applyTheme(loadTheme());

const SID = "00000000-0000-0000-0000-000000000001";

// The pane renders for a kind:"agent" session; the harness has no store
// session at all, so nothing to force — it drives OmpChatPane directly.
setRootStore("terminal_title", SID, "π > render check");

// A realistic conversation: user msg, assistant (thinking+text+toolCall),
// a separate toolResult message, and a live toolEvent (running chip).
const msgs: ChatMessage[] = [
  {
    id: "u1", parentId: "", ts: "2026-07-24T10:00:00Z", role: "user", synthetic: false,
    blocks: [{ kind: "text", text: "**Read** this file and summarize." }],
  },
  {
    id: "a1", parentId: "u1", ts: "2026-07-24T10:00:01Z", role: "assistant", synthetic: false,
    blocks: [
      { kind: "thinking", text: "Let me read the file the user asked about.", truncated: false, fullLen: 41 },
      { kind: "text", text: "Reading `README.md` now." },
      { kind: "toolCall", callId: "call_1", name: "read", argsJson: '{"path":"README.md"}' },
    ],
  },
  {
    id: "tr1", parentId: "a1", ts: "2026-07-24T10:00:02Z", role: "toolResult", synthetic: false,
    blocks: [{
      kind: "toolResult", callId: "call_1", name: "read", isError: false,
      text: "", truncated: false, fullLen: 56,
      rawJson: JSON.stringify({
        toolCallId: "call_1", toolName: "read", isError: false,
        content: [{ type: "text", text: "# README\n\nThis is a test project.\n\n## Install\n\nRun `bun install`." }],
        details: { path: "README.md" },
      }),
    }],
  },
  {
    id: "a2", parentId: "tr1", ts: "2026-07-24T10:00:03Z", role: "assistant", synthetic: false,
    blocks: [
      { kind: "text", text: "The README describes a test project with a simple `bun install` setup." },
      { kind: "toolCall", callId: "call_2", name: "edit", argsJson: '{"path":"README.md","content":"updated"}' },
    ],
  },
  // A live tool_event (start, no end/result) → "running" chip.
  {
    id: "e1", parentId: "a2", ts: "2026-07-24T10:00:04Z", role: "assistant", synthetic: false,
    blocks: [{ kind: "toolEvent", callId: "call_2", name: "edit", phase: "update", intent: "Editing", output: "beat 1\nbeat 2\nbeat 3\n" }],
  },
  // Native-engine approvals: pending (buttons) + already answered (static).
  {
    id: "ap1", parentId: "e1", ts: "2026-07-24T10:00:05Z", role: "developer", synthetic: false,
    blocks: [{
      kind: "approval", requestId: "ui-1", method: "confirm", title: "Run bash",
      message: "rm -rf /tmp/roost-approval-probe", options: [], resolved: false, answer: "",
      richOptions: [], header: "", progress: "", multi: false,
    }],
  },
  // A pending ask multi-select — the selection card: header chip, progress,
  // checkboxes with one ticked, a recommended pill, descriptions, and the
  // Done/Next footer. The only way to eyeball it without a live omp.
  {
    id: "ap3", parentId: "ap1", ts: "2026-07-24T10:00:06Z", role: "developer", synthetic: false,
    blocks: [{
      kind: "approval", requestId: "ui-2", method: "select",
      title: "Which storage backends should ship in v1?",
      message: "", resolved: false, answer: "",
      header: "Storage", progress: "2/3", multi: true,
      options: ["SQLite", "PostgreSQL (Recommended)", "Redis", "Other (type your own)", "Next →"],
      richOptions: [
        { value: "SQLite", label: "SQLite", description: "Zero-config file database; the default for single-node deploys.", recommended: false, checked: true, role: "option" },
        { value: "PostgreSQL (Recommended)", label: "PostgreSQL", description: "Needed for the multi-writer coordination path.", recommended: true, checked: false, role: "option" },
        { value: "Redis", label: "Redis", description: "", recommended: false, checked: false, role: "option" },
        { value: "Other (type your own)", label: "Other (type your own)", description: "", recommended: false, checked: false, role: "other" },
        { value: "Next →", label: "Next →", description: "", recommended: false, checked: false, role: "next" },
      ],
    }],
  },
  {
    id: "ap2", parentId: "ap3", ts: "2026-07-24T10:00:07Z", role: "developer", synthetic: false,
    blocks: [{
      kind: "approval", requestId: "ui-0", method: "select", title: "Pick a model",
      message: "", options: ["sonnet", "opus"], resolved: true, answer: "opus",
      richOptions: [], header: "", progress: "", multi: false,
    }],
  },
];

// The cwd + branch chips read the SESSION record, not the chat frame (Roost
// already tracks both; omp's transcript carries neither), so the harness must
// seed one or half the status row silently renders nothing.
setRootStore("sessions", SID, {
  session_id: SID, cwd: "/Users/mike/Code/idea", git_branch: "main",
} as never);

setRootStore("chat_omp", SID, { messages: msgs, seq: 5, status: "resolved", streaming: true, model: "anthropic/claude-opus-5", modelName: "Claude Opus 5", thinkingLevel: "low", contextTokens: 31240, contextWindow: 1_000_000, mode: "plan" });

render(() => <OmpChatPane sessionId={SID} />, document.getElementById("app")!);
