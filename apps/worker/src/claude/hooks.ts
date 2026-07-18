// Claude --settings hooks JSON builder + UDS listener for hook callbacks.
// The PTY claude (keeper-spawned, the process the user drives) carries
// --settings from buildHooksSettings; each hook execs cli/hook.ts which
// POSTs one JSON line to the worker's hook UDS ($ROOST_HOOK_SOCKET), tagged
// with $ROOST_SURFACE_ID (the roost session id). handleHookLine translates
// events into AgentState patches consumed by SessionManager.applyAgentPatch.
//
// STATUS CONTRACT: liveStatus in the SPA prefers agent.status over the
// claude_status scrape, and foldEvent's defaultAgentState fills
// status:"running" on any partial merge — so every patch emitted here MUST
// carry the correct status (incl. needs-input for AskUserQuestion +
// permission Notifications) or it shadows the scrape and breaks
// needsAttention (needs-you strip, project_sidebar_one_mode_strip_plus_folders).
//
// Callers: main.ts (startHookListener), session-manager.ts (buildHooksSettings).
//
// ─── AGENT-INTEGRATION-POINT ────────────────────────────────────────────────
// This is the per-agent ADAPTER layer — where additional coding agents (Codex,
// opencode, aider, gemini, …) plug in ALONGSIDE Claude Code: native hooks
// where the agent has them (claude --settings → this file), else the generic
// terminal-scrape fallback (apps/worker/src/detect/). All adapters feed the
// SAME downstream (AgentState SessionEvents → coord projection → sidebar).
// To add an agent: a hooks-sibling here + a `kind` in the wire Session + a
// spawn branch in session-manager.ts.

import { createServer, type Socket } from "node:net";
import { existsSync, unlinkSync } from "node:fs";
import { log } from "@roost/shared";
import type { AgentState } from "@roost/shared";

// ─── hooks settings JSON ────────────────────────────────────────────

const HOOK_SOCKET_ENV = "ROOST_HOOK_SOCKET";

// PascalCase claude hook event → kebab subcommand (cli/hook.ts argv[2]).
// ONLY events that are real Claude Code hooks AND handled below — every
// entry costs one bun process per firing.
const HOOK_EVENTS: Array<[string, string]> = [
  ["SessionStart", "session-start"],
  ["SessionEnd", "session-end"],
  ["UserPromptSubmit", "prompt-submit"],
  ["Stop", "stop"],
  ["PreToolUse", "pre-tool-use"],
  ["Notification", "notification"],
];

/** Build the JSON string passed as --settings to claude. */
export function buildHooksSettings(socketPath: string, hookCmd: string): string {
  const hooks: Record<string, unknown> = {};
  for (const [pascal, kebab] of HOOK_EVENTS) {
    hooks[pascal] = [{ matcher: "", hooks: [{ type: "command", command: `${hookCmd} ${kebab}` }] }];
  }
  return JSON.stringify({
    hooks,
    preferredNotifChannel: "notifications_disabled",
    roostHookSocket: socketPath,
  });
}

// ─── hook listener ──────────────────────────────────────────────────

export type HookPatch = {
  sessionId?: string;
  agentPatch: Partial<AgentState>;
};

type PatchCallback = (patch: HookPatch) => void;

/**
 * Start UDS server. Accepts one JSON line per connection.
 * Translates each hook event into a HookPatch callback.
 * Returns a teardown function.
 */
export function startHookListener(socketPath: string, onPatch: PatchCallback): () => void {
  if (existsSync(socketPath)) { try { unlinkSync(socketPath); } catch { /* ignore */ } }

  const server = createServer((socket: Socket) => {
    let buf = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      buf += chunk;
      const idx = buf.indexOf("\n");
      if (idx === -1) return;
      const line = buf.slice(0, idx).trim();
      buf = buf.slice(idx + 1);
      if (!line) return;
      void handleHookLine(line, onPatch);
    });
    socket.on("error", () => { /* ignore */ });
  });

  server.listen(socketPath);
  server.on("error", (err) => log.error("hook-listener", "server error", { error: String(err) }));
  log.info("hook-listener", "bound", { socketPath });

  return () => { server.close(); };
}

async function handleHookLine(line: string, onPatch: PatchCallback): Promise<void> {
  let msg: Record<string, unknown>;
  try { msg = JSON.parse(line); } catch { return; }

  const event = String(msg.event ?? "");
  const payload = (msg.payload ?? {}) as Record<string, unknown>;
  const sessionId = msg.agent ? String(msg.agent) : undefined;

  switch (event) {
    case "session-start":
      // Booted, sitting at the prompt awaiting input.
      onPatch({ sessionId, agentPatch: { kind: "claude", status: "idle" } });
      break;
    case "prompt-submit":
      onPatch({ sessionId, agentPatch: { kind: "claude", status: "running", current_tool: null } });
      break;
    case "pre-tool-use": {
      const toolName = String(payload.tool_name ?? "");
      // AskUserQuestion blocks on the user — needs-input, not running
      // (STATUS CONTRACT above: this must match what the scrape would say).
      const status = toolName === "AskUserQuestion" ? "needs-input" : "running";
      onPatch({ sessionId, agentPatch: {
        kind: "claude",
        status,
        current_tool: toolName ? { name: toolName, input_summary: "" } : null,
      }});
      break;
    }
    case "stop": {
      // Turn finished. last_message (tail of the transcript) is what powers
      // needsAttention's finished-with-unseen-output trigger + row subtitles.
      // Claude appends the final assistant entry to the transcript AROUND
      // Stop-hook dispatch (verified live 2026-07-04: read-at-hook-time was
      // consistently one turn behind). hook.ts already exited — this delay
      // blocks nothing; it just lets the write land before the tail read.
      await Bun.sleep(STOP_TRANSCRIPT_SETTLE_MS);
      const text = await readLastAssistantText(String(payload.transcript_path ?? ""));
      onPatch({ sessionId, agentPatch: {
        kind: "claude",
        status: "idle",
        current_tool: null,
        ...(text ? { last_message: { role: "assistant" as const, text, ts: Date.now() } } : {}),
      }});
      break;
    }
    case "session-end":
      onPatch({ sessionId, agentPatch: { kind: "claude", status: "done", current_tool: null } });
      break;
    case "notification": {
      // Only permission waits patch: needs-input (blocked until answered;
      // strip trigger-1, does NOT clear on view). The generic idle nag
      // ("Claude is waiting for your input", fires after every unattended
      // turn) is DROPPED: as needs-input it parks every idle claude in the
      // needs-you strip forever (trigger-1 ignores seen-state), and as
      // last_message it overwrites the stop hook's transcript tail — the
      // actual finished-with-unseen signal — with boilerplate.
      const text = String(payload.message ?? payload.text ?? "");
      if (!/permission/i.test(text)) break;
      onPatch({ sessionId, agentPatch: {
        kind: "claude",
        status: "needs-input",
        last_message: { role: "assistant", text, ts: Date.now() },
      }});
      break;
    }
    default:
      log.debug("hook-listener", "unhandled event", { event });
  }
}

// Last assistant text from the claude transcript JSONL — reads only the
// final 64 KB so Stop hooks on long sessions stay cheap.
const STOP_TRANSCRIPT_SETTLE_MS = 1000;
const TRANSCRIPT_TAIL_BYTES = 65536;
const LAST_MESSAGE_MAX_CHARS = 300;

export async function readLastAssistantText(transcriptPath: string): Promise<string | null> {
  if (!transcriptPath) return null;
  try {
    const file = Bun.file(transcriptPath);
    const size = file.size;
    const tail = await file.slice(Math.max(0, size - TRANSCRIPT_TAIL_BYTES)).text();
    const lines = tail.trimEnd().split("\n");
    for (let i = lines.length - 1; i >= 0; i--) {
      let obj: Record<string, unknown>;
      try { obj = JSON.parse(lines[i]); } catch { continue; } // first line may be a partial record
      if (obj?.type !== "assistant") continue;
      const content = (obj.message as Record<string, unknown> | undefined)?.content;
      if (!Array.isArray(content)) continue;
      const text = content
        .filter((c): c is { type: string; text: string } => c?.type === "text" && typeof c.text === "string")
        .map((c) => c.text)
        .join("\n")
        .trim();
      if (text) return text.length > LAST_MESSAGE_MAX_CHARS ? text.slice(0, LAST_MESSAGE_MAX_CHARS) : text;
    }
  } catch (e) {
    log.debug("hook-listener", "transcript tail read failed", { transcriptPath, error: String(e) });
  }
  return null;
}
