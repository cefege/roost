import { describe, expect, test } from "bun:test";
import { create, fromBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  KeepaliveFrameSchema,
  SyncClientFrameSchema,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import {
  canAcceptSyncLink,
  canOpenSyncLink,
  createSingleSyncLoopStarter,
  decodeFirehoseFrame,
  dispatchSyncFrameCausally,
  isImmediateSyncRedial,
  isSyncBackpressureClose,
  type SyncFlowLink,
} from "../src/store/sync-flow.ts";

const OPEN = 1;

interface TestLink extends SyncFlowLink {
  id: string;
  abortReason: string | null;
  sent: Uint8Array[];
}

function makeLink(id: string): TestLink {
  const sent: Uint8Array[] = [];
  return {
    id,
    abortReason: null,
    sent,
    accepting: true,
    ws: {
      readyState: OPEN,
      send: ((data: Uint8Array) => { sent.push(data); }) as WebSocket["send"],
    },
  };
}

function frame(deliverySeq: bigint): FirehoseFrame {
  return create(FirehoseFrameSchema, {
    deliverySeq,
    frame: {
      case: "keepalive",
      value: create(KeepaliveFrameSchema),
    },
  });
}

describe("causal Sync delivery", () => {
  test("ACK is emitted only after synchronous dispatch", () => {
    const link = makeLink("current");
    const order: string[] = [];
    link.ws.send = ((data: Uint8Array) => {
      order.push("ack");
      link.sent.push(data);
    }) as WebSocket["send"];

    expect(dispatchSyncFrameCausally(
      () => link,
      link,
      OPEN,
      frame(41n),
      () => { order.push("dispatch"); return true; },
    )).toBe("acked");
    expect(order).toEqual(["dispatch", "ack"]);
    expect(fromBinary(SyncClientFrameSchema, link.sent[0]!)).toMatchObject({
      ackDeliverySeq: 41n,
    });
  });

  test("dispatcher false or throw emits no ACK", () => {
    const link = makeLink("current");
    expect(() => decodeFirehoseFrame(new Uint8Array([0xff]))).toThrow();
    expect(link.sent).toEqual([]);
    let emptyDispatches = 0;
    expect(dispatchSyncFrameCausally(
      () => link,
      link,
      OPEN,
      create(FirehoseFrameSchema, { deliverySeq: 41n }),
      () => { emptyDispatches += 1; return true; },
    )).toBe("unapplied");
    expect(emptyDispatches).toBe(0);
    expect(link.sent).toEqual([]);


    expect(dispatchSyncFrameCausally(
      () => link,
      link,
      OPEN,
      frame(42n),
      () => false,
    )).toBe("unapplied");
    expect(link.sent).toEqual([]);

    expect(() => dispatchSyncFrameCausally(
      () => link,
      link,
      OPEN,
      frame(43n),
      () => { throw new Error("dispatch failed"); },
    )).toThrow("dispatch failed");
    expect(link.sent).toEqual([]);
  });

  test("stale-generation open and live message callbacks are ignored", () => {
    const stale = makeLink("stale");
    const current = makeLink("current");
    let dispatches = 0;

    expect(canOpenSyncLink(current, stale, OPEN)).toBe(false);
    expect(canAcceptSyncLink(current, stale, OPEN)).toBe(false);
    if (canAcceptSyncLink(current, stale, OPEN)) {
      dispatchSyncFrameCausally(
        () => current,
        stale,
        OPEN,
        frame(44n),
        () => { dispatches += 1; return true; },
      );
    }
    expect(dispatches).toBe(0);
    expect(stale.sent).toEqual([]);
  });

  test("generation replacement during dispatch suppresses the old-link ACK", () => {
    const oldLink = makeLink("old");
    const replacement = makeLink("replacement");
    let current: TestLink = oldLink;

    expect(dispatchSyncFrameCausally(
      () => current,
      oldLink,
      OPEN,
      frame(45n),
      () => { current = replacement; return true; },
    )).toBe("dispatched");
    expect(oldLink.sent).toEqual([]);
  });
});


describe("Sync reconnect ownership", () => {
  test("only an exact application-pressure close redials immediately", () => {
    expect(isSyncBackpressureClose(1013, "sync backpressure")).toBe(true);
    expect(isSyncBackpressureClose(1013, "other")).toBe(false);
    expect(isSyncBackpressureClose(1006, "sync backpressure")).toBe(false);
    expect(isImmediateSyncRedial("flow")).toBe(true);
    expect(isImmediateSyncRedial(null)).toBe(false);
  });

  test("repeated bootstrap retries retain one infinite-loop owner", () => {
    let starts = 0;
    const start = createSingleSyncLoopStarter(() => { starts += 1; });
    expect(start()).toBe(true);
    expect(start()).toBe(false);
    expect(start()).toBe(false);
    expect(starts).toBe(1);
  });
});
