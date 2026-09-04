import { afterEach, describe, expect, test } from "bun:test";
import * as fs from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  coordinatorEnvironmentForQuickstart,
  openQuickstartBrowser,
  resolveQuickstartEndpoint,
  validateQuickstartTlsFiles,
  waitForCoordHealth,
  type QuickstartEndpoint,
  type QuickstartTlsFileSystem,
} from "../src/quickstart.ts";

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

type TestFetchImplementation = (
  input: string | URL | Request,
  init?: BunFetchRequestInit,
) => Promise<Response>;

function testFetch(implementation: TestFetchImplementation): typeof fetch {
  return Object.assign(implementation, { preconnect: fetch.preconnect });
}

function directArgs(url = "https://Example.COM:4443/"): string[] {
  return [
    "--coordinator-url", url,
    "--tls-cert", "/secure/fullchain.pem",
    "--tls-key", "/secure/privkey.pem",
  ];
}

function directEndpoint(): QuickstartEndpoint {
  return resolveQuickstartEndpoint(directArgs(), {}, "linux");
}

describe("resolveQuickstartEndpoint", () => {
  test("no endpoint flags selects automatic mode regardless of ambient endpoint variables", () => {
    expect(resolveQuickstartEndpoint([], {
      ROOST_COORDINATOR_URL: "https://ambient.invalid:9999",
      ROOST_TLS_CERT_PATH: "/ambient/cert",
      ROOST_TLS_KEY_PATH: "/ambient/key",
    }, "linux")).toEqual({
      mode: "automatic",
      origin: null,
      hostname: null,
      port: null,
      tlsCertPath: null,
      tlsKeyPath: null,
    });
  });

  test("normalizes an explicit HTTPS origin and preserves its arbitrary listener port", () => {
    expect(directEndpoint()).toEqual({
      mode: "explicit",
      origin: "https://example.com:4443",
      hostname: "example.com",
      port: 4443,
      tlsCertPath: "/secure/fullchain.pem",
      tlsKeyPath: "/secure/privkey.pem",
    });
    expect(resolveQuickstartEndpoint(directArgs("https://example.com:443"), {}, "linux"))
      .toMatchObject({ origin: "https://example.com", port: 443 });
  });

  test("accepts equals-form flags without allowing them to fall back to automatic mode", () => {
    expect(resolveQuickstartEndpoint([
      "--coordinator-url=https://host.example:7443",
      "--tls-cert=/tls/cert.pem",
      "--tls-key=/tls/key.pem",
    ], {}, "linux")).toMatchObject({ mode: "explicit", port: 7443 });
  });

  test("requires the complete explicit flag group", () => {
    for (const args of [
      ["--coordinator-url", "https://host.example:4102"],
      ["--tls-cert", "/tls/cert.pem", "--tls-key", "/tls/key.pem"],
      ["--coordinator-url=https://host.example:4102", "--tls-cert=/tls/cert.pem"],
    ]) {
      expect(() => resolveQuickstartEndpoint(args, {}, "linux")).toThrow(/provided together/);
    }
  });

  test("rejects every unsafe URL shape", () => {
    const invalid = [
      "http://host.example:4102",
      "https://user@host.example:4102",
      "https://user:pass@host.example:4102",
      "https://host.example",
      "https://host.example:0",
      "https://host.example:65536",
      "https://host.example:4102/app",
      "https://host.example:4102/a/..",
      "https://host.example:4102/?query=1",
      "https://host.example:4102/#fragment",
      "https://host.example:4102\\other",
    ];
    for (const url of invalid) {
      expect(() => resolveQuickstartEndpoint(directArgs(url), {}, "linux")).toThrow();
    }
  });

  test("requires absolute and lexically distinct TLS paths on each platform", () => {
    expect(() => resolveQuickstartEndpoint([
      "--coordinator-url", "https://host.example:4102",
      "--tls-cert", "relative.pem",
      "--tls-key", "/tls/key.pem",
    ], {}, "linux")).toThrow(/absolute path/);
    expect(() => resolveQuickstartEndpoint([
      "--coordinator-url", "https://host.example:4102",
      "--tls-cert", "/tls/one/../same.pem",
      "--tls-key", "/tls/same.pem",
    ], {}, "linux")).toThrow(/distinct paths/);
    expect(() => resolveQuickstartEndpoint([
      "--coordinator-url", "https://host.example:4102",
      "--tls-cert", "C:\\TLS\\CERT.pem",
      "--tls-key", "c:\\tls\\cert.PEM",
    ], {}, "win32")).toThrow(/distinct paths/);
  });
});

describe("validateQuickstartTlsFiles", () => {
  test("automatic mode performs no filesystem operation", () => {
    const endpoint = resolveQuickstartEndpoint([], {}, "linux");
    const fail = () => { throw new Error("filesystem was touched"); };
    const injected = {
      lstatSync: fail,
      accessSync: fail,
      realpathSync: fail,
      statSync: fail,
      constants: fs.constants,
    } as unknown as QuickstartTlsFileSystem;
    expect(() => validateQuickstartTlsFiles(endpoint, injected)).not.toThrow();
  });

  test("accepts two readable regular files", () => {
    const root = fs.mkdtempSync(join(tmpdir(), "roost-endpoint-"));
    roots.push(root);
    const cert = join(root, "cert.pem");
    const key = join(root, "key.pem");
    fs.writeFileSync(cert, "certificate");
    fs.writeFileSync(key, "private-key");
    const endpoint = resolveQuickstartEndpoint([
      "--coordinator-url", "https://host.example:5443",
      "--tls-cert", cert,
      "--tls-key", key,
    ], {}, "linux");
    expect(() => validateQuickstartTlsFiles(endpoint, fs)).not.toThrow();
  });

  test("rejects symlinks, directories, hard-link aliases, and unreadable files", () => {
    const root = fs.mkdtempSync(join(tmpdir(), "roost-endpoint-"));
    roots.push(root);
    const cert = join(root, "cert.pem");
    const key = join(root, "key.pem");
    const alias = join(root, "alias.pem");
    const symlink = join(root, "symlink.pem");
    fs.writeFileSync(cert, "certificate");
    fs.writeFileSync(key, "private-key");
    fs.linkSync(cert, alias);
    fs.symlinkSync(key, symlink);
    const endpointFor = (certPath: string, keyPath: string) => resolveQuickstartEndpoint([
      "--coordinator-url", "https://host.example:5443",
      "--tls-cert", certPath,
      "--tls-key", keyPath,
    ], {}, "linux");

    expect(() => validateQuickstartTlsFiles(endpointFor(symlink, key), fs)).toThrow(/non-symlink/);
    expect(() => validateQuickstartTlsFiles(endpointFor(root, key), fs)).toThrow(/regular file/);
    expect(() => validateQuickstartTlsFiles(endpointFor(cert, alias), fs)).toThrow(/distinct files/);

    const unreadableFs = {
      lstatSync: fs.lstatSync,
      accessSync: () => { throw new Error("denied"); },
      realpathSync: fs.realpathSync,
      statSync: fs.statSync,
      constants: fs.constants,
    } as unknown as QuickstartTlsFileSystem;
    expect(() => validateQuickstartTlsFiles(endpointFor(cert, key), unreadableFs)).toThrow(/not readable/);
  });
});

describe("quickstart endpoint consumers", () => {
  test("direct services receive the exact direct HTTPS contract", () => {
    expect(coordinatorEnvironmentForQuickstart(directEndpoint(), "linux")).toEqual({
      ROOST_FRONTED: "0",
      ROOST_COORDINATOR_BIND: "0.0.0.0:4443",
      ROOST_COORDINATOR_PUBLIC_URL: "https://example.com:4443",
      ROOST_TLS_CERT_PATH: "/secure/fullchain.pem",
      ROOST_TLS_KEY_PATH: "/secure/privkey.pem",
      ROOST_SKIP_ENV_LOCAL: "1",
    });
  });

  test("automatic POSIX keeps Serve while automatic Windows keeps its direct tailnet certificate", () => {
    const automatic: QuickstartEndpoint = {
      mode: "automatic",
      origin: "https://host.tail.example:4102",
      hostname: "host.tail.example",
      port: 4102,
      tlsCertPath: "/tls/host.crt",
      tlsKeyPath: "/tls/host.key",
    };
    expect(coordinatorEnvironmentForQuickstart(automatic, "linux")).toEqual({
      ROOST_FRONTED: "1",
      ROOST_COORD_LOOPBACK_PORT: "4103",
      ROOST_TAILNET_HTTPS_PORT: "4102",
      ROOST_COORDINATOR_PUBLIC_URL: "https://host.tail.example:4102",
    });
    expect(coordinatorEnvironmentForQuickstart(automatic, "win32")).toMatchObject({
      ROOST_FRONTED: "0",
      ROOST_COORDINATOR_BIND: "0.0.0.0:4102",
      ROOST_TAILNET_HTTPS_PORT: "4102",
      ROOST_TLS_CERT_PATH: "/tls/host.crt",
      ROOST_TLS_KEY_PATH: "/tls/host.key",
    });
  });

  test("health probes the exact normalized endpoint and requires the affirmative payload", async () => {
    const urls: string[] = [];
    let now = 0;
    const ok = await waitForCoordHealth(directEndpoint(), 2_000, {
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
      "https://example.com:4443/roost.v1.CoordinatorService/MiscHealth",
    ]);

    now = 0;
    expect(await waitForCoordHealth(directEndpoint(), 1_000, {
      fetch: testFetch(async () => new Response(JSON.stringify({ ok: false }), { status: 200 })),
      now: () => now,
      sleep: async (ms) => { now += ms; },
    })).toBe(false);
  });

  test("browser opener alone receives the bearer and all failures are constant", async () => {
    const token = "roost_bt_top_secret";
    let command: readonly string[] = [];
    await openQuickstartBrowser(directEndpoint(), token, "linux", async (value) => {
      command = value;
      return 0;
    });
    expect(command[0]).toBe("xdg-open");
    expect(command[1]).toBe(`https://example.com:4443/#pair=${token}`);

    let message = "";
    try {
      await openQuickstartBrowser(directEndpoint(), token, "linux", async (value) => {
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
