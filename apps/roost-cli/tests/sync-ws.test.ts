import { describe, expect, test } from "bun:test";
import { X_ROOST_DASHBOARD_ID } from "@roost/shared/wire/headers";
import {
  buildHeadlessSyncWsOptions,
  buildHeadlessSyncWsUrl,
} from "../src/sync-ws.ts";

describe("headless Sync dashboard scope", () => {
  test("preserves a shared-route prefix and carries the explicit dashboard", () => {
    const url = new URL(buildHeadlessSyncWsUrl(
      "wss://dashboard.example/_roost/t/" + "a".repeat(64),
      "dashboard-a",
      42,
    ));
    expect(url.pathname).toBe(`/_roost/t/${"a".repeat(64)}/ws/coord-sync`);
    expect(url.searchParams.get("dashboard")).toBe("dashboard-a");
    expect(url.searchParams.get("since")).toBe("42");
  });

  test("carries the selected dashboard in the WebSocket upgrade header", () => {
    const options = buildHeadlessSyncWsOptions("secret-jwt", " dashboard-a ");
    expect("protocols" in options).toBe(true);
    if (!("protocols" in options)) throw new Error("WebSocket options omitted subprotocols");
    expect(options.protocols).toEqual(["roost-auth", "secret-jwt"]);
    expect(options.headers?.[X_ROOST_DASHBOARD_ID]).toBe("dashboard-a");
  });

  test("refuses an empty dashboard instead of opening an unscoped firehose", () => {
    expect(() => buildHeadlessSyncWsUrl("wss://dashboard.example", "  "))
      .toThrow("explicit dashboard ID");
  });
});
