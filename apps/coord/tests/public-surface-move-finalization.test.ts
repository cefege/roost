/**
 * WHY: This suite owns public-edge behavior during coordinator moves and response finalization.
 * Bun discovers it to verify retirement gates, security headers, failures, and CORS exposure.
 * It depends on the shared harness for deterministic middleware delegation and write modes.
 */
import { describe, expect, test } from "bun:test";
import {
  RPC,
  assertHardened,
  deviceRequest,
  makeHarness,
  managedRequest,
  publicServer,
  request,
} from "./public-surface-harness.ts";

describe("public move gate and response finalization", () => {
  test("keeps discovery reachable but rejects protected writes after retirement", async () => {
    const { surface, coordCalls } = makeHarness({ mode: "retired" });
    expect((await surface.fetch(managedRequest(
      `${RPC}AuthCoordIdentity`,
      { method: "POST" },
    ), publicServer))?.status).toBe(200);
    expect((await surface.fetch(deviceRequest(
      `${RPC}SessionsInput`,
      { method: "POST" },
    ), publicServer))?.status).toBe(410);
    expect(coordCalls.map((call) => call.path)).toEqual([
      `${RPC}AuthCoordIdentity`,
    ]);
  });

  test("hardens denial, rate-limit, delegated, Sync, and error responses", async () => {
    const deniedHarness = makeHarness();
    const denied = await deniedHarness.surface.fetch(
      managedRequest("/api/db-export"),
      publicServer,
    );
    expect(denied?.status).toBe(404);
    if (denied) assertHardened(denied);

    const limitedHarness = makeHarness({
      access: "reject",
      customLimit: () => new Response("limited", { status: 429 }),
    });
    const limited = await limitedHarness.surface.fetch(
      request("/login"),
      publicServer,
    );
    expect(limited?.status).toBe(429);
    if (limited) assertHardened(limited);

    const syncHarness = makeHarness({
      syncResponse: new Response("bad sync", { status: 403 }),
    });
    const syncRejected = await syncHarness.surface.fetch(
      managedRequest("/ws/coord-sync"),
      publicServer,
    );
    expect(syncRejected?.status).toBe(403);
    if (syncRejected) assertHardened(syncRejected);

    const delegated = await deniedHarness.surface.fetch(
      managedRequest("/login"),
      publicServer,
    );
    if (delegated) assertHardened(delegated);

    const failure = deniedHarness.surface.error(new Error("boom"));
    expect(failure.status).toBe(500);
    assertHardened(failure);
  });

  test("exposes Access provenance only to explicitly allowed CORS origins", async () => {
    const { surface } = makeHarness({ access: "reject" });
    const response = await surface.fetch(request("/login", {
      headers: { origin: "https://caller.example" },
    }), publicServer);
    expect(response?.headers.get("access-control-allow-origin"))
      .toBe("https://caller.example");
    expect(response?.headers.get("access-control-expose-headers"))
      .toContain("x-roost-auth-layer");
  });
});
