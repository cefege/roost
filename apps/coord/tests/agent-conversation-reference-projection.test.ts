// Covers transactional private conversation-reference projection and ordering.
// The suite uses the real migrated coordinator database and durable append path.
// It proves snapshots preserve recovery state while close deletes it with Session.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import {
  AgentConversationReferenceV1Schema,
  type AgentConversationReferenceV1,
} from "@roost/shared/agent-conversation-reference";
import { SessionEvent, type SessionEvent as SessionEventValue } from "@roost/shared/wire";
import { sessionBus } from "../src/buses.ts";
import { appendEvent } from "../src/event-log.ts";
import { readSessionsListProjection } from "../src/connect/session-list-projection.ts";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";

const fixture = createDurablePublicationFixture({
  slug: "agent-reference-projection",
  primaryFingerprintByte: "e1",
  secondaryFingerprintByte: "e2",
  sessionGroup: "5",
});
const { FP, SID_A, openedEvent, liveSession, snapshotEvent } = fixture;

const FIRST_REFERENCE = AgentConversationReferenceV1Schema.parse({
  schema_version: 1,
  agent_id: "omp",
  kind: "path",
  value: "/tmp/private first/'$conversation.json",
});
const SECOND_REFERENCE = AgentConversationReferenceV1Schema.parse({
  schema_version: 1,
  agent_id: "omp",
  kind: "id",
  value: "private-second-conversation",
});

beforeEach(async () => {
  await fixture.reset();
  await fixture.append(openedEvent(SID_A, 11));
});
afterAll(() => fixture.close());

function referenceEvent(
  reference: AgentConversationReferenceV1 | null,
  ts: number,
): Extract<SessionEventValue, { kind: "agent_reference" }> {
  const event = SessionEvent.parse({
    kind: "agent_reference",
    session_id: SID_A,
    reference,
    ts,
  });
  if (event.kind !== "agent_reference") {
    throw new Error("expected agent reference fixture");
  }
  return event;
}

async function appendAt(
  event: SessionEventValue,
  clientSeq: number,
) {
  return appendEvent(fixture.writer.db, event, {
    worker_fp: FP,
    client_seq: clientSeq,
    dashboardId: fixture.dashboardId,
  });
}

async function recoveryRow() {
  return fixture.writer.db.selectFrom("sessions")
    .select(["agent_reference_json", "agent_reference_client_seq"])
    .where("id", "=", SID_A)
    .executeTakeFirst();
}

function stringifyTestValue(value: unknown): string {
  return JSON.stringify(value, (_key, item) =>
    typeof item === "bigint" ? item.toString() : item
  ) ?? "";
}

describe("private agent reference projection", () => {
  test("returns one explicit never-set recovery row for each open session", async () => {
    await expect(fixture.writer.db.updateTable("sessions")
      .set({ agent_reference_client_seq: 0 })
      .where("id", "=", SID_A)
      .execute()).rejects.toThrow();
    const projection = await readSessionsListProjection(fixture.writer.db, {
      workerFp: FP,
      status: "open",
      includeRecovery: true,
    });
    expect(projection.sessionIds).toEqual([SID_A]);
    expect(projection.recoveryMetadata).toHaveLength(1);
    const recovery = projection.recoveryMetadata[0];
    expect(recovery?.sessionId).toBe(SID_A);
    expect(recovery?.agentReference).toBeUndefined();
    expect(recovery?.agentReferenceClientSeq).toBe(0n);
  });

  test("keeps corrupt private state out of public lists and fails worker recovery closed", async () => {
    const privateSecret = "CORRUPT_PRIVATE_REFERENCE_18a9";
    await fixture.writer.db.updateTable("sessions")
      .set({
        agent_reference_json: `{"value":"${privateSecret}"`,
        agent_reference_client_seq: 9,
      })
      .where("id", "=", SID_A)
      .execute();
    const publicProjection = await readSessionsListProjection(fixture.writer.db, {
      status: "open",
      includeRecovery: false,
    });
    expect(publicProjection.sessions).toHaveLength(1);
    expect(publicProjection.recoveryMetadata).toEqual([]);
    expect(stringifyTestValue(publicProjection)).not.toContain(privateSecret);

    const privateRead = readSessionsListProjection(fixture.writer.db, {
      workerFp: FP,
      status: "open",
      includeRecovery: true,
    });
    await expect(privateRead).rejects.toThrow(
      "stored agent conversation recovery metadata is invalid",
    );
    try {
      await privateRead;
    } catch (error) {
      expect(String(error)).not.toContain(privateSecret);
    }
  });

  test("sets, replaces, ignores a stale clear, deduplicates, then clears", async () => {
    const staleClearSeq = fixture.nextClientSeq();
    const setSeq = fixture.nextClientSeq();
    const setResult = await appendAt(referenceEvent(FIRST_REFERENCE, 10), setSeq);
    expect(setResult).toMatchObject({ admitted: true, inserted: true, published: false });
    expect(await recoveryRow()).toEqual({
      agent_reference_json: JSON.stringify(FIRST_REFERENCE),
      agent_reference_client_seq: setSeq,
    });

    const replaceSeq = fixture.nextClientSeq();
    await appendAt(referenceEvent(SECOND_REFERENCE, 11), replaceSeq);
    await appendAt(referenceEvent(null, 12), staleClearSeq);
    expect(await recoveryRow()).toEqual({
      agent_reference_json: JSON.stringify(SECOND_REFERENCE),
      agent_reference_client_seq: replaceSeq,
    });

    const duplicate = await appendAt(referenceEvent(SECOND_REFERENCE, 11), replaceSeq);
    expect(duplicate).toMatchObject({ admitted: true, inserted: false, published: false });
    const privateRowsBeforeClear = await fixture.writer.db.selectFrom("events")
      .select("id")
      .where("kind", "=", "agent_reference")
      .execute();
    expect(privateRowsBeforeClear).toHaveLength(3);

    const clearSeq = fixture.nextClientSeq();
    await appendAt(referenceEvent(null, 13), clearSeq);
    expect(await recoveryRow()).toEqual({
      agent_reference_json: null,
      agent_reference_client_seq: clearSeq,
    });
  });

  test("snapshot upsert cannot erase or rewind private recovery state", async () => {
    const setSeq = fixture.nextClientSeq();
    await appendAt(referenceEvent(FIRST_REFERENCE, 20), setSeq);
    const snapshotSeq = fixture.nextClientSeq();
    await appendAt(snapshotEvent([liveSession(SID_A, 22)]), snapshotSeq);
    expect(await recoveryRow()).toEqual({
      agent_reference_json: JSON.stringify(FIRST_REFERENCE),
      agent_reference_client_seq: setSeq,
    });
  });

  test("session close clears recovery state by deleting the complete row", async () => {
    await appendAt(referenceEvent(FIRST_REFERENCE, 30), fixture.nextClientSeq());
    await appendAt(SessionEvent.parse({
      kind: "closed",
      session_id: SID_A,
      exit_code: null,
      ts: 31,
    }), fixture.nextClientSeq());
    expect(await recoveryRow()).toBeUndefined();
  });

  test("private appends never publish to the live session bus", async () => {
    const publishedKinds: string[] = [];
    const unsubscribe = sessionBus.subscribe((event) => {
      publishedKinds.push(event.kind);
    });
    try {
      await appendAt(referenceEvent(FIRST_REFERENCE, 40), fixture.nextClientSeq());
      await appendAt(referenceEvent(null, 41), fixture.nextClientSeq());
    } finally {
      unsubscribe();
    }
    expect(publishedKinds).toEqual([]);
  });

  test("rejects non-worker delivery and malformed bounds without persistence", async () => {
    await expect(appendEvent(
      fixture.writer.db,
      referenceEvent(FIRST_REFERENCE, 50),
      { worker_fp: null, client_seq: null, dashboardId: fixture.dashboardId },
    )).rejects.toThrow("requires worker delivery");

    // Deliberately bypass the compile-time shape to exercise appendEvent's
    // runtime boundary against an untrusted oversized worker frame.
    const malformed = {
      kind: "agent_reference",
      session_id: SID_A,
      reference: {
        ...FIRST_REFERENCE,
        value: "x".repeat(4_097),
      },
      ts: 51,
    } as unknown as SessionEventValue;
    await expect(appendAt(malformed, fixture.nextClientSeq()))
      .rejects.toThrow("invalid agent conversation reference event");
    const privateRows = await fixture.writer.db.selectFrom("events")
      .select("id")
      .where("kind", "=", "agent_reference")
      .execute();
    expect(privateRows).toEqual([]);
  });
});
