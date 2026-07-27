// Idempotent kill: a kill for a session the worker no longer holds (orphaned —
// the keeper that owned the PTY was restarted, so getBySessionId returns
// undefined) must still emit a `closed` tombstone, or coord keeps the session
// `open` forever (unkillable). Reproduces the main.ts onBrowserCommand
// case "kill" !rec branch: getBySessionId(sid) === undefined → emitClosedTombstone(sid).
// See worker commit "worker: idempotent kill — tombstone orphaned sessions".

import { test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asWorkerFp } from "@roost/shared";
import type { SessionEvent } from "@roost/shared";

test("kill of an orphaned (unknown) session emits a closed tombstone", () => {
  const events: SessionEvent[] = [];
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: (e) => events.push(e) },
  });

  const orphanSid = asSessionId("11111111-1111-1111-1111-111111111111");

  // The main.ts kill handler: an orphaned session is unknown to this worker.
  expect(mgr.getBySessionId(orphanSid)).toBeUndefined();

  // ...so it takes the tombstone branch instead of silently returning.
  mgr.emitClosedTombstone(orphanSid);

  expect(events).toHaveLength(1);
  expect(events[0]).toMatchObject({
    kind: "closed",
    session_id: orphanSid,
    exit_code: null,
  });
});
