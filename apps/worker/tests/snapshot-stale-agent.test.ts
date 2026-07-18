// emitSnapshot announces agent:null for EVERY session. The A1 stale-badge
// era (bridge:null claude → stale marker) is retired: hooks ride the PTY
// claude (--settings + $ROOST_HOOK_SOCKET env), which survives worker
// restarts — live agent events repopulate state on the next hook firing,
// so the announced null is transient, not a dead chip.
//
// History: A1 from reliability audit wf_728b67c1; inverted 2026-07-04 with
// the shadow-ClaudeBridge deletion (hooks-on-PTY-claude).

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { emitSnapshot } from "../src/snapshot.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import type { SessionEvent } from "@roost/shared/wire";

const FP = asWorkerFp("00".repeat(32));

function freshMgr(): SessionManager {
  return new SessionManager({ workerFp: FP, sink: { emit: () => {} }, hookSocketPath: "/dev/null" });
}

// Inject a minimal record (only the fields emitSnapshot reads) into the
// private sessions map.
function inject(mgr: SessionManager, channelId: number, kind: "shell" | "claude"): void {
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(channelId, {
    sessionId: asSessionId(`00000000-0000-0000-0000-${String(channelId).padStart(12, "0")}`),
    channelId: asChannelId(channelId),
    kind, cwd: "/tmp",
  });
}

describe("emitSnapshot announces agent:null for every session kind", () => {
  test("claude and shell both announce agent:null", async () => {
    const mgr = freshMgr();
    inject(mgr, 1, "claude"); // resumed OR fresh — no distinction anymore
    inject(mgr, 2, "shell");

    let captured: SessionEvent | null = null;
    await emitSnapshot({ mgr, sink: { emit: (e) => { captured = e as SessionEvent; } }, workerFp: FP });

    expect(captured).not.toBeNull();
    const snap = captured as unknown as Extract<SessionEvent, { kind: "snapshot" }>;
    const byCh = new Map(snap.sessions.map((s) => [s.channel as number, s]));

    expect(byCh.get(1)!.agent).toBeNull();
    expect(byCh.get(2)!.agent).toBeNull();
  });
});
