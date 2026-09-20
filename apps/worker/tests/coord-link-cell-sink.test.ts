// The coordinator link's lifecycle callbacks drive the "coord" cell sink's
// suspend/resume state machine. A clean boot must end up DELIVERING (a sink
// left suspended emits nothing and never arms the writable notification that
// would resume it), and a link bounce must cost exactly one forced full
// without touching the stream generation a viewer is watching.
import { afterEach, expect, test } from "bun:test";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { buildCoordLinkDeps, type CoordLinkRefs } from "../src/coord-link-deps.ts";
import type { CoordLink } from "../src/transport/coord-link.ts";
import type { SessionEventStore } from "../src/transport/session-event-store.ts";
import { asWorkerFp } from "@roost/shared/wire";
import { installAutoKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  enableStream,
  makeHarness,
  STREAM_A,
  TEST_COLS,
  TEST_ROWS,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

afterEach(cleanupStreamHarnesses);

test("a clean boot delivers cells and a link bounce costs one forced full", async () => {
  trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
  const core = await createWtermCore(TEST_COLS, TEST_ROWS);
  const harness = await makeHarness(core);
  const refs: CoordLinkRefs = {
    link: { send: () => true, pipelineState: () => ({}) } as unknown as CoordLink,
    sessionMgr: harness.manager,
    agentRegistry: null,
    agentDetector: null,
    acquireKeeperUpdateBoundary: null,
  };
  const deps = buildCoordLinkDeps({
    coordHttpUrl: "http://127.0.0.1:4103",
    workerFp: asWorkerFp("00".repeat(32)),
    processEpoch: "test-process-epoch",
    mintJwt: async () => "jwt",
    refs,
    sessionEventStore: {} as SessionEventStore,
  });

  deps.onOpen?.(false);
  deps.onHelloAck?.({ reconnected: false, terminalMetadataNegotiated: true });
  deps.onSnapshotReady?.({ reconnected: false });
  await enableStream(harness.manager, STREAM_A);
  expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true]);

  core.writeString("\x1b[2;1HLIVE");
  harness.manager.emitCellFrame(CHANNEL_ID, false);
  expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true, false]);

  deps.onDetach?.();
  core.writeString("\x1b[3;1HDOWN");
  harness.manager.emitCellFrame(CHANNEL_ID, false);
  expect(harness.frameAttempts).toHaveLength(2);

  deps.onOpen?.(true);
  deps.onHelloAck?.({ reconnected: true, terminalMetadataNegotiated: true });
  deps.onSnapshotReady?.({ reconnected: true });
  expect(harness.frameAttempts.map((frame) => frame.full)).toEqual([true, false, true]);
  expect(harness.manager.terminalStreams.get(CHANNEL_ID)).toMatchObject({
    streamId: STREAM_A,
    enabled: true,
    cols: TEST_COLS,
    rows: TEST_ROWS,
  });
});
