import { afterEach, describe, expect, test } from "bun:test";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { installFakeKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  makeHarness,
  SESSION_ID,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

afterEach(cleanupStreamHarnesses);

describe("worker-owned keeper input correlation", () => {
  test("two browser-local input_seq=1 commands coexist and settle by distinct keeper keys", async () => {
    const twoWrites = Promise.withResolvers<void>();
    let inputWrites = 0;
    const keeper = trackKeeper(installFakeKeeper({
      onWrite: (write) => {
        if (write.type !== MuxFrameType.PtyInRequest) return;
        inputWrites += 1;
        if (inputWrites === 2) twoWrites.resolve();
      },
    }));
    const harness = await makeHarness();

    const first = harness.manager.writeTerminalInput(
      SESSION_ID,
      1n,
      new Uint8Array([0x41]),
    );
    const second = harness.manager.writeTerminalInput(
      SESSION_ID,
      1n,
      new Uint8Array([0x42, 0x43]),
    );
    // A browser-owned keeper key makes the second command disappear while the
    // first still legitimately awaits its ACK, so no production event can
    // settle this failure branch. The timer only bounds that regression path;
    // the real second socket write clears it immediately on the success path.
    let missingWriteTimer: NodeJS.Timeout | undefined;
    const missingSecondWrite = new Promise<never>((_resolve, reject) => {
      missingWriteTimer = setTimeout(
        () => reject(new Error("second equal browser input sequence never reached the keeper")),
        1_000,
      );
    });
    try {
      await Promise.race([twoWrites.promise, missingSecondWrite]);
    } finally {
      clearTimeout(missingWriteTimer);
    }

    const writes = keeper.writes.filter((write) => write.type === MuxFrameType.PtyInRequest);
    expect(writes).toHaveLength(2);
    expect(writes.map((write) => Array.from(write.bytes ?? []))).toEqual([
      [0x41],
      [0x42, 0x43],
    ]);
    const firstKeeperSeq = writes[0]!.seq!;
    const secondKeeperSeq = writes[1]!.seq!;
    expect(firstKeeperSeq).not.toBe(secondKeeperSeq);
    expect(new Set([firstKeeperSeq, secondKeeperSeq]).size).toBe(2);
    const pool = getMultiplexedPool();
    expect(pool.pendingInputs.has(`${CHANNEL_ID}:${firstKeeperSeq}`)).toBe(true);
    expect(pool.pendingInputs.has(`${CHANNEL_ID}:${secondKeeperSeq}`)).toBe(true);

    keeper.inputAck(CHANNEL_ID, secondKeeperSeq, 2);
    keeper.inputAck(CHANNEL_ID, firstKeeperSeq, 1);
    const [firstResult, secondResult] = await Promise.all([first, second]);
    expect(firstResult).toEqual({ status: "accepted", writtenBytes: 1 });
    expect(secondResult).toEqual({ status: "accepted", writtenBytes: 2 });
    expect(pool.pendingInputs.has(`${CHANNEL_ID}:${firstKeeperSeq}`)).toBe(false);
    expect(pool.pendingInputs.has(`${CHANNEL_ID}:${secondKeeperSeq}`)).toBe(false);
  });
});
