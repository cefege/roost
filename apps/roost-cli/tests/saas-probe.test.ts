import { describe, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import { AuthCoordIdentityResponseSchema } from "@roost/shared/proto/coordinator_pb";
import { AUTH_LAYER_DEVICE, X_ROOST_AUTH_LAYER } from "@roost/shared/wire/headers";
import {
  ManagedRouteProbe,
  managedProbeContract,
} from "../src/saas/probe.ts";
import type { RegistryAccount, RegistryCoordinator } from "../src/saas/registry.ts";

const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
const ROUTE_KEY = "ab".repeat(32);
const DIRECT_IDENTITY = toBinary(
  AuthCoordIdentityResponseSchema,
  create(AuthCoordIdentityResponseSchema, {
    instanceId: COORDINATOR_ID,
    saasMode: true,
    publicListener: true,
  }),
);
const inspectRunner = async () => ({
  exitCode: 0,
  stdout: JSON.stringify([{
    Name: `/roost-coord-${COORDINATOR_ID.replaceAll("-", "")}`,
    State: { Running: true },
    NetworkSettings: { Networks: { web: { IPAddress: "172.18.0.42" } } },
  }]),
  stderr: "",
});

function coordinator(): RegistryCoordinator {
  return {
    id: COORDINATOR_ID,
    accountId: "11111111-1111-4111-8111-111111111111",
    routeKey: ROUTE_KEY,
    ordinal: 1,
    hostname: "c-22222222222242228222222222222222.dashboard.roosttt.com",
    containerName: "roost-coord-22222222222242228222222222222222",
    dataDir: `/srv/data/roost/instances/${COORDINATOR_ID}/data`,
    imageDigest: `sha256:${"a".repeat(64)}`,
    state: "routed",
    createdAtMs: 1,
    seededAtMs: 1,
    runningAtMs: 1,
    routedAtMs: 1,
    invitedAtMs: null,
    activatedAtMs: null,
    disabledAtMs: null,
    failedAtMs: null,
    updatedAtMs: 1,
    lastError: null,
  };
}

function account(): RegistryAccount {
  return {
    id: "11111111-1111-4111-8111-111111111111",
    emailNormalized: "owner@example.com",
    routeKey: ROUTE_KEY,
    state: "pending",
    createdAtMs: 1,
    activatedAtMs: null,
    disabledAtMs: null,
  };
}

interface FetchCall {
  url: string;
  headers: Headers;
  method: string;
}

function probeResponse(url: string): Response {
  if (url.startsWith("http://172.18.0.42:4104/")) {
    return new Response(DIRECT_IDENTITY, { status: 200 });
  }
  if (url.endsWith(managedProbeContract.identityPath)) {
    return new Response(new Uint8Array([0x08, 0x01]), {
      status: 200,
      headers: { "x-content-type-options": "nosniff" },
    });
  }
  if (url.endsWith("/roost.v1.CoordinatorService/SessionsList")) {
    return new Response(null, {
      status: 401,
      headers: { [X_ROOST_AUTH_LAYER]: AUTH_LAYER_DEVICE },
    });
  }
  return new Response(null, { status: 404 });
}

describe("managed tenant route probe", () => {
  test("targets the selected route on local and shared public origins without exposing identity routing", async () => {
    const calls: FetchCall[] = [];
    const fetchImpl = Object.assign(
      async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
        const url = String(input);
        calls.push({
          url,
          headers: new Headers(init?.headers),
          method: init?.method ?? "GET",
        });
        return probeResponse(url);
      },
      { preconnect: fetch.preconnect },
    );
    const probe = new ManagedRouteProbe({ fetchImpl, runner: inspectRunner });

    await probe.verify(coordinator());

    const localCalls = calls.filter((call) => call.url.startsWith("http://127.0.0.1:8080/"));
    const publicCalls = calls.filter((call) => call.url.startsWith("https://dashboard.roosttt.com/"));
    expect(localCalls).toHaveLength(12);
    expect(publicCalls).toHaveLength(12);
    expect(localCalls.every((call) => call.headers.get("host") === "dashboard.roosttt.com")).toBe(true);
    expect(publicCalls.every((call) => call.headers.get("host") === null)).toBe(true);
    expect(calls.some((call) =>
      call.url.includes(`/_roost/t/${ROUTE_KEY}${managedProbeContract.identityPath}`)
    )).toBe(true);
    expect(calls.some((call) =>
      call.url.includes(`/_roost/t/not-a-tenant${managedProbeContract.identityPath}`)
    )).toBe(true);
    const tenantSurfaceCalls = calls.filter((call) => !call.url.endsWith(managedProbeContract.identityPath));
    expect(tenantSurfaceCalls).toHaveLength(18);
    expect(tenantSurfaceCalls.every((call) => call.url.includes(`/_roost/t/${ROUTE_KEY}/`))).toBe(true);
    expect(calls.every((call) => !call.url.includes("c-22222222222242228222222222222222"))).toBe(true);
    expect(calls.filter((call) => call.url.endsWith(managedProbeContract.identityPath))
      .every((call) => call.method === "POST")).toBe(true);
  });

  test("proves the resolver POST through local and public shared Caddy", async () => {
    const calls: Array<{ url: string; headers: Headers; body: string }> = [];
    const fetchImpl = Object.assign(
      async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
        calls.push({
          url: String(input),
          headers: new Headers(init?.headers),
          body: String(init?.body),
        });
        return Response.json(
          { routeKey: ROUTE_KEY },
          {
            status: 200,
            headers: {
              "cache-control": "no-store",
              "content-type": "application/json; charset=utf-8",
              "x-content-type-options": "nosniff",
            },
          },
        );
      },
      { preconnect: fetch.preconnect },
    );
    const probe = new ManagedRouteProbe({ fetchImpl, runner: inspectRunner });

    await probe.verifyResolver(account());

    expect(calls.map((call) => call.url)).toEqual([
      `http://127.0.0.1:8080${managedProbeContract.resolverPath}`,
      `https://dashboard.roosttt.com${managedProbeContract.resolverPath}`,
    ]);
    expect(calls.every((call) => call.headers.get("origin") === "https://dashboard.roosttt.com"))
      .toBe(true);
    expect(calls[0]?.headers.get("host")).toBe("dashboard.roosttt.com");
    expect(calls[1]?.headers.get("host")).toBeNull();
    expect(calls.every((call) => call.body === JSON.stringify({ email: "owner@example.com" })))
      .toBe(true);
  });

  test("fails when known and fake prefixed identities differ", async () => {
    const fetchImpl = Object.assign(
      async (input: string | URL | Request): Promise<Response> => {
        if (String(input).startsWith("http://172.18.0.42:4104/")) {
          return new Response(DIRECT_IDENTITY, { status: 200 });
        }
        const body = String(input).includes(`/_roost/t/${ROUTE_KEY}/`)
          ? new Uint8Array([1])
          : new Uint8Array([2]);
        return new Response(body, {
          status: 200,
          headers: { "x-content-type-options": "nosniff" },
        });
      },
      { preconnect: fetch.preconnect },
    );
    const probe = new ManagedRouteProbe({ fetchImpl, runner: inspectRunner });

    await expect(probe.verify(coordinator())).rejects.toThrow("exposed tenant routing");
  });
});
