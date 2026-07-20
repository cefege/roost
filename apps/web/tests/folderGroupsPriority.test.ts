// folderGroups priority sort + attentionKind — the "blocked to the top" fix.
// Locks that a folder BLOCKED waiting on your input sorts above a folder that
// merely FINISHED with unseen output, even when the finished folder's activity
// is more recent (the core regression). Also pins attentionKind's precedence:
// needs-input → offline worker → idle/done-with-unseen → null.
//
// buildFolderGroups takes an explicit sessions array here (its test seam) so the
// sort is exercised directly, without the frozen allSessions() memo that bun's
// Solid server build produces — see sessionSeen.test.ts for the SSR caveat.

import { expect, test, describe, beforeEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session, Worker } from "@roost/shared/wire";
import { setRootStore } from "../src/store/root.ts";
import { setRoutableFps } from "../src/store/sync-routable.ts";
import { markSeen } from "../src/lib/sessionSeen.ts";
import { buildFolderGroups } from "../src/lib/folderGroups.ts";
import { attentionKind } from "../src/lib/attention.ts";

const FP = asWorkerFp("aa".repeat(32));
const uuid = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;

function agent(over: Record<string, unknown>): Session["agent"] {
  return { kind: "claude", status: "idle", last_message: null, current_tool: null, current_block: null, ...over } as Session["agent"];
}
function sess(over: Omit<Partial<Session>, "id"> & { id: string }): Session {
  const { id, ...rest } = over;
  return {
    worker_fp: FP, channel: asChannelId(1), kind: "claude",
    cwd: "/x", spawn_cwd: "/x", workspace_id: null, status: "open", agent: null,
    created_at: 1000, closed_at: null, custom_title: null,
    ...rest, id: asSessionId(id),
  } as Session;
}

describe("buildFolderGroups — priority sort", () => {
  beforeEach(() => {
    // Reachable worker (routable=null → freshness check → online), read live at build time.
    setRoutableFps(null);
    setRootStore("workers", { [FP]: { fp: FP, last_seen_ms: Date.now() } as Worker });
  });

  // done's last_message (ts 900) is NEWER than blocked's (ts 100): recency alone
  // would float done up, but blocked (waiting on YOU) must still sort first.
  const folders = (): Session[] => [
    sess({ id: uuid(1), cwd: "/blocked", agent: agent({ status: "needs-input", last_message: { text: "?", ts: 100 } }) }),
    sess({ id: uuid(2), cwd: "/done", agent: agent({ status: "done", last_message: { text: "shipped", ts: 900 } }) }),
    sess({ id: uuid(3), cwd: "/running", agent: agent({ status: "running", last_message: { text: "r", ts: 500 } }) }),
    sess({ id: uuid(4), cwd: "/idle", agent: agent({ status: "idle", last_message: null }) }),
  ];

  test("blocked sorts above a MORE-RECENT done-unseen folder", () => {
    const order = buildFolderGroups(folders()).map((grp) => grp.key);
    expect(order.indexOf(`${FP}::/blocked`)).toBeLessThan(order.indexOf(`${FP}::/done`));
  });

  test("four folders order blocked → done-unseen → running → idle", () => {
    const groups = buildFolderGroups(folders());
    expect(groups.map((grp) => grp.key)).toEqual([
      `${FP}::/blocked`, `${FP}::/done`, `${FP}::/running`, `${FP}::/idle`,
    ]);
    const priorityByKey: Record<string, number> = {};
    for (const grp of groups) priorityByKey[grp.key] = grp.priority;
    expect(priorityByKey[`${FP}::/blocked`]).toBe(3);
    expect(priorityByKey[`${FP}::/done`]).toBe(2);
    expect(priorityByKey[`${FP}::/running`]).toBe(1);
    expect(priorityByKey[`${FP}::/idle`]).toBe(0);
  });
});

describe("attentionKind", () => {
  beforeEach(() => {
    setRootStore("workers", FP, undefined as unknown as Worker);
    setRoutableFps(null);
  });

  test("needs-input → blocked", () => {
    expect(attentionKind(sess({ id: uuid(10), agent: agent({ status: "needs-input" }) }))).toBe("blocked");
  });

  test("idle agent on an offline machine → offline", () => {
    setRootStore("workers", { [FP]: { fp: FP } as Worker });
    setRoutableFps(new Set<string>()); // routable known + empty → FP unreachable
    expect(attentionKind(sess({ id: uuid(11), agent: agent({ status: "idle" }) }))).toBe("offline");
  });

  test("done with last_message newer than lastSeenAt → done", () => {
    expect(attentionKind(sess({ id: uuid(12), agent: agent({ status: "done", last_message: { text: "x", ts: 5 } }) }))).toBe("done");
  });

  test("a seen done → null", () => {
    const s = sess({ id: uuid(13), agent: agent({ status: "done", last_message: { text: "x", ts: 5 } }) });
    markSeen(s.id, 5); // seen up to the last message → nothing unseen
    expect(attentionKind(s)).toBeNull();
  });
});
