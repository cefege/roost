// Real-handler proof for the approver status read and the completion notice.
// PairApprovalStatus admits only the exact approving device (or direct on-host
// for host approvals), discloses nothing else, and a completed confirmation
// publishes one non-secret "new browser paired" delta to pairBus.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { PAIRING_CEREMONY_VERSION, generatePairVerificationCode } from "@roost/shared/pairing";
import {
  PairApprovalStatusRequestSchema,
  PairApproveRequestSchema,
  PairConfirmRequestSchema,
  PairDenyRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { AUTH_LAYER_DEVICE, X_ROOST_AUTH_LAYER } from "@roost/shared/wire/headers";
import { callerKey, onHostKey } from "../src/connect/auth-interceptor.ts";
import { pairFrame } from "../src/connect/sync-feed-frames.ts";
import { openPairingHandlerHarness, type CreatedPairRequest, type PairHandlers, type PairingHandlerHarness } from "./pairing-handler-fixture.ts";

let harness: PairingHandlerHarness;
let handlers: PairHandlers;

beforeAll(async () => {
  harness = await openPairingHandlerHarness();
  handlers = harness.handlers;
});
afterAll(async () => harness.close());

function deviceContext(fingerprint: string, onHost = false): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device", fingerprint, label: "device", accountId: harness.accountId,
  } as never);
  values.set(onHostKey, onHost);
  return { values } as unknown as HandlerContext;
}

const approverCtx = () => deviceContext("fp-test");
const otherDeviceCtx = () => deviceContext("fp-other-device");
const onHostCtx = () => harness.remoteContext("127.0.0.1", true);
const anonymousRemoteCtx = () => harness.remoteContext("203.0.113.20");

function statusMessage(request: CreatedPairRequest, ceremonyVersion = PAIRING_CEREMONY_VERSION) {
  return create(PairApprovalStatusRequestSchema, { ceremonyVersion, ephemeralId: request.ephemeralId });
}

async function approve(request: CreatedPairRequest, ctx: HandlerContext) {
  const verificationCode = generatePairVerificationCode();
  await handlers.pairApprove(create(PairApproveRequestSchema, {
    ceremonyVersion: PAIRING_CEREMONY_VERSION, ephemeralId: request.ephemeralId, verificationCode,
  }), ctx);
  return verificationCode;
}

function confirm(request: CreatedPairRequest, verificationCode: string) {
  return handlers.pairConfirm(create(PairConfirmRequestSchema, {
    ceremonyVersion: PAIRING_CEREMONY_VERSION,
    ephemeralId: request.ephemeralId,
    requesterToken: request.requesterToken,
    verificationCode,
  }), harness.remoteContext("203.0.113.21"));
}

function deny(request: CreatedPairRequest) {
  return handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: request.ephemeralId }), onHostCtx());
}

function rowStatus(request: CreatedPairRequest) {
  return harness.db.selectFrom("pair_requests").select("status")
    .where("ephemeral_id", "=", request.ephemeralId).executeTakeFirstOrThrow();
}

describe("pair approval status", () => {
  test("exact approver sees verification_required then completed without secrets", async () => {
    const request = await harness.createRequest("status-exact");
    const code = await approve(request, approverCtx());
    const pending = await handlers.pairApprovalStatus(statusMessage(request), approverCtx());
    expect(pending.status).toBe("verification_required");
    expect((await confirm(request, code)).ok).toBe(true);
    const completed = await handlers.pairApprovalStatus(statusMessage(request), approverCtx());
    expect(completed.status).toBe("completed");
    const serialized = JSON.stringify(completed);
    expect(serialized).not.toContain(request.requesterToken);
    expect(serialized).not.toContain(code);
    expect(serialized).not.toContain(request.fingerprint);
    expect(Object.keys(completed).filter((key) => !key.startsWith("$"))).toEqual(["status"]);
  });

  test("other devices and on-host callers cannot read a device-approved request", async () => {
    const request = await harness.createRequest("status-foreign");
    await approve(request, approverCtx());
    await expect(handlers.pairApprovalStatus(statusMessage(request), otherDeviceCtx()))
      .rejects.toMatchObject({ code: Code.NotFound });
    await expect(handlers.pairApprovalStatus(statusMessage(request), onHostCtx()))
      .rejects.toMatchObject({ code: Code.NotFound });
    await deny(request);
    expect((await handlers.pairApprovalStatus(statusMessage(request), approverCtx())).status)
      .toBe("denied");
  });

  test("anonymous remote callers are rejected with the device-auth marker", async () => {
    const request = await harness.createRequest("status-anonymous");
    await approve(request, approverCtx());
    const rejection = await handlers.pairApprovalStatus(statusMessage(request), anonymousRemoteCtx())
      .then(() => null, (error: unknown) => error);
    expect(rejection).toBeInstanceOf(ConnectError);
    expect((rejection as ConnectError).code).toBe(Code.Unauthenticated);
    expect((rejection as ConnectError).metadata.get(X_ROOST_AUTH_LAYER)).toBe(AUTH_LAYER_DEVICE);
    await deny(request);
  });

  test("pending and unknown requests are not found", async () => {
    const request = await harness.createRequest("status-pending");
    await expect(handlers.pairApprovalStatus(statusMessage(request), approverCtx()))
      .rejects.toMatchObject({ code: Code.NotFound });
    await expect(handlers.pairApprovalStatus(statusMessage(request), onHostCtx()))
      .rejects.toMatchObject({ code: Code.NotFound });
    await deny(request);
    const unknown = await harness.createRequest("status-unknown");
    await harness.db.deleteFrom("pair_requests").where("ephemeral_id", "=", unknown.ephemeralId).execute();
    await expect(handlers.pairApprovalStatus(statusMessage(unknown), approverCtx()))
      .rejects.toMatchObject({ code: Code.NotFound });
  });

  test("direct on-host reads only host-approved requests", async () => {
    const request = await harness.createRequest("status-on-host");
    await approve(request, onHostCtx());
    expect((await handlers.pairApprovalStatus(statusMessage(request), onHostCtx())).status)
      .toBe("verification_required");
    await expect(handlers.pairApprovalStatus(statusMessage(request), approverCtx()))
      .rejects.toMatchObject({ code: Code.NotFound });
    await deny(request);
  });

  test("normalizes an elapsed live request to expired without writing it", async () => {
    const request = await harness.createRequest("status-elapsed");
    await approve(request, approverCtx());
    await harness.db.updateTable("pair_requests").set({ expires_at_ms: Date.now() - 1 })
      .where("ephemeral_id", "=", request.ephemeralId).execute();
    expect((await handlers.pairApprovalStatus(statusMessage(request), approverCtx())).status)
      .toBe("expired");
    expect(await rowStatus(request)).toEqual({ status: "verification_required" });
  });

  test("rejects stale ceremony versions and malformed request ids", async () => {
    const request = await harness.createRequest("status-version");
    await approve(request, approverCtx());
    await expect(handlers.pairApprovalStatus(statusMessage(request, 0), approverCtx()))
      .rejects.toMatchObject({ code: Code.FailedPrecondition, rawMessage: "pairing client must reload" });
    await expect(handlers.pairApprovalStatus(create(PairApprovalStatusRequestSchema, {
      ceremonyVersion: PAIRING_CEREMONY_VERSION, ephemeralId: "not a request id",
    }), approverCtx())).rejects.toMatchObject({ code: Code.InvalidArgument });
    await deny(request);
  });
});

describe("paired browser notice", () => {
  test("a completing confirmation publishes one non-secret completed delta", async () => {
    const request = await harness.createRequest("notice-label");
    await harness.db.updateTable("pair_requests").set({
      client_browser: "Chrome", client_os: "macOS", client_device_type: "desktop",
      country_code: "DE", region: "Berlin", city: "Berlin",
      source_ip: "198.51.100.7", user_agent: "Mozilla/5.0 notice-agent",
      edge_identity: "person@example.test",
    }).where("ephemeral_id", "=", request.ephemeralId).execute();
    const code = await approve(request, approverCtx());
    const wrongCode = code === "000000" ? "000001" : "000000";
    const capture = harness.capture();
    try {
      expect((await confirm(request, wrongCode)).ok).toBe(false);
      expect(capture.messages).toEqual([]);
      const before = Date.now();
      expect((await confirm(request, code)).ok).toBe(true);
      await expect(confirm(request, code)).rejects.toMatchObject({ code: Code.FailedPrecondition });
      expect(capture.messages).toEqual([{
        kind: "completed",
        ephemeral_id: request.ephemeralId,
        label: "notice-label",
        client_browser: "Chrome",
        client_os: "macOS",
        client_device_type: "desktop",
        country_code: "DE",
        region: "Berlin",
        city: "Berlin",
        paired_at_ms: expect.any(Number),
      }]);
      const [notice] = capture.messages;
      if (notice?.kind !== "completed") throw new Error("expected a completed notice");
      expect(notice.paired_at_ms).toBeGreaterThanOrEqual(before);
      const frame = pairFrame(notice);
      if (frame.frame.case !== "pairRequestDelta") throw new Error("expected a pair frame");
      const kind = frame.frame.value.kind;
      expect(kind.case).toBe("completed");
      if (kind.case !== "completed") return;
      expect(kind.value.pairedAtMs).toBe(BigInt(notice.paired_at_ms));
      const serialized = JSON.stringify(kind.value, (_key, value) =>
        typeof value === "bigint" ? value.toString() : value);
      for (const secret of [request.requesterToken, code, request.fingerprint, "198.51.100.7", "notice-agent", "person@example.test"]) {
        expect(serialized).not.toContain(secret);
      }
    } finally {
      capture.stop();
    }
  });
});
