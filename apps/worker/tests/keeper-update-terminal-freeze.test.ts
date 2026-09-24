// Proves keeper-update preparation freezes terminal writes before they reach
// the keeper it is about to replace, and that both preparation outcomes thaw
// them. The real admission lane, keeper socket and prepare handler run; only
// the keeper maintenance action is deferred so the frozen window is observable.

import { create } from "@bufbuild/protobuf";
import { afterEach, describe, expect, test } from "bun:test";
import { DKeeperUpdatePrepareSchema } from "@roost/protocol/proto/worker_transport_pb";
import { createKeeperUpdatePrepareHandler } from "../src/coord-link-keeper-update.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { KEEPER_UPDATE_WRITE_REFUSAL } from "../src/session-control-lanes.ts";
import { installFakeKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  makeHarness,
  SESSION_ID,
  STREAM_A,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

const INPUT = new TextEncoder().encode("frozen-input");

function inputAckKeeper(): FakeKeeper {
  let keeper!: FakeKeeper;
  keeper = trackKeeper(installFakeKeeper({
    onWrite: (write) => {
      if (write.type === MuxFrameType.PtyInRequest) {
        keeper.inputAck(write.channelId, write.seq!, write.bytes!.byteLength);
      }
    },
  }));
  return keeper;
}

function maintenanceRequest() {
  return create(DKeeperUpdatePrepareSchema, {
    requestId: "keeper-update-terminal-freeze",
    direction: "",
    maintenance: true,
    forceLive: true,
    coordinatorOpenSessionIds: [String(SESSION_ID)],
  });
}

function inputWrites(keeper: FakeKeeper): number {
  return keeper.writes.filter(
    (write) => write.type === MuxFrameType.PtyInRequest,
  ).length;
}

function resizeWrites(keeper: FakeKeeper): number {
  return keeper.writes.filter(
    (write) => write.type === MuxFrameType.ResizeRequest,
  ).length;
}

afterEach(() => {
  cleanupStreamHarnesses();
});

describe("keeper update preparation freezes terminal writes", () => {
  test("input is rejected pre-write with zero keeper writes and accepted after rollback", async () => {
    const harness = await makeHarness();
    const keeper = inputAckKeeper();
    let reconcileRollbacks = 0;
    const maintenance = Promise.withResolvers<"shutdown" | "already-absent">();
    const prepareKeeperUpdate = createKeeperUpdatePrepareHandler({
      sessionManager: () => harness.manager,
      acquireKeeperUpdateBoundary: () => async () => () => {
        reconcileRollbacks += 1;
      },
      shutdownKeeperForMaintenance: () => maintenance.promise,
    });
    const preparation = prepareKeeperUpdate(maintenanceRequest());

    expect(await harness.manager.writeTerminalInput(
      String(SESSION_ID),
      1n,
      INPUT,
    )).toEqual({
      status: "rejected",
      writtenBytes: 0,
      reason: KEEPER_UPDATE_WRITE_REFUSAL,
    });
    expect(inputWrites(keeper)).toBe(0);

    maintenance.reject(new Error("injected maintenance failure"));
    await expect(preparation).rejects.toThrow("injected maintenance failure");
    expect(reconcileRollbacks).toBe(1);

    expect(await harness.manager.writeTerminalInput(
      String(SESSION_ID),
      2n,
      INPUT,
    )).toEqual({ status: "accepted", writtenBytes: INPUT.byteLength });
    expect(inputWrites(keeper)).toBe(1);
  });

  test("a stream resize is refused without mutating stream state, and success thaws writes", async () => {
    const harness = await makeHarness();
    const keeper = inputAckKeeper();
    const maintenance = Promise.withResolvers<"shutdown" | "already-absent">();
    const prepareKeeperUpdate = createKeeperUpdatePrepareHandler({
      sessionManager: () => harness.manager,
      acquireKeeperUpdateBoundary: () => async () => () => {},
      shutdownKeeperForMaintenance: () => maintenance.promise,
    });
    const preparation = prepareKeeperUpdate(maintenanceRequest());

    expect(await harness.manager.applyTerminalStreamState({
      requestId: "keeper-update-terminal-freeze-resize",
      sessionId: String(SESSION_ID),
      streamId: STREAM_A,
      enabled: true,
      cols: 40,
      rows: 10,
    })).toMatchObject({
      status: "rejected",
      failure: "retryable_pre_write",
      phase: "pre_write",
      reason: KEEPER_UPDATE_WRITE_REFUSAL,
    });
    expect(resizeWrites(keeper)).toBe(0);
    expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toBeUndefined();

    maintenance.resolve("shutdown");
    expect(await preparation).toEqual({ outcome: "shutdown" });

    expect(await harness.manager.writeTerminalInput(
      String(SESSION_ID),
      1n,
      INPUT,
    )).toEqual({ status: "accepted", writtenBytes: INPUT.byteLength });
    expect(inputWrites(keeper)).toBe(1);
  });
});
