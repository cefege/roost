// folderPathOf — the sidebar/TabBar folder-grouping key. Asserts a session's
// folder follows its LIVE cwd, even when workspace-linked (a cd re-homes the
// terminal so an emptied workspace folder drops out of the sidebar). Naming still
// resolves via workspaceForFolder while cwd == the workspace's folder_path.

import { expect, test, describe, beforeEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId, asWorkspaceId } from "@roost/shared/wire";
import type { Session, Workspace } from "@roost/shared/wire";
import { setRootStore } from "../src/store/root.ts";
import { folderPathOf, folderKeyOf, folderDisplayName } from "../src/lib/folderKey.ts";

const FP_A = asWorkerFp("aa".repeat(32));
const WS_ID = asWorkspaceId("11111111-1111-4000-8000-000000000001");

function sess(over: Omit<Partial<Session>, "id"> & { id: string }): Session {
  const { id, ...rest } = over;
  return {
    worker_fp: FP_A, channel: asChannelId(1), kind: "shell",
    cwd: "/Users/you/roost", spawn_cwd: "/Users/you/roost",
    workspace_id: null, status: "open", agent: null,
    created_at: 1000, closed_at: null, custom_title: null,
    ...rest, id: asSessionId(id),
  } as Session;
}

describe("folderPathOf follows live cwd", () => {
  beforeEach(() => {
    setRootStore("sessions", {} as Record<string, Session>);
    setRootStore("workspaces", {
      [WS_ID]: { id: WS_ID, worker_fp: FP_A, folder_path: "/Users/you/roost", name: "MyProj" } as Workspace,
    } as Record<string, Workspace>);
  });

  test("workspace-linked session at spawn folder groups by folder_path (== cwd)", () => {
    const s = sess({ id: "00000000-0000-4000-8000-000000000001", workspace_id: WS_ID });
    expect(folderPathOf(s)).toBe("/Users/you/roost");
    expect(folderKeyOf(s)).toBe(`${FP_A}::/Users/you/roost`);
    expect(folderDisplayName(s)).toBe("MyProj"); // workspaceForFolder match
  });

  test("cd away: workspace-linked session re-homes to new cwd, drops the workspace name", () => {
    const s = sess({ id: "00000000-0000-4000-8000-000000000001", workspace_id: WS_ID, cwd: "/tmp" });
    expect(folderPathOf(s)).toBe("/tmp"); // NOT the frozen /Users/you/roost
    expect(folderKeyOf(s)).toBe(`${FP_A}::/tmp`);
    expect(folderDisplayName(s)).toBe("tmp"); // basename fallback — no workspace at /tmp
  });
});
