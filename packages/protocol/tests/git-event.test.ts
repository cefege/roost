// `git` SessionEvent: folds a branch onto the session + survives the proto
// round-trip (incl. the null "not a repo" case). Guards the wire ritual added
// for the cell-grid folder-row branch subtitle.

import { test, expect } from "bun:test";
import { SessionEvent, foldAll } from "../src/wire/event.ts";
import { eventToProto, protoToEvent } from "../src/wire/event-proto.ts";

const SID = "00000000-0000-4000-8000-000000000abc";
const WFP = "a".repeat(64);

test("git event folds branch onto an opened session", () => {
  const opened = SessionEvent.parse({
    kind: "opened", session_id: SID, worker_fp: WFP, channel: 3,
    session_kind: "shell", cwd: "/x", ts: 1,
  });
  const git = SessionEvent.parse({ kind: "git", session_id: SID, branch: "feat/x", ts: 2 });
  expect(foldAll([opened, git]).get(SID)?.git_branch).toBe("feat/x");
});

test("git event proto round-trips, including null branch", () => {
  for (const branch of ["main", null] as const) {
    const e = SessionEvent.parse({ kind: "git", session_id: SID, branch, ts: 5 });
    const back = protoToEvent(eventToProto(e, 9))!;
    expect(back).toMatchObject({ kind: "git", session_id: SID, branch, _event_id: 9 });
  }
});

test("git event carries + folds an optional remote (owner/repo)", () => {
  const opened = SessionEvent.parse({
    kind: "opened", session_id: SID, worker_fp: WFP, channel: 3,
    session_kind: "shell", cwd: "/x", ts: 1,
  });
  const git = SessionEvent.parse({ kind: "git", session_id: SID, branch: "main", remote: "o/r", ts: 2 });
  const back = protoToEvent(eventToProto(git, 3))!;
  expect(back).toMatchObject({ kind: "git", branch: "main", remote: "o/r" });
  expect(foldAll([opened, git]).get(SID)?.git_remote).toBe("o/r");
});
