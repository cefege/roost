/**
 * WHY: This suite owns compatibility between the public edge and configured Cloudflare Access.
 * Bun discovers it to verify authentication precedence, route denial, and Sync expiry bounds.
 * It depends on signed Access fixtures plus the shared public-surface harness.
 */
import { beforeAll, describe, expect, test } from "bun:test";
import { AUTH_LAYER_ACCESS } from "@roost/shared/wire/headers";
import { isPublicPathDenied } from "../src/middleware/public-surface.ts";
import {
  generateAccessSigningKey,
  signAccessToken,
  validAccessClaims,
  type AccessSigningKey,
} from "./helpers/cf-access-fixture.ts";
import {
  NOW_MS,
  RPC,
  assertHardened,
  makeHarness as makePublicSurfaceHarness,
  publicServer,
  request,
} from "./public-surface-harness.ts";

let signingKey: AccessSigningKey;
let accessToken: string;

beforeAll(async () => {
  signingKey = await generateAccessSigningKey("public-surface-key");
  accessToken = await signAccessToken(signingKey, validAccessClaims(NOW_MS / 1000));
});

type AccessHarnessOptions =
  | { access: "valid"; syncResponse?: Response | undefined | null }
  | { access: "reject" };

function makeHarness(options: AccessHarnessOptions) {
  return makePublicSurfaceHarness(
    options.access === "valid"
      ? { ...options, accessSigningKey: signingKey }
      : options,
  );
}

function accessRequest(
  path: string,
  init: RequestInit = {},
  token: string = accessToken,
): Request {
  const headers = new Headers(init.headers);
  headers.set("cf-access-jwt-assertion", token);
  headers.set("cf-connecting-ip", "203.0.113.9");
  return request(path, { ...init, headers });
}

describe("configured Cloudflare Access compatibility", () => {
  test("keeps Access authentication ahead of legacy route classification", async () => {
    const { surface } = makeHarness({ access: "reject" });
    for (const path of ["/login", "/api/db-export", `${RPC}AuthRedeemWorker`]) {
      const response = await surface.fetch(request(path), publicServer);
      expect(response?.status).toBe(401);
      expect(response?.headers.get("x-roost-auth-layer")).toBe(AUTH_LAYER_ACCESS);
      if (response) assertHardened(response);
    }
  });

  test("preserves the Access-fronted delegated and denied surfaces", async () => {
    const { surface, coordCalls } = makeHarness({ access: "valid" });
    for (const path of ["/", `${RPC}PairCreate`, `${RPC}SessionsInput`]) {
      const response = await surface.fetch(accessRequest(
        path,
        { method: path === "/" ? "GET" : "POST" },
      ), publicServer);
      expect(response?.status, path).toBe(200);
    }

    expect(coordCalls.map((call) => call.path)).toEqual([
      "/",
      `${RPC}PairCreate`,
      `${RPC}SessionsInput`,
    ]);
    expect(coordCalls[0]?.origin).toEqual({
      listener: "public-edge",
      clientIp: "203.0.113.9",
      onHost: false,
    });

    for (const path of [
      "/api/db-export",
      `${RPC}AuthRedeemWorker`,
      `${RPC}CoordinatorMoveStart`,
    ]) {
      expect(isPublicPathDenied(path)).toBe(true);
      const response = await surface.fetch(accessRequest(
        path,
        { method: "POST" },
      ), publicServer);
      expect(response?.status, path).toBe(404);
    }
  });

  test("bounds Sync by Access expiry when Access is configured", async () => {
    const { surface, syncCalls } = makeHarness({
      access: "valid",
      syncResponse: undefined,
    });
    expect(await surface.fetch(
      accessRequest("/ws/coord-sync"),
      publicServer,
    )).toBeUndefined();
    expect(syncCalls).toEqual([{
      path: "/ws/coord-sync",
      reauthAtMs: NOW_MS + 300_000,
    }]);

    const claims = validAccessClaims(NOW_MS / 1000);
    claims.exp = NOW_MS / 1000 + 10;
    const shortToken = await signAccessToken(signingKey, claims);
    const shortHarness = makeHarness({
      access: "valid",
      syncResponse: undefined,
    });
    expect(await shortHarness.surface.fetch(
      accessRequest("/ws/coord-sync", {}, shortToken),
      publicServer,
    )).toBeUndefined();
    expect(shortHarness.syncCalls).toEqual([{
      path: "/ws/coord-sync",
      reauthAtMs: NOW_MS + 10_000,
    }]);
  });
});
