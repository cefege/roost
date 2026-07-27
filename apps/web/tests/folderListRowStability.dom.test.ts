// Sidebar row stability under agent folds — the perf-sweep headline: an agent
// WS tick must NOT tear down and recreate row DOM.
//
// Deviation from the plan's literal "render FolderList, capture element refs"
// test: this repo runs NO jsdom by design (see cellRenderer.dom.test.ts), and
// Solid resolves to its SSR build under `bun test` (effects inert, JSX not
// solid-compiled) — the real FolderList component cannot render here. What
// this file locks instead is the mechanism that DOM stability rests on, at
// the exact layer that regressed before the sweep: foldEventIntoStore's
// reconcile keeps the SESSION STORE OBJECT IDENTITY stable across an agent
// fold (a plain per-key set object-replaced the whole Session → every
// downstream reader invalidated → FolderList's reference-keyed <For> rebuilt
// every row). Element-level stability is verified live via the plan's
// MutationObserver probe (Verification §4) / roost-smoke.

import { expect, test, describe } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { SessionEvent } from "@roost/shared/wire";
import { foldEventIntoStore } from "../src/store/projector.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";
import { getSessionTraceId, sessionTraceSize } from "../src/lib/diag.ts";
import { InputChannel, inputMapSizes } from "../src/ws/input-channel.ts";

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

function agentTick(id: string, text: string, ts: number): SessionEvent {
  return {
    kind: "agent",
    session_id: asSessionId(id),
    patch: {
      kind: "agent",
      status: "running",
      last_message: { role: "assistant", text, ts },
    },
    ts,
  } as SessionEvent;
}

describe("session store row stability (projector reconcile)", () => {
  test("agent fold updates fields WITHOUT replacing the session object", () => {
    foldEventIntoStore(opened(SID_A, "/repo/a"));
    foldEventIntoStore(opened(SID_B, "/repo/b"));
    foldEventIntoStore(agentTick(SID_A, "first", 2000));

    const refA = rootStore.sessions[SID_A];
    const refB = rootStore.sessions[SID_B];
    expect(refA?.agent?.last_message?.text).toBe("first");

    foldEventIntoStore(agentTick(SID_A, "second", 3000));

    // The store entry is the SAME object — reconcile diffed in place. This is
    // what keeps reference-keyed <For> rows (and every s.agent?.* reader)
    // from being torn down per tick; a plain set (pre-sweep) minted a fresh
    // object here and this assertion fails.
    expect(rootStore.sessions[SID_A]).toBe(refA!);
    // ...while the changed leaves DID update.
    expect(rootStore.sessions[SID_A]?.agent?.last_message?.text).toBe("second");
    expect(rootStore.sessions[SID_A]?.agent?.last_message?.ts).toBe(3000);

    // The untouched sibling keeps identity AND content.
    expect(rootStore.sessions[SID_B]).toBe(refB!);
    expect(rootStore.sessions[SID_B]?.agent).toBeNull();
  });

  test("closed reaps the session, its volatile slices, AND the per-session accumulators", () => {
    // Seed every per-session accumulator the close reaper owns so each drop is
    // observable. _sessionTrace and _lastSendTs have no other reaper.
    getSessionTraceId(SID_A);                                         // → diag _sessionTrace[SID_A]
    new InputChannel(async () => {}).sendInput(SID_A, new Uint8Array([65])); // → _lastSendTs[SID_A]
    const traceBefore = sessionTraceSize();
    const mapsBefore = inputMapSizes();

    foldEventIntoStore({ kind: "closed", session_id: SID_A, ts: 4000 } as SessionEvent);

    expect(rootStore.sessions[SID_A]).toBeUndefined();
    expect(sessionTraceSize()).toBe(traceBefore - 1);
    expect(inputMapSizes()).toBe(mapsBefore - 1);
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
