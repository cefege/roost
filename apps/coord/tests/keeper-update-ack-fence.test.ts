// Proves a held keeper-update fence withholds the durable worker event ACK.
// Acking under the fence would let the worker outbox drop the entry, losing the
// record of which PTYs are live across the keeper update; the worker must be
// able to replay it once the fence releases. Drives the real worker connection
// so the frame dispatcher's own ordering — not a stub's — is what is proven.

import { afterAll, beforeEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WHelloSchema,
  WSessionEventSchema,
  type CoordWorkerDown,
} from "@roost/shared/proto/worker_transport_pb";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { makeWorkerConn, type WorkerServiceDeps } from "../src/connect/worker-conn.ts";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { PendingEventPublicationStore } from "../src/pending-event-publications.ts";
import { createDurablePublicationFixture } from "./durable-publication-fixture.ts";

const fixture = createDurablePublicationFixture({
  slug: "keeper-ack-fence",
  primaryFingerprintByte: "a9",
  secondaryFingerprintByte: "aa",
  sessionGroup: "8",
});
const { FP, SID_A, openedEvent } = fixture;

let writer: typeof fixture.writer;

beforeEach(async () => {
  await fixture.reset();
  writer = fixture.writer;
});
afterAll(() => fixture.close());

test("a held keeper-update fence withholds the event ACK until release", async () => {
  const gate = new CoordinatorWriteGate();
  const deps = {
    db: writer.db, pendingPublications: new PendingEventPublicationStore(),
    writeGate: gate, selfHostedTenant: fixture.tenant,
  } as unknown as WorkerServiceDeps;
  const ackedSeqs: bigint[] = [];
  const connection = makeWorkerConn(
    deps,
    { fingerprint: FP },
    (frame: CoordWorkerDown) => {
      if (frame.frame.case === "eventAck") ackedSeqs.push(frame.frame.value.clientSeq);
      return 1;
    },
    () => {},
  );
  await connection.handleUpstream(create(CoordWorkerUpSchema, {
    frame: { case: "hello", value: create(WHelloSchema, { workerFp: FP, version: "test" }) },
  }));
  const opened = create(CoordWorkerUpSchema, {
    frame: { case: "event", value: create(WSessionEventSchema, {
      event: eventToProto(openedEvent(SID_A, 11), 0)!,
      clientSeq: 1n,
    }) },
  });

  const exclusive = await gate.acquireExclusive("keeper-update:ack-fence");
  await connection.handleUpstream(opened);
  expect(ackedSeqs).toEqual([]);
  expect(await writer.db.selectFrom("events").select("client_seq").execute()).toEqual([]);

  exclusive.release();
  await connection.handleUpstream(opened);
  expect(ackedSeqs).toEqual([1n]);
  connection.close();
});
