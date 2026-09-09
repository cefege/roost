// Covers the browser secrecy boundary for durable conversation-reference events.
// It exercises live publication, v1/v2 replay, public cutoffs, frame construction,
// and pre-snapshot worker replay acknowledgement against a real migrated database.

import { afterAll, beforeEach, describe, expect, spyOn, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { AgentConversationReferenceV1Schema } from "@roost/shared/agent-conversation-reference";
import {
  CoordWorkerUpSchema,
  WHelloSchema,
  WSessionEventSchema,
} from "@roost/shared/proto/worker_transport_pb";
import {
  AgentReferenceEvtSchema,
  SessionEventProtoSchema,
} from "@roost/shared/proto/events_pb";
import {
  AgentConversationReferenceV1Schema as AgentConversationReferenceV1ProtoSchema,
} from "@roost/shared/proto/wire_pb";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { SessionEvent, type SessionEvent as SessionEventValue } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import { sessionBus } from "../src/buses.ts";
import {
  appendEvent,
  getEventMaxId,
  getEventsSince,
  getEventsThrough,
} from "../src/event-log.ts";
import { makeWorkerConn, type WorkerServiceDeps } from "../src/connect/worker-conn.ts";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { sessionFirehoseFrame } from "../src/connect/sync-feed-frames.ts";
import {
  startSyncFeed,
  type SyncResourceIndex,
} from "../src/connect/sync-feed.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";

const fixture = createDurablePublicationFixture({
  slug: "agent-reference-secrecy",
  primaryFingerprintByte: "e3",
  secondaryFingerprintByte: "e4",
  sessionGroup: "6",
});
const { FP, OTHER_FP, SID_A, openedEvent } = fixture;
const PRIVATE_VALUE = "private-opaque/'$conversation";
const REFERENCE = AgentConversationReferenceV1Schema.parse({
  schema_version: 1,
  agent_id: "omp",
  kind: "id",
  value: PRIVATE_VALUE,
});

beforeEach(async () => {
  await fixture.reset();
  await fixture.append(openedEvent(SID_A, 11));
});
afterAll(() => fixture.close());

/** Deps for a worker link whose durable appends stamp the scoped column. */
function workerDeps(): WorkerServiceDeps {
  return {
    db: fixture.writer.db,
    writeGate: new CoordinatorWriteGate(),
    selfHostedTenant: fixture.tenant,
  } as unknown as WorkerServiceDeps;
}

function referenceEvent(value = REFERENCE): Extract<
  SessionEventValue,
  { kind: "agent_reference" }
> {
  const event = SessionEvent.parse({
    kind: "agent_reference",
    session_id: SID_A,
    reference: value,
    ts: 20,
  });
  if (event.kind !== "agent_reference") {
    throw new Error("expected agent reference fixture");
  }
  return event;
}

function sessionEventKinds(frames: readonly FirehoseFrame[]): string[] {
  return frames.flatMap((frame) =>
    frame.frame.case === "sessionEvent"
      ? [frame.frame.value.kind.case ?? "missing"]
      : []
  );
}

function stringifyTestValue(value: unknown): string {
  return JSON.stringify(value, (_key, item) =>
    typeof item === "bigint" ? item.toString() : item
  ) ?? "";
}

describe("private event browser exclusion", () => {
  test("outage replay, live delivery, and public max cutoff all omit references", async () => {
    const openedRow = await fixture.writer.db.selectFrom("events")
      .select("id")
      .where("kind", "=", "opened")
      .executeTakeFirstOrThrow();
    const openedId = Number(openedRow.id);
    const privateResult = await fixture.append(referenceEvent());
    expect(privateResult.published).toBe(false);
    expect(await getEventsSince(fixture.writer.db, openedId)).toEqual([]);
    expect(await getEventsThrough(
      fixture.writer.db,
      openedId,
      Number.MAX_SAFE_INTEGER,
    )).toEqual([]);
    expect(await getEventMaxId(fixture.writer.db)).toBe(openedId);

    const scope = (): SyncResourceIndex => ({
      ownerWorkerFp: null,
      workerFps: new Set([FP]),
      sessionIds: new Set([SID_A]),
      workspaceIds: new Set(),
    });
    const recoveryResets: string[] = [];
    const deps = { db: fixture.writer.db } as unknown as ConnectDeps;
    const v1Frames: FirehoseFrame[] = [];
    const v2Frames: FirehoseFrame[] = [];
    const v1 = startSyncFeed(
      deps,
      scope(),
      openedId,
      (frame) => v1Frames.push(frame),
      null,
      false,
    );
    const v2 = startSyncFeed(
      deps,
      scope(),
      openedId,
      (frame) => v2Frames.push(frame),
      null,
      false,
      { version: 2, socketId: "private-replay-test", onRecoveryReset: (reason) => recoveryResets.push(reason) },
    );
    try {
      await Promise.all([v1.backfill(), v2.backfill()]);
      expect(sessionEventKinds(v1Frames)).toEqual([]);
      expect(sessionEventKinds(v2Frames)).toEqual([]);
      expect(recoveryResets).toEqual([]);

      await fixture.append(referenceEvent(AgentConversationReferenceV1Schema.parse({
        ...REFERENCE,
        value: `${PRIVATE_VALUE}-replacement`,
      })));
      expect(sessionEventKinds(v1Frames)).toEqual([]);
      expect(sessionEventKinds(v2Frames)).toEqual([]);

      await fixture.append(SessionEvent.parse({
        kind: "cwd",
        session_id: SID_A,
        cwd: "/tmp/public-after-private",
        ts: 21,
      }));
      expect(sessionEventKinds(v1Frames)).toEqual(["cwd"]);
      expect(sessionEventKinds(v2Frames)).toEqual(["cwd"]);
      expect(stringifyTestValue(v1Frames)).not.toContain(PRIVATE_VALUE);
      expect(stringifyTestValue(v2Frames)).not.toContain(PRIVATE_VALUE);

      const publicMax = await getEventMaxId(fixture.writer.db);
      expect(publicMax).toBeGreaterThan(openedId);
      const publicTail = await getEventsThrough(
        fixture.writer.db,
        openedId,
        publicMax,
      );
      expect(publicTail.map(({ event }) => event.kind)).toEqual(["cwd"]);
    } finally {
      v1.dispose();
      v2.dispose();
    }
  });

  test("browser frame construction refuses a private event with a generic error", () => {
    expect(() => sessionFirehoseFrame(referenceEvent(), 99))
      .toThrow("private session event cannot enter a browser frame");
    try {
      sessionFirehoseFrame(referenceEvent(), 99);
      throw new Error("expected private frame rejection");
    } catch (error) {
      expect(String(error)).not.toContain(PRIVATE_VALUE);
    }
  });
});

describe("pre-snapshot durable replay", () => {
  test("acks and projects agent_reference before snapshot readiness without publishing", async () => {
    const clientSeq = fixture.nextClientSeq();
    const acknowledgements: bigint[] = [];
    const published: string[] = [];
    const unsubscribe = sessionBus.subscribe((event) => {
      published.push(event.kind);
    });
    const connection = makeWorkerConn(
      workerDeps(),
      { fingerprint: FP },
      (frame) => {
        if (frame.frame.case === "eventAck") {
          acknowledgements.push(frame.frame.value.clientSeq);
        }
        return 1;
      },
      () => { /* test connection */ },
    );
    try {
      await connection.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "hello",
          value: create(WHelloSchema, { workerFp: FP, version: "test" }),
        },
      }));
      expect(connection.isReady()).toBe(false);
      await connection.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "event",
          value: create(WSessionEventSchema, {
            event: eventToProto(referenceEvent(), 0),
            clientSeq: BigInt(clientSeq),
          }),
        },
      }));
      expect(connection.isReady()).toBe(false);
      expect(acknowledgements).toEqual([BigInt(clientSeq)]);
      expect(published).toEqual([]);
      const row = await fixture.writer.db.selectFrom("sessions")
        .select(["agent_reference_json", "agent_reference_client_seq"])
        .where("id", "=", SID_A)
        .executeTakeFirstOrThrow();
      expect(row).toEqual({
        agent_reference_json: JSON.stringify(REFERENCE),
        agent_reference_client_seq: clientSeq,
      });
      const auditRows = await fixture.writer.db.selectFrom("audit_log")
        .selectAll()
        .execute();
      expect(JSON.stringify(auditRows)).not.toContain(PRIVATE_VALUE);
    } finally {
      unsubscribe();
      connection.close();
    }
  });

  test("acks an owned queued reference after an offline force-close", async () => {
    await appendEvent(fixture.writer.db, SessionEvent.parse({
      kind: "closed",
      session_id: SID_A,
      exit_code: null,
      ts: 21,
    }), {
      worker_fp: null,
      client_seq: null,
      dashboardId: fixture.dashboardId,
    });
    const clientSeq = fixture.nextClientSeq();
    const acknowledgements: bigint[] = [];
    const connection = makeWorkerConn(
      workerDeps(),
      { fingerprint: FP },
      (frame) => {
        if (frame.frame.case === "eventAck") {
          acknowledgements.push(frame.frame.value.clientSeq);
        }
        return 1;
      },
      () => { /* test connection */ },
    );
    try {
      await connection.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "hello",
          value: create(WHelloSchema, { workerFp: FP, version: "test" }),
        },
      }));
      await connection.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "event",
          value: create(WSessionEventSchema, {
            event: eventToProto(referenceEvent(), 0),
            clientSeq: BigInt(clientSeq),
          }),
        },
      }));
      expect(acknowledgements).toEqual([BigInt(clientSeq)]);
      expect(await fixture.writer.db.selectFrom("sessions")
        .select("id")
        .where("id", "=", SID_A)
        .executeTakeFirst()).toBeUndefined();
      await appendEvent(fixture.writer.db, openedEvent(SID_A, 12, OTHER_FP), {
        worker_fp: OTHER_FP,
        client_seq: fixture.nextClientSeq(),
        dashboardId: fixture.dashboardId,
      });
      const currentOwnerReference = AgentConversationReferenceV1Schema.parse({
        ...REFERENCE,
        value: "current-owner-private-reference",
      });
      const currentOwnerReferenceSeq = fixture.nextClientSeq();
      await appendEvent(fixture.writer.db, referenceEvent(currentOwnerReference), {
        worker_fp: OTHER_FP,
        client_seq: currentOwnerReferenceSeq,
        dashboardId: fixture.dashboardId,
      });
      await connection.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "event",
          value: create(WSessionEventSchema, {
            event: eventToProto(referenceEvent(), 0),
            clientSeq: BigInt(fixture.nextClientSeq()),
          }),
        },
      }));
      expect(acknowledgements).toEqual([BigInt(clientSeq)]);
      expect(await fixture.writer.db.selectFrom("sessions")
        .select(["worker_fp", "agent_reference_json", "agent_reference_client_seq"])
        .where("id", "=", SID_A)
        .executeTakeFirst()).toEqual({
          worker_fp: OTHER_FP,
          agent_reference_json: JSON.stringify(currentOwnerReference),
          agent_reference_client_seq: currentOwnerReferenceSeq,
        });
    } finally {
      connection.close();
    }
  });

  test("redacts malformed private reference values from decode logs", async () => {
    const malformedSecret = "PRIVATE_DECODE_SECRET_93eac4";
    const warnSpy = spyOn(log, "warn").mockImplementation(() => undefined);
    const acknowledgements: bigint[] = [];
    const connection = makeWorkerConn(
      workerDeps(),
      { fingerprint: FP },
      (frame) => {
        if (frame.frame.case === "eventAck") {
          acknowledgements.push(frame.frame.value.clientSeq);
        }
        return 1;
      },
      () => { /* test connection */ },
    );
    try {
      await connection.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "hello",
          value: create(WHelloSchema, { workerFp: FP, version: "test" }),
        },
      }));
      await connection.handleUpstream(create(CoordWorkerUpSchema, {
        frame: {
          case: "event",
          value: create(WSessionEventSchema, {
            event: create(SessionEventProtoSchema, {
              kind: {
                case: "agentReference",
                value: create(AgentReferenceEvtSchema, {
                  sessionId: SID_A,
                  reference: create(AgentConversationReferenceV1ProtoSchema, {
                    schemaVersion: 1,
                    agentId: "omp",
                    kind: "id",
                    value: malformedSecret.repeat(300),
                  }),
                  ts: 22n,
                }),
              },
            }),
            clientSeq: BigInt(fixture.nextClientSeq()),
          }),
        },
      }));
      expect(acknowledgements).toEqual([]);
      expect(stringifyTestValue(warnSpy.mock.calls)).not.toContain(malformedSecret);
    } finally {
      connection.close();
      warnSpy.mockRestore();
    }
  });
});
