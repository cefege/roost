/**
 * WHY: This suite owns the managed public-edge contract when Cloudflare Access is absent.
 * Bun discovers it to verify anonymous, device, worker, and Sync admission boundaries.
 * It depends on the shared public-surface harness and the production auth-layer marker.
 */
import { describe, expect, test } from "bun:test";
import { AUTH_LAYER_DEVICE } from "@roost/shared/wire/headers";
import {
  RPC,
  assertHardened,
  deviceRequest,
  makeHarness,
  managedRequest,
  publicServer,
} from "./public-surface-harness.ts";

describe("managed public surface without Cloudflare Access", () => {
  test("serves /login and exact login RPCs anonymously", async () => {
    const { surface, coordCalls } = makeHarness();
    const requests = [
      managedRequest("/", { method: "GET" }),
      managedRequest("/login", { method: "GET" }),
      managedRequest(`${RPC}AuthCoordIdentity`, { method: "POST" }),
      managedRequest(`${RPC}AuthPasswordLogin`, { method: "POST" }),
      managedRequest(`${RPC}AuthPasswordResetRequest`, { method: "POST" }),
      managedRequest(`${RPC}AuthPasswordResetRedeem`, { method: "POST" }),
      managedRequest(`${RPC}AuthFederatedContinue`, { method: "POST" }),
    ];
    for (const req of requests) {
      const response = await surface.fetch(req, publicServer);
      expect(response?.status).toBe(200);
      if (response) assertHardened(response);
    }
    expect(coordCalls.map((call) => call.path)).toEqual([
      "/",
      "/login",
      `${RPC}AuthCoordIdentity`,
      `${RPC}AuthPasswordLogin`,
      `${RPC}AuthPasswordResetRequest`,
      `${RPC}AuthPasswordResetRedeem`,
      `${RPC}AuthFederatedContinue`,
    ]);
    for (const call of coordCalls) {
      expect(call.origin).toEqual({
        listener: "public-edge",
        clientIp: "203.0.113.9",
        onHost: false,
      });
    }
  });

  test("applies the base budget before malformed bearer dispatch", async () => {
    const { surface, coordCalls, rateCalls } = makeHarness();
    const response = await surface.fetch(managedRequest(
      `${RPC}SessionsList`,
      {
        method: "POST",
        headers: { authorization: "Bearer malformed-jwt" },
      },
    ), publicServer);

    expect(response?.status).toBe(200);
    expect(coordCalls.map((call) => call.path)).toEqual([`${RPC}SessionsList`]);
    expect(rateCalls[0]).toEqual({
      clientIp: "203.0.113.9",
      group: "managed-public-base",
      tokensPerWindow: 100,
      windowMs: undefined,
    });
  });

  test("denies anonymous browser and worker-principal RPCs before coordinator dispatch", async () => {
    const { surface, coordCalls } = makeHarness();
    for (const path of [`${RPC}SessionsList`, `${RPC}WorkersRegister`, `${RPC}WorkersHeartbeat`]) {
      for (const authorization of [undefined, "Basic forged", "Bearer "]) {
        const headers = authorization ? { authorization } : undefined;
        const response = await surface.fetch(managedRequest(
          path,
          { method: "POST", headers },
        ), publicServer);
        expect(response?.status).toBe(401);
        expect(response?.headers.get("x-roost-auth-layer")).toBe(AUTH_LAYER_DEVICE);
        if (response) assertHardened(response);
      }
    }
    expect(coordCalls).toEqual([]);
  });

  test("sends bearer-protected RPCs, including device management and logout, to the typed principal boundary", async () => {
    const { surface, coordCalls } = makeHarness();
    const paths = [
      `${RPC}SessionsList`,
      `${RPC}AuthMintBootstrap`,
      `${RPC}DevicesList`,
      `${RPC}DevicesRevoke`,
      `${RPC}AuthLogout`,
      `${RPC}AuthCredentialsGet`,
      `${RPC}AuthPasswordAdd`,
      `${RPC}AuthFederatedLinkBegin`,
      `${RPC}AuthFederatedLink`,
      `${RPC}WorkersRegister`,
      `${RPC}WorkersHeartbeat`,
    ];
    for (const path of paths) {
      const response = await surface.fetch(deviceRequest(path, { method: "POST" }), publicServer);
      expect(response?.status).toBe(200);
    }
    expect(coordCalls.map((call) => call.path)).toEqual(paths);
    for (const call of coordCalls) {
      expect(call.authorization).toBe("Bearer device-jwt");
      expect(call.origin).toEqual({
        listener: "public-edge",
        clientIp: "203.0.113.9",
        onHost: false,
      });
    }
  });

  test("denies private, on-host, diagnostic, malformed worker, and unknown paths", async () => {
    const paths = [
      "/internal",
      "/internal/coord-handoff/commit",
      "/ws/coord-worker",
      "/ws/coord-worker/not-a-fingerprint",
      "/api/db-export",
      `${RPC}AuthRedeemBrowser`,
      `${RPC}PairCreate`,
      `${RPC}WorkersDeployStart`,
      `${RPC}WorkersDeployOutput`,
      `${RPC}PairPoll`,
      `${RPC}PairList`,
      `${RPC}PairApprove`,
      `${RPC}PairDeny`,
      `${RPC}MiscDbExportUrl`,
      `${RPC}MiscMetrics`,
      `${RPC}DiagDebugLogBatch`,
      `${RPC}DiagSnapshot`,
      ...[
        ["Web", "hookTokensList"].join(""),
        ["Web", "hookTokensMint"].join(""),
        ["Web", "hookTokensDelete"].join(""),
        ["Permis", "sionsList"].join(""),
        ["Permis", "sionsCreate"].join(""),
        ["Permis", "sionsUpdate"].join(""),
        ["Permis", "sionsDelete"].join(""),
      ].map((method) => `${RPC}${method}`),
      `${RPC}FuturePrivilegedMethod`,
    ];
    const { surface, coordCalls, syncCalls, workerCalls } = makeHarness();
    for (const path of paths) {
      const response = await surface.fetch(deviceRequest(
        path,
        { method: "POST" },
      ), publicServer);
      expect(response?.status, path).toBe(404);
      expect(await response?.text(), path).toBe("not found");
    }
    expect(coordCalls).toEqual([]);
    expect(syncCalls).toEqual([]);
    expect(workerCalls).toEqual([]);
  });

  test("uses exact ten-per-minute credential groups and base-limits reset email requests", async () => {
    const publicCases = [
      [`${RPC}AuthPasswordLogin`, "public-password-login"],
      [`${RPC}AuthOwnerActivate`, "public-owner-activation"],
      [`${RPC}AuthPasswordResetRedeem`, "public-password-reset-redeem"],
      [`${RPC}AuthFederatedContinue`, "public-federated-continue"],
    ] as const;
    for (const [path, group] of publicCases) {
      const { surface, rateCalls } = makeHarness();
      const response = await surface.fetch(managedRequest(path, { method: "POST" }), publicServer);
      expect(response?.status).toBe(200);
      expect(rateCalls.map((call) => [call.group, call.tokensPerWindow])).toEqual([
        ["managed-public-base", 100],
        [group, 10],
      ]);
    }
    const protectedCases = [
      [`${RPC}AuthPasswordAdd`, "protected-password-add"],
      [`${RPC}AuthFederatedLinkBegin`, "protected-federated-link-begin"],
      [`${RPC}AuthFederatedLink`, "protected-federated-link"],
    ] as const;
    for (const [path, group] of protectedCases) {
      const { surface, rateCalls } = makeHarness();
      const response = await surface.fetch(deviceRequest(path, { method: "POST" }), publicServer);
      expect(response?.status).toBe(200);
      expect(rateCalls.map((call) => [call.group, call.tokensPerWindow])).toEqual([
        ["managed-public-base", 100],
        [group, 10],
      ]);
    }

    const resetRequest = makeHarness();
    const response = await resetRequest.surface.fetch(managedRequest(
      `${RPC}AuthPasswordResetRequest`,
      { method: "POST" },
    ), publicServer);
    expect(response?.status).toBe(200);
    expect(resetRequest.rateCalls.map((call) => [call.group, call.tokensPerWindow]))
      .toEqual([["managed-public-base", 100]]);
  });

  test("admits exact worker redemption anonymously through its own rate-limit group", async () => {
    const { surface, coordCalls, rateGroups } = makeHarness();
    const response = await surface.fetch(managedRequest(
      `${RPC}AuthRedeemWorker`,
      { method: "POST" },
    ), publicServer);
    expect(response?.status).toBe(200);
    expect(coordCalls.map((call) => call.path)).toEqual([`${RPC}AuthRedeemWorker`]);
    expect(rateGroups).toContain("203.0.113.9:public-worker-redeem");
  });

  test("delegates only exact worker websocket paths to authenticated worker admission", async () => {
    const fingerprint = "a".repeat(64);
    const { surface, coordCalls, workerCalls, rateGroups } = makeHarness({
      workerResponse: new Response("unauthorized", { status: 401 }),
    });
    const response = await surface.fetch(
      managedRequest(`/ws/coord-worker/${fingerprint}?token=invalid`),
      publicServer,
    );
    expect(response?.status).toBe(401);
    expect(coordCalls).toEqual([]);
    expect(workerCalls).toEqual([`/ws/coord-worker/${fingerprint}`]);
    expect(rateGroups).toContain("203.0.113.9:public-worker-upgrade");
  });

  test("delegates Sync to its own device-authenticated upgrader without an edge deadline", async () => {
    const { surface, coordCalls, syncCalls } = makeHarness({
      syncResponse: undefined,
    });
    const response = await surface.fetch(
      managedRequest("/ws/coord-sync"),
      publicServer,
    );
    expect(response).toBeUndefined();
    expect(coordCalls).toEqual([]);
    expect(syncCalls).toEqual([{
      path: "/ws/coord-sync",
      reauthAtMs: null,
    }]);
  });
});
