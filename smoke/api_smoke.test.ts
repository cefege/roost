// Headless API smoke — the data-plane portion of the roost-smoke flow driven
// PURELY over Connect RPC against a LIVE coord. No browser: this proves the
// spawn → PTY echo → rename → workspace lifecycle → sync-stream → kill loop
// works end-to-end (coord + real worker + real PTY) in seconds, replacing the
// slow humanchrome pass for data-plane-only changes. Browser-only concerns
// (DOM paint, focus pipeline, error boundaries) stay in the roost-smoke skill.
// Requires a live tailnet coordinator. The `live-api` profile is intentionally
// separate from local smoke tests; a missing URL is a hard failure, never skip.
//
// No fixed sleeps anywhere: every step is a bounded 100ms poll (pollUntil),
// so wall-clock tracks actual coord/worker latency. Target < 10s total;
// elapsed is printed at the end (informational, not asserted — live coords
// vary; the 30s test timeout is the hard bound).

import { describe, test, expect } from "bun:test";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { buildAuthorizedApiClient } from "../apps/roost-cli/src/api.ts";
import { loadWorkerConfig } from "../apps/worker/src/config.ts";
import { openSyncWs } from "../apps/roost-cli/src/sync-ws.ts";
// Relative import, not "@roost/shared/...": the smoke workspace declares no
// @roost deps, so the bare specifier only resolves from files living under
// apps/* (their node_modules carry the workspace symlink). Same pattern as
// roost-cli importing worker modules relatively.
import { protoToEvent } from "../apps/shared/src/wire/event-proto.ts";

const coordinatorUrl = process.env.ROOST_COORD_URL;
if (!coordinatorUrl) {
  throw new Error(
    "ROOST_COORD_URL is required; run ROOST_COORD_URL=https://<current-tailnet-coord>:4102 bun run test:live-api",
  );
}
const workerConfig = loadWorkerConfig();
const keyPath = existsSync(workerConfig.workerKeyPath)
  ? workerConfig.workerKeyPath
  : join(homedir(), ".roost", "cli-key");

/**
 * Bounded poll: re-run `fn` every 100ms until it returns a truthy value
 * (returned to the caller) or `timeoutMs` elapses (labeled throw). Transient
 * RPC errors inside `fn` do NOT fail the poll — sessionsGetScrollbackCells throws
 * Unavailable mid worker-resume and the next tick recovers;
 */
async function pollUntil<T>(
  label: string,
  fn: () => Promise<T | undefined | null | false>,
  timeoutMs = 5000,
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
    // Real delay on purpose (ts-no-test-timers exception): this polls a LIVE
    // remote coord over the network — fake timers can't advance the server.
    // 100ms is the pacing floor, not a race-masking guess; the poll exits the
    // instant the condition holds.
    await Bun.sleep(100);
  }
}

describe("api smoke (headless, live coord)", () => {
  test("spawn → echo → rename → workspace lifecycle → sync stream → kill", async () => {
    const t0 = performance.now();
    const c = await buildAuthorizedApiClient({ coordinatorUrl, keyPath, label: "api-smoke" });

    // Cleanup ledger — the finally block below kills/deletes anything a
    // mid-flight failure left behind, so a red run never litters the live
    // coord with api-smoke sessions/workspaces. Steps that clean up their
    // own resources remove them from the ledger on success.
    const spawnedSessions = new Set<string>();
    const createdWorkspaces = new Set<string>();

    /** Find one session in the full projection (undefined once purged). */
    const findSession = async (sid: string) => {
      const { sessions } = await c.sessionsList({ status: "all" });
      return sessions.find((s) => s.id === sid);
    };

    /**
     * CAS-safe workspace delete: version must be read fresh because create/
     * update/set-sessions each bump it; one retry absorbs a concurrent bump
     * between our read and the delete. Missing ws = already gone = success.
     */
    const deleteWorkspace = async (wsId: string) => {
      for (let attempt = 0; attempt < 2; attempt++) {
        const { workspaces } = await c.workspacesList({});
        const ws = workspaces.find((w) => w.id === wsId);
        if (!ws) return;
        try {
          await c.workspacesDelete({ id: wsId, ifVersion: ws.version });
          return;
        } catch (e) {
          if (attempt === 1) throw e; // second CAS miss = real problem
        }
      }
    };

    try {
    // ─── 1. Spawn a shell on the first routable worker ──────────────────
    const { routableFps } = await c.workersList({});
    const workerFp = routableFps[0];
    if (!workerFp) {
      throw new Error(
        "api smoke: no routable worker online on this coord — bring a worker up and re-run",
      );
    }
    const spawn = await c.sessionsSpawn({ workerFp, kind: "shell", folder: "/tmp" });
    const sid = spawn.sessionId;
      expect(sid).toBeTruthy();
      spawnedSessions.add(sid);
      await pollUntil(`session ${sid} reaches status=open`, async () => {
        const s = await findSession(sid);
        return s?.status === "open";
      });

      // ─── 2. Echo round-trip: input bytes → PTY → rendered grid ──────────
      // `readScrollbackRangeCells` deliberately exposes only history, not the
      // live viewport. Emit enough follow-on rows to move the marker into
      // scrollback before querying the RPC; its own line remains exact.
      const marker = `API_SMOKE_${crypto.randomUUID().slice(0, 8)}`;
      await c.sessionsInput({
        sessionId: sid,
        data: new TextEncoder().encode(`printf '%s\\n' ${marker}; seq 1 128\n`),
      });
      const grid = await pollUntil(`echo marker ${marker} on its own output line`, async () => {
        // Request a tail bounded above the current grid. The worker clamps the
        // range; zero is a literal exclusive end row, not a tail sentinel.
        const r = await c.sessionsGetScrollbackCells({
          sessionId: sid,
          endRow: BigInt(Number.MAX_SAFE_INTEGER),
          maxRows: 200,
        });
        const text = r.rows.map(row => row.spans.map(s => s.text || " ").join("").trimEnd()).join("\n");
        return text.split("\n").some((l: string) => l.trim() === marker) ? text : undefined;
      });
      expect(grid).toContain(marker);

      // ─── 3. Rename: set custom title, then clear it ──────────────────────
      await c.sessionsRename({ sessionId: sid, title: "api-smoke-title" });
      await pollUntil("customTitle reflects rename", async () => {
        const s = await findSession(sid);
        return s?.customTitle === "api-smoke-title";
      });
      await c.sessionsRename({ sessionId: sid, title: "" }); // "" clears the override
      await pollUntil("customTitle cleared (auto title returns)", async () => {
        const s = await findSession(sid);
        return s !== undefined && !s.customTitle;
      });

      // ─── 4. Workspace lifecycle: create+attach, move, delete ────────────
      const ws1 = (
        await c.workspacesCreate({
          workerFp, name: "api-smoke-ws", folderPath: "/tmp",
          attachSessionIds: [sid],
        })
      ).workspace!;
      createdWorkspaces.add(ws1.id);
      await pollUntil("ws1 membership includes session", async () => {
        const { workspaces } = await c.workspacesList({});
        return workspaces.find((w) => w.id === ws1.id)?.sessionIds.includes(sid);
      });

      const ws2 = (
        await c.workspacesCreate({
          workerFp,
          name: "api-smoke-ws-2",
          folderPath: `/tmp/api-smoke-ws-2-${crypto.randomUUID().slice(0, 8)}`,
        })
      ).workspace!;
      createdWorkspaces.add(ws2.id);
      await c.sessionsAssignWorkspace({ sessionId: sid, workspaceId: ws2.id });
      await pollUntil("session moved ws1 → ws2 (both memberships flipped)", async () => {
        const { workspaces } = await c.workspacesList({});
        const in1 = workspaces.find((w) => w.id === ws1.id)?.sessionIds.includes(sid);
        const in2 = workspaces.find((w) => w.id === ws2.id)?.sessionIds.includes(sid);
        return in2 === true && in1 === false;
      });

      // Delete the session-holding workspace. The coord cascade is MEMBERSHIP
      // only: workspace_sessions rows go with the ws (FK ON DELETE CASCADE,
      // migrations/0001_init.sql:78) but the session itself has no FK to
      // workspaces and stays OPEN — the browser smoke's "cascade" assertion
      // is about sidebar ROWS, not session lifetime. So: ws gone from
      // workspacesList AND the session still alive in sessionsList.
      await deleteWorkspace(ws2.id);
      createdWorkspaces.delete(ws2.id);
      await pollUntil("ws2 gone from workspacesList after delete", async () => {
        const { workspaces } = await c.workspacesList({});
        return !workspaces.some((w) => w.id === ws2.id);
      });
      const survivor = await findSession(sid);
      expect(survivor?.status).toBe("open"); // delete must NOT kill the session

      await deleteWorkspace(ws1.id); // cleanup — already empty after the move
      createdWorkspaces.delete(ws1.id);
      await pollUntil("ws1 gone from workspacesList after delete", async () => {
        const { workspaces } = await c.workspacesList({});
        return !workspaces.some((w) => w.id === ws1.id);
      });

      // ─── 5. Sync stream: live sessionEvent frames for spawn + kill ──────
      // Open the stream BEFORE mutating (sinceEventId:0 = live-only, no
      // backfill — the coord gates replay on sinceEventId>0), so the opened/
      // closed events for the throwaway session MUST arrive as live frames.
      // The collector runs in the background; the 8s timeout is the hard
      // upper bound, the AbortController is the happy-path terminator.
      const seen: Array<{ kind: string; session_id: string }> = [];
      const ac = new AbortController();
      const collector = (async () => {
        try {
          const signal = AbortSignal.any([ac.signal, AbortSignal.timeout(8000)]);
          for await (const frame of await openSyncWs({ signal })) {
            if (frame.frame.case !== "sessionEvent") continue;
            const ev = protoToEvent(frame.frame.value as never) as
              | (Record<string, unknown> & { kind: string; session_id?: string })
              | null;
            // snapshot events carry no session_id — irrelevant here, skip.
            if (ev?.session_id) seen.push({ kind: ev.kind, session_id: ev.session_id });
          }
        } catch (e) {
          // connect surfaces our abort as a stream error — normal terminator
          // (same regex as the roost-cli events verb).
          if (!/abort|timed?.?out|deadline|cancel/i.test(String(e))) throw e;
        }
      })();

      const throwaway = await c.sessionsSpawn({ workerFp, kind: "shell", folder: "/tmp" });
      const tsid = throwaway.sessionId;
      spawnedSessions.add(tsid);
      await pollUntil(`throwaway session ${tsid} open`, async () => {
        const s = await findSession(tsid);
        return s?.status === "open";
      });
      await c.sessionsKill({ sessionId: tsid });
      await pollUntil("sync stream delivered opened+closed for throwaway", async () => {
        const kinds = new Set(seen.filter((e) => e.session_id === tsid).map((e) => e.kind));
        return kinds.has("opened") && kinds.has("closed");
      });
      spawnedSessions.delete(tsid); // kill confirmed via the event stream
      ac.abort();
      await collector;

      // ─── 6. Kill the main session → closed/gone in the projection ───────
      await c.sessionsKill({ sessionId: sid });
      await pollUntil(`session ${sid} closed or purged after kill`, async () => {
        const s = await findSession(sid);
        return s === undefined || s.status === "closed";
      });
      spawnedSessions.delete(sid);

      console.log(`api smoke: all steps green in ${((performance.now() - t0) / 1000).toFixed(2)}s`);
    } finally {
      // Best-effort teardown of anything a failure stranded — every error
      // swallowed on purpose: cleanup must never mask the real assertion
      // failure, and a half-dead session/ws will 4xx here harmlessly.
      for (const sid of spawnedSessions) {
        try { await c.sessionsKill({ sessionId: sid }); } catch {}
      }
      for (const wsId of createdWorkspaces) {
        try { await deleteWorkspace(wsId); } catch {}
      }
    }
  }, 30_000);

  test("upload (chunked) → dedup probe → chunked download round-trips >25MB", async () => {
    const c = await buildAuthorizedApiClient({ coordinatorUrl, keyPath, label: "api-smoke" });
    const spawnedSessions = new Set<string>();
    try {
      const { routableFps } = await c.workersList({});
      const workerFp = routableFps[0];
      if (!workerFp) throw new Error("api smoke: no routable worker online on this coord");
      const spawn = await c.sessionsSpawn({ workerFp, kind: "shell", folder: "/tmp" });
      const sid = spawn.sessionId;
      spawnedSessions.add(sid);
      await pollUntil(`session ${sid} reaches status=open`, async () => {
        const { sessions } = await c.sessionsList({ status: "all" });
        return sessions.find((s) => s.id === sid)?.status === "open" || undefined;
      });

      // 30 MB deterministic buffer — above the retired 25 MB unary read cap, so
      // a successful download also proves the cap is gone.
      const SIZE = 30 * 1024 * 1024;
      const CHUNK = 4 * 1024 * 1024;
      const buf = new Uint8Array(SIZE);
      for (let i = 0; i < SIZE; i++) buf[i] = i & 0xff;
      const sha = new Bun.CryptoHasher("sha256").update(buf).digest("hex");

      // Upload via the chunked AttachFileChunk loop (mirrors the SPA path).
      const uploadId = crypto.randomUUID();
      let absPath = "";
      let seq = 0;
      for (let offset = 0; offset === 0 || offset < SIZE; offset += CHUNK) {
        const data = buf.subarray(offset, Math.min(offset + CHUNK, SIZE));
        const last = offset + CHUNK >= SIZE;
        const res = await c.attachFileChunk({
          uploadId, sessionId: sid, filename: "smoke-transfer.bin", shortPath: false, data, last, seq: seq++,
        });
        if (last) absPath = res.absPath;
      }
      expect(absPath).toBeTruthy();

      // Dedup: probing the same content returns a hit at the same stored path.
      const probe = await c.attachmentProbe({
        sessionId: sid, sha256: sha, size: BigInt(SIZE), filename: "smoke-transfer.bin", shortPath: false,
      });
      expect(probe.hit).toBe(true);
      expect(probe.absPath).toBe(absPath);

      // Chunked download over filesReadChunk → reassemble → integrity + no cap.
      const parts: Uint8Array[] = [];
      let dlOffset = 0;
      let total = 0;
      for (;;) {
        const res = await c.filesReadChunk({ workerFp, path: absPath, offset: BigInt(dlOffset), len: CHUNK });
        total = Number(res.size);
        if (res.data.length) { parts.push(res.data); dlOffset += res.data.length; }
        if (res.eof || res.data.length === 0) break;
      }
      expect(total).toBe(SIZE);
      expect(dlOffset).toBe(SIZE);
      const all = new Uint8Array(dlOffset);
      let o = 0;
      for (const part of parts) { all.set(part, o); o += part.length; }
      expect(new Bun.CryptoHasher("sha256").update(all).digest("hex")).toBe(sha);
    } finally {
      for (const sid of spawnedSessions) {
        try { await c.sessionsKill({ sessionId: sid }); } catch {}
      }
    }
  }, 60_000);
});
