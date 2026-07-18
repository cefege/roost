// computeFolderActivity — cumulative session counts per folder.
// Tests terminal/agent aggregation including subtree descendants.

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
    agent: null,
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
    expect(result.get("/a")).toEqual({ terminals: 1, agentsRunning: 0, needsInput: 0 });
    expect(result.has("/b")).toBe(false);
  });

  test("cumulative: session in subfolder counted toward parent", () => {
    const sessions = [sess({ cwd: "/a/b/c" })];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 1, agentsRunning: 0, needsInput: 0 });
  });

  test("cumulative: multiple sessions in subtree summed", () => {
    const sessions = [
      sess({ id: asSessionId("00000000-0000-4000-8000-000000000001"), cwd: "/a" }),
      sess({ id: asSessionId("00000000-0000-4000-8000-000000000002"), cwd: "/a/b" }),
      sess({ id: asSessionId("00000000-0000-4000-8000-000000000003"), cwd: "/a/c/d" }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 3, agentsRunning: 0, needsInput: 0 });
  });

  test("filters by worker_fp", () => {
    const sessions = [
      sess({ cwd: "/a", worker_fp: FP }),
      sess({ cwd: "/a", worker_fp: FP2 }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 1, agentsRunning: 0, needsInput: 0 });
  });

  test("agent running counted", () => {
    const sessions = [
      sess({
        cwd: "/a",
        kind: "claude",
        agent: { kind: "claude", status: "running", last_message: null, current_tool: null, current_block: null },
      }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 1, agentsRunning: 1, needsInput: 0 });
  });

  test("agent needs-input counted as running + needs", () => {
    const sessions = [
      sess({
        cwd: "/a",
        kind: "claude",
        agent: { kind: "claude", status: "needs-input", last_message: null, current_tool: null, current_block: null },
      }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 1, agentsRunning: 1, needsInput: 1 });
  });

  test("multiple agents: running and needs-input summed separately", () => {
    const sessions = [
      sess({
        id: asSessionId("00000000-0000-4000-8000-000000000001"),
        cwd: "/a",
        kind: "claude",
        agent: { kind: "claude", status: "needs-input", last_message: null, current_tool: null, current_block: null },
      }),
      sess({
        id: asSessionId("00000000-0000-4000-8000-000000000002"),
        cwd: "/a",
        kind: "claude",
        agent: { kind: "claude", status: "running", last_message: null, current_tool: null, current_block: null },
      }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/a"]);
    expect(result.get("/a")).toEqual({ terminals: 2, agentsRunning: 2, needsInput: 1 });
  });

  test("trailing slash on path handled", () => {
    const sessions = [sess({ cwd: "/a" })];
    const result = computeFolderActivity(sessions, FP, ["/a/"]);
    expect(result.get("/a/")).toEqual({ terminals: 1, agentsRunning: 0, needsInput: 0 });
  });

  test("root path / handled", () => {
    const sessions = [
      sess({ cwd: "/" }),
      sess({ cwd: "/a" }),
      sess({ cwd: "/a/b" }),
    ];
    const result = computeFolderActivity(sessions, FP, ["/"]);
    // Every session with cwd "/" or starting with "/" is under root
    expect(result.get("/")).toEqual({ terminals: 3, agentsRunning: 0, needsInput: 0 });
  });

  test("~ home path handled", () => {
    const sessions = [sess({ cwd: "~/a" })];
    const result = computeFolderActivity(sessions, FP, ["~"]);
    expect(result.get("~")).toEqual({ terminals: 1, agentsRunning: 0, needsInput: 0 });
  });
});
