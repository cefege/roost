import { describe, expect, test } from "bun:test";
import { loadCoordConfig } from "../src/config.ts";

const AUD = "a".repeat(64);

function publicEnv(overrides: Record<string, string | undefined> = {}): Record<string, string | undefined> {
  return {
    ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
    ROOST_TRUST_PROXY: "1",
    ROOST_PUBLIC_BIND: "127.0.0.1:4104",
    ROOST_CF_ACCESS_TEAM_DOMAIN: "example.cloudflareaccess.com",
    ROOST_CF_ACCESS_AUD: AUD,
    ROOST_WEB_PUBLIC_URL: "https://roost.example.com",
    ROOST_COORDINATOR_PUBLIC_URL: "https://private.example.ts.net:4102",
    ...overrides,
  };
}

describe("coordinator listener trust configuration", () => {
  test("accepts distinct loopback private and public listeners", () => {
    const cfg = loadCoordConfig(publicEnv());
    expect(cfg.trustProxy).toBe(true);
    expect(cfg.publicBind).toBe("127.0.0.1:4104");
    expect(cfg.webPublicUrl).toBe("https://roost.example.com");
    expect(cfg.publicUrl).toBe("https://private.example.ts.net:4102");
  });

  for (const bind of ["0.0.0.0:4103", "[::]:4103", "192.168.1.8:4103"]) {
    test(`rejects trusted proxy headers on network-reachable bind ${bind}`, () => {
      expect(() => loadCoordConfig({
        ROOST_COORDINATOR_BIND: bind,
        ROOST_TRUST_PROXY: "1",
      })).toThrow("ROOST_COORDINATOR_BIND must use 127.0.0.1:<port>");
    });
  }

  test("requires all Access and browser URL settings", () => {
    for (const missing of [
      "ROOST_CF_ACCESS_TEAM_DOMAIN",
      "ROOST_CF_ACCESS_AUD",
      "ROOST_WEB_PUBLIC_URL",
    ]) {
      expect(() => loadCoordConfig(publicEnv({ [missing]: undefined })), missing)
        .toThrow("ROOST_PUBLIC_BIND requires");
    }
  });

  test("requires public mode to preserve private tailscale-serve trust", () => {
    expect(() => loadCoordConfig(publicEnv({ ROOST_TRUST_PROXY: undefined })))
      .toThrow("ROOST_PUBLIC_BIND requires ROOST_TRUST_PROXY=1");
  });

  test("rejects a network-reachable or occupied private-origin public bind", () => {
    expect(() => loadCoordConfig(publicEnv({ ROOST_PUBLIC_BIND: "0.0.0.0:4104" })))
      .toThrow("ROOST_PUBLIC_BIND must use 127.0.0.1:<port>");
    expect(() => loadCoordConfig(publicEnv({ ROOST_PUBLIC_BIND: "127.0.0.1:70000" })))
      .toThrow("ROOST_PUBLIC_BIND port must be 1-65535");
    expect(() => loadCoordConfig(publicEnv({ ROOST_PUBLIC_BIND: "127.0.0.1:4103" })))
      .toThrow("ROOST_PUBLIC_BIND must differ");
  });

  test("pins the Access JWKS host and audience shape", () => {
    expect(() => loadCoordConfig(publicEnv({
      ROOST_CF_ACCESS_TEAM_DOMAIN: "attacker.example.com",
    }))).toThrow("ROOST_CF_ACCESS_TEAM_DOMAIN");
    expect(() => loadCoordConfig(publicEnv({ ROOST_CF_ACCESS_AUD: "not-an-aud" })))
      .toThrow("ROOST_CF_ACCESS_AUD");
  });

  test("requires distinct bare HTTPS worker and browser origins", () => {
    expect(() => loadCoordConfig(publicEnv({ ROOST_WEB_PUBLIC_URL: "http://roost.example.com" })))
      .toThrow("ROOST_WEB_PUBLIC_URL must be an HTTPS origin");
    expect(() => loadCoordConfig(publicEnv({ ROOST_WEB_PUBLIC_URL: "https://roost.example.com/path" })))
      .toThrow("ROOST_WEB_PUBLIC_URL must be an HTTPS origin");
    expect(() => loadCoordConfig(publicEnv({
      ROOST_COORDINATOR_PUBLIC_URL: "https://same.example/",
      ROOST_WEB_PUBLIC_URL: "https://same.example",
    }))).toThrow("must differ from the browser-only");
  });

  test("prohibits relaxed CSP on the public listener", () => {
    expect(() => loadCoordConfig(publicEnv({ ROOST_RELAXED_CSP: "1" })))
      .toThrow("ROOST_PUBLIC_BIND cannot be combined");
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
});
