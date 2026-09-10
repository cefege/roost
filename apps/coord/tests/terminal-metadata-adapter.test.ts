// Coordinator metadata adapter coverage for old and negotiated worker frames.
// It proves raw WBinary remains a parser-only compatibility input while title
// and activity hubs receive only semantic observations tied to live routes.

import { afterEach, beforeEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { WBinarySchema, WTerminalMetadataSchema } from "@roost/shared/proto/worker_transport_pb";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import { primeChannelMap, replaceWorkerChannelIndex } from "../src/byte-hub.ts";
import { lastActivityBus, titleBus } from "../src/buses.ts";
import {
  acceptLegacyTerminalMetadata,
  acceptTerminalMetadata,
  startTerminalMetadataAdapter,
} from "../src/terminal-metadata-adapter.ts";
import { dispatchLegacyTerminalMetadataFrame } from "../src/connect/worker-terminal-metadata-frame.ts";
import { startLastActivityHub } from "../src/last-activity-hub.ts";
import { startTerminalTitleHub } from "../src/terminal-title-hub.ts";

const workerFp = asWorkerFp("ab".repeat(32));
const channelId = asChannelId(71);
let stopAdapter: (() => void) | undefined;
let stopActivityHub: (() => void) | undefined;
let stopTitleHub: (() => void) | undefined;

beforeEach(() => {
  stopAdapter = startTerminalMetadataAdapter();
  stopActivityHub = startLastActivityHub();
  stopTitleHub = startTerminalTitleHub();
});

afterEach(() => {
  stopAdapter?.();
  stopActivityHub?.();
  stopTitleHub?.();
  stopAdapter = undefined;
  stopActivityHub = undefined;
  stopTitleHub = undefined;
  replaceWorkerChannelIndex(workerFp, []);
});

test("adapts split old-worker binary title output without publishing raw bytes", () => {
  const sessionId = "metadata-legacy";
  primeChannelMap([{ id: sessionId, worker_fp: workerFp, channel: channelId }]);
  const titles: string[] = [];
  const unsubscribe = titleBus.subscribe((message) => {
    if (message.session_id === sessionId) titles.push(message.title);
  });

  acceptLegacyTerminalMetadata(workerFp, channelId, new TextEncoder().encode("\x1b"));
  acceptLegacyTerminalMetadata(workerFp, channelId, new TextEncoder().encode("]0;legacy title\x07"));

  unsubscribe();
  expect(titles).toEqual(["legacy title"]);
});

test("retires only the legacy parser whose exact route was replaced", () => {
  const retiredSessionId = "00000000-0000-4000-8000-000000000071";
  const currentSessionId = "00000000-0000-4000-8000-000000000072";
  const replacementChannel = asChannelId(72);
  const currentChannel = asChannelId(73);
  primeChannelMap([
    { id: retiredSessionId, worker_fp: workerFp, channel: channelId },
    { id: currentSessionId, worker_fp: workerFp, channel: currentChannel },
  ]);
  const titles: Array<{ sessionId: string; title: string }> = [];
  const unsubscribe = titleBus.subscribe((message) => {
    if (message.session_id === retiredSessionId || message.session_id === currentSessionId) {
      titles.push({ sessionId: message.session_id, title: message.title });
    }
  });

  acceptLegacyTerminalMetadata(workerFp, channelId, new TextEncoder().encode("\x1b"));
  acceptLegacyTerminalMetadata(workerFp, currentChannel, new TextEncoder().encode("\x1b"));
  replaceWorkerChannelIndex(workerFp, [
    { sessionId: asSessionId(retiredSessionId), channelId: replacementChannel },
    { sessionId: asSessionId(currentSessionId), channelId: currentChannel },
  ]);
  primeChannelMap([{ id: retiredSessionId, worker_fp: workerFp, channel: channelId }]);
  acceptLegacyTerminalMetadata(workerFp, channelId, new TextEncoder().encode("]0;stale\x07"));
  acceptLegacyTerminalMetadata(workerFp, channelId, new TextEncoder().encode("\x1b]0;fresh-retired\x07"));
  acceptLegacyTerminalMetadata(workerFp, currentChannel, new TextEncoder().encode("]0;current-title\x07"));

  unsubscribe();
  expect(titles).toEqual([
    { sessionId: retiredSessionId, title: "fresh-retired" },
    { sessionId: currentSessionId, title: "current-title" },
  ]);
});

test("projects negotiated title and activity metadata through the same hubs", () => {
  const sessionId = "metadata-semantic";
  primeChannelMap([{ id: sessionId, worker_fp: workerFp, channel: channelId }]);
  const titles: string[] = [];
  const activity: number[] = [];
  const stopTitles = titleBus.subscribe((message) => {
    if (message.session_id === sessionId) titles.push(message.title);
  });
  const stopActivity = lastActivityBus.subscribe((message) => {
    if (message.session_id === sessionId) activity.push(message.ts_ms);
  });

  acceptTerminalMetadata(workerFp, create(WTerminalMetadataSchema, {
    channelId,
    titleChanged: true,
    title: "semantic title",
    activityChanged: true,
    activityTsMs: 123n,
  }));

  stopTitles();
  stopActivity();
  expect(titles).toEqual(["semantic title"]);
  expect(activity).toEqual([123]);
});

test("drops input-direction and post-negotiation raw metadata frames", () => {
  const sessionId = "metadata-gated";
  primeChannelMap([{ id: sessionId, worker_fp: workerFp, channel: channelId }]);
  const titles: string[] = [];
  const unsubscribe = titleBus.subscribe((message) => {
    if (message.session_id === sessionId) titles.push(message.title);
  });
  const rawTitle = new TextEncoder().encode("\x1b]0;must not publish\x07");

  dispatchLegacyTerminalMetadataFrame(workerFp, false, create(WBinarySchema, {
    channelId,
    direction: 1,
    data: rawTitle,
  }));
  dispatchLegacyTerminalMetadataFrame(workerFp, true, create(WBinarySchema, {
    channelId,
    direction: 0,
    data: rawTitle,
  }));

  unsubscribe();
  expect(titles).toEqual([]);
});
