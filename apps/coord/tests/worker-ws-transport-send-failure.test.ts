// Verifies coordinator write failures close only the worker socket that owned the frame.
// Bun discovers this suite directly and supplies isolated durable state through the shared fixture.
// The contract covers hello/event ACK loss, reconnect replay, and generation-local teardown.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WHelloSchema,
  WSessionEventSchema,
  type CoordWorkerDown,
} from "@roost/shared/proto/worker_transport_pb";
import { eventToProto } from "@roost/shared/wire/event-proto";
import type { SessionEvent } from "@roost/shared/wire";
import {
  makeWorkerConn,
  type WorkerConn,
  type WorkerServiceDeps,
} from "../src/connect/worker-conn.ts";
import { connectWorkers } from "../src/connect/worker-registry.ts";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";

const fixture = createDurablePublicationFixture({
  slug: "worker-send-failure",
  primaryFingerprintByte: "a7",
  secondaryFingerprintByte: "a8",
  sessionGroup: "7",
});
const { FP, OTHER_FP, SID_A, DASHBOARD_ID, openedEvent } = fixture;

let deps: WorkerServiceDeps;

beforeEach(async () => {
  await fixture.reset();
  deps = { db: fixture.writer.db } as unknown as WorkerServiceDeps;
});
afterAll(() => fixture.close());

function helloFrame(workerFp: string) {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "hello",
      value: create(WHelloSchema, { workerFp, version: "test" }),
    },
  });
}

function eventFrame(event: SessionEvent, clientSeq: number) {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "event",
      value: create(WSessionEventSchema, {
        event: eventToProto(event, 0)!,
        clientSeq: BigInt(clientSeq),
      }),
    },
  });
}

function recordSuccessfulSend(frames: CoordWorkerDown[]) {
  return (frame: CoordWorkerDown): number => {
    frames.push(frame);
    return 1;
  };
}

describe("coordinator→worker send failure teardown", () => {
  test("a dropped hello ACK closes only its socket and permits a fresh hello", async () => {
    const otherFrames: CoordWorkerDown[] = [];
    let otherCloseRequests = 0;
    const other = makeWorkerConn(
      deps,
      { fingerprint: OTHER_FP },
      recordSuccessfulSend(otherFrames),
      () => { otherCloseRequests += 1; },
      undefined,
      DASHBOARD_ID,
    );
    let failedCloseRequests = 0;
    const failed = makeWorkerConn(
      deps,
      { fingerprint: FP },
      (frame) => frame.frame.case === "helloAck" ? 0 : 1,
      () => { failedCloseRequests += 1; },
      undefined,
      DASHBOARD_ID,
    );
    let replacement: WorkerConn | null = null;
    try {
      await other.handleUpstream(helloFrame(OTHER_FP));
      await failed.handleUpstream(helloFrame(FP));

      expect(otherFrames.map((frame) => frame.frame.case)).toEqual(["helloAck"]);
      expect(otherCloseRequests).toBe(0);
      expect(failedCloseRequests).toBe(1);

      // The WebSocket close callback performs connection cleanup after the
      // connection-local close request observed above.
      failed.close();
      expect(connectWorkers.has(FP)).toBe(false);
      expect(connectWorkers.get(OTHER_FP)?.workerFp).toBe(OTHER_FP);

      const replacementFrames: CoordWorkerDown[] = [];
      let replacementCloseRequests = 0;
      replacement = makeWorkerConn(
        deps,
        { fingerprint: FP },
        recordSuccessfulSend(replacementFrames),
        () => { replacementCloseRequests += 1; },
        undefined,
        DASHBOARD_ID,
      );
      await replacement.handleUpstream(helloFrame(FP));

      expect(replacementFrames.map((frame) => frame.frame.case)).toEqual(["helloAck"]);
      expect(replacementCloseRequests).toBe(0);
      expect(connectWorkers.get(FP)?.workerFp).toBe(FP);
      expect(otherCloseRequests).toBe(0);
    } finally {
      failed.close();
      replacement?.close();
      other.close();
    }
  });

  test("a throwing durable event ACK closes its socket and replay ACKs on replacement", async () => {
    const clientSeq = 7001;
    let failedEventAckAttempts = 0;
    let failedCloseRequests = 0;
    const failed = makeWorkerConn(
      deps,
      { fingerprint: FP },
      (frame) => {
        if (frame.frame.case === "eventAck") {
          failedEventAckAttempts += 1;
          throw new Error("injected worker send failure");
        }
        return 1;
      },
      () => { failedCloseRequests += 1; },
      undefined,
      DASHBOARD_ID,
    );
    let replacement: WorkerConn | null = null;
    try {
      await failed.handleUpstream(helloFrame(FP));
      await failed.handleUpstream(eventFrame(openedEvent(SID_A, 17), clientSeq));

      expect(failedEventAckAttempts).toBe(1);
      expect(failedCloseRequests).toBe(1);
      failed.close();

      const replacementFrames: CoordWorkerDown[] = [];
      let replacementCloseRequests = 0;
      replacement = makeWorkerConn(
        deps,
        { fingerprint: FP },
        recordSuccessfulSend(replacementFrames),
        () => { replacementCloseRequests += 1; },
        undefined,
        DASHBOARD_ID,
      );
      await replacement.handleUpstream(helloFrame(FP));
      await replacement.handleUpstream(eventFrame(openedEvent(SID_A, 17), clientSeq));

      const replayAcks = replacementFrames.filter((frame) =>
        frame.frame.case === "eventAck"
        && frame.frame.value.clientSeq === BigInt(clientSeq)
      );
      expect(replayAcks).toHaveLength(1);
      expect(replacementCloseRequests).toBe(0);
      expect(connectWorkers.get(FP)?.workerFp).toBe(FP);
      const rows = await fixture.writer.db.selectFrom("events")
        .select("client_seq")
        .where("worker_fp", "=", FP)
        .execute();
      expect(rows.map((row) => Number(row.client_seq))).toEqual([clientSeq]);
    } finally {
      failed.close();
      replacement?.close();
    }
  });
});
