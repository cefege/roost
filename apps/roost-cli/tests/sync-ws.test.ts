import { describe, expect, test } from "bun:test";
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
});
