// folderRowTargetId — the tab a sidebar folder row opens on click. Asserts a
// needy folder opens its lead; a remembered pane reopens only when it is still
// an OPEN pane in THIS folder; a closed / cd'd-away / absent remembered pane
// falls back to the lead (the "clicking a row opens another workspace" bug).

import { expect, test, describe, beforeEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { setRootStore } from "../src/store/root.ts";
import { folderRowTargetId } from "../src/store/selectors.ts";
import { folderKeyOf } from "../src/lib/folderKey.ts";

const FP = asWorkerFp("aa".repeat(32));
const A = "/Users/you/A";
const B = "/Users/you/B";
const uuid = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;

function sess(over: Omit<Partial<Session>, "id"> & { id: string }): Session {
  const { id, ...rest } = over;
  return {
    worker_fp: FP, channel: asChannelId(1), kind: "shell",
    cwd: A, spawn_cwd: A, workspace_id: null, status: "open", agent: null,
    created_at: 1000, closed_at: null, custom_title: null,
    ...rest, id: asSessionId(id),
  } as Session;
}
function seed(list: Session[]) {
  const rec: Record<string, Session> = {};
  for (const s of list) rec[s.id] = s;
  setRootStore("sessions", rec);
}

const lead = sess({ id: uuid(1), cwd: A });
const KEY = folderKeyOf(lead); // `${FP}::/Users/you/A`

describe("folderRowTargetId", () => {
  beforeEach(() => setRootStore("sessions", {} as Record<string, Session>));

  test("needs folder → always the lead, ignoring any remembered tab", () => {
    const s2 = sess({ id: uuid(2), cwd: A });
    seed([lead, s2]);
    expect(folderRowTargetId(KEY, "needs", lead.id, s2.id)).toBe(lead.id);
  });

  test("remembered open pane in THIS folder → reopen it", () => {
    const s2 = sess({ id: uuid(2), cwd: A });
    seed([lead, s2]);
    expect(folderRowTargetId(KEY, "idle", lead.id, s2.id)).toBe(s2.id);
  });

  test("remembered pane cd'd to ANOTHER folder → fall back to lead", () => {
    const s2 = sess({ id: uuid(2), cwd: B }); // left folder A, now lives in B
    seed([lead, s2]);
    expect(folderRowTargetId(KEY, "idle", lead.id, s2.id)).toBe(lead.id);
  });

  test("remembered pane closed → fall back to lead", () => {
    const s2 = sess({ id: uuid(2), cwd: A, status: "closed" });
    seed([lead, s2]);
    expect(folderRowTargetId(KEY, "running", lead.id, s2.id)).toBe(lead.id);
  });

  test("no remembered pane → lead", () => {
    seed([lead]);
    expect(folderRowTargetId(KEY, "idle", lead.id, null)).toBe(lead.id);
  });
});
