// `spawn_cwd` fold — the immutable spawn folder behind the /t/ stable URL.
// Asserts: opened captures it; a later cwd (OSC7 cd) event updates cwd but NOT
// spawn_cwd; a worker snapshot (which doesn't carry it) preserves the prior value.

import { describe, test, expect } from "bun:test";
import { foldAll, type SessionEvent } from "../src/wire/event.ts";
import { asSessionId, asWorkerFp, asChannelId } from "../src/wire/brand.ts";

const SID = asSessionId("00000000-0000-4000-8000-000000000001");
const FP = asWorkerFp("aa".repeat(32));
const CH = asChannelId(1);

const opened: SessionEvent = {
  kind: "opened", session_id: SID, worker_fp: FP, channel: CH,
  session_kind: "shell", cwd: "/Users/you/roost", ts: 1,
};

describe("spawn_cwd fold", () => {
  test("opened captures spawn_cwd from the initial cwd", () => {
    const m = foldAll([opened]);
    expect(m.get(SID)?.spawn_cwd).toBe("/Users/you/roost");
    expect(m.get(SID)?.cwd).toBe("/Users/you/roost");
  });

  test("a later cwd (cd) event drifts cwd but spawn_cwd stays put", () => {
    const m = foldAll([
      opened,
      { kind: "cwd", session_id: SID, cwd: "/Users/you/roost/apps/web", ts: 2 },
    ]);
    expect(m.get(SID)?.cwd).toBe("/Users/you/roost/apps/web");   // drifted
    expect(m.get(SID)?.spawn_cwd).toBe("/Users/you/roost");      // immutable
  });

  test("snapshot (worker doesn't announce spawn_cwd) preserves the prior value", () => {
    const m = foldAll([
      opened,
      { kind: "cwd", session_id: SID, cwd: "/tmp", ts: 2 },
      {
        kind: "snapshot", worker_fp: FP, ts: 3,
        sessions: [{
          id: SID, worker_fp: FP, channel: CH, kind: "shell", cwd: "/tmp",
          workspace_id: null, status: "open",
          created_at: 1, closed_at: null, custom_title: null,
        }],
      },
    ]);
    expect(m.get(SID)?.spawn_cwd).toBe("/Users/you/roost");
  });
});
