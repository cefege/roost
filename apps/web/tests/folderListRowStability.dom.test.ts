// Sidebar row stability under session folds: a session patch must not tear down
// and recreate row DOM.
//
// This repo runs no jsdom, and Solid resolves to its SSR build under `bun test`,
// so FolderList cannot render here. The test instead locks the mechanism DOM
// stability rests on: foldEventIntoStore's reconcile keeps each session store
// object stable while updating its leaves. FolderList's reference-keyed <For>
// can therefore preserve the corresponding row.

import { expect, test, describe } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { SessionEvent } from "@roost/shared/wire";
import { foldEventIntoStore } from "../src/store/projector.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";
import { getSessionTraceId, sessionTraceSize } from "../src/lib/diag.ts";

const FP = asWorkerFp("c".repeat(64));
const SID_A = asSessionId("00000000-0000-4000-8000-00000000c001");
const SID_B = asSessionId("00000000-0000-4000-8000-00000000c002");

function opened(id: string, cwd: string): SessionEvent {
  return {
    kind: "opened",
    session_id: asSessionId(id),
    worker_fp: FP,
    channel: asChannelId(1),
    session_kind: "shell",
    cwd,
    ts: 1000,
  } as SessionEvent;
}

describe("session store row stability (projector reconcile)", () => {
  test("respawned fold updates fields WITHOUT replacing the session object", () => {
    foldEventIntoStore(opened(SID_A, "/repo/a"));
    foldEventIntoStore(opened(SID_B, "/repo/b"));

    const refA = rootStore.sessions[SID_A];
    const refB = rootStore.sessions[SID_B];

    foldEventIntoStore({
      kind: "respawned",
      session_id: SID_A,
      new_channel: asChannelId(2),
      ts: 3000,
    } as SessionEvent);

    // Reconcile updates leaves in place, preserving the reference keyed by <For>.
    expect(rootStore.sessions[SID_A]).toBe(refA!);
    expect(rootStore.sessions[SID_A]?.channel).toBe(asChannelId(2));
    expect(rootStore.sessions[SID_A]?.status).toBe("open");

    // The untouched sibling keeps identity and content.
    expect(rootStore.sessions[SID_B]).toBe(refB!);
    expect(rootStore.sessions[SID_B]?.cwd).toBe("/repo/b");
  });

  test("closed reaps the session, its volatile slices, AND the per-session accumulators", () => {
    // Seed the diagnostic per-session accumulator so its close cleanup remains observable.
    getSessionTraceId(SID_A);
    const traceBefore = sessionTraceSize();

    // exit_code is required (nullable) on `closed`: the projector validates the
    // event at its boundary, and an under-specified payload is rejected whole.
    foldEventIntoStore({ kind: "closed", session_id: SID_A, exit_code: null, ts: 4000 } as SessionEvent);

    expect(rootStore.sessions[SID_A]).toBeUndefined();
    expect(sessionTraceSize()).toBe(traceBefore - 1);
    // Sibling untouched by the deletion.
    expect(rootStore.sessions[SID_B]?.cwd).toBe("/repo/b");
  });

  test("cwd fold flows through reconcile (leaf update, same object)", () => {
    const refB = rootStore.sessions[SID_B];
    foldEventIntoStore({ kind: "cwd", session_id: SID_B, cwd: "/repo/b/sub", ts: 5000 } as SessionEvent);
    expect(rootStore.sessions[SID_B]).toBe(refB!);
    expect(rootStore.sessions[SID_B]?.cwd).toBe("/repo/b/sub");
  });
});
