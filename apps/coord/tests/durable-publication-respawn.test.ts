// Verifies that respawn publication swaps channel routes atomically for PTY output and input.
// Bun discovers this suite directly and drives an isolated durable-publication fixture.
// The contract depends on event-log projection, byte-hub routing, and the global bytes bus.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import {
  getCachedSessionWorker,
  lookupSessionId,
  publishBytes,
} from "../src/byte-hub.ts";
import { globalBytesBus } from "../src/buses.ts";
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
  test("routes PTY metadata on the new channel immediately, old channel dead", async () => {
    await append(openedEvent(SID_A, 11));

    const bytes: string[] = [];
    const unsubBytes = globalBytesBus.subscribe((m) => {
      if (m.session_id === SID_A) bytes.push(new TextDecoder().decode(m.bytes));
    });
    try {
      await append(respawnedEvent(SID_A, 12));

      // No browser reconnect, no re-`opened`: the new channel is already live.
      publishBytes(FP, asChannelId(12), new TextEncoder().encode("\x1b]0;new title\x07"));
      // The dead channel routes nothing — a surviving stale key would fan the
      // old core's trailing output into the same session.
      publishBytes(FP, asChannelId(11), new TextEncoder().encode("stale"));
    } finally {
      unsubBytes();
    }

    expect(bytes).toEqual(["\x1b]0;new title\x07"]);
    expect(lookupSessionId(FP, asChannelId(11))).toBeUndefined();
    expect(lookupSessionId(FP, asChannelId(12))).toBe(SID_A);
    // Input/claim routing (getCachedSessionWorker) moves in the same step.
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
