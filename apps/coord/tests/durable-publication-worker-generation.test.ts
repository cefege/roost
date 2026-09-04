// Verifies that only the current worker connection becomes routable after its snapshot publishes.
// Bun discovers this suite directly and supplies isolated durable state through the shared fixture.
// The contract depends on worker-connection ACK ordering, registry fencing, and byte-hub routing.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WBinarySchema,
  WHelloSchema,
  WSessionEventSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { SessionEvent, asChannelId } from "@roost/shared/wire";
import type { KyselyDB } from "../src/db/connection.ts";
import {
  isWorkerChannelIndexReconciled,
  applyDurableChannelIndex,
  lookupSessionId,
} from "../src/byte-hub.ts";
import { globalBytesBus, sessionBus, workspaceBus } from "../src/buses.ts";
import { makeWorkerConn, type WorkerServiceDeps } from "../src/connect/worker-conn.ts";
import { connectWorkers, listRoutableFps } from "../src/connect/worker-registry.ts";
import { getWorkerHubSocket } from "../src/connect/worker-send.ts";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";
import { appendEvent } from "../src/event-log.ts";
import { PendingEventPublicationStore } from "../src/pending-event-publications.ts";

const fixture = createDurablePublicationFixture({
  slug: "worker-generation",
  primaryFingerprintByte: "d7",
  secondaryFingerprintByte: "d8",
  sessionGroup: "4",
});
const {
  FP,
  SID_A,
  SID_B,
  DASHBOARD_ID,
  liveSession,
  openedEvent,
  snapshotEvent,
} = fixture;

let writer: typeof fixture.writer;

beforeEach(async () => {
  await fixture.reset();
  writer = fixture.writer;
});
afterAll(() => fixture.close());

function pauseFirstTransactionAfterCommit(db: KyselyDB) {
  const committed = Promise.withResolvers<void>();
  const resume = Promise.withResolvers<void>();
  let pause = true;
  const pausedDb = {
    transaction() {
      return {
        async execute<Result>(
          callback: (transaction: KyselyDB) => Promise<Result>,
        ): Promise<Result> {
          const result = await db.transaction().execute((transaction) =>
            callback(transaction as unknown as KyselyDB)
          );
          if (pause) {
            pause = false;
            committed.resolve();
            await resume.promise;
          }
          return result;
        },
      };
    },
  } as unknown as KyselyDB;
  return { db: pausedDb, committed: committed.promise, resume: resume.resolve };
}

describe("superseded worker generation fence", () => {
  function upFrame(event: SessionEvent, seq: number) {
    return create(CoordWorkerUpSchema, {
      frame: { case: "event", value: create(WSessionEventSchema, {
        event: eventToProto(event, 0)!,
        clientSeq: BigInt(seq),
      }) },
    });
  }

  function helloFrame() {
    return create(CoordWorkerUpSchema, {
      frame: {
        case: "hello",
        value: create(WHelloSchema, { workerFp: FP, version: "test" }),
      },
    });
  }

  test("only the current generation becomes routable after snapshot publication", async () => {
    const deps = {
      db: writer.db,
      pendingPublications: new PendingEventPublicationStore(),
    } as unknown as WorkerServiceDeps;
    const hello = helloFrame();
    let closedOld = 0;
    const oldConn = makeWorkerConn(
      deps, { fingerprint: FP }, () => 1, () => { closedOld += 1; }, undefined, DASHBOARD_ID,
    );
    const ackStates: Array<{ seq: bigint; ready: boolean; route: string | undefined }> = [];
    const newConn = makeWorkerConn(
      deps,
      { fingerprint: FP },
      (frame) => {
        if (frame.frame.case === "eventAck") {
          ackStates.push({
            seq: frame.frame.value.clientSeq,
            ready: connectWorkers.get(FP)?.ready ?? false,
            route: lookupSessionId(FP, asChannelId(21)),
          });
        }
        return 1;
      },
      () => { /* current */ },
      undefined,
      DASHBOARD_ID,
    );
    try {
      await oldConn.handleUpstream(hello);
      expect(oldConn.isCurrentGeneration()).toBe(true);
      expect(oldConn.isReady()).toBe(false);
      expect(listRoutableFps(DASHBOARD_ID)).not.toContain(FP);

      await newConn.handleUpstream(hello);
      expect(closedOld).toBe(1);
      expect(oldConn.isCurrentGeneration()).toBe(false);
      expect(newConn.isCurrentGeneration()).toBe(true);
      expect(newConn.isReady()).toBe(false);

      // Durable lifecycle replay is admitted and ACKed, but cannot make the
      // current raw socket routable or admit ordinary byte traffic.
      await newConn.handleUpstream(upFrame(openedEvent(SID_A, 11), 101));
      expect(getWorkerHubSocket(FP)).toBeNull();
      expect(listRoutableFps(DASHBOARD_ID)).not.toContain(FP);
      const preReadyBytes: string[] = [];
      const unsubBytes = globalBytesBus.subscribe((message) => {
        if (message.session_id === SID_A) {
          preReadyBytes.push(new TextDecoder().decode(message.bytes));
        }
      });
      try {
        await newConn.handleUpstream(create(CoordWorkerUpSchema, {
          frame: {
            case: "binary",
            value: create(WBinarySchema, {
              channelId: 11,
              direction: 1,
              seq: 1n,
              data: new TextEncoder().encode("too-early"),
            }),
          },
        }));
      } finally {
        unsubBytes();
      }
      expect(preReadyBytes).toEqual([]);

      const snapshotPublication: Array<{ ready: boolean; route: string | undefined }> = [];
      const unsubSessions = sessionBus.subscribe((event) => {
        if (event.kind === "snapshot") {
          snapshotPublication.push({
            ready: connectWorkers.get(FP)?.ready ?? false,
            route: lookupSessionId(FP, asChannelId(21)),
          });
        }
      });
      try {
        await newConn.handleUpstream(upFrame(
          snapshotEvent([liveSession(SID_A, 21)]),
          102,
        ));
      } finally {
        unsubSessions();
      }

      // Event-log installed the exact route and published while the handle was
      // still unready. The same callback then made only this generation ready
      // before issuing the exact snapshot ACK.
      expect(snapshotPublication).toEqual([{ ready: false, route: SID_A }]);
      expect(newConn.isReady()).toBe(true);
      expect(getWorkerHubSocket(FP)).not.toBeNull();
      expect(listRoutableFps(DASHBOARD_ID)).toContain(FP);
      expect(ackStates).toEqual([
        { seq: 101n, ready: false, route: undefined },
        { seq: 102n, ready: true, route: SID_A },
      ]);

      // Late frames from the superseded socket cannot append or replace the
      // current generation's exact index.
      await oldConn.handleUpstream(upFrame(openedEvent(SID_B, 12), 103));
      await oldConn.handleUpstream(upFrame(snapshotEvent([]), 104));
      expect(lookupSessionId(FP, asChannelId(21))).toBe(SID_A);
      expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
      expect(isWorkerChannelIndexReconciled(FP)).toBe(true);
      const rows = await writer.db.selectFrom("events").select("client_seq").orderBy("id").execute();
      expect(rows.map((row) => Number(row.client_seq))).toEqual([101, 102]);
    } finally {
      oldConn.close();
      newConn.close();
    }
  });

  test("exact dedupe publishes a supersession-lost close before ACK", async () => {
    const clientSeq = 201;
    const pendingPublications = new PendingEventPublicationStore();
    const paused = pauseFirstTransactionAfterCommit(writer.db);
    const deps = {
      db: paused.db,
      pendingPublications,
    } as unknown as WorkerServiceDeps;
    const oldAcks: bigint[] = [];
    let oldCloseRequests = 0;
    const oldConn = makeWorkerConn(
      deps,
      { fingerprint: FP },
      (frame) => {
        if (frame.frame.case === "eventAck") {
          oldAcks.push(frame.frame.value.clientSeq);
        }
        return 1;
      },
      () => { oldCloseRequests += 1; },
      undefined,
      DASHBOARD_ID,
    );
    const timeline: string[] = [];
    const acks: bigint[] = [];
    let replacementCloseRequests = 0;
    const replacement = makeWorkerConn(
      deps,
      { fingerprint: FP },
      (frame) => {
        if (frame.frame.case === "eventAck") {
          acks.push(frame.frame.value.clientSeq);
          if (frame.frame.value.clientSeq === BigInt(clientSeq)) {
            timeline.push("ack");
          }
        }
        return 1;
      },
      () => { replacementCloseRequests += 1; },
      undefined,
      DASHBOARD_ID,
    );
    const workspaceId = "00000000-0000-4000-8000-000000000404";
    const publications: Array<{
      dashboardId: string;
      eventId: number | undefined;
      route: string | undefined;
    }> = [];
    const foreignPublications: SessionEvent[] = [];
    const cascades: string[] = [];
    const unsubscribeSessions = sessionBus.subscribe((event) => {
      if (event.kind !== "closed" || event.session_id !== SID_A) return;
      timeline.push("session");
      publications.push({
        dashboardId: event._dashboard_id,
        eventId: event._event_id,
        route: lookupSessionId(FP, asChannelId(21)),
      });
    }, DASHBOARD_ID);
    const unsubscribeForeign = sessionBus.subscribe((event) => {
      foreignPublications.push(event);
    }, "another-dashboard");
    const unsubscribeWorkspaces = workspaceBus.subscribe((event) => {
      if (event.kind === "deleted" && event.id === workspaceId) {
        timeline.push("workspace");
        cascades.push(event.id);
      }
    }, DASHBOARD_ID);
    const closedEvent = SessionEvent.parse({
      kind: "closed",
      session_id: SID_A,
      exit_code: 0,
      ts: 3,
    });
    try {
      await oldConn.handleUpstream(helloFrame());
      await appendEvent(writer.db, openedEvent(SID_A, 21), {
        worker_fp: FP,
        client_seq: 200,
        dashboardId: DASHBOARD_ID,
      });
      await writer.db.insertInto("workspaces").values({
        id: workspaceId,
        dashboard_id: DASHBOARD_ID,
        worker_fp: FP,
        name: "recovery cascade",
        folder_path: "/tmp",
        color: null,
        position: 0,
        version: 0,
        created_at_ms: 1,
        updated_at_ms: 1,
      }).execute();
      await writer.db.insertInto("workspace_sessions").values({
        workspace_id: workspaceId,
        dashboard_id: DASHBOARD_ID,
        session_id: SID_A,
        added_at_ms: 1,
      }).execute();

      const oldAppend = oldConn.handleUpstream(upFrame(closedEvent, clientSeq));
      await paused.committed;
      const committed = await writer.db.selectFrom("events")
        .select("id")
        .where("worker_fp", "=", FP)
        .where("client_seq", "=", clientSeq)
        .executeTakeFirstOrThrow();
      expect(await writer.db.selectFrom("sessions").select("id")
        .where("id", "=", SID_A).executeTakeFirst()).toBeUndefined();
      expect(lookupSessionId(FP, asChannelId(21))).toBe(SID_A);
      expect(publications).toEqual([]);

      await replacement.handleUpstream(helloFrame());
      expect(oldCloseRequests).toBe(1);
      applyDurableChannelIndex(openedEvent(SID_A, 21), FP);
      expect(lookupSessionId(FP, asChannelId(21))).toBe(SID_A);
      paused.resume();
      await oldAppend;
      expect(pendingPublications.size).toBe(1);
      expect(oldAcks).toEqual([]);
      expect(publications).toEqual([]);

      await replacement.handleUpstream(upFrame(openedEvent(SID_B, 22), clientSeq));
      expect(replacementCloseRequests).toBe(1);
      expect(pendingPublications.size).toBe(1);
      expect(acks).toEqual([]);
      expect(publications).toEqual([]);

      await replacement.handleUpstream(upFrame(closedEvent, clientSeq));
      expect(pendingPublications.size).toBe(0);
      expect(publications).toEqual([{
        dashboardId: DASHBOARD_ID,
        eventId: Number(committed.id),
        route: undefined,
      }]);
      expect(cascades).toEqual([workspaceId]);
      expect(foreignPublications).toEqual([]);
      expect(timeline).toEqual(["session", "workspace", "ack"]);
      expect(acks).toEqual([BigInt(clientSeq)]);
      expect(replacement.isReady()).toBe(false);

      await replacement.handleUpstream(upFrame(closedEvent, clientSeq));
      expect(publications).toHaveLength(1);
      expect(cascades).toHaveLength(1);
      expect(acks).toEqual([BigInt(clientSeq), BigInt(clientSeq)]);
    } finally {
      paused.resume();
      unsubscribeWorkspaces();
      unsubscribeForeign();
      unsubscribeSessions();
      oldConn.close();
      replacement.close();
    }
  });

  test("capacity rejects before commit and worker cleanup releases every slot", async () => {
    const pendingPublications = new PendingEventPublicationStore(1);
    const options = {
      worker_fp: FP,
      dashboardId: DASHBOARD_ID,
      canPublish: () => false,
      pendingPublications,
    };
    await appendEvent(writer.db, openedEvent(SID_A, 31), {
      ...options,
      client_seq: 301,
    });
    expect(pendingPublications.size).toBe(1);

    await expect(appendEvent(writer.db, openedEvent(SID_B, 32), {
      ...options,
      client_seq: 302,
    })).rejects.toThrow("pending event publication capacity exceeded");
    expect(await writer.db.selectFrom("events").select("id")
      .where("client_seq", "=", 302).executeTakeFirst()).toBeUndefined();
    expect(await writer.db.selectFrom("sessions").select("id")
      .where("id", "=", SID_B).executeTakeFirst()).toBeUndefined();

    expect(pendingPublications.clearWorker(FP)).toBe(1);
    expect(pendingPublications.size).toBe(0);
    await appendEvent(writer.db, openedEvent(SID_B, 32), {
      ...options,
      client_seq: 302,
    });
    expect(pendingPublications.size).toBe(1);
    expect(pendingPublications.clearWorker(FP)).toBe(1);
  });
});
