// Hook → agent-event pipeline: buildHooksSettings shape, the UDS
// round-trip (cli/hook.ts wire shape → startHookListener → HookPatch),
// and readLastAssistantText (transcript JSONL tail parse — powers
// needsAttention's finished-with-unseen-output trigger).
//
// Regression for the shadow-ClaudeBridge era: hooks MUST target the PTY
// claude and every patch MUST carry a correct status (STATUS CONTRACT in
// claude/hooks.ts — agent.status shadows the claude_status scrape).

import { describe, test, expect, afterAll } from "bun:test";
import { createConnection } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { writeFileSync, rmSync, mkdtempSync } from "node:fs";
import { buildHooksSettings, startHookListener, readLastAssistantText, type HookPatch } from "../src/claude/hooks.ts";

const TMP = mkdtempSync(join(tmpdir(), "roost-hooks-test-"));
afterAll(() => { try { rmSync(TMP, { recursive: true, force: true }); } catch { /* ignore */ } });

describe("buildHooksSettings", () => {
  test("emits only real+handled events, each exec'ing the hook cmd", () => {
    const json = JSON.parse(buildHooksSettings("/tmp/hook.sock", '"/usr/bin/bun" run "/x/hook.ts"'));
    const events = Object.keys(json.hooks);
    expect(events.sort()).toEqual(
      ["Notification", "PreToolUse", "SessionStart", "SessionEnd", "Stop", "UserPromptSubmit"].sort(),
    );
    for (const ev of events) {
      const cmd = json.hooks[ev][0].hooks[0].command as string;
      expect(cmd.startsWith('"/usr/bin/bun" run "/x/hook.ts" ')).toBe(true);
    }
    expect(json.roostHookSocket).toBe("/tmp/hook.sock");
  });
});

describe("hook UDS round-trip → HookPatch", () => {
  // One JSON line per connection — same wire shape cli/hook.ts writes.
  function postHookLine(socketPath: string, line: object): Promise<void> {
    return new Promise((resolve, reject) => {
      const sock = createConnection(socketPath, () => {
        sock.write(JSON.stringify(line) + "\n", () => { sock.destroy(); resolve(); });
      });
      sock.on("error", reject);
    });
  }

  async function roundTrip(msg: object): Promise<HookPatch> {
    const socketPath = join(TMP, `hook-${Math.random().toString(36).slice(2)}.sock`);
    return await new Promise<HookPatch>((resolve, reject) => {
      const stop = startHookListener(socketPath, (patch) => { stop(); resolve(patch); });
      postHookLine(socketPath, msg).catch(reject);
      setTimeout(() => reject(new Error("no HookPatch in 2s")), 2000);
    });
  }

  test("prompt-submit → running, tool cleared", async () => {
    const p = await roundTrip({ event: "prompt-submit", agent: "sid-1", payload: {} });
    expect(p.sessionId).toBe("sid-1");
    expect(p.agentPatch.status).toBe("running");
    expect(p.agentPatch.current_tool).toBeNull();
  });

  test("pre-tool-use carries current_tool; AskUserQuestion → needs-input", async () => {
    const bash = await roundTrip({ event: "pre-tool-use", agent: "sid-1", payload: { tool_name: "Bash" } });
    expect(bash.agentPatch.status).toBe("running");
    expect(bash.agentPatch.current_tool).toEqual({ name: "Bash", input_summary: "" });

    const ask = await roundTrip({ event: "pre-tool-use", agent: "sid-1", payload: { tool_name: "AskUserQuestion" } });
    expect(ask.agentPatch.status).toBe("needs-input");
  });

  test("permission notification → needs-input; idle nag → dropped", async () => {
    const perm = await roundTrip({ event: "notification", agent: "sid-1", payload: { message: "Claude needs your permission to use Bash" } });
    expect(perm.agentPatch.status).toBe("needs-input");
    expect(perm.agentPatch.last_message?.text).toContain("permission");

    // Generic idle nag emits NO patch: needs-input would park every idle
    // claude in the strip forever; last_message would overwrite the stop
    // hook's transcript tail with boilerplate. Assert the nag produces no
    // callback while a follow-up permission event still does.
    const socketPath = join(TMP, `hook-nag-${Math.random().toString(36).slice(2)}.sock`);
    const patches: HookPatch[] = [];
    const stop = startHookListener(socketPath, (p) => patches.push(p));
    await postHookLine(socketPath, { event: "notification", agent: "sid-1", payload: { message: "Claude is waiting for your input" } });
    await postHookLine(socketPath, { event: "notification", agent: "sid-1", payload: { message: "Claude needs your permission to use Read" } });
    await new Promise((r) => setTimeout(r, 300));
    stop();
    expect(patches.length).toBe(1);
    expect(patches[0].agentPatch.status).toBe("needs-input");
  });

  test("stop → idle with last_message from transcript tail", async () => {
    const transcript = join(TMP, "transcript.jsonl");
    writeFileSync(transcript, [
      JSON.stringify({ type: "user", message: { content: [{ type: "text", text: "hi" }] } }),
      JSON.stringify({ type: "assistant", message: { content: [{ type: "text", text: "final answer text" }] } }),
      JSON.stringify({ type: "system", subtype: "turn_end" }),
    ].join("\n"));
    const p = await roundTrip({ event: "stop", agent: "sid-1", payload: { transcript_path: transcript } });
    expect(p.agentPatch.status).toBe("idle");
    expect(p.agentPatch.current_tool).toBeNull();
    expect(p.agentPatch.last_message?.text).toBe("final answer text");
  });
});

describe("readLastAssistantText", () => {
  test("skips tool_use-only assistant entries, joins text blocks, truncates at 300", async () => {
    const transcript = join(TMP, "transcript2.jsonl");
    const long = "x".repeat(400);
    writeFileSync(transcript, [
      JSON.stringify({ type: "assistant", message: { content: [{ type: "text", text: long }] } }),
      JSON.stringify({ type: "assistant", message: { content: [{ type: "tool_use", name: "Bash" }] } }),
    ].join("\n"));
    const text = await readLastAssistantText(transcript);
    expect(text).toBe("x".repeat(300));
  });

  test("missing file / empty path → null", async () => {
    expect(await readLastAssistantText("")).toBeNull();
    expect(await readLastAssistantText(join(TMP, "nope.jsonl"))).toBeNull();
  });
});
