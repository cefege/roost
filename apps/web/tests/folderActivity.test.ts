// computeFolderActivity — cumulative terminal counts per folder.
// Tests terminal aggregation including subtree descendants.

import { expect, test, describe } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { computeFolderActivity } from "../src/lib/folderActivity.ts";

const FP = asWorkerFp("aa".repeat(32));
const FP2 = asWorkerFp("bb".repeat(32));

function sess(over: Record<string, unknown>): Session {
  return {
    id: asSessionId("00000000-0000-4000-8000-000000000001"),
    worker_fp: FP,
    channel: asChannelId(1),
    kind: "shell",
    cwd: "/x",
    spawn_cwd: "/x",
    workspace_id: null,
    status: "open",
    created_at: 1000,
    closed_at: null,
    custom_title: null,
    ...over,
  } as Session;
}

describe("computeFolderActivity", () => {
  test("empty inputs → empty map", () => {
    expect(computeFolderActivity([], FP, [])).toEqual(new Map());
    expect(computeFolderActivity([], FP, ["/a"])).toEqual(new Map());
  });

  test("no sessions in folders → empty map", () => {
    const sessions = [sess({ cwd: "/other" })];
    expect(computeFolderActivity(sessions, FP, ["/a", "/b"])).toEqual(new Map());
  });

  test("direct session in folder counted", () => {
    const sessions = [sess({ cwd: "/a" })];
    const result = computeFolderActivity(sessions, FP, ["/a", "/b"]);
    expect(result.get("/a")).toEqual({ terminals: 1 });
    expect(result.has("/b")).toBe(false);
  });

  test("cumulative: session in subfolder counted toward parent", () => {
    const sessions = [sess({ cwd: "/a/b/c" })];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 1 });
  });

  test("cumulative: multiple sessions in subtree summed", () => {
    const sessions = [
      sess({ id: asSessionId("00000000-0000-4000-8000-000000000001"), cwd: "/a" }),
      sess({ id: asSessionId("00000000-0000-4000-8000-000000000002"), cwd: "/a/b" }),
      sess({ id: asSessionId("00000000-0000-4000-8000-000000000003"), cwd: "/a/c/d" }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 3 });
  });

  test("filters by worker_fp", () => {
    const sessions = [
      sess({ cwd: "/a", worker_fp: FP }),
      sess({ cwd: "/a", worker_fp: FP2 }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 1 });
  });

  test("trailing slash on path handled", () => {
    const sessions = [sess({ cwd: "/a" })];
    const result = computeFolderActivity(sessions, FP, ["/a/"]);
    expect(result.get("/a/")).toEqual({ terminals: 1 });
  });

  test("root path / handled", () => {
    const sessions = [
      sess({ cwd: "/" }),
      sess({ cwd: "/a" }),
      sess({ cwd: "/a/b" }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/"]);
    // Every session with cwd "/" or starting with "/" is under root
    expect(result.get("/")).toEqual({ terminals: 3 });
  });

  test("~ home path handled", () => {
    const sessions = [sess({ cwd: "~/a" })];
    const result = computeFolderActivity(sessions, FP, ["~"]);
    expect(result.get("~")).toEqual({ terminals: 1 });
  });
  test("Windows descendants compare case-insensitively at segment boundaries", () => {
    const sessions = [
      sess({ cwd: "c:/work/project/src" }),
      sess({ cwd: "C:/WORKER/not-a-child" }),
    ];
    const result = computeFolderActivity(sessions, FP, ["C:/Work/Project"]);
    expect(result.get("C:/Work/Project")).toEqual({ terminals: 1 });
  });
});
