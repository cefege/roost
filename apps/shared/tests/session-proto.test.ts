// Round-trip coverage for the canonical terminal Session protobuf adapter.

import { describe, it, expect } from "bun:test";
import fc from "fast-check";
import { sessionToProto, sessionFromProto } from "../src/wire/session-proto.ts";
import { asSessionId, asWorkerFp, asWorkspaceId, asChannelId } from "../src/wire/brand.ts";

const sidArb = fc.uuid().map(asSessionId);
const wfpArb = fc.hexaString({ minLength: 64, maxLength: 64 }).map(asWorkerFp);
const channelArb = fc.integer({ min: 0, max: 0xffffffff }).map(asChannelId);
const tsArb = fc.integer({ min: 1, max: 2 ** 50 });

describe("session-proto", () => {
  it("round-trips an open shell session", () => {
    fc.assert(fc.property(sidArb, wfpArb, channelArb, fc.string(), tsArb, (sid, wfp, channel, cwd, ts) => {
      const session = {
        id: sid,
        worker_fp: wfp,
        channel,
        kind: "shell" as const,
        cwd,
        workspace_id: null,
        status: "open" as const,
        created_at: ts,
        closed_at: null,
        custom_title: null,
      };
      expect(sessionFromProto(sessionToProto(session))).toEqual(session);
    }));
  });

  it("round-trips every optional terminal metadata field", () => {
    const session = {
      id: asSessionId("00000000-0000-4000-8000-000000000001"),
      worker_fp: asWorkerFp("aa".repeat(32)),
      channel: asChannelId(7),
      kind: "shell" as const,
      cwd: "/repo/subdir",
      spawn_cwd: "/repo",
      workspace_id: asWorkspaceId("00000000-0000-4000-8000-000000000002"),
      status: "closed" as const,
      created_at: 1_781_500_000,
      closed_at: 1_781_500_100,
      custom_title: "release",
      git_branch: "main",
      git_remote: "owner/repo",
      pr_number: 42,
      pr_state: "merged" as const,
      pr_checks: "passing" as const,
      pr_url: "https://github.com/owner/repo/pull/42",
      ports: [3000, 5173],
    };
    expect(sessionFromProto(sessionToProto(session))).toEqual(session);
  });
});
