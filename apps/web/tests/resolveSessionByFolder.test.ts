// resolveSessionByFolder — the /t/:workerFp/*folderPath route resolver.
// Asserts: matches OPEN session by (worker_fp, spawn_cwd); collision tiebreak =
// newest created_at; ignores closed / wrong-worker / wrong-folder; falls back to
// cwd for pre-migration rows with no spawn_cwd.

import { expect, test, describe, beforeEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../src/store/root.ts";
import { resolveSessionByFolder } from "../src/store/selectors.ts";

const FP_A = asWorkerFp("aa".repeat(32));
const FP_B = asWorkerFp("bb".repeat(32));

function sess(over: Omit<Partial<Session>, "id"> & { id: string }): Session {
  const { id, ...rest } = over;
  return {
    worker_fp: FP_A,
    channel: asChannelId(1),
    kind: "shell",
    cwd: "/Users/you/roost",
    spawn_cwd: "/Users/you/roost",
    workspace_id: null,
    status: "open",
    created_at: 1000,
    closed_at: null,
    custom_title: null,
    ...rest,
    id: asSessionId(id),
  } as Session;
}

function seed(sessions: Session[]) {
  const rec: Record<string, Session> = {};
  for (const s of sessions) rec[s.id] = s;
  setRootStore("sessions", rec);
}

describe("resolveSessionByFolder", () => {
  beforeEach(() => setRootStore("sessions", {} as Record<string, Session>));

  test("matches an open session by (worker, spawn folder)", () => {
    seed([sess({ id: "00000000-0000-4000-8000-000000000001" })]);
    expect(resolveSessionByFolder(FP_A, "/Users/you/roost")?.id as string).toBe(
      "00000000-0000-4000-8000-000000000001",
    );
  });

  test("collision tiebreak → newest created_at wins", () => {
    seed([
      sess({ id: "00000000-0000-4000-8000-000000000001", created_at: 1000 }),
      sess({ id: "00000000-0000-4000-8000-000000000002", created_at: 3000 }),
      sess({ id: "00000000-0000-4000-8000-000000000003", created_at: 2000 }),
    ]);
    expect(resolveSessionByFolder(FP_A, "/Users/you/roost")?.id as string).toBe(
      "00000000-0000-4000-8000-000000000002",
    );
  });

  test("ignores closed, wrong-worker, wrong-folder", () => {
    seed([
      sess({ id: "00000000-0000-4000-8000-000000000001", status: "closed" }),
      sess({ id: "00000000-0000-4000-8000-000000000002", worker_fp: FP_B }),
      sess({ id: "00000000-0000-4000-8000-000000000003", spawn_cwd: "/elsewhere" }),
    ]);
    expect(resolveSessionByFolder(FP_A, "/Users/you/roost")).toBeNull();
  });

  test("spawn_cwd drift-proof: matches spawn folder, not the drifted cwd", () => {
    seed([sess({ id: "00000000-0000-4000-8000-000000000001", cwd: "/Users/you/roost/apps/web" })]);
    expect(resolveSessionByFolder(FP_A, "/Users/you/roost/apps/web")).toBeNull();
    expect(resolveSessionByFolder(FP_A, "/Users/you/roost")?.id as string).toBe(
      "00000000-0000-4000-8000-000000000001",
    );
  });

  test("pre-migration row (no spawn_cwd) falls back to cwd", () => {
    const s = sess({ id: "00000000-0000-4000-8000-000000000001", cwd: "/legacy" });
    delete (s as { spawn_cwd?: string }).spawn_cwd;
    seed([s]);
    expect(resolveSessionByFolder(FP_A, "/legacy")?.id as string).toBe(
      "00000000-0000-4000-8000-000000000001",
    );
  });
});

void rootStore;
