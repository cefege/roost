import { appendFileSync, lstatSync, readFileSync, realpathSync } from "node:fs";
import { randomUUID, createHash } from "node:crypto";
import { networkInterfaces } from "node:os";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { allowOwnedLoopbackOrigin, revokeOwnedLoopbackOrigin, type OwnedLoopbackListener } from "./network-guard";
import { assertOwned } from "./profile-manifest";
import { objectValue, stringValue } from "./runtime";

const callbackSource = "packages/ai/src/registry/oauth/callback-server.ts";
let installed = false;

interface Listener {
  origin: string;
  provenance: OwnedLoopbackListener;
}

function proxyTrace(value: unknown): unknown {
  if (typeof value !== "string") return value ?? null;
  try {
    const url = new URL(value);
    return { origin: url.origin, credentials: Boolean(url.username || url.password) };
  } catch { return { sha256: createHash("sha256").update(value).digest("hex") }; }
}

function headersTrace(headers: Headers): unknown[] {
  return [...headers].map(([name, value]) => /^(authorization|proxy-authorization|cookie|set-cookie)$/i.test(name)
    ? { name, redacted: true, sha256: createHash("sha256").update(value).digest("hex") }
    : { name, value });
}

function preflightCodexCallback(record: (value: Record<string, unknown>) => void): void {
  const tool = process.platform === "linux" ? "ss" : process.platform === "darwin" ? "/usr/sbin/lsof" : undefined;
  if (!tool) throw new Error("Controlled Codex OAuth callback preflight requires Linux ss or macOS lsof");
  const args = process.platform === "linux" ? ["-H", "-ltn", "sport = :1455"] : ["-nP", "-iTCP:1455", "-sTCP:LISTEN", "-t"];
  const result = spawnSync(tool, args, { encoding: "utf8", timeout: 2_000, maxBuffer: 64 * 1024, stdio: ["ignore", "pipe", "pipe"] });
  const emptyLsof = process.platform === "darwin" && result.status === 1 && !result.stdout?.trim();
  if (result.error || result.signal || result.stderr?.trim() || (result.status !== 0 && !emptyLsof)) {
    record({ type: "codex-callback-preflight", port: 1455, tool, error: "Unable to inspect listening sockets" });
    throw new Error(`Codex OAuth requires a working ${tool} listener inventory before binding port 1455`, { cause: result.error });
  }
  const occupied = Boolean(result.stdout.trim());
  record({ type: "codex-callback-preflight", port: 1455, tool, occupied });
  if (occupied) throw Object.assign(new Error("Codex OAuth callback port 1455 has an existing listener; isolated login refuses to reuse it"), { code: "EADDRINUSE" });
}

function authRequestTrace(raw: string, contentType: string | null): Record<string, unknown> {
  if (Buffer.byteLength(raw) > 2 * 1024 * 1024) throw new Error("Auth fixture request exceeded its bound");
  const form = contentType?.includes("application/x-www-form-urlencoded") ? new URLSearchParams(raw) : undefined;
  return { bytes: Buffer.byteLength(raw), sha256: createHash("sha256").update(raw).digest("hex"),
    ...(form ? { fields: [...new Set(form.keys())], grantType: form.get("grant_type"),
      authorizationCodePresent: form.has("code"), codeVerifierPresent: form.has("code_verifier"), refreshTokenPresent: form.has("refresh_token") } : {}) };
}

export function installFixtureTransportReplay(): void {
  const registryDestination = process.env.OMP_PARITY_SMITHERY_REGISTRY_URL;
  const codexTokenDestination = process.env.OMP_PARITY_CODEX_OAUTH_TOKEN_URL;
  const codexAuthDestination = process.env.OMP_PARITY_CODEX_AUTH_ORIGIN;
  const observeCallbacks = process.env.OMP_PARITY_MCP_OAUTH_CALLBACKS === "1";
  if (!registryDestination && !codexTokenDestination && !codexAuthDestination && !observeCallbacks) return;
  if (installed) throw new Error("Fixture transport replay was installed twice");
  if (process.env.OMP_PARITY_MCP_OAUTH_CALLBACKS && !observeCallbacks) throw new Error("Invalid MCP callback observer declaration");
  if (codexTokenDestination && !observeCallbacks) throw new Error("Codex OAuth token replay requires actual callback listener observation");
  const workspace = realpathSync(process.env.OMP_PARITY_WORKSPACE ?? "");
  const home = realpathSync(process.env.HOME ?? "");
  const manifestPath = realpathSync(join(workspace, ".parity-profile.json"));
  assertOwned(workspace, manifestPath);
  const profile = objectValue(JSON.parse(readFileSync(manifestPath, "utf8")), "owned profile");
  const sourceRoot = realpathSync(stringValue(profile.sourceRoot, "profile source root"));
  const ownedSourceRoot = join(import.meta.dir, "../../../target/omp-parity-source");
  if (lstatSync(ownedSourceRoot).isSymbolicLink() || sourceRoot !== realpathSync(ownedSourceRoot)) throw new Error("Fixture transport requires the owned pinned source checkout");
  if (profile.version !== 1 || realpathSync(stringValue(profile.workspace, "profile workspace")) !== workspace || home !== realpathSync(stringValue(profile.home, "profile home")) || home !== join(workspace, ".parity-home") || realpathSync(process.env.OMP_PARITY_SOURCE_ROOT ?? "") !== sourceRoot) throw new Error("Fixture transport requires its owned profile and source");
  const trace = realpathSync(process.env.OMP_PARITY_NETWORK_TRACE ?? "");
  if (trace !== join(home, ".parity/network.jsonl")) throw new Error("Fixture transport requires the owned network trace");
  assertOwned(home, trace);
  const generationPath = realpathSync(join(home, ".parity/process-generation.json"));
  assertOwned(home, generationPath);
  const processRecord = objectValue(JSON.parse(readFileSync(generationPath, "utf8")), "actual source process generation");
  const generation = processRecord.generation;
  if (processRecord.pid !== process.pid || !Number.isSafeInteger(generation) || typeof generation !== "number" || generation < 1 || realpathSync(stringValue(processRecord.sourceRoot, "generation source root")) !== sourceRoot || realpathSync(stringValue(processRecord.workspace, "generation workspace")) !== workspace) throw new Error("Fixture transport process generation is not current and owned");
  let events = 0;
  const record = (value: Record<string, unknown>): void => {
    if (++events > 1024) throw new Error("Fixture transport exceeded its bounded trace");
    appendFileSync(trace, `${JSON.stringify({ ...value, pid: process.pid, generation })}\n`);
  };
  const wire = (raw: string): Record<string, unknown> => {
    if (Buffer.byteLength(raw) > 2 * 1024 * 1024) throw new Error("Fixture transport payload exceeded its bound");
    return { raw, sha256: createHash("sha256").update(raw).digest("hex") };
  };
  if (registryDestination || codexTokenDestination || codexAuthDestination) {
    const ownedDestination = (value: string | undefined, expectedPath: string): URL | undefined => {
      if (!value) return undefined;
      const destination = new URL(value);
      if (destination.protocol !== "http:" || destination.hostname !== "127.0.0.1" || !destination.port || destination.username || destination.password || destination.pathname !== expectedPath || destination.search || destination.hash || !(process.env.OMP_PARITY_ALLOWED_ORIGINS ?? "").split(",").includes(destination.origin)) throw new Error("Fixture destination must be its declared allowed owned loopback HTTP endpoint");
      return destination;
    };
    const registry = ownedDestination(registryDestination, "/");
    const codexToken = ownedDestination(codexTokenDestination, "/oauth/token");
    const codexAuth = ownedDestination(codexAuthDestination, "/");
    let codexClientVersion: string | undefined;
    if (codexAuth) {
      const source = realpathSync(join(sourceRoot, "packages/catalog/src/wire/codex.ts"));
      assertOwned(sourceRoot, source);
      // Use the owned source pin rather than maintaining a second discovery-version allowlist.
      const literal = readFileSync(source, "utf8").match(/^export const CODEX_CLIENT_VERSION = ("[^"\r\n]+");\r?$/m)?.[1];
      const version: unknown = literal === undefined ? undefined : JSON.parse(literal);
      if (typeof version !== "string" || !version || version !== version.trim()) throw new Error("The owned Codex client-version export is not a nonempty string literal");
      codexClientVersion = version;
    }
    const guardedFetch = globalThis.fetch;
    const replayFetch: typeof fetch = Object.assign(async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const original = input instanceof Request ? input.url : String(input);
      const url = new URL(original);
      const method = (init?.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
      const search = url.pathname === "/servers" && [...url.searchParams.keys()].every(key => ["q", "pageSize", "page"].includes(key) && url.searchParams.getAll(key).length === 1);
      const details = /^\/servers\/[A-Za-z0-9@_.-]+\/[A-Za-z0-9@_.-]+$/.test(url.pathname) && !url.search;
      let service: "smithery" | "codex-oauth" | "codex-auth" | undefined;
      let mapped: string | undefined;
      if (!url.username && !url.password && !url.hash) {
        if (registry && url.origin === "https://registry.smithery.ai" && method === "GET" && (search || details)) {
          service = "smithery"; mapped = new URL(`${url.pathname}${url.search}`, registry).href;
        } else if (codexToken && original === "https://auth.openai.com/oauth/token" && method === "POST") {
          service = "codex-oauth"; mapped = codexToken.href;
        } else if (codexAuth && url.origin === "https://chatgpt.com" && (
          method === "GET" && ["/backend-api/wham/usage", "/backend-api/wham/rate-limit-reset-credits", "/backend-api/accounts/verified_access"].includes(url.pathname)
          || method === "POST" && url.pathname === "/backend-api/wham/rate-limit-reset-credits/consume"
          || method === "GET" && url.pathname === "/backend-api/codex/models" && url.searchParams.size === 1 && url.searchParams.get("client_version") === codexClientVersion
        )) {
          service = "codex-auth"; mapped = new URL(`${url.pathname}${url.search}`, codexAuth).href;
        }
      }
      if (!service || !mapped) return guardedFetch(input, init);
      const request = new Request(input, init);
      const routed = { ...init } as RequestInit & { proxy?: unknown };
      const proxy = proxyTrace(routed.proxy);
      const initKeys = Object.keys(routed);
      delete routed.proxy;
      const raw = await request.clone().text();
      record({ type: "fixture-transport", service, phase: "request", original, mapped, method: request.method,
        headers: headersTrace(request.headers), init: { keys: initKeys, proxy, redirect: request.redirect, credentials: request.credentials, mode: request.mode, cache: request.cache, keepalive: request.keepalive, signalAborted: request.signal.aborted },
        ...(service === "smithery" ? wire(raw) : authRequestTrace(raw, request.headers.get("content-type"))) });
      const response = await guardedFetch(new Request(mapped, request), routed);
      record({ type: "fixture-transport", service, phase: "response", original, mapped, status: response.status, headers: headersTrace(response.headers),
        ...(service === "smithery" ? wire(await response.clone().text()) : {}) });
      return response;
    }, { preconnect: guardedFetch.preconnect.bind(guardedFetch) });
    globalThis.fetch = replayFetch;
  }
  if (observeCallbacks) {
    const sourcePath = realpathSync(join(sourceRoot, callbackSource));
    assertOwned(sourceRoot, sourcePath);
    const actualServe = Bun.serve;
    const listeners = new Map<string, Listener>();
    const aliases = new Map<number, Listener>();
    const ipv6Available = Object.values(networkInterfaces()).some(addresses => addresses?.some(address => address.internal && address.family === "IPv6"));
    const retire = (listener: Listener): void => {
      revokeOwnedLoopbackOrigin(listener.origin, listener.provenance.listenerId);
    };
    const register = (hostname: string, port: number, listenerIds?: string[]): Listener => {
      const origin = `http://${hostname === "::1" ? "[::1]" : hostname}:${port}`;
      const provenance: OwnedLoopbackListener = { pid: process.pid, generation, listenerId: randomUUID(), source: callbackSource, sourcePath, hostname, port, ...(listenerIds ? { listenerIds } : {}) };
      allowOwnedLoopbackOrigin(origin, provenance);
      return { origin, provenance };
    };
    Bun.serve = new Proxy(actualServe, {
      apply(target, receiver, args: unknown[]) {
        // An OAuth ancestor alone cannot establish ownership of a listener opened by its caller.
        const caller = new Error().stack?.split("\n").slice(1).find(line => /:\d+:\d+\)?$/.test(line.trim()) && !line.includes(`${import.meta.path}:`));
        if (!caller?.includes(`${sourcePath}:`)) return Reflect.apply(target, receiver, args);
        const options = objectValue(args[0], "actual OAuth callback listener options");
        const hostname = options.hostname;
        if (typeof hostname !== "string" || !["127.0.0.1", "::1"].includes(hostname) || options.reusePort !== false || "unix" in options || "tls" in options) throw new Error("OAuth callback attempted a non-owned or public listener");
        if (codexTokenDestination && options.port === 1455 && ![...listeners.values()].some(entry => entry.provenance.port === 1455)) preflightCodexCallback(record);
        const server = Reflect.apply(target, receiver, args) as Bun.Server<unknown>;
        const port = server.port;
        const boundHostname = server.hostname?.replace(/^\[|\]$/g, "");
        if (typeof port !== "number" || !Number.isSafeInteger(port) || port < 1 || port > 65535 || boundHostname !== hostname) {
          void server.stop(true);
          throw new Error("Actual OAuth callback listener address did not match its loopback bind");
        }
        const actualStop = server.stop;
        let listener: Listener | undefined;
        let closed = false;
        const retireOwned = (): void => {
          if (!listener || closed) return;
          closed = true;
          const alias = aliases.get(port);
          try {
            if (alias?.provenance.listenerIds?.includes(listener.provenance.listenerId)) {
              aliases.delete(port);
              retire(alias);
            }
          } finally {
            listeners.delete(listener.provenance.listenerId);
            retire(listener);
          }
        };
        try {
          listener = register(hostname, port);
          listeners.set(listener.provenance.listenerId, listener);
          const samePort = [...listeners.values()].filter(entry => entry.provenance.port === port);
          const hasIPv4 = samePort.some(entry => entry.provenance.hostname === "127.0.0.1");
          const hasIPv6 = samePort.some(entry => entry.provenance.hostname === "::1");
          if (!aliases.has(port) && hasIPv4 && (hasIPv6 || !ipv6Available)) aliases.set(port, register("localhost", port, samePort.map(entry => entry.provenance.listenerId)));
          record({ type: "owned-listener-observed", origin: listener.origin, listenerId: listener.provenance.listenerId, source: callbackSource, sourcePath, caller: caller.trim().slice(0, 4096), hostname, port });
          server.stop = function(closeActiveConnections?: boolean) {
            const result = actualStop.call(this, closeActiveConnections);
            retireOwned();
            return result;
          };
          return server;
        } catch (cause) {
          void actualStop.call(server, true);
          retireOwned();
          throw cause;
        }
      },
    });
  }
  installed = true;
  record({ type: "fixture-transport-installed", smithery: Boolean(registryDestination), codexOAuth: Boolean(codexTokenDestination), codexAuth: Boolean(codexAuthDestination), oauthCallbacks: observeCallbacks });
}
