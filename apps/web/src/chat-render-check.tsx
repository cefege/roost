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
import "./styles/voice-input.css";
import { applyTheme, loadTheme } from "./lib/theme.ts";
import { rootStore, setRootStore } from "./store/root.ts";
import { setOmpChatView } from "./store/uiStore.ts";
import { OmpChatPane } from "./components/chat/omp/OmpChatPane.tsx";
import type { ChatMessage } from "@roost/shared/chat/wire";

applyTheme(loadTheme());

const SID = "00000000-0000-0000-0000-000000000001";

// Force omp eligibility + chat view mode.
setRootStore("terminal_title", SID, "π: render check");
setOmpChatView(SID, "chat");

// A realistic conversation: user msg, assistant (thinking+text+toolCall),
// a separate toolResult message, and a live toolEvent (running chip).
const msgs: ChatMessage[] = [
  {
    id: "u1", parentId: "", ts: "2026-07-24T10:00:00Z", role: "user",
    blocks: [{ kind: "text", text: "**Read** this file and summarize." }],
  },
  {
    id: "a1", parentId: "u1", ts: "2026-07-24T10:00:01Z", role: "assistant",
    blocks: [
      { kind: "thinking", text: "Let me read the file the user asked about.", truncated: false, fullLen: 41 },
      { kind: "text", text: "Reading `README.md` now." },
      { kind: "toolCall", callId: "call_1", name: "read", argsJson: '{"path":"README.md"}' },
    ],
  },
  {
    id: "tr1", parentId: "a1", ts: "2026-07-24T10:00:02Z", role: "toolResult",
    blocks: [{ kind: "toolResult", callId: "call_1", name: "read", isError: false, text: "# README\n\nThis is a test project.\n\n## Install\n\nRun `bun install`.", truncated: false, fullLen: 56 }],
  },
  {
    id: "a2", parentId: "tr1", ts: "2026-07-24T10:00:03Z", role: "assistant",
    blocks: [
      { kind: "text", text: "The README describes a test project with a simple `bun install` setup." },
      { kind: "toolCall", callId: "call_2", name: "edit", argsJson: '{"path":"README.md","content":"updated"}' },
    ],
  },
  // A live tool_event (start, no end/result) → "running" chip.
  {
    id: "e1", parentId: "a2", ts: "2026-07-24T10:00:04Z", role: "assistant",
    blocks: [{ kind: "toolEvent", callId: "call_2", name: "edit", phase: "update", intent: "Editing", output: "beat 1\nbeat 2\nbeat 3\n" }],
  },
  // Native-engine approvals: pending (buttons) + already answered (static).
  {
    id: "ap1", parentId: "e1", ts: "2026-07-24T10:00:05Z", role: "developer",
    blocks: [{
      kind: "approval", requestId: "ui-1", method: "confirm", title: "Run bash",
      message: "rm -rf /tmp/roost-approval-probe", options: [], resolved: false, answer: "",
    }],
  },
  {
    id: "ap2", parentId: "ap1", ts: "2026-07-24T10:00:06Z", role: "developer",
    blocks: [{
      kind: "approval", requestId: "ui-0", method: "select", title: "Pick a model",
      message: "", options: ["sonnet", "opus"], resolved: true, answer: "opus",
    }],
  },
];

setRootStore("chat_omp", SID, { messages: msgs, seq: 5, status: "resolved", streaming: true, model: "anthropic/claude-opus-5", contextPct: 3, contextTokens: 31240 });

render(() => <OmpChatPane sessionId={SID} />, document.getElementById("app")!);
