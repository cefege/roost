// These tests own managed-container health waits and direct identity verification.
// They exercise the same production runtime with protocol responses from the shared fixture.
// Keeping failure-path verification separate leaves layout and sandbox assertions cohesive.
import { afterEach, describe, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import { AuthCoordIdentityResponseSchema } from "@roost/shared/proto/coordinator_pb";
import { statSync } from "node:fs";
import { ManagedInstanceRuntime, type CommandResult } from "../src/saas/docker.ts";
import {
  CONTAINER,
  COORDINATOR_ID,
  commandOk,
  cleanupDockerFixtures,
  testSpec,
  validInspect,
} from "./saas-docker-fixture.ts";

afterEach(cleanupDockerFixtures);

describe("managed Docker runtime", () => {
  test("waits for health and proves the exact direct instance identity", async () => {
    const spec = testSpec();
    const uid = process.getuid?.() ?? statSync(spec.root).uid;
    const gid = process.getgid?.() ?? statSync(spec.root).gid;
    const inspect = validInspect(spec);
    const calls: string[][] = [];
    const runtime = new ManagedInstanceRuntime({
      uid,
      gid,
      randomKey: () => new Uint8Array(32),
      runner: async (argv) => {
        calls.push([...argv]);
        if (argv[1] === "inspect") return commandOk(JSON.stringify([inspect]));
        return commandOk(CONTAINER);
      },
      fetchImpl: Object.assign(async () => new Response(toBinary(
        AuthCoordIdentityResponseSchema,
        create(AuthCoordIdentityResponseSchema, {
          saasMode: true,
          publicListener: true,
          instanceId: COORDINATOR_ID,
        }),
      ), { status: 200 }), { preconnect: fetch.preconnect }),
    });
    await runtime.startAndVerify({
      account: spec.account,
      authVerifyKeyFile: spec.authVerifyKeyFile,
      coordinator: spec.coordinator,
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "Roost <noreply@example.com>",
        sharedResendApiKeyPath: spec.sharedKey,
      },
    });
    expect(calls.some((argv) => argv[1] === "start" && argv[2] === CONTAINER)).toBe(true);
  });

  test("fails closed on direct identity mismatch and malformed activation status", async () => {
    const spec = testSpec();
    const uid = process.getuid?.() ?? statSync(spec.root).uid;
    const gid = process.getgid?.() ?? statSync(spec.root).gid;
    const inspect = validInspect(spec);
    const runner = async (argv: readonly string[]): Promise<CommandResult> => {
      if (argv[1] === "inspect") return commandOk(JSON.stringify([inspect]));
      if (argv.includes("activation-status")) return commandOk(JSON.stringify({ activated: true }));
      return commandOk();
    };
    const runtime = new ManagedInstanceRuntime({
      uid,
      gid,
      runner,
      randomKey: () => new Uint8Array(32),
      fetchImpl: Object.assign(async () => new Response(toBinary(
        AuthCoordIdentityResponseSchema,
        create(AuthCoordIdentityResponseSchema, {
          saasMode: true,
          publicListener: true,
          instanceId: "33333333-3333-4333-8333-333333333333",
        }),
      )), { preconnect: fetch.preconnect }),
    });
    const input = {
      account: spec.account,
      coordinator: spec.coordinator,
      authVerifyKeyFile: spec.authVerifyKeyFile,
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "Roost <noreply@example.com>",
        sharedResendApiKeyPath: spec.sharedKey,
      },
    };
    await expect(runtime.startAndVerify(input)).rejects.toThrow("identity mismatch");
    await expect(runtime.activationStatus(spec.coordinator)).rejects.toThrow("response was invalid");
  });
});
