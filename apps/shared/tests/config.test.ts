// Covers the coordinator configuration contract: loopback-only trust, declared public
// origins, and allowed-origin parsing. The suite drives loadCoordConfig at the
// environment boundary, so every assertion is what a booting coordinator would see.

import { describe, expect, test } from "bun:test";
import { DEFAULT_COORDINATOR_BIND, loadCoordConfig } from "../src/config.ts";

describe("coordinator configuration", () => {
  for (const bind of ["0.0.0.0:4103", "[::]:4103", "192.168.1.8:4103"]) {
    test(`rejects trusted proxy headers on network-reachable bind ${bind}`, () => {
      expect(() => loadCoordConfig({
        ROOST_COORDINATOR_BIND: bind,
        ROOST_TRUST_PROXY: "1",
      })).toThrow("ROOST_COORDINATOR_BIND must use 127.0.0.1:<port>");
    });
  }

  test("accepts a loopback bind behind a trusted proxy and bounds its port", () => {
    const cfg = loadCoordConfig({
      ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
      ROOST_TRUST_PROXY: "1",
      ROOST_WEB_PUBLIC_URL: "https://roost.example.com",
    });
    expect(cfg.trustProxy).toBe(true);
    expect(cfg.bind).toBe("127.0.0.1:4103");
    expect(cfg.webPublicUrl).toBe("https://roost.example.com");

    expect(() => loadCoordConfig({
      ROOST_COORDINATOR_BIND: "127.0.0.1:70000",
      ROOST_TRUST_PROXY: "1",
    })).toThrow("ROOST_COORDINATOR_BIND port must be 1-65535");
  });

  test("leaves a network-reachable bind alone without a trusted proxy", () => {
    expect(loadCoordConfig({ ROOST_COORDINATOR_BIND: "0.0.0.0:4102" }).bind)
      .toBe("0.0.0.0:4102");
  });

  test("an unset bind resolves to the one declared loopback default", () => {
    // A plaintext listener must not reach every interface when nothing is set,
    // and every caller that dials a bare coordinator reads this same value.
    expect(DEFAULT_COORDINATOR_BIND).toBe("127.0.0.1:4103");
    expect(loadCoordConfig({}).bind).toBe(DEFAULT_COORDINATOR_BIND);
  });

  test("declares public origins and never derives one", () => {
    const cfg = loadCoordConfig({});
    expect(cfg.webPublicUrl).toBeUndefined();
    expect(cfg.publicUrl).toBeUndefined();

    for (const envName of ["ROOST_WEB_PUBLIC_URL", "ROOST_COORDINATOR_PUBLIC_URL"]) {
      for (const value of [
        "http://roost.example.com",
        "https://roost.example.com/path",
        "https://user@roost.example.com",
        "https://roost.example.com?token=secret",
        "https://roost.example.com#fragment",
        "not a URL",
      ]) {
        expect(() => loadCoordConfig({ [envName]: value }), `${envName}=${value}`)
          .toThrow(envName);
      }
    }
  });

  test("accepts one front door for both browser and worker traffic", () => {
    const cfg = loadCoordConfig({
      ROOST_WEB_PUBLIC_URL: "https://roost.example.com/",
      ROOST_COORDINATOR_PUBLIC_URL: "https://roost.example.com",
    });
    expect(cfg.webPublicUrl).toBe("https://roost.example.com");
    expect(cfg.publicUrl).toBe("https://roost.example.com");
  });

  test("keeps a distinct worker origin when the operator declares one", () => {
    const cfg = loadCoordConfig({
      ROOST_WEB_PUBLIC_URL: "https://roost.example.com",
      ROOST_COORDINATOR_PUBLIC_URL: "https://coord.example.com:4102",
    });
    expect(cfg.publicUrl).toBe("https://coord.example.com:4102");
  });

  test("validates every CORS entry as a bare HTTP(S) origin", () => {
    expect(() => loadCoordConfig({ ROOST_CORS_ALLOWED_ORIGINS: "file:///tmp/a" }))
      .toThrow("bare HTTP(S) origins");
    expect(() => loadCoordConfig({ ROOST_CORS_ALLOWED_ORIGINS: "https://example.com/path" }))
      .toThrow("bare HTTP(S) origins");
    expect(() => loadCoordConfig({ ROOST_CORS_ALLOWED_ORIGINS: "not a URL" }))
      .toThrow("contains an invalid origin");
    expect(loadCoordConfig({
      ROOST_CORS_ALLOWED_ORIGINS: "http://localhost:3000,https://example.com",
    }).corsAllowedOrigins).toEqual(["http://localhost:3000", "https://example.com"]);
  });

  test("parses exact bare HTTPS Push provider origins and defaults to disabled", () => {
    expect(loadCoordConfig({}).pushAllowedOrigins).toEqual([]);
    expect(loadCoordConfig({
      ROOST_PUSH_ALLOWED_ORIGINS: "https://push.example, https://updates.example:8443",
    }).pushAllowedOrigins).toEqual([
      "https://push.example",
      "https://updates.example:8443",
    ]);

    for (const origin of [
      "http://push.example",
      "https://push.example/",
      "https://push.example/path",
      "https://push.example?token=secret",
      "https://user@push.example",
      "not a URL",
    ]) {
      expect(() => loadCoordConfig({ ROOST_PUSH_ALLOWED_ORIGINS: origin }), origin)
        .toThrow("ROOST_PUSH_ALLOWED_ORIGINS");
    }
  });
});
