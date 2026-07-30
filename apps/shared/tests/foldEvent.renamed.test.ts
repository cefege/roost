// `renamed` fold — the sticky custom-title override (cell-grid model).
// Asserts: rename sets custom_title; auto-title events never touch
// it; "" clears it; snapshot preserves it across a worker restart.

import { describe, test, expect } from "bun:test";
import { foldAll, type SessionEvent } from "../src/wire/event.ts";
import { asSessionId, asWorkerFp, asChannelId } from "../src/wire/brand.ts";

const SID = asSessionId("00000000-0000-4000-8000-000000000001");
const FP = asWorkerFp("aa".repeat(32));
const CH = asChannelId(1);

const opened: SessionEvent = {
  kind: "opened", session_id: SID, worker_fp: FP, channel: CH,
  session_kind: "shell", cwd: "/repo", ts: 1,
};

describe("renamed fold", () => {
  test("sets custom_title", () => {
    const m = foldAll([opened, { kind: "renamed", session_id: SID, custom_title: "My Pane", ts: 2 }]);
    expect(m.get(SID)?.custom_title).toBe("My Pane");
  });

  test("sticky — a later cwd (auto-title) event does not clobber the rename", () => {
    const m = foldAll([
      opened,
      { kind: "renamed", session_id: SID, custom_title: "Keep Me", ts: 2 },
      { kind: "cwd", session_id: SID, cwd: "/elsewhere", ts: 3 },
    ]);
    expect(m.get(SID)?.custom_title).toBe("Keep Me");
    expect(m.get(SID)?.cwd).toBe("/elsewhere"); // cwd still updates, title doesn't
  });

  test('"" clears the override (revert to auto)', () => {
    const m = foldAll([
      opened,
      { kind: "renamed", session_id: SID, custom_title: "Temp", ts: 2 },
      { kind: "renamed", session_id: SID, custom_title: "", ts: 3 },
    ]);
    expect(m.get(SID)?.custom_title).toBeNull();
  });

  test("snapshot preserves the rename across a worker restart", () => {
    // Worker re-announces the session with custom_title:null (it doesn't track it).
    const m = foldAll([
      opened,
      { kind: "renamed", session_id: SID, custom_title: "Survives", ts: 2 },
      {
        kind: "snapshot", worker_fp: FP, ts: 3,
        sessions: [{
          id: SID, worker_fp: FP, channel: CH, kind: "shell", cwd: "/repo",
          workspace_id: null, status: "open",
          created_at: 1, closed_at: null, custom_title: null,
        }],
      },
    ]);
    expect(m.get(SID)?.custom_title).toBe("Survives");
  });
});
