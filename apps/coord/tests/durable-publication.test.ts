// Verifies that durable lifecycle events publish only after their database and route commit.
// Bun discovers this suite directly and resets isolated state through its publication fixture.
// The contract depends on appendEvent ordering, the session bus, and exact byte-hub bindings.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { appendEvent } from "../src/event-log.ts";
import { getCachedSessionWorker, lookupSessionId } from "../src/byte-hub.ts";
import { sessionBus } from "../src/buses.ts";
import { asChannelId } from "@roost/shared/wire";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";

const fixture = createDurablePublicationFixture({
  slug: "publication-order",
  primaryFingerprintByte: "d1",
  secondaryFingerprintByte: "d2",
  sessionGroup: "1",
});
const {
  FP,
  SID_A,
  SID_C,
  DASHBOARD_ID,
  committedChannel,
  openedEvent,
  respawnedEvent,
} = fixture;

let writer: typeof fixture.writer;
let clientSeq = 0;

function append(
  event: Parameters<typeof fixture.append>[0],
  workerFp: string | null = FP,
) {
  const pendingAppend = fixture.append(event, workerFp);
  clientSeq = fixture.clientSeq;
  return pendingAppend;
}

beforeEach(async () => {
  await fixture.reset();
  writer = fixture.writer;
  clientSeq = fixture.clientSeq;
});
afterAll(() => fixture.close());

describe("appendEvent durable publication order", () => {
  test("no sessionBus publication precedes commit + exact channel binding", async () => {
    interface Observation {
      kind: string;
      boundNew: string | undefined;
      boundOld: string | undefined;
      cachedChannel: number | undefined;
      committedChannel: number | null;
    }
    const seen: Observation[] = [];
    const unsub = sessionBus.subscribe((ev) => {
      seen.push({
        kind: ev.kind,
        boundNew: lookupSessionId(FP, asChannelId(ev.kind === "opened" ? 11 : 12)),
        boundOld: ev.kind === "respawned" ? lookupSessionId(FP, asChannelId(11)) : undefined,
        cachedChannel: getCachedSessionWorker(SID_A)?.channel,
        committedChannel: committedChannel(SID_A),
      });
    });
    try {
      await append(openedEvent(SID_A, 11));
      await append(respawnedEvent(SID_A, 12));
    } finally {
      unsub();
    }

    expect(seen).toEqual([
      // The `opened` subscriber already saw the committed row AND its route.
      { kind: "opened", boundNew: SID_A, boundOld: undefined, cachedChannel: 11, committedChannel: 11 },
      // The `respawned` subscriber sees the new route bound and the old one gone.
      { kind: "respawned", boundNew: SID_A, boundOld: undefined, cachedChannel: 12, committedChannel: 12 },
    ]);
  });

  test("a deduped replay publishes nothing and leaves the index untouched", async () => {
    await append(openedEvent(SID_A, 11));
    const published: string[] = [];
    const unsub = sessionBus.subscribe((ev) => published.push(ev.kind));
    try {
      // Same (worker_fp, client_seq) as the append above → dedupe hit.
      await appendEvent(writer.db, respawnedEvent(SID_A, 12), {
        worker_fp: FP, client_seq: clientSeq, dashboardId: DASHBOARD_ID,
      });
    } finally {
      unsub();
    }
    expect(published).toEqual([]);
    expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
    expect(getCachedSessionWorker(SID_A)?.channel).toBe(11);
  });

  test("an unknown-session event neither publishes nor binds", async () => {
    const published: string[] = [];
    const unsub = sessionBus.subscribe((ev) => published.push(ev.kind));
    try {
      await append(respawnedEvent(SID_C, 44));
    } finally {
      unsub();
    }
    expect(published).toEqual([]);
    expect(lookupSessionId(FP, asChannelId(44))).toBeUndefined();
    expect(getCachedSessionWorker(SID_C)).toBeUndefined();
  });

  test("respawned without an authenticated worker never infers one from the cache", async () => {
    await append(openedEvent(SID_A, 11));
    // A producer with no fingerprint cannot bind the new channel: guessing the
    // worker from the route cache could bind on a worker already replaced.
    await append(respawnedEvent(SID_A, 12), null);
    expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
    expect(lookupSessionId(FP, asChannelId(11))).toBe(SID_A);
  });
});
