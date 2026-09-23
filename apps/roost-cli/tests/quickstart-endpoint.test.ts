// Quickstart endpoint tests pin strict no-effect argv parsing and the profiles
// consumed by fresh install, rerun, promotion, health, and secret-safe opening.
import { describe, expect, test } from "bun:test";
import {
  coordinatorEnvironmentForQuickstart,
  openQuickstartBrowser,
  parseQuickstartOptions,
  quickstart,
  quickstartLoopbackOrigin,
  resolveQuickstartEndpoint,
  waitForCoordHealth,
  type QuickstartEndpoint,
} from "../src/quickstart.ts";

type TestFetchImplementation = (
  input: string | URL | Request,
  init?: BunFetchRequestInit,
) => Promise<Response>;

function testFetch(implementation: TestFetchImplementation): typeof fetch {
  return Object.assign(implementation, { preconnect: fetch.preconnect });
}

function endpointFor(url: string, platform: NodeJS.Platform = "linux"): QuickstartEndpoint {
  return resolveQuickstartEndpoint(["--coordinator-url", url], platform);
}

const INSTALLED_HOSTED = {
  ROOST_COORDINATOR_BIND: "127.0.0.1:4207",
  ROOST_TRUST_PROXY: "1",
  ROOST_WEB_PUBLIC_URL: "https://installed.example.test",
  ROOST_COORDINATOR_PUBLIC_URL: "https://workers.example.test",
  ROOST_CORS_ALLOWED_ORIGINS: "https://installed.example.test,https://console.example.test",
};

describe("resolveQuickstartEndpoint", () => {
  test("selects a fresh local profile without reading ambient endpoint variables", () => {
    expect(resolveQuickstartEndpoint([], "linux")).toEqual({
      mode: "local",
      origin: "http://127.0.0.1:4103",
      loopbackPort: 4103,
      webPublicUrl: null,
      coordinatorPublicUrl: null,
      corsAllowedOrigins: ["http://127.0.0.1:4103"],
    });
    expect(() => resolveQuickstartEndpoint([], "win32"))
      .toThrow(/requires --coordinator-url/);
  });

  test("normalizes explicit HTTPS origins and retains front-door behavior", () => {
    expect(endpointFor("https://Example.COM:4443/")).toEqual({
      mode: "front-door",
      origin: "https://example.com:4443",
      loopbackPort: 4103,
      webPublicUrl: "https://example.com:4443",
      coordinatorPublicUrl: null,
      corsAllowedOrigins: ["http://127.0.0.1:4103"],
    });
    expect(endpointFor("https://dash.example.test:443").origin)
      .toBe("https://dash.example.test");
  });

  test("preserves installed hosted state on a no-URL rerun and only promotes endpoint fields", () => {
    const rerun = resolveQuickstartEndpoint([], "linux", INSTALLED_HOSTED);
    expect(rerun).toEqual({
      mode: "front-door",
      origin: "https://installed.example.test",
      loopbackPort: 4207,
      webPublicUrl: "https://installed.example.test",
      coordinatorPublicUrl: "https://workers.example.test",
      corsAllowedOrigins: ["https://installed.example.test", "https://console.example.test"],
    });
    const promoted = resolveQuickstartEndpoint(
      ["--coordinator-url", "https://new.example.test"],
      "linux",
      INSTALLED_HOSTED,
    );
    expect(promoted.webPublicUrl).toBe("https://new.example.test");
    expect(promoted.coordinatorPublicUrl).toBe("https://workers.example.test");
    expect(promoted.loopbackPort).toBe(4207);
    expect(promoted.corsAllowedOrigins).toEqual([
      "https://installed.example.test",
      "https://console.example.test",
      "http://127.0.0.1:4207",
    ]);
  });

  test("rejects unknown, repeated, valueless, and retired options before work", async () => {
    expect(() => parseQuickstartOptions(["--typo"])).toThrow(/unknown quickstart option/);
    expect(() => parseQuickstartOptions(["--dry-run", "--dry-run"])).toThrow(/only once/);
    expect(() => parseQuickstartOptions(["--coordinator-url", "--force"])).toThrow(/requires a value/);
    expect(() => parseQuickstartOptions(["--tls-cert", "/a.pem"])).toThrow(/no longer accepted/);
    await expect(quickstart(["--unknown-option"])).rejects.toThrow(/unknown quickstart option/);
    await expect(quickstart(["--coordinator-url", "http://host.example"]))
      .rejects.toThrow(/must use https/);
  });

  test("rejects every unsafe HTTPS URL shape", () => {
    const invalid = [
      "http://host.example",
      "https://user@host.example",
      "https://user:pass@host.example",
      "https://host.example:0",
      "https://host.example:65536",
      "https://host.example/app",
      "https://host.example/a/..",
      "https://host.example/?query=1",
      "https://host.example/#fragment",
      "https://host.example\\other",
      " https://host.example",
      "https://",
    ];
    for (const url of invalid) expect(() => endpointFor(url), url).toThrow();
  });
});

describe("quickstart endpoint consumers", () => {
  test("persists the fresh local profile without inherited public values", () => {
    expect(coordinatorEnvironmentForQuickstart(resolveQuickstartEndpoint([], "linux")))
      .toEqual({
        ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
        ROOST_TRUST_PROXY: "0",
        ROOST_WEB_PUBLIC_URL: "",
        ROOST_COORDINATOR_PUBLIC_URL: "",
        ROOST_CORS_ALLOWED_ORIGINS: "http://127.0.0.1:4103",
        ROOST_SKIP_ENV_LOCAL: "1",
      });
  });

  test("persists the fresh explicit front-door profile with local CORS", () => {
    expect(coordinatorEnvironmentForQuickstart(endpointFor("https://dash.example.test")))
      .toEqual({
        ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
        ROOST_TRUST_PROXY: "1",
        ROOST_WEB_PUBLIC_URL: "https://dash.example.test",
        ROOST_COORDINATOR_PUBLIC_URL: "",
        ROOST_CORS_ALLOWED_ORIGINS: "http://127.0.0.1:4103",
        ROOST_SKIP_ENV_LOCAL: "1",
      });
  });

  test("health polls the canonical loopback listener, not a front door", async () => {
    const urls: string[] = [];
    let now = 0;
    const endpoint = endpointFor("https://dash.example.test");
    const ok = await waitForCoordHealth(endpoint, 2_000, {
      fetch: testFetch(async (input) => {
        urls.push(String(input));
        return new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }),
      now: () => now,
      sleep: async (ms) => { now += ms; },
    });
    expect(ok).toBe(true);
    expect(urls).toEqual([
      `${quickstartLoopbackOrigin(endpoint)}/roost.v1.CoordinatorService/MiscHealth`,
    ]);
  });

  test("browser opener alone receives the bearer and failures never expose it", async () => {
    const endpoint = endpointFor("https://dash.example.test");
    const token = "roost_bt_top_secret";
    let command: readonly string[] = [];
    await openQuickstartBrowser(endpoint, token, "linux", async (value) => {
      command = value;
      return 0;
    });
    expect(command).toEqual(["xdg-open", `https://dash.example.test/#pair=${token}`]);

    let message = "";
    try {
      await openQuickstartBrowser(endpoint, token, "linux", async (value) => {
        throw new Error(`opener echoed ${value.join(" ")}`);
      });
    } catch (error) {
      message = String(error);
    }
    expect(message).toContain("browser opener failed");
    expect(message).not.toContain(token);
    expect(message).not.toContain("#pair=");
  });
});
