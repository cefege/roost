// Attachment peer ownership stays isolated from terminal peer ownership even when
// both borrow the same native runtime. A failed attachment PC must retire only
// attachment transport state, never a terminal PC or its direct port.

import { create } from "@bufbuild/protobuf";
import { expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import {
  DLocalAttachmentPeerOfferSchema,
  DLocalTerminalPeerOfferSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import { AttachmentPeerOwner } from "../src/attachment-peer-owner.ts";
import { TerminalPeerOwner } from "../src/terminal-peer-owner.ts";
import {
  createFakeNativeFixture,
  OFFER_SDP,
} from "./terminal-peer-owner-fixture.ts";

const WORKER_EPOCH = "11111111-1111-4111-8111-111111111111";
const DEVICE = "c".repeat(64);

function liveBudget() {
  return {
    isCurrentConnection: () => true,
    remainingMs: () => 8_000,
  };
}

test("attachment peer failure leaves the terminal peer established", async () => {
  const fake = createFakeNativeFixture();
  const terminalOwner = new TerminalPeerOwner({
    processEpoch: WORKER_EPOCH,
    enabled: true,
    isCurrentCoordinator: () => true,
    authorizeGrant: () => "authorized",
    openPeerPort: () => ({ onMessage: () => undefined }),
    nativeLoader: async () => fake.native,
  });
  const attachmentOwner = new AttachmentPeerOwner({
    processEpoch: WORKER_EPOCH,
    enabled: true,
    isCurrentCoordinator: () => true,
    authorizeGrant: () => "authorized",
    openPeerPort: () => ({ onMessage: () => undefined }),
    nativeLoader: async () => fake.native,
  });
  const terminalOffer = create(DLocalTerminalPeerOfferSchema, {
    requestId: randomUUID(),
    connectionGeneration: randomUUID(),
    workerEpoch: WORKER_EPOCH,
    grantId: randomUUID(),
    peerId: randomUUID(),
    deviceFingerprint: DEVICE,
    tabId: randomUUID(),
    offerSdp: OFFER_SDP,
    budgetMs: 8_000,
    stunUrls: [],
  });
  const attachmentOffer = create(DLocalAttachmentPeerOfferSchema, {
    requestId: randomUUID(),
    connectionGeneration: randomUUID(),
    workerEpoch: WORKER_EPOCH,
    grantId: randomUUID(),
    peerId: randomUUID(),
    deviceFingerprint: DEVICE,
    tabId: randomUUID(),
    offerSdp: OFFER_SDP,
    budgetMs: 8_000,
    stunUrls: [],
  });

  await terminalOwner.offer(terminalOffer, liveBudget());
  await attachmentOwner.offer(attachmentOffer, liveBudget());
  expect(terminalOwner.establishedCount).toBe(1);
  expect(attachmentOwner.establishedCount).toBe(1);

  fake.peers[1]!.emitIceFailure();

  expect(attachmentOwner.establishedCount).toBe(0);
  expect(terminalOwner.establishedCount).toBe(1);
  expect(fake.peers[0]!.closed).toBe(false);
  attachmentOwner.dispose();
  terminalOwner.dispose();
});
