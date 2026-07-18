// closeSession.test.ts — the shared close helpers behind the instant tab close.
// Asserts: closeLabelsFor reproduces the snackbar triple (incl. the fp-slice
// server fallback); siblingOrHomeHref lands on a same-folder sibling / Home and
// skips a sibling that's already soft-closing; killAfterUndo fires the kill in
// the background with an accepted→force escalation and one toast on failure;
// newestOpenSessionForFolderKey excludes a pending-close id (Step 5).
// See src/lib/closeSession.ts + src/store/selectors.ts.

import { expect, test, describe, beforeEach, afterEach, mock } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session, Worker } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../src/store/root.ts";
import { scheduleClose, undoAll } from "../src/lib/pendingClose.ts";
import { newestOpenSessionForFolderKey } from "../src/store/selectors.ts";
import { folderKeyOf } from "../src/lib/folderKey.ts";
import { coordClient } from "../src/connect.ts";
import { toasts } from "../src/lib/toastStore.ts";
import { closeLabelsFor, siblingOrHomeHref, killAfterUndo } from "../src/lib/closeSession.ts";

const FP = asWorkerFp("aa".repeat(32));

let idSeq = 0;
function seedSession(over: Partial<Session> = {}): Session {
  idSeq += 1;
  // Test fixture: a full Session literal cast to the branded type, mirroring
  // tests/optimisticSpawn.test.ts's anchor() (optional pr_/git_ fields omitted).
  const s = {
    id: asSessionId(`00000000-0000-4000-8000-${String(idSeq).padStart(12, "0")}`),
    worker_fp: FP,
    channel: asChannelId(idSeq),
    kind: "shell",
    cwd: "/Users/you/roost",
    spawn_cwd: "/Users/you/roost",
    workspace_id: null,
    status: "open",
    agent: null,
    created_at: 1000 + idSeq,
    closed_at: null,
    custom_title: null,
    ...over,
  } as Session;
  setRootStore("sessions", s.id, s);
  return s;
}

// coordClient.sessionsKill returns a full Connect/protobuf response; the code
// under test reads only `.accepted`, so a minimal fake is enough. The unchecked
// cast lives here once, on the value — never inlined into a property access.
type KillFn = typeof coordClient.sessionsKill;
const asKillFn = (
  impl: (a: { sessionId: string; force?: boolean }) => Promise<{ accepted: boolean }>,
): KillFn => impl as unknown as KillFn;

const realKill = coordClient.sessionsKill;

function clearStore(): void {
  // Per-key delete (the projector's delete path) — a whole-record set is a
  // Solid merge no-op, and this store is a process-wide singleton across suites.
  for (const id of Object.keys(rootStore.sessions)) setRootStore("sessions", id, undefined as unknown as Session);
  for (const fp of Object.keys(rootStore.workers)) setRootStore("workers", fp, undefined as unknown as Worker);
}

describe("closeSession", () => {
  beforeEach(() => { clearStore(); undoAll(); idSeq = 0; });
  // undoAll clears any pending-close timers this file scheduled so they never
  // fire into a sibling suite; restore the real RPC method we stubbed.
  afterEach(() => { clearStore(); undoAll(); coordClient.sessionsKill = realKill; });

  test("closeLabelsFor reproduces the terminal/folder/server triple", () => {
    // Test fixture worker: shortServerLabel reads only `.label`.
    setRootStore("workers", FP, { label: "mymac" } as unknown as Worker);
    const s = seedSession();
    expect(closeLabelsFor(s)).toEqual({ terminalName: "you/roost", folder: "roost", server: "mymac" });
  });

  test("closeLabelsFor server falls back to fp.slice(0,6) with no worker label", () => {
    const s = seedSession(); // no worker seeded → fp prefix
    expect(closeLabelsFor(s).server).toBe("aaaaaa");
  });

  test("siblingOrHomeHref → newest open sibling in the same folder", () => {
    const s1 = seedSession();
    const s2 = seedSession(); // same worker_fp + cwd ⇒ same folder, newer created_at
    expect(siblingOrHomeHref(s1)).toBe(`/s/${s2.id}`);
  });

  test("siblingOrHomeHref → Home when the folder has no other open session", () => {
    const s1 = seedSession();
    expect(siblingOrHomeHref(s1)).toBe("/");
  });

  test("siblingOrHomeHref → Home when the only sibling is soft-closing (Step 5)", () => {
    const s1 = seedSession();
    const s2 = seedSession();
    scheduleClose(s2.id, { terminalName: "t", folder: "f", server: "s" }, () => {});
    expect(siblingOrHomeHref(s1)).toBe("/");
  });

  test("newestOpenSessionForFolderKey excludes a pending-close id (Step 5)", () => {
    const s1 = seedSession();
    const s2 = seedSession();
    const fk = folderKeyOf(s1);
    expect(newestOpenSessionForFolderKey(fk, s1.id)?.id).toBe(s2.id);
    scheduleClose(s2.id, { terminalName: "t", folder: "f", server: "s" }, () => {});
    expect(newestOpenSessionForFolderKey(fk, s1.id)).toBeNull();
  });

  test("killAfterUndo fires sessionsKill once when accepted", async () => {
    let last: Promise<{ accepted: boolean }> = Promise.resolve({ accepted: true });
    const killMock = mock((_a: { sessionId: string; force?: boolean }) => {
      last = Promise.resolve({ accepted: true });
      return last;
    });
    coordClient.sessionsKill = asKillFn(killMock);
    killAfterUndo("sess-1")();
    await last; // resumes the accepted-check continuation → proves no force call
    expect(killMock).toHaveBeenCalledTimes(1);
    expect(killMock.mock.calls[0]?.[0]).toEqual({ sessionId: "sess-1" });
  });

  test("killAfterUndo escalates to force when the first kill is not accepted", async () => {
    let last: Promise<{ accepted: boolean }> = Promise.resolve({ accepted: false });
    const killMock = mock((_a: { sessionId: string; force?: boolean }) => {
      last = Promise.resolve({ accepted: false });
      return last;
    });
    coordClient.sessionsKill = asKillFn(killMock);
    killAfterUndo("sess-2")();
    await last; // resumes closeSession → the force call fires before we assert
    expect(killMock).toHaveBeenCalledTimes(2);
    expect(killMock.mock.calls[1]?.[0]).toEqual({ sessionId: "sess-2", force: true });
  });

  test("killAfterUndo surfaces one error toast when the kill throws", async () => {
    let last: Promise<{ accepted: boolean }> = Promise.resolve({ accepted: true });
    const killMock = mock((_a: { sessionId: string; force?: boolean }) => {
      last = Promise.reject(new Error("worker offline"));
      return last;
    });
    coordClient.sessionsKill = asKillFn(killMock);
    const before = toasts().length;
    killAfterUndo("sess-3")();
    await last.catch(() => {}); // closeSession's catch (registered first) runs before this settles
    expect(killMock).toHaveBeenCalledTimes(1);
    const added = toasts().slice(before);
    expect(added).toHaveLength(1);
    expect(added[0]?.kind).toBe("err");
    expect(added[0]?.msg).toContain("Close failed: worker offline");
  });
});
