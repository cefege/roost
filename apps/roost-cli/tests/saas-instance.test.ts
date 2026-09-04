/**
 * This suite pins hidden SaaS instance command parsing and loopback health probing.
 * Keeping protocol-only cases here separates them from database and subprocess flows.
 * Shared constants keep every split suite on the same managed-instance identities.
 */
import { create, toBinary } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import type { CoordConfig } from "@roost/shared/config";
import { AuthCoordIdentityResponseSchema } from "@roost/shared/proto/coordinator_pb";
import {
  checkSaasInstanceHealth,
  parseSaasInstanceCommand,
} from "../src/saas-instance.ts";
import {
  ACCOUNT_ID,
  COORDINATOR_ID,
  EMAIL,
} from "./saas-instance-fixtures.ts";

type TestFetchImplementation = (
  input: string | URL | Request,
  init?: BunFetchRequestInit,
) => Promise<Response>;

function testFetch(implementation: TestFetchImplementation): typeof fetch {
  return Object.assign(implementation, { preconnect: fetch.preconnect });
}

describe("hidden SaaS instance command parsing", () => {
  test("accepts only the exact named seed shape and argument-free release/status actions", () => {
    expect(parseSaasInstanceCommand([
      "seed-owner-activation",
      "--email",
      " Owner@Example.COM ",
      "--coordinator-id",
      COORDINATOR_ID.toUpperCase(),
      "--account-id",
      ACCOUNT_ID,
    ])).toEqual({
      action: "seed-owner-activation",
      input: {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: EMAIL,
      },
    });
    expect(parseSaasInstanceCommand(["release-owner-activation-email"])).toEqual({
      action: "release-owner-activation-email",
    });
    expect(parseSaasInstanceCommand(["activation-status"])).toEqual({ action: "activation-status" });
    expect(parseSaasInstanceCommand(["health"])).toEqual({ action: "health" });
    expect(parseSaasInstanceCommand([
      "seed-google-owner",
      "--account-id",
      ACCOUNT_ID,
      "--coordinator-id",
      COORDINATOR_ID,
    ])).toEqual({
      action: "seed-google-owner",
      input: { accountId: ACCOUNT_ID, coordinatorId: COORDINATOR_ID },
    });
    expect(parseSaasInstanceCommand([
      "seed-signup-gateway-owner-activation",
      "--account-id",
      ACCOUNT_ID,
      "--coordinator-id",
      COORDINATOR_ID,
      "--email",
      EMAIL,
    ])).toEqual({
      action: "seed-signup-gateway-owner-activation",
      input: { accountId: ACCOUNT_ID, coordinatorId: COORDINATOR_ID, email: EMAIL },
    });

    for (const rejected of [
      [],
      ["unknown"],
      ["activation-status", "extra"],
      ["health", "extra"],
      ["release-owner-activation-email", "--coordinator-id", COORDINATOR_ID],
      ["seed-owner-activation", ACCOUNT_ID, "--coordinator-id", COORDINATOR_ID, "--email", EMAIL],
      ["seed-owner-activation", "--account-id", ACCOUNT_ID, "--account-id", ACCOUNT_ID,
        "--coordinator-id", COORDINATOR_ID, "--email", EMAIL],
      ["seed-owner-activation", "--account-id", ACCOUNT_ID, "--coordinator-id", COORDINATOR_ID,
        "--email", EMAIL, "--unknown", "value"],
      ["seed-owner-activation", "--account-id", "not-a-uuid", "--coordinator-id", COORDINATOR_ID,
        "--email", EMAIL],
      ["seed-owner-activation", "--account-id", ACCOUNT_ID, "--coordinator-id", COORDINATOR_ID,
        "--email", "invalid"],
    ]) {
      expect(() => parseSaasInstanceCommand(rejected)).toThrow();
    }
  });
});

describe("managed instance health", () => {
  const HEALTH_INSTANCE_ID = "abcdefab-cdef-4abc-8def-abcdefabcdef";
  const config = {
    managedContainer: true,
    saasMode: true,
    instanceId: HEALTH_INSTANCE_ID,
  } as CoordConfig;

  function identityFetch(
    identity: {
      saasMode: boolean;
      publicListener: boolean;
      instanceId: string;
    },
    observe?: (url: string, init: RequestInit | undefined) => void,
  ): typeof fetch {
    return testFetch(async (input, init) => {
      observe?.(String(input), init);
      return new Response(toBinary(
        AuthCoordIdentityResponseSchema,
        create(AuthCoordIdentityResponseSchema, identity),
      ), {
        headers: { "content-type": "application/proto" },
      });
    });
  }

  test("probes the loopback public identity endpoint with native Connect protobuf", async () => {
    let requestUrl = "";
    let requestInit: RequestInit | undefined;
    await checkSaasInstanceHealth(config, identityFetch({
      saasMode: true,
      publicListener: true,
      instanceId: HEALTH_INSTANCE_ID,
    }, (url, init) => {
      requestUrl = url;
      requestInit = init;
    }));

    expect(requestUrl).toBe(
      "http://127.0.0.1:4104/roost.v1.CoordinatorService/AuthCoordIdentity",
    );
    expect(requestInit?.method).toBe("POST");
    expect(requestInit?.headers).toEqual({
      "content-type": "application/proto",
      "connect-protocol-version": "1",
    });
    expect(requestInit?.redirect).toBe("error");
    expect(requestInit?.signal).toBeInstanceOf(AbortSignal);
  });

  test("fails closed for an unavailable, malformed, private, generic, or misrouted identity", async () => {
    const failingIdentities = [
      { saasMode: false, publicListener: true, instanceId: HEALTH_INSTANCE_ID },
      { saasMode: true, publicListener: false, instanceId: HEALTH_INSTANCE_ID },
      { saasMode: true, publicListener: true, instanceId: ACCOUNT_ID },
      { saasMode: true, publicListener: true, instanceId: HEALTH_INSTANCE_ID.toUpperCase() },
    ];
    for (const identity of failingIdentities) {
      await expect(checkSaasInstanceHealth(config, identityFetch(identity)))
        .rejects.toThrow("health identity mismatch");
    }

    const unavailable = testFetch(async () => new Response(null, { status: 503 }));
    await expect(checkSaasInstanceHealth(config, unavailable))
      .rejects.toThrow("health identity request failed");

    const malformed = testFetch(async () => new Response(Uint8Array.of(0xff)));
    await expect(checkSaasInstanceHealth(config, malformed))
      .rejects.toThrow("health identity response was invalid");
  });
});
