import { describe, expect, test } from "bun:test";
import {
  assertOnHost,
  assertOnHostOrTailnet,
  isTailnetAddr,
  resolveCallerOrigin,
} from "../src/middleware/caller-origin.ts";

describe("resolveCallerOrigin", () => {
  test("direct loopback is on-host and ignores forwarding headers", () => {
    expect(resolveCallerOrigin(
      "direct",
      "127.0.0.1",
      new Headers({ "x-forwarded-for": "100.64.0.1" }),
    )).toEqual({ listener: "direct", clientIp: "127.0.0.1", onHost: true });
  });

  test("tailscale serve uses the first forwarded address but never marks it on-host", () => {
    expect(resolveCallerOrigin(
      "tailscale-serve",
      "127.0.0.1",
      new Headers({ "x-forwarded-for": " 100.64.0.1, 127.0.0.1" }),
    )).toEqual({ listener: "tailscale-serve", clientIp: "100.64.0.1", onHost: false });
    expect(resolveCallerOrigin(
      "tailscale-serve",
      "127.0.0.1",
      new Headers({ "x-forwarded-for": "127.0.0.1" }),
    )).toEqual({ listener: "tailscale-serve", clientIp: "127.0.0.1", onHost: false });
  });

  test("tailscale serve without XFF preserves direct on-host recovery", () => {
    expect(resolveCallerOrigin(
      "tailscale-serve",
      "::1",
      new Headers(),
    )).toEqual({ listener: "tailscale-serve", clientIp: "::1", onHost: true });
  });

  test("public edge sanitizes privileged-looking and empty addresses", () => {
    for (const clientIp of ["", "127.0.0.1", "100.64.0.1", "fd7a:115c:a1e0::1"]) {
      const headers = new Headers();
      if (clientIp) headers.set("cf-connecting-ip", clientIp);
      expect(resolveCallerOrigin("public-edge", "127.0.0.1", headers))
        .toEqual({ listener: "public-edge", clientIp: "public", onHost: false });
    }
  });

  test("public edge retains a normal address only for rate limiting and audit", () => {
    expect(resolveCallerOrigin(
      "public-edge",
      "127.0.0.1",
      new Headers({ "cf-connecting-ip": " 203.0.113.7 " }),
    )).toEqual({ listener: "public-edge", clientIp: "203.0.113.7", onHost: false });
  });
});

describe("caller origin guards", () => {
  test("recognizes both Tailscale address families", () => {
    expect(isTailnetAddr("100.127.255.1")).toBe(true);
    expect(isTailnetAddr("100.128.0.1")).toBe(false);
    expect(isTailnetAddr("fd7a:115c:a1e0::123")).toBe(true);
  });

  test("on-host and tailnet privileges are distinct", () => {
    expect(() => assertOnHost({ listener: "direct", clientIp: "127.0.0.1", onHost: true })).not.toThrow();
    expect(() => assertOnHost({ listener: "direct", clientIp: "127.0.0.1", onHost: false })).toThrow("on-host only");
    expect(() => assertOnHostOrTailnet({ listener: "tailscale-serve", clientIp: "100.101.102.103", onHost: false })).not.toThrow();
    expect(() => assertOnHostOrTailnet({ listener: "public-edge", clientIp: "public", onHost: false }))
      .toThrow("on-host or tailnet only");
  });
});
