// Verifies that worker deletion fences publication already committed by an in-flight append.
// Bun discovers this suite directly and uses an isolated durable-publication database fixture.
// The contract depends on event-log publication gating, credential fencing, and route retirement.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { appendEvent } from "../src/event-log.ts";
import {
  getCachedSessionWorker,
  lookupSessionId,
  retireWorkerRoutes,
} from "../src/byte-hub.ts";
import { sessionBus } from "../src/buses.ts";
import {
  __setConnectWorkerForTest,
  connectWorkers,
  fenceWorkerCredential,
  listRoutableFps,
} from "../src/connect/worker-registry.ts";
import { asChannelId } from "@roost/shared/wire";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";

const fixture = createDurablePublicationFixture({
  slug: "worker-deletion",
  primaryFingerprintByte: "d9",
  secondaryFingerprintByte: "da",
  sessionGroup: "5",
});
const {
  FP,
  SID_A,
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

describe("worker deletion publication fence", () => {
  test("an event committed before deletion cannot repopulate routes afterward", async () => {
    await append(openedEvent(SID_A, 11));
    const liveHandle = {
      workerFp: FP,
      dashboardId: DASHBOARD_ID,
      revoked: false,
      send: () => 1,
    };
    __setConnectWorkerForTest(FP, liveHandle);
    const published: string[] = [];
    const unsub = sessionBus.subscribe((event) => published.push(event.kind));
    let deletionRan = false;
    try {
      await appendEvent(writer.db, respawnedEvent(SID_A, 12), {
        worker_fp: FP,
        client_seq: ++clientSeq,
        dashboardId: DASHBOARD_ID,
        canPublish: () => {
          // appendEvent invokes this only after its event/projection transaction
          // committed. Tombstone and fence in this boundary to model deletion
          // winning before the suspended append resumes live publication.
          writer.sqlite.transaction(() => {
            writer.sqlite.query(`
              INSERT INTO authorized_key_revocations
                (fingerprint, revoked_at_ms, revoked_by_fp, reason)
              VALUES (?, ?, 'test-delete', 'worker-deleted')
            `).run(FP, 123);
            writer.sqlite.query(`
              UPDATE workers SET deleted_at_ms = ? WHERE fp = ?
            `).run(123, FP);
          })();
          fenceWorkerCredential(FP);
          retireWorkerRoutes(FP);
          deletionRan = true;
          return false;
        },
      });

      expect(deletionRan).toBe(true);
      expect(committedChannel(SID_A)).toBe(12);
      expect(published).toEqual([]);
      expect(lookupSessionId(FP, asChannelId(11))).toBeUndefined();
      expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
      expect(getCachedSessionWorker(SID_A)).toBeUndefined();
      expect(liveHandle.revoked).toBe(true);
      expect(listRoutableFps(DASHBOARD_ID)).not.toContain(FP);
    } finally {
      unsub();
      connectWorkers.delete(FP);
      writer.sqlite.query("DELETE FROM authorized_key_revocations WHERE fingerprint = ?").run(FP);
      writer.sqlite.query("UPDATE workers SET deleted_at_ms = NULL WHERE fp = ?").run(FP);
    }
  });
});
