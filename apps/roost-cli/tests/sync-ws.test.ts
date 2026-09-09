import { describe, expect, test } from "bun:test";
import { CLI_DASHBOARD_HEADER } from "../src/cli-auth.ts";
import {
  buildHeadlessSyncWsOptions,
  buildHeadlessSyncWsUrl,
} from "../src/sync-ws.ts";

describe("headless Sync link construction", () => {
  test("preserves a shared-route prefix and carries only the replay cursor", () => {
    const url = new URL(buildHeadlessSyncWsUrl(
      "wss://coord.example/_roost/t/" + "a".repeat(64),
      42,
    ));
    expect(url.pathname).toBe(`/_roost/t/${"a".repeat(64)}/ws/coord-sync`);
    expect(url.search).toBe("?since=42");
  });

  test("carries the auth JWT in the subprotocol and sends no upgrade headers", () => {
    const options = buildHeadlessSyncWsOptions("secret-jwt");
    expect(Object.keys(options)).toEqual(["protocols"]);
    expect(options).toEqual({ protocols: ["roost-auth", "secret-jwt"] });
  });

  test("adds a legacy dashboard only to the scoped Sync request", () => {
    const dashboardId = "edd99394-ebb2-4d14-a883-f7b5f14a5bb9";
    const url = new URL(buildHeadlessSyncWsUrl(
      "wss://coord.example/_roost/t/" + "a".repeat(64),
      42,
      dashboardId,
    ));
    expect(url.search).toBe(`?since=42&dashboard=${dashboardId}`);
    expect(buildHeadlessSyncWsOptions("secret-jwt", dashboardId)).toEqual({
      protocols: ["roost-auth", "secret-jwt"],
      headers: { [CLI_DASHBOARD_HEADER]: dashboardId },
    });
  });
});
