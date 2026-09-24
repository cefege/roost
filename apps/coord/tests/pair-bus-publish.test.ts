// Real-handler proof for the versioned requester-confirmation ceremony.
// Pair Sync exposes only pending provenance; approval records a code but never
// authorizes a browser, and confirmation is single-use account-device creation.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code } from "@connectrpc/connect";
import { PAIRING_CEREMONY_VERSION, generatePairRequestId, generatePairRequesterToken, generatePairVerificationCode } from "@roost/protocol/pairing";
import { PairApproveRequestSchema, PairConfirmRequestSchema, PairCreateRequestSchema, PairDenyRequestSchema, PairPollRequestSchema } from "@roost/protocol/proto/coordinator_pb";
import { openPairingHandlerHarness, type CreatedPairRequest, type PairHandlers, type PairingHandlerHarness } from "./pairing-handler-fixture.ts";

let harness: PairingHandlerHarness;
let handlers: PairHandlers;
let testDb: PairingHandlerHarness["db"];
let authCtx: PairingHandlerHarness["authContext"];

beforeAll(async () => {
  harness = await openPairingHandlerHarness();
  handlers = harness.handlers;
  testDb = harness.db;
  authCtx = harness.authContext;
});
afterAll(async () => harness.close());

function remoteCtx(address: string | undefined, onHost = false) {
  return harness.remoteContext(address, onHost);
}

function createMessage(request: CreatedPairRequest, label: string, requesterToken = request.requesterToken, ceremonyVersion = PAIRING_CEREMONY_VERSION) {
  return create(PairCreateRequestSchema, {
    sshPubkeyB64: Buffer.from(request.publicKey).toString("base64"),
    label, ceremonyVersion, ephemeralId: request.ephemeralId, requesterToken,
  });
}

function approvalMessage(request: CreatedPairRequest, verificationCode: string, ceremonyVersion = PAIRING_CEREMONY_VERSION) {
  return create(PairApproveRequestSchema, {
    ephemeralId: request.ephemeralId, ceremonyVersion, verificationCode,
  });
}

function confirmationMessage(request: CreatedPairRequest, verificationCode: string, requesterToken = request.requesterToken, ceremonyVersion = PAIRING_CEREMONY_VERSION) {
  return create(PairConfirmRequestSchema, {
    ephemeralId: request.ephemeralId, requesterToken, verificationCode, ceremonyVersion,
  });
}

function pairRow(ephemeralId: string) {
  return testDb.selectFrom("pair_requests").selectAll()
    .where("ephemeral_id", "=", ephemeralId).executeTakeFirstOrThrow();
}

async function approve(request: CreatedPairRequest, code = generatePairVerificationCode()) {
  const response = await handlers.pairApprove(approvalMessage(request, code), authCtx);
  expect(response.ok).toBe(true);
  expect("verificationCode" in response).toBe(false);
  return code;
}

describe("pair creation", () => {
  test("returns only the caller-owned id and retries exact requests without publication", async () => {
    const request = await harness.createRequest("idempotent-create");
    const capture = harness.capture();
    const retry = await handlers.pairCreate(createMessage(request, "idempotent-create"), authCtx);
    capture.stop();
    expect(retry.ephemeralId).toBe(request.ephemeralId);
    expect("requesterToken" in retry).toBe(false);
    expect(capture.messages).toEqual([]);
    const stored = await pairRow(request.ephemeralId);
    expect(stored).toMatchObject({ ceremony_version: PAIRING_CEREMONY_VERSION });
    expect(stored.requester_token_hash).not.toBe(request.requesterToken);
    await expect(handlers.pairCreate(createMessage(request, "other-label"), authCtx))
      .rejects.toMatchObject({ code: Code.AlreadyExists });
    await expect(handlers.pairCreate(
      createMessage(request, "idempotent-create", generatePairRequesterToken()), authCtx,
    )).rejects.toMatchObject({ code: Code.AlreadyExists });
    const otherKey = await harness.createRequest("idempotent-other-key");
    await expect(handlers.pairCreate(create(PairCreateRequestSchema, {
      sshPubkeyB64: Buffer.from(otherKey.publicKey).toString("base64"),
      label: "idempotent-create",
      ceremonyVersion: PAIRING_CEREMONY_VERSION,
      ephemeralId: request.ephemeralId,
      requesterToken: request.requesterToken,
    }), authCtx)).rejects.toMatchObject({ code: Code.AlreadyExists });
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: request.ephemeralId }), authCtx);
    await expect(handlers.pairCreate(createMessage(request, "idempotent-create"), authCtx))
      .rejects.toMatchObject({ code: Code.FailedPrecondition });
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: otherKey.ephemeralId }), authCtx);
  });

  test("replaces same-key live requests and clears their verifier after commit", async () => {
    const request = await harness.createRequest("replace-first");
    const code = await approve(request);
    const capture = harness.capture();
    const replacement = await harness.createRequest("replace-second", { publicKey: request.publicKey });
    capture.stop();
    expect(await pairRow(request.ephemeralId)).toMatchObject({
      status: "expired", verification_code_hash: null,
    });
    expect(capture.messages).toEqual(expect.arrayContaining([
      { kind: "removed", ephemeral_id: request.ephemeralId },
      expect.objectContaining({ kind: "pending", ephemeral_id: replacement.ephemeralId }),
    ]));
    await expect(handlers.pairConfirm(confirmationMessage(request, code), remoteCtx("203.0.113.1")))
      .rejects.toMatchObject({ code: Code.FailedPrecondition });
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: replacement.ephemeralId }), authCtx);
  });

  test("caps live requests at 32 without counting terminal tombstones", async () => {
    const liveRequests: CreatedPairRequest[] = [];
    for (let index = 0; index < 32; index += 1) {
      liveRequests.push(await harness.createRequest(`live-cap-${index}`));
    }
    await expect(harness.createRequest("live-cap-overflow"))
      .rejects.toMatchObject({ code: Code.ResourceExhausted });
    for (const request of liveRequests) {
      await handlers.pairDeny(
        create(PairDenyRequestSchema, { ephemeralId: request.ephemeralId }),
        authCtx,
      );
    }
  });

  test("rejects stale ceremony requests before any row transition", async () => {
    const request = await harness.createRequest("version-fence");
    const code = generatePairVerificationCode();
    const staleOperations = [
      () => handlers.pairCreate(
        createMessage(request, "version-fence", request.requesterToken, 0),
        authCtx,
      ),
      () => handlers.pairApprove(approvalMessage(request, code, 0), authCtx),
      () => handlers.pairPoll(create(PairPollRequestSchema, {
        ephemeralId: request.ephemeralId, requesterToken: request.requesterToken, ceremonyVersion: 0,
      }), remoteCtx("203.0.113.2")),
      () => handlers.pairConfirm(
        confirmationMessage(request, code, request.requesterToken, 0),
        remoteCtx("203.0.113.2"),
      ),
    ];
    for (const operation of staleOperations) {
      await expect(operation()).rejects.toMatchObject({
        code: Code.FailedPrecondition, rawMessage: "pairing client must reload",
      });
    }
    expect(await pairRow(request.ephemeralId)).toMatchObject({
      status: "pending", verification_code_hash: null,
    });
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: request.ephemeralId }), authCtx);
  });
});

describe("pair approval and confirmation", () => {
  test("accepts only an exact approval retry and never rotates its code", async () => {
    const request = await harness.createRequest("approval-retry");
    const code = await approve(request);
    const before = await pairRow(request.ephemeralId);
    expect(before.verification_code_hash).not.toBe(code);
    const capture = harness.capture();
    await handlers.pairApprove(approvalMessage(request, code), authCtx);
    capture.stop();
    expect(capture.messages).toEqual([]);
    await expect(handlers.pairApprove(
      approvalMessage(request, code === "000000" ? "000001" : "000000"), authCtx,
    )).rejects.toMatchObject({ code: Code.FailedPrecondition });
    expect(await pairRow(request.ephemeralId)).toMatchObject({
      status: "verification_required",
      verification_code_hash: before.verification_code_hash,
      verification_attempts: before.verification_attempts,
    });
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: request.ephemeralId }), authCtx);
  });

  test("approval is non-authorizing; matching confirmation authorizes once and clears secrets", async () => {
    const request = await harness.createRequest("confirm-authority");
    const code = await approve(request);
    const pollRequest = create(PairPollRequestSchema, {
      ephemeralId: request.ephemeralId,
      requesterToken: request.requesterToken,
      ceremonyVersion: PAIRING_CEREMONY_VERSION,
    });
    expect(await handlers.pairPoll(pollRequest, remoteCtx("203.0.113.3")))
      .toMatchObject({ status: "verification_required" });
    expect(await testDb.selectFrom("authorized_keys").select("fingerprint")
      .where("fingerprint", "=", request.fingerprint).executeTakeFirst()).toBeUndefined();
    expect(await testDb.selectFrom("account_devices").select("fingerprint")
      .where("fingerprint", "=", request.fingerprint).executeTakeFirst()).toBeUndefined();
    expect((await handlers.pairConfirm(confirmationMessage(request, code), remoteCtx("203.0.113.3"))).ok)
      .toBe(true);
    expect(await handlers.pairPoll(pollRequest, remoteCtx("203.0.113.3")))
      .toMatchObject({ status: "completed" });
    expect(await pairRow(request.ephemeralId)).toMatchObject({
      status: "completed", verification_code_hash: null,
    });
    expect(await testDb.selectFrom("account_devices").select("fingerprint")
      .where("fingerprint", "=", request.fingerprint).executeTakeFirst())
      .toEqual({ fingerprint: request.fingerprint });
    expect(await testDb.selectFrom("audit_log")
      .select(["caller_fp", "dashboard_id", "method", "path", "status", "trace_id"])
      .where("caller_fp", "=", request.fingerprint)
      .where("path", "=", "/roost.v1.CoordinatorService/PairConfirm").execute())
      .toEqual([{
        caller_fp: request.fingerprint, dashboard_id: harness.dashboardId, method: "POST",
        path: "/roost.v1.CoordinatorService/PairConfirm", status: 200, trace_id: null,
      }]);
    await expect(handlers.pairConfirm(confirmationMessage(request, code), remoteCtx("203.0.113.3")))
      .rejects.toMatchObject({ code: Code.FailedPrecondition });
  });

  test("binds polls and confirmation to the requester token without exposing it", async () => {
    const request = await harness.createRequest("token-bound");
    const wrongToken = generatePairRequesterToken();
    await expect(handlers.pairPoll(create(PairPollRequestSchema, {
      ephemeralId: request.ephemeralId, requesterToken: wrongToken, ceremonyVersion: PAIRING_CEREMONY_VERSION,
    }), remoteCtx("203.0.113.4"))).rejects.toMatchObject({ code: Code.NotFound });
    const code = await approve(request);
    await expect(handlers.pairConfirm(
      confirmationMessage(request, code, wrongToken), remoteCtx("203.0.113.4"),
    )).rejects.toMatchObject({ code: Code.NotFound });
    const poll = await handlers.pairPoll(create(PairPollRequestSchema, {
      ephemeralId: request.ephemeralId,
      requesterToken: request.requesterToken,
      ceremonyVersion: PAIRING_CEREMONY_VERSION,
    }), remoteCtx("203.0.113.4"));
    expect(JSON.stringify(poll, (_key, value) => typeof value === "bigint" ? value.toString() : value))
      .not.toContain(request.requesterToken);
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: request.ephemeralId }), authCtx);
  });

  test("bounds wrong-code attempts, clears the verifier, and refuses terminal replay", async () => {
    const request = await harness.createRequest("attempt-bound");
    const code = await approve(request);
    const wrongCode = code === "000000" ? "000001" : "000000";
    for (let attempt = 0; attempt < 5; attempt += 1) {
      expect((await handlers.pairConfirm(confirmationMessage(request, wrongCode), remoteCtx("203.0.113.5"))).ok)
        .toBe(false);
    }
    expect(await pairRow(request.ephemeralId)).toMatchObject({
      status: "verification_failed", verification_attempts: 5, verification_code_hash: null,
    });
    await expect(handlers.pairConfirm(confirmationMessage(request, code), remoteCtx("203.0.113.5")))
      .rejects.toMatchObject({ code: Code.FailedPrecondition });
  });

  test("terminalizes expired and denied requests without retaining a verifier", async () => {
    const expired = await harness.createRequest("expired-confirmation");
    const expiredCode = await approve(expired);
    await testDb.updateTable("pair_requests").set({ expires_at_ms: Date.now() - 1 })
      .where("ephemeral_id", "=", expired.ephemeralId).execute();
    await expect(handlers.pairConfirm(confirmationMessage(expired, expiredCode), remoteCtx("203.0.113.6")))
      .rejects.toMatchObject({ code: Code.FailedPrecondition });
    expect(await pairRow(expired.ephemeralId)).toMatchObject({
      status: "expired", verification_code_hash: null,
    });
    const denied = await harness.createRequest("denied-confirmation");
    await approve(denied);
    await handlers.pairDeny(create(PairDenyRequestSchema, { ephemeralId: denied.ephemeralId }), authCtx);
    expect(await pairRow(denied.ephemeralId)).toMatchObject({
      status: "denied", verification_code_hash: null,
    });
  });
  test("allows only an authenticated browser or direct-on-host approver", async () => {
    const request = await harness.createRequest("tailnet-reject");
    await expect(handlers.pairApprove(
      approvalMessage(request, generatePairVerificationCode()), remoteCtx("100.101.102.103"),
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    expect(await pairRow(request.ephemeralId)).toMatchObject({ status: "pending" });
    await handlers.pairDeny(
      create(PairDenyRequestSchema, { ephemeralId: request.ephemeralId }),
      remoteCtx("127.0.0.1", true),
    );
    const onHost = await harness.createRequest("on-host-approve");
    await handlers.pairApprove(
      approvalMessage(onHost, generatePairVerificationCode()),
      remoteCtx("127.0.0.1", true),
    );
    expect(await pairRow(onHost.ephemeralId)).toMatchObject({ status: "verification_required" });
    await handlers.pairDeny(
      create(PairDenyRequestSchema, { ephemeralId: onHost.ephemeralId }),
      remoteCtx("127.0.0.1", true),
    );
  });
});

describe("confirmation rechecks", () => {
  test("refuses worker keys and browser keys already owned by another account", async () => {
    const workerKey = await harness.createRequest("worker-key");
    const workerCode = await approve(workerKey);
    await testDb.insertInto("workers").values({
      fp: workerKey.fingerprint, dashboard_id: harness.dashboardId, label: "conflicting-worker",
      os: "linux", git_sha: null, host_metrics_json: null,
      registered_at_ms: Date.now(), last_seen_ms: Date.now(), reachable_addr: null,
    }).execute();
    await expect(handlers.pairConfirm(confirmationMessage(workerKey, workerCode), remoteCtx("203.0.113.7")))
      .rejects.toMatchObject({ code: Code.PermissionDenied });
    const otherAccountKey = await harness.createRequest("other-account-key");
    const otherAccountCode = await approve(otherAccountKey);
    const otherAccountId = `pair-other-${generatePairRequestId()}`;
    await testDb.insertInto("accounts").values({
      id: otherAccountId, email_normalized: `${otherAccountId}@example.test`,
      status: "active", created_at_ms: Date.now(),
    }).execute();
    await testDb.insertInto("authorized_keys").values({
      fingerprint: otherAccountKey.fingerprint,
      public_key: otherAccountKey.publicKey,
      label: "other-account-device",
      added_at: Date.now(),
    }).execute();
    await testDb.insertInto("account_devices").values({
      fingerprint: otherAccountKey.fingerprint, account_id: otherAccountId,
      added_at_ms: Date.now(), last_seen_at_ms: Date.now(),
    }).execute();
    await expect(handlers.pairConfirm(
      confirmationMessage(otherAccountKey, otherAccountCode), remoteCtx("203.0.113.7"),
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    for (const request of [workerKey, otherAccountKey]) {
      expect(await pairRow(request.ephemeralId)).toMatchObject({
        status: "verification_failed", verification_code_hash: null,
      });
    }
  });

  test("requires an unrevoked requester and continuous approver account ownership", async () => {
    const revokedRequester = await harness.createRequest("revoked-requester");
    const requesterCode = await approve(revokedRequester);
    await testDb.insertInto("authorized_key_revocations").values({
      fingerprint: revokedRequester.fingerprint, revoked_at_ms: Date.now(),
      revoked_by_fp: "fp-test", reason: "test",
    }).execute();
    await expect(handlers.pairConfirm(
      confirmationMessage(revokedRequester, requesterCode), remoteCtx("203.0.113.8"),
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    const changedApprover = await harness.createRequest("changed-approver");
    const approverCode = await approve(changedApprover);
    const otherAccountId = `pair-approver-${generatePairRequestId()}`;
    await testDb.insertInto("accounts").values({
      id: otherAccountId, email_normalized: `${otherAccountId}@example.test`,
      status: "active", created_at_ms: Date.now(),
    }).execute();
    await testDb.updateTable("account_devices").set({ account_id: otherAccountId })
      .where("fingerprint", "=", "fp-test").execute();
    try {
      await expect(handlers.pairConfirm(
        confirmationMessage(changedApprover, approverCode), remoteCtx("203.0.113.8"),
      )).rejects.toMatchObject({ code: Code.PermissionDenied });
    } finally {
      await testDb.updateTable("account_devices").set({ account_id: harness.accountId })
        .where("fingerprint", "=", "fp-test").execute();
    }
  });

});
