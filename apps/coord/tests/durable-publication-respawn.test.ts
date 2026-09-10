// Verifies that respawn publication atomically replaces the live channel route.
// The durable route is shared by terminal cells, semantic metadata, and input.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { getCachedSessionWorker, lookupSessionId } from "../src/byte-hub.ts";
import { SessionEvent, asChannelId } from "@roost/shared/wire";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";

const fixture = createDurablePublicationFixture({
  slug: "respawn",
  primaryFingerprintByte: "d3",
  secondaryFingerprintByte: "d4",
  sessionGroup: "2",
});
const {
  FP,
  SID_A,
  append,
  committedChannel,
  openedEvent,
  respawnedEvent,
} = fixture;

beforeEach(() => fixture.reset());
afterAll(() => fixture.close());

describe("respawned channel-index operation", () => {
  test("binds the new channel immediately and removes the stale route", async () => {
    await append(openedEvent(SID_A, 11));
    await append(respawnedEvent(SID_A, 12));

    expect(lookupSessionId(FP, asChannelId(11))).toBeUndefined();
    expect(lookupSessionId(FP, asChannelId(12))).toBe(SID_A);
    expect(getCachedSessionWorker(SID_A)).toEqual({ worker_fp: FP, channel: 12 });
    expect(committedChannel(SID_A)).toBe(12);
  });

  test("a later close for the respawned session prunes exactly the new binding", async () => {
    await append(openedEvent(SID_A, 11));
    await append(respawnedEvent(SID_A, 12));
    await append(SessionEvent.parse({
      kind: "closed", session_id: SID_A, exit_code: 0, ts: 3,
    }));
    expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
    expect(getCachedSessionWorker(SID_A)).toBeUndefined();
  });
});
