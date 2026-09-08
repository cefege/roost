// Quickstart endpoint tests pin the no-effect selection boundary and the exact
// service-facing values it produces. Injected runtime edges prove the health
// probe targets the coordinator's own listener and that the browser bearer
// never escapes into an error message.
import { describe, expect, test } from "bun:test";
import {
  coordinatorEnvironmentForQuickstart,
  openQuickstartBrowser,
  quickstart,
  resolveQuickstartEndpoint,
  waitForCoordHealth,
  type QuickstartEndpoint,
} from "../src/quickstart.ts";
import { _resolveDeployEnvValue } from "../src/deploy-plist-env.ts";

type TestFetchImplementation = (
  input: string | URL | Request,
  init?: BunFetchRequestInit,
) => Promise<Response>;

function testFetch(implementation: TestFetchImplementation): typeof fetch {
  return Object.assign(implementation, { preconnect: fetch.preconnect });
}

function endpointFor(url: string, platform: NodeJS.Platform = "linux"): QuickstartEndpoint {
  return resolveQuickstartEndpoint(["--coordinator-url", url], {}, platform);
}

describe("resolveQuickstartEndpoint", () => {
  test("refuses to invent a front door from ambient variables", () => {
    expect(() => resolveQuickstartEndpoint([], {
      ROOST_COORDINATOR_URL: "https://ambient.invalid:9999",
      ROOST_WEB_PUBLIC_URL: "https://ambient.invalid",
    }, "linux")).toThrow(/--coordinator-url is required/);
  });

  test("normalizes the declared origin and defaults a bare host to 443", () => {
    expect(endpointFor("https://Example.COM:4443/")).toEqual({
      origin: "https://example.com:4443",
      loopbackPort: 4103,
    });
    expect(endpointFor("https://dash.example.test").origin).toBe("https://dash.example.test");
    expect(endpointFor("https://dash.example.test:443").origin)
      .toBe("https://dash.example.test");
  });

  test("accepts the equals form and refuses a repeated or valueless flag", () => {
    expect(resolveQuickstartEndpoint(
      ["--coordinator-url=https://host.example:7443"],
      {},
      "linux",
    ).origin).toBe("https://host.example:7443");
    expect(() => resolveQuickstartEndpoint([
      "--coordinator-url", "https://a.example",
      "--coordinator-url", "https://b.example",
    ], {}, "linux")).toThrow(/only once/);
    expect(() => resolveQuickstartEndpoint(
      ["--coordinator-url", "--force"],
      {},
      "linux",
    )).toThrow(/requires a value/);
  });

  test("rejects every unsafe URL shape", () => {
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
    for (const url of invalid) {
      expect(() => endpointFor(url), url).toThrow();
    }
  });

  test("the retired TLS flags are refused rather than silently ignored", () => {
    for (const args of [
      ["--coordinator-url", "https://dash.example.test", "--tls-cert", "/a.pem"],
      ["--coordinator-url", "https://dash.example.test", "--tls-key=/b.pem"],
    ]) {
      expect(() => resolveQuickstartEndpoint(args, {}, "linux"))
        .toThrow(/no longer accepted/);
    }
  });

  test("quickstart refuses at its no-effect boundary before any mutation", async () => {
    await expect(quickstart([])).rejects.toThrow(/--coordinator-url is required/);
    await expect(quickstart(["--coordinator-url", "http://host.example"]))
      .rejects.toThrow(/must use https/);
  });
});

describe("quickstart endpoint consumers", () => {
  test("the coordinator service is bound to loopback behind a trusted proxy", () => {
    expect(coordinatorEnvironmentForQuickstart(endpointFor("https://dash.example.test")))
      .toEqual({
        ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
        ROOST_TRUST_PROXY: "1",
        ROOST_WEB_PUBLIC_URL: "https://dash.example.test",
        ROOST_SKIP_ENV_LOCAL: "1",
      });
  });

  test("selected endpoint overrides only a stale installed worker URL", () => {
    const installed = {
      ROOST_COORDINATOR_URL: "https://stale.example.test:4102",
      ROOST_WORKER_LABEL: "existing-worker",
    };
    expect(_resolveDeployEnvValue(
      "ROOST_COORDINATOR_URL",
      installed,
      "https://selected.example.test:8443",
      "self",
    )).toBe("https://selected.example.test:8443");
    expect(_resolveDeployEnvValue("ROOST_COORDINATOR_URL", installed, undefined, "self"))
      .toBe("https://stale.example.test:4102");
    expect(_resolveDeployEnvValue("ROOST_WORKER_LABEL", installed, undefined, "self"))
      .toBe("existing-worker");
  });

  test("health polls the coordinator's own listener, not the front door", async () => {
    const urls: string[] = [];
    let now = 0;
    const ok = await waitForCoordHealth(endpointFor("https://dash.example.test"), 2_000, {
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
      "http://127.0.0.1:4103/roost.v1.CoordinatorService/MiscHealth",
    ]);

    now = 0;
    expect(await waitForCoordHealth(endpointFor("https://dash.example.test"), 1_000, {
      fetch: testFetch(async () => new Response(JSON.stringify({ ok: false }), { status: 200 })),
      now: () => now,
      sleep: async (ms) => { now += ms; },
    })).toBe(false);
  });

  test("browser opener alone receives the bearer and all failures are constant", async () => {
    const endpoint = endpointFor("https://dash.example.test");
    const token = "roost_bt_top_secret";
    let command: readonly string[] = [];
    await openQuickstartBrowser(endpoint, token, "linux", async (value) => {
      command = value;
      return 0;
    });
    expect(command[0]).toBe("xdg-open");
    expect(command[1]).toBe(`https://dash.example.test/#pair=${token}`);

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
