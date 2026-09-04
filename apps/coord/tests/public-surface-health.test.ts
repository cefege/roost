/**
 * WHY: This suite owns the managed-versus-self-hosted authorization contract for public health.
 * Bun discovers it to verify the system handler requires a device only in managed mode.
 * It depends on the generated health request and the production system-handler boundary.
 */
import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { MiscHealthRequestSchema } from "@roost/shared/proto/coordinator_pb";
import type { ConnectDeps } from "../src/connect/router.ts";
import { makeSystemHandlers } from "../src/connect/handlers-system.ts";

describe("managed health authorization", () => {
  test("requires a verified device in managed mode and stays public when self-hosted", async () => {
    const context = { values: createContextValues() } as HandlerContext;
    const request = create(MiscHealthRequestSchema, {});
    const managed = makeSystemHandlers({
      cfg: { saasMode: true },
    } as ConnectDeps);
    await expect(managed.miscHealth(request, context)).rejects.toMatchObject({
      code: Code.Unauthenticated,
    });

    const selfHosted = makeSystemHandlers({
      cfg: { saasMode: false },
    } as ConnectDeps);
    await expect(selfHosted.miscHealth(request, context)).resolves.toMatchObject({ ok: true });
  });
});
