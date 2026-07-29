// Headless AGENT smoke — the omp-RPC data plane end to end.
// Proves the whole new path in one pass: spawn a kind="agent" session (worker
// forks `omp --mode rpc-ui`), stream a real model turn back as AgentEntry
// frames, backfill the same entries over the unary RPC, and complete an
// approval round-trip (the one interaction that can wedge the agent forever if
// any link in the chain drops a frame).
//
// Two modes, no skips either way:
//   ROOST_COORD_URL set → run against that live coord (the api_smoke contract).
//   unset              → spin an isolated coord+worker on ephemeral ports via
//                        the terminal harness, with the REAL $HOME so the
//                        forked omp finds its model credentials.
// The isolated mode is the default because it guarantees the code under test is
// the code on disk, rather than a maybe-hot-reloaded shared dev coord.
//
// Every wait is a bounded poll — no fixed sleeps.
//
// This test costs real model tokens. It lives in the live-api profile for
// exactly that reason.

import { describe, test, expect } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { buildAuthorizedApiClient } from "../apps/roost-cli/src/api.ts";
import { loadWorkerConfig } from "../apps/worker/src/config.ts";
import { openSyncWs } from "../apps/roost-cli/src/sync-ws.ts";
// Relative imports, not "@roost/shared/...": the smoke workspace declares no
// @roost deps. Same rationale as api_smoke.test.ts.
import { protoToEvent } from "../apps/shared/src/wire/event-proto.ts";
import { agentEntryFromProto } from "../apps/shared/src/wire/agent-proto.ts";
import type { AgentEntry } from "../apps/shared/src/wire/agent-entry.ts";

import { startTerminalTestStack, type TerminalTestStack } from "./terminal/stack.ts";

// Same key resolution openSyncWs uses, so the Connect client and the firehose
// present the SAME identity — it self-authorizes once against a fresh coord.
const keyPath = (() => {
  const cfg = loadWorkerConfig();
  return existsSync(cfg.workerKeyPath) ? cfg.workerKeyPath : join(homedir(), ".roost", "cli-key");
})();

async function pollUntil<T>(
  label: string,
  fn: () => Promise<T | undefined | null | false>,
  timeoutMs = 20_000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let lastErr: unknown;
  for (;;) {
    try {
      const v = await fn();
      if (v) return v;
      lastErr = undefined;
    } catch (e) {
      lastErr = e;
    }
    if (Date.now() >= deadline) {
      throw new Error(
        `pollUntil timeout after ${timeoutMs}ms: ${label}` +
          (lastErr ? ` (last error: ${String(lastErr)})` : ""),
      );
    }
    // Real delay on purpose (ts-no-test-timers exception): polls a LIVE remote
    // coord; fake timers cannot advance the server.
    await Bun.sleep(100);
  }
}

const entryText = (e: AgentEntry): string =>
  e.kind === "user" || e.kind === "assistant" || e.kind === "thinking" || e.kind === "notice"
    ? e.text
    : "";

describe("agent smoke (headless)", () => {
  test("spawn agent → streamed turn → backfill → approval round-trip → kill", async () => {
    const t0 = performance.now();
    // openSyncWs reads the coord URL from the environment, so an isolated
    // stack has to publish its ephemeral URL there before the stream opens.
    let stack: TerminalTestStack | undefined;
    let coordinatorUrl = process.env.ROOST_COORD_URL;
    if (!coordinatorUrl) {
      stack = await startTerminalTestStack({ useRealHome: true });
      coordinatorUrl = stack.baseUrl;
      process.env.ROOST_COORD_URL = stack.baseUrl;
    }
    // A failure here would otherwise orphan the stack's coord/worker/keeper
    // children and its temp root, and leak a fresh set on every re-run.
    const c = await buildAuthorizedApiClient({ coordinatorUrl, keyPath, label: "agent-smoke" })
      .catch(async (e: unknown) => {
        if (stack) await stack.stop().catch(() => undefined);
        throw e;
      });

    const spawnedSessions = new Set<string>();
    // Live transcript, upserted by seq exactly as the SPA store does — this
    // collector IS the client-side contract under test.
    const entries = new Map<number, AgentEntry>();
    const updatesBySeq = new Map<number, AgentEntry[]>();
    const sessionEvents: Array<{ kind: string; session_id: string; status?: string }> = [];
    const ac = new AbortController();

    const findSession = async (sid: string) => {
      const { sessions } = await c.sessionsList({ status: "all" });
      return sessions.find((s) => s.id === sid);
    };

    try {
      // The isolated stack already waited for its own worker to be routable.
      const workerFp = stack
        ? stack.workerFp
        : (await c.workersList({})).routableFps[0];
      if (!workerFp) {
        throw new Error("agent smoke: no routable worker online on this coord — bring one up");
      }

      // ─── 1. Scratch folder for the agent's cwd ──────────────────────────
      const folder = `/tmp/agent-smoke-${crypto.randomUUID().slice(0, 8)}`;
      await c.filesMkdir({ workerFp, path: folder });

      // ─── 2. Sync stream FIRST, so the opened event and every agentEntries
      // frame for this session arrive live (sinceEventId:0 = no backfill). ──
      let sid = "";
      const collector = (async () => {
        try {
          const signal = AbortSignal.any([ac.signal, AbortSignal.timeout(180_000)]);
          for await (const frame of await openSyncWs({ signal })) {
            if (frame.frame.case === "sessionEvent") {
              const ev = protoToEvent(frame.frame.value as never) as
                | (Record<string, unknown> & { kind: string; session_id?: string })
                | null;
              if (ev?.session_id) {
                // The `agent` variant alone carries a patch; narrow rather
                // than assert, so a shape change fails loudly here.
                let status: string | undefined;
                if ("patch" in ev && ev.patch && typeof ev.patch === "object" && "status" in ev.patch) {
                  const raw = ev.patch.status;
                  status = typeof raw === "string" ? raw : undefined;
                }
                sessionEvents.push({
                  kind: ev.kind,
                  session_id: ev.session_id,
                  status,
                });
              }
              continue;
            }
            if (frame.frame.case !== "agentEntries") continue;
            const f = frame.frame.value;
            // Drop everything until our session id is known: `seq` is
            // per-session, so another live agent session's entries would
            // collide with ours in the map. Entries cannot arrive before the
            // spawn RPC returns, so nothing of ours is lost here.
            if (!sid || f.sessionId !== sid) continue;
            // Upsert by seq: entries are re-emitted as they grow.
            for (const pe of f.entries) {
              const e = agentEntryFromProto(pe);
              entries.set(e.seq, e);
              const updates = updatesBySeq.get(e.seq);
              if (updates) updates.push(e);
              else updatesBySeq.set(e.seq, [e]);
            }
          }
        } catch (e) {
          if (!/abort|timed?.?out|deadline|cancel/i.test(String(e))) throw e;
        }
      })();

      // ─── 3. Spawn the agent session ─────────────────────────────────────
      const spawn = await c.sessionsSpawn({ workerFp, kind: "agent", folder });
      sid = spawn.sessionId;
      expect(sid).toBeTruthy();
      spawnedSessions.add(sid);

      const opened = await pollUntil(`agent session ${sid} reaches status=open`, async () => {
        const s = await findSession(sid);
        return s?.status === "open" ? s : undefined;
      });
      // The session kind must survive the whole round trip: worker record →
      // coord row → projection. A shell here means _spawnFrameFor regressed.
      expect(opened.kind).toBe("agent");

      // ─── 4. A real turn, streamed back as growing assistant entries ─────
      await c.sessionsUserMessage({
        sessionId: sid,
        text: "Without using any tools, write the word pong exactly 80 times, separated by spaces, and nothing else.",
      });
      const firstTurn = await pollUntil(
        "a completed assistant entry or provider overload notice arrives",
        async () => {
          const assistant = [...entries.values()].find(
            (e) => e.kind === "assistant" && e.done && /pong/i.test(entryText(e)),
          );
          if (assistant) return { outcome: "assistant" as const, entry: assistant };
          const notices = [...entries.values()].filter(
            (e) =>
              e.kind === "notice" &&
              (e.level === "warn" || e.level === "error") &&
              /overloaded/i.test(e.text),
          );
          return notices.length > 0 ? { outcome: "overloaded" as const, notices } : undefined;
        },
        120_000,
      );
      if (firstTurn.outcome === "overloaded") {
        expect(firstTurn.notices.length).toBeGreaterThan(0);
        expect(firstTurn.notices.every((notice) => /overloaded/i.test(notice.text))).toBe(true);
        ac.abort();
        await collector;
        await c.sessionsKill({ sessionId: sid });
        spawnedSessions.delete(sid);
        return;
      }
      const pong = firstTurn.entry;
      if (pong.kind !== "assistant") throw new Error("expected an assistant entry");
      const streamed = updatesBySeq.get(pong.seq) ?? [];
      let growthAt = -1;
      for (let i = 1; i < streamed.length; i++) {
        const previous = streamed[i - 1];
        const current = streamed[i];
        if (
          previous?.kind === "assistant" &&
          current?.kind === "assistant" &&
          !previous.done &&
          !current.done &&
          current.text.length > previous.text.length
        ) {
          growthAt = i;
          break;
        }
      }
      expect(growthAt).toBeGreaterThan(0);
      expect(streamed.slice(growthAt + 1).some((e) => e.kind === "assistant" && e.done)).toBe(true);

      // ─── 5. The same entry comes back over the unary backfill path ──────
      // (beforeSeq 0 = newest page). This is what a page reload uses; an empty
      // page here means the SPA would show a blank transcript on refresh.
      const page = await pollUntil(
        "sessionsGetAgentEntries returns the newest page",
        async () => {
          const r = await c.sessionsGetAgentEntries({ sessionId: sid, beforeSeq: 0n });
          return r.entries.length > 0 ? r : undefined;
        },
      );
      const backfilled = page.entries.map(agentEntryFromProto);
      expect(backfilled.some((e) => e.kind === "assistant" && /pong/i.test(entryText(e)))).toBe(true);
      // Ascending by seq — the store inserts in order and never re-sorts.
      const seqs = backfilled.map((e) => e.seq);
      expect([...seqs].sort((a, b) => a - b)).toEqual(seqs);
      // The user turn is in the transcript too, not just the model's reply.
      expect(backfilled.some((e) => e.kind === "user")).toBe(true);

      // ─── 6. Todo state is projected as a durable panel entry ────────────
      await c.sessionsUserMessage({
        sessionId: sid,
        text: "Use the todo tool to init a 2-item list, then reply DONE",
      });
      const todo = await pollUntil(
        "todo entry contains one phase with exactly two tasks",
        async () => {
          for (const candidate of entries.values()) {
            if (candidate.kind !== "todo") continue;
            try {
              const phases: unknown = JSON.parse(candidate.phases_json);
              if (
                Array.isArray(phases) &&
                phases.some(
                  (phase) =>
                    typeof phase === "object" &&
                    phase !== null &&
                    "tasks" in phase &&
                    Array.isArray(phase.tasks) &&
                    phase.tasks.length === 2,
                )
              ) {
                return candidate;
              }
            } catch {
              // A malformed payload is not the expected parity result.
            }
          }
          return undefined;
        },
        120_000,
      );
      expect(todo.kind).toBe("todo");
      await pollUntil(
        "agent returns to idle after creating the todo list",
        async () => (await findSession(sid))?.agent?.status === "idle",
        120_000,
      );

      // ─── 7. Approval round-trip — the wedge-or-work moment ──────────────
      // Must be a command only `bash` can serve: `--approval-mode write`
      // auto-approves the read/write tiers, and omp steers directory listings
      // to the `read` tool, which would never prompt.
      await c.sessionsUserMessage({
        sessionId: sid,
        text: "Use the bash tool to run exactly this command: echo agent-smoke-ok",
      });
      const prompt = await pollUntil(
        "approval prompt entry reaches the client as pending",
        async () =>
          [...entries.values()].find(
            (e) => e.kind === "prompt" && e.prompt_kind === "approval" && e.state === "pending",
          ),
        120_000,
      );
      if (prompt.kind !== "prompt") throw new Error("unreachable: narrowed above");
      expect(prompt.options).toContain("Approve");
      expect(prompt.title).toContain("Allow tool: ");

      // The pending prompt must also light up the EXISTING attention surface
      // (sidebar / push), which reads AgentStatus, not the transcript.
      await pollUntil("agent patch reports status=needs-input", async () =>
        sessionEvents.some(
          (e) => e.session_id === sid && e.kind === "agent" && e.status === "needs-input",
        ),
      );

      await c.sessionsAgentRespond({
        sessionId: sid,
        promptId: prompt.prompt_id,
        value: "Approve",
        cancelled: false,
      });
      await pollUntil("prompt entry flips to answered (same seq, upserted)", async () => {
        const e = entries.get(prompt.seq);
        return e?.kind === "prompt" && e.state === "answered";
      });
      // Approving must actually release the tool — this is the proof the reply
      // reached the omp child and nothing is wedged.
      const tool = await pollUntil(
        "bash tool entry reaches status=ok after approval",
        async () =>
          [...entries.values()].find(
            (e) => e.kind === "tool" && e.name === "bash" && e.status === "ok",
          ),
        120_000,
      );
      if (tool.kind !== "tool") throw new Error("unreachable: narrowed above");
      expect(JSON.parse(tool.args_json)).toHaveProperty("command");

      // ─── 7. Cancelling a prompt must release the agent, not wedge it ─────
      // An unanswered prompt blocks the omp child forever, so "the user
      // dismissed it" has to travel as an explicit cancelled reply.
      await c.sessionsUserMessage({
        sessionId: sid,
        text: "Use the bash tool to run exactly this command: echo second-approval",
      });
      const second = await pollUntil(
        "a second approval prompt arrives",
        async () =>
          [...entries.values()].find(
            (e) =>
              e.kind === "prompt" &&
              e.prompt_kind === "approval" &&
              e.state === "pending" &&
              e.seq !== prompt.seq,
          ),
        120_000,
      );
      if (second.kind !== "prompt") throw new Error("unreachable: narrowed above");
      await c.sessionsAgentRespond({
        sessionId: sid,
        promptId: second.prompt_id,
        value: "",
        cancelled: true,
      });
      await pollUntil("cancelled prompt flips to state=cancelled", async () => {
        const e = entries.get(second.seq);
        return e?.kind === "prompt" && e.state === "cancelled";
      });
      // The turn must actually END. A wedged child sits in `running` while
      // blocked on the unanswered request, so accepting `running` here would
      // pass on the first tick and prove nothing: `idle` is the only state that
      // distinguishes released from wedged.
      await pollUntil(
        "agent returns to idle after the cancel (turn released, not wedged)",
        async () => (await findSession(sid))?.agent?.status === "idle",
        120_000,
      );

      // ─── 8. Abort a turn in flight ──────────────────────────────────────
      await c.sessionsUserMessage({
        sessionId: sid,
        text: "Count slowly from 1 to 500, one number per line, with no tools.",
      });
      await pollUntil(
        "turn is running before the abort",
        async () => (await findSession(sid))?.agent?.status === "running",
        60_000,
      );
      expect((await c.sessionsAgentAbort({ sessionId: sid })).accepted).toBe(true);
      await pollUntil(
        "agent returns to idle after abort",
        async () => (await findSession(sid))?.agent?.status === "idle",
        120_000,
      );

      // ─── 10. Restart: worker re-adopts, durable history stays intact ─────
      if (stack) {
        const before = await findSession(sid);
        expect(before?.agent?.sessionFile).toBeTruthy();
        const beforeRestart = await c.sessionsGetAgentEntries({ sessionId: sid, beforeSeq: 0n });
        const maxSeqBeforeRestart = Math.max(...beforeRestart.entries.map((entry) => Number(entry.seq)));
        const pongTextBeforeRestart = pong.text;

        await stack.restartWorker();
        const afterRestart = await pollUntil(
          "worker re-adopts the agent and durable history remains intact",
          async () => {
            const r = await c.sessionsGetAgentEntries({ sessionId: sid, beforeSeq: 0n });
            const es = r.entries.map(agentEntryFromProto);
            const adopted = es.some(
              (e) =>
                e.kind === "notice" &&
                e.text === "resumed" &&
                e.seq > maxSeqBeforeRestart,
            );
            const recollected = es.some(
              (e) =>
                e.kind === "assistant" &&
                e.seq === pong.seq &&
                e.text === pongTextBeforeRestart,
            );
            return adopted && recollected ? es : undefined;
          },
          60_000,
        );
        expect(afterRestart.some((e) => e.kind === "user")).toBe(true);
        expect(new Set(afterRestart.map((e) => e.seq)).size).toBe(afterRestart.length);
      }

      ac.abort();
      await collector;

      // ─── 10. Kill ───────────────────────────────────────────────────────
      await c.sessionsKill({ sessionId: sid });
      await pollUntil(`agent session ${sid} closed or purged after kill`, async () => {
        const s = await findSession(sid);
        return s === undefined || s.status === "closed";
      });
      spawnedSessions.delete(sid);

      console.log(`agent smoke: all steps green in ${((performance.now() - t0) / 1000).toFixed(2)}s`);
    } catch (failure) {
      // A timeout here says only "the entry never arrived". Dump what DID
      // arrive: that difference is the whole diagnosis (wrong projection,
      // dropped frame, or the model simply not calling the tool).
      const dump = [...entries.values()]
        .sort((a, b) => a.seq - b.seq)
        .map((e) => {
          const detail =
            e.kind === "tool" ? `${e.name} ${e.status}`
            : e.kind === "prompt" ? `${e.prompt_kind} ${e.state} ${JSON.stringify(e.title)}`
            : JSON.stringify(entryText(e).slice(0, 120));
          return `  #${e.seq} ${e.kind} ${detail}`;
        })
        .join("\n");
      console.error(`agent smoke: transcript at failure (${entries.size} entries):\n${dump}`);
      const statuses = sessionEvents.filter((e) => e.kind === "agent" && e.status).map((e) => e.status);
      console.error(`agent smoke: agent statuses seen: ${statuses.join(" → ") || "<none>"}`);
      // Read it now: stack.stop() in the finally removes the temp root.
      if (stack) {
        try {
          // Filter: a failing backfill poll floods the log with identical
          // get-omp-transcript-page lines that bury the lifecycle events.
          const lines = readFileSync(stack.workerLogPath, "utf8")
            .split("\n")
            .filter((l) => /agent|resume|respawn|error|warn|spawn/i.test(l) && !/get-omp-transcript-page/.test(l));
          console.error(`agent smoke: worker log (lifecycle):\n${lines.slice(-40).join("\n")}`);
        } catch {}
      }
      throw failure;
    } finally {
      ac.abort();
      for (const s of spawnedSessions) {
        try { await c.sessionsKill({ sessionId: s }); } catch {}
      }
      // Errors swallowed on purpose: cleanup must never mask the real
      // assertion failure, and the stack's own stop() already reports its own.
      if (stack) {
        try { await stack.stop(); } catch (e) { console.warn(`agent smoke: stack teardown: ${String(e)}`); }
      }
    }
  }, 420_000);
});
