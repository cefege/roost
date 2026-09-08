import { describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  assertOnHost,
  resolveCallerOrigin,
} from "../src/middleware/caller-origin.ts";

const NO_HEADERS = new Headers();

describe("resolveCallerOrigin", () => {
  test("a trusted proxy supplies the client address through X-Forwarded-For", () => {
    expect(resolveCallerOrigin(
      "trusted-proxy",
      "127.0.0.1",
      new Headers({ "x-forwarded-for": "203.0.113.7, 10.0.0.1" }),
    )).toEqual({ listener: "trusted-proxy", clientIp: "203.0.113.7", onHost: false });
  });

  test("a forwarded request never counts as on-host, even from loopback", () => {
    expect(resolveCallerOrigin(
      "trusted-proxy",
      "127.0.0.1",
      new Headers({ "x-forwarded-for": "127.0.0.1" }),
    )).toEqual({ listener: "trusted-proxy", clientIp: "127.0.0.1", onHost: false });
  });

  test("an unforwarded loopback request on the proxied listener is on-host", () => {
    expect(resolveCallerOrigin("trusted-proxy", "::1", NO_HEADERS))
      .toEqual({ listener: "trusted-proxy", clientIp: "::1", onHost: true });
  });

  test("a direct listener ignores X-Forwarded-For entirely", () => {
    expect(resolveCallerOrigin(
      "direct",
      "127.0.0.1",
      new Headers({ "x-forwarded-for": "203.0.113.7" }),
    )).toEqual({ listener: "direct", clientIp: "127.0.0.1", onHost: true });
  });

  test("a direct non-loopback peer is neither forged nor on-host", () => {
    expect(resolveCallerOrigin("direct", "198.51.100.4", NO_HEADERS))
      .toEqual({ listener: "direct", clientIp: "198.51.100.4", onHost: false });
  });

  test("an unknown socket peer is never on-host", () => {
    expect(resolveCallerOrigin("direct", undefined, NO_HEADERS))
      .toEqual({ listener: "direct", clientIp: "unknown", onHost: false });
  });
});

describe("assertOnHost", () => {
  test("rejects a proxied caller with permission-denied", () => {
    try {
      assertOnHost({ listener: "trusted-proxy", clientIp: "203.0.113.7", onHost: false });
      throw new Error("expected assertOnHost to throw");
    } catch (error) {
      expect(error).toBeInstanceOf(ConnectError);
      expect((error as ConnectError).code).toBe(Code.PermissionDenied);
    }
  });

  test("admits an on-host caller", () => {
    expect(() => assertOnHost({
      listener: "direct",
      clientIp: "127.0.0.1",
      onHost: true,
    })).not.toThrow();
  });
});
