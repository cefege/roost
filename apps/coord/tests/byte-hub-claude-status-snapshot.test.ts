// Regression: claude_status is volatile live-delta (B10). A browser that
// connects AFTER a claude session's last transition must still learn the
// session is a claude — the Sync handler replays snapshotClaudeStatuses() on
// connect. Here we drive the cache directly: publish updates the snapshot,
// last-write-wins per session, and `closed` prunes it.

import { describe, it, expect } from "bun:test";
import { publishClaudeStatus, snapshotClaudeStatuses, primeChannelMap, installByteHubBusHook } from "../src/byte-hub.ts";
import { sessionBus } from "../src/buses.ts";

installByteHubBusHook(); // installs the sessionBus `closed`-prune (idempotent)
import { asWorkerFp, asChannelId, asSessionId } from "@roost/shared/wire";
import type { SessionEvent } from "@roost/shared/wire";

const FP = "aa".repeat(32);
const statusFor = (sid: string) =>
  snapshotClaudeStatuses().find((s) => s.session_id === sid)?.status;

describe("byte-hub claude_status snapshot cache", () => {
  it("caches last status per session (last-write-wins) and prunes on close", () => {
    const sid = "11111111-1111-1111-1111-111111111111";
    primeChannelMap([{ id: sid, worker_fp: FP, channel: 7 }]);

    publishClaudeStatus(asWorkerFp(FP), asChannelId(7), "running");
    expect(statusFor(sid)).toBe("running");

    publishClaudeStatus(asWorkerFp(FP), asChannelId(7), "idle");
    expect(statusFor(sid)).toBe("idle"); // last-write-wins, not appended

    const closed: SessionEvent = { kind: "closed", session_id: asSessionId(sid), exit_code: 0, ts: Date.now() };
    sessionBus.publish(closed);
    expect(statusFor(sid)).toBeUndefined(); // pruned alongside the channel map
  });

  it("drops status for an unmapped channel (no snapshot leak)", () => {
    const before = snapshotClaudeStatuses().length;
    publishClaudeStatus(asWorkerFp(FP), asChannelId(9999), "running");
    expect(snapshotClaudeStatuses().length).toBe(before);
  });
});
