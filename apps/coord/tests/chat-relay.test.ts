// Coord chat relay test — proves publishChat stamps session_id and fans a
// ChatFrame onto globalChatBus (the worker→Sync leg). This is the coord-side
// piece of the chat path; the worker side is proven separately on live data.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { globalChatBus } from "../src/buses.ts";
import { publishChat, primeChannelMap } from "../src/byte-hub.ts";
import { asWorkerFp, asChannelId, asSessionId } from "@roost/shared/wire";
import { chatFrameToProto, type ChatFrame } from "@roost/shared/chat/wire";
import type { ChatFrame as PbChatFrame } from "@roost/shared/proto/sync_pb";

const WORKER = asWorkerFp("ab".repeat(32));
const CH = asChannelId(7);
const SID = asSessionId("01234567-0123-0123-0123-0123456789ab");

beforeAll(() => {
  // Prime the channel→session map so publishChat can resolve the session_id.
  primeChannelMap([{ id: SID, worker_fp: WORKER, channel: 7 }]);
});

describe("chat relay (publishChat → globalChatBus)", () => {
  test("publishChat stamps session_id + publishes the frame", async () => {
    const wire: ChatFrame = {
      sessionId: "", // worker sends empty; coord stamps it
      append: [{
        id: "m1", parentId: "", ts: "t", role: "user",
        blocks: [{ kind: "text", text: "hello" }],
      }],
      seq: 1, reset: false,
    };
    const pb = chatFrameToProto(wire);

    const received = new Promise<PbChatFrame>((resolve) => {
      const unsub = globalChatBus.subscribe((f) => {
        unsub();
        resolve(f);
      });
    });

    publishChat(WORKER, CH, pb);
    const out = await received;

    expect(out.sessionId).toBe(SID);          // stamped by byte-hub
    expect(Number(out.seq)).toBe(1);
    expect(out.append).toHaveLength(1);
    expect(out.append[0].role).toBe("user");
    expect(out.append[0].blocks[0].kind.case).toBe("text");
  });

  test("publishChat drops unmapped channel (no crash, no publish)", async () => {
    const wire: ChatFrame = { sessionId: "", append: [], seq: 0, reset: true };
    let fired = false;
    const unsub = globalChatBus.subscribe(() => { fired = true; });
    // An unmapped channel id (not primed) → dropped, bus never fires.
    publishChat(WORKER, asChannelId(9999), chatFrameToProto(wire));
    // publishChat is synchronous — if it were going to publish, it has by now.
    unsub();
    expect(fired).toBe(false);
  });
});

afterAll(() => { /* bus is a process singleton; no per-test teardown needed */ });
