// activityLine — the one context line on a folder row. Locks the `needs`
// bucket's three sub-cases (needs-input → "Waiting on your input"; worker
// present-but-offline → "Machine offline"; idle/done-with-unseen-output →
// last message or "Finished"), added when the separate flat "Needs you" strip
// was deleted (2026-07-07) and needs-state moved onto the folder rows.

import { expect, test, describe, beforeEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session, Worker } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../src/store/root.ts";
import { setRoutableFps } from "../src/store/sync-routable.ts";
import { activityLine } from "../src/lib/folderSubtitle.ts";

const FP = asWorkerFp("aa".repeat(32));

function sess(agent: Session["agent"]): Session {
  return {
    id: asSessionId("00000000-0000-4000-8000-000000000001"),
    worker_fp: FP,
    channel: asChannelId(1),
    kind: agent ? "claude" : "shell",
    cwd: "/x",
    spawn_cwd: "/x",
    workspace_id: null,
    status: "open",
    agent,
    created_at: 1000,
    closed_at: null,
    custom_title: null,
  } as Session;
}

function agent(over: Record<string, unknown>): Session["agent"] {
  return { kind: "claude", status: "idle", last_message: null, current_tool: null, current_block: null, ...over } as Session["agent"];
}

describe("activityLine — needs bucket", () => {
  beforeEach(() => {
    setRootStore("sessions", {} as Record<string, Session>);
    setRootStore("workers", FP, undefined as unknown as Worker); // per-key delete — replacing the Record subtree is a silent no-op (L11)
    setRoutableFps(null);
  });

  test("needs-input → Waiting on your input", () => {
    expect(activityLine(sess(agent({ status: "needs-input" })), "needs")).toBe("Waiting on your input");
  });

  test("worker present but unreachable → Machine offline", () => {
    setRootStore("workers", { [FP]: { fp: FP } as Worker });
    setRoutableFps(new Set<string>()); // routable set known + empty → FP not routable
    expect(activityLine(sess(agent({ status: "idle" })), "needs")).toBe("Machine offline — reopen to refresh");
  });

  test("done with unseen output, worker gone → first line of last message", () => {
    expect(
      activityLine(sess(agent({ status: "done", last_message: { text: "Migration applied\nrest", ts: 5 } })), "needs"),
    ).toBe("Migration applied");
  });

  test("done, no last message, worker gone → Finished (not mislabeled offline)", () => {
    expect(activityLine(sess(agent({ status: "done" })), "needs")).toBe("Finished");
  });

  test("running bucket unchanged → tool name", () => {
    expect(activityLine(sess(agent({ status: "running", current_tool: { name: "Bash" } })), "running")).toBe("Bash…");
  });
});

void rootStore;

describe("activityLine — idle bucket", () => {
  test("idle + a message → first line of the message", () => {
    expect(
      activityLine(sess(agent({ status: "idle", last_message: { text: "Done refactor\nmore", ts: 9 } })), "idle"),
    ).toBe("Done refactor");
  });

  // Regression: a bare git branch used to be dumped here as the idle fallback,
  // reading as unlabeled mystery text. It's a self-labeled ⎇ chip now — the
  // idle-no-message subtitle is blank.
  test("idle + no message → blank, NOT the git branch", () => {
    const s = { ...sess(agent({ status: "idle" })), git_branch: "feat/sidebar-chips" } as Session;
    expect(activityLine(s, "idle")).toBe("");
  });
});
