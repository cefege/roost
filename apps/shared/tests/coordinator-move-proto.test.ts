import { describe, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  AuthCoordIdentityResponseSchema,
  AuthMintCoordinatorRelocationRequestSchema,
  AuthMintCoordinatorRelocationResponseSchema,
  AuthRedeemCoordinatorRelocationRequestSchema,
  AuthRedeemCoordinatorRelocationResponseSchema,
  CoordinatorMovePhase,
  CoordinatorMovePreflightRequestSchema,
  CoordinatorMovePreflightResponseSchema,
  CoordinatorMoveStartRequestSchema,
  CoordinatorMoveStartResponseSchema,
  CoordinatorMoveStatusRequestSchema,
  CoordinatorMoveStatusResponseSchema,
  CoordinatorService,
} from "../src/gen/roost/v1/coordinator_pb.ts";
import {
  CoordWorkerDownSchema,
  DCoordMovePrepareSchema,
  DCoordMoveSnapshotChunkSchema,
  DCoordMoveSnapshotStartSchema,
} from "../src/gen/roost/v1/worker_transport_pb.ts";
import {
  CoordinatorRelocationFrameSchema,
  FirehoseFrameSchema,
} from "../src/gen/roost/v1/sync_pb.ts";

function roundTrip<T>(schema: Parameters<typeof toBinary>[0], message: T): T {
  return fromBinary(schema as never, toBinary(schema as never, message as never)) as T;
}

describe("coordinator-move protobuf contract", () => {
  test("exposes every coordinator move RPC", () => {
    expect(CoordinatorService.methods.map((method) => method.localName)).toEqual(expect.arrayContaining([
      "coordinatorMovePreflight",
      "coordinatorMoveStart",
      "coordinatorMoveStatus",
      "authMintCoordinatorRelocation",
      "authRedeemCoordinatorRelocation",
    ]));
  });

  test("round-trips every coordinator move RPC shape and phase", () => {
    expect(roundTrip(CoordinatorMovePreflightRequestSchema, create(CoordinatorMovePreflightRequestSchema, {
      targetWorkerFp: "target",
    }))).toMatchObject({ targetWorkerFp: "target" });
    expect(roundTrip(CoordinatorMovePreflightResponseSchema, create(CoordinatorMovePreflightResponseSchema, {
      eligible: true, sourceUrl: "https://source.ts.net:4102", targetUrl: "https://target.ts.net:4102",
      blockers: [{ code: "target_offline", message: "offline", workerFp: "target" }],
    }))).toMatchObject({ eligible: true, blockers: [{ code: "target_offline", workerFp: "target" }] });
    expect(roundTrip(CoordinatorMoveStartRequestSchema, create(CoordinatorMoveStartRequestSchema, {
      targetWorkerFp: "target",
    }))).toMatchObject({ targetWorkerFp: "target" });
    expect(roundTrip(CoordinatorMoveStartResponseSchema, create(CoordinatorMoveStartResponseSchema, {
      handoffId: "handoff",
    }))).toMatchObject({ handoffId: "handoff" });
    expect(roundTrip(CoordinatorMoveStatusRequestSchema, create(CoordinatorMoveStatusRequestSchema, {
      handoffId: "handoff",
    }))).toMatchObject({ handoffId: "handoff" });
    expect(roundTrip(AuthCoordIdentityResponseSchema, create(AuthCoordIdentityResponseSchema, {
      gitSha: "sha", publicUrl: "https://target.ts.net:4102",
      relocatedToUrl: "https://next.ts.net:4102", handoffId: "handoff",
    }))).toMatchObject({ relocatedToUrl: "https://next.ts.net:4102", handoffId: "handoff" });
    expect(roundTrip(AuthMintCoordinatorRelocationRequestSchema, create(AuthMintCoordinatorRelocationRequestSchema, {
      handoffId: "handoff",
    }))).toMatchObject({ handoffId: "handoff" });
    expect(roundTrip(AuthMintCoordinatorRelocationResponseSchema, create(AuthMintCoordinatorRelocationResponseSchema, {
      token: "token", targetUrl: "https://target.ts.net:4102",
    }))).toMatchObject({ token: "token" });
    expect(roundTrip(AuthRedeemCoordinatorRelocationRequestSchema, create(AuthRedeemCoordinatorRelocationRequestSchema, {
      token: "token", sshPubkeyB64: "key", label: "browser",
    }))).toMatchObject({ token: "token", sshPubkeyB64: "key", label: "browser" });
    expect(roundTrip(AuthRedeemCoordinatorRelocationResponseSchema, create(AuthRedeemCoordinatorRelocationResponseSchema, {
      fingerprint: "browser", label: "Browser",
    }))).toMatchObject({ fingerprint: "browser", label: "Browser" });
    for (const phase of [
      CoordinatorMovePhase.PREPARING_TARGET, CoordinatorMovePhase.STAGING_WORKERS,
      CoordinatorMovePhase.DRAINING_SOURCE, CoordinatorMovePhase.COPYING_STATE,
      CoordinatorMovePhase.WAITING_FOR_WORKERS, CoordinatorMovePhase.COMMITTING,
      CoordinatorMovePhase.COMMITTED, CoordinatorMovePhase.ROLLING_BACK,
      CoordinatorMovePhase.ROLLED_BACK, CoordinatorMovePhase.FAILED,
    ]) {
      expect(roundTrip(CoordinatorMoveStatusResponseSchema, create(CoordinatorMoveStatusResponseSchema, {
        phase, sourceUrl: "https://source.ts.net:4102", targetUrl: "https://target.ts.net:4102", error: "failure",
      }))).toMatchObject({ phase, error: "failure" });
    }
  });

  test("round-trips binary snapshot transfer and every relocation action", () => {
    const raw = new Uint8Array([0, 255, 1]);
    expect(roundTrip(DCoordMoveSnapshotStartSchema, create(DCoordMoveSnapshotStartSchema, {
      requestId: "request", handoffId: "handoff", totalSize: 3n, sha256: "a".repeat(64),
      coordKeyPem: raw, authorizedKeys: raw, secretSha256: "b".repeat(64), expectedWorkerFps: ["source", "target"],
    }))).toMatchObject({ coordKeyPem: raw, authorizedKeys: raw });
    expect(roundTrip(DCoordMoveSnapshotChunkSchema, create(DCoordMoveSnapshotChunkSchema, {
      handoffId: "handoff", seq: 2, data: raw, last: true,
    }))).toMatchObject({ handoffId: "handoff", seq: 2, data: raw, last: true });
    for (const action of ["CHECK", "PREPARE"]) {
      const down = create(CoordWorkerDownSchema, { frame: { case: "coordMovePrepare", value: create(DCoordMovePrepareSchema, {
        requestId: "request", handoffId: "handoff", sourceUrl: "https://source.ts.net:4102",
        targetUrl: "https://target.ts.net:4102", expectedCoordKid: "kid", expectedGitSha: "sha",
        estimatedDbSize: 3n, action,
      }) } });
      expect(roundTrip(CoordWorkerDownSchema, down).frame.case).toBe("coordMovePrepare");
    }
    for (const action of ["STAGE", "ACTIVATE", "COMMIT", "ABORT"]) {
      const downstream = create(CoordWorkerDownSchema, { frame: { case: "coordRelocate", value: {
        requestId: "request", handoffId: "handoff", sourceUrl: "https://source.ts.net:4102",
        targetUrl: "https://target.ts.net:4102", action,
      } } });
      expect(roundTrip(CoordWorkerDownSchema, downstream).frame).toMatchObject({ case: "coordRelocate", value: { action } });
    }
    const firehose = create(FirehoseFrameSchema, { frame: { case: "coordinatorRelocation", value: create(CoordinatorRelocationFrameSchema, {
      handoffId: "handoff", sourceUrl: "https://source.ts.net:4102", targetUrl: "https://target.ts.net:4102",
    }) } });
    expect(roundTrip(FirehoseFrameSchema, firehose).frame.case).toBe("coordinatorRelocation");
  });
});
