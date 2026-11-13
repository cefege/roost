import { describe, expect, it } from "bun:test";
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { isolatedEnvironment, objectValue, within } from "./runtime";

interface BoundaryResult {
  value: Record<string, unknown>;
  rows: Record<string, unknown>[];
  pid: number;
}

async function runBoundary(program: string, responder: (request: Request) => Response | Promise<Response>, options: { oauth?: boolean; codexAuth?: boolean } = {}): Promise<BoundaryResult> {
  const workspace = await realpath(await mkdtemp(join(tmpdir(), "omp-transport-boundary-")));
  const home = join(workspace, ".parity-home");
  const sourceRoot = await realpath(join(import.meta.dir, "../../../target/omp-parity-source"));
  await mkdir(join(home, ".parity"), { recursive: true });
  const trace = join(home, ".parity/network.jsonl");
  await writeFile(trace, "");
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: responder });
  const origin = `http://127.0.0.1:${server.port}`;
  await writeFile(join(workspace, ".parity-profile.json"), JSON.stringify({ version: 1, workspace, home, sourceRoot }));
  const bootstrap = `
    import { writeFileSync } from "node:fs";
    import { join } from "node:path";
    const home = process.env.HOME;
    const workspace = process.env.OMP_PARITY_WORKSPACE;
    const sourceRoot = process.env.OMP_PARITY_SOURCE_ROOT;
    // This isolated test process owns its first observed boundary generation.
    writeFileSync(join(home, ".parity/process-generation.json"), JSON.stringify({ generation: 1, pid: process.pid, sourceRoot, workspace }));
    // Guard installation must observe the isolated child's environment before source modules load.
    const { installFixtureTransportReplay } = await import(${JSON.stringify(pathToFileURL(join(import.meta.dir, "fixture-transport-replay.ts")).href)});
    installFixtureTransportReplay();
    ${program}
  `;
  const child = Bun.spawn([process.execPath, "--eval", bootstrap], { cwd: workspace,
    env: isolatedEnvironment(home, { OMP_PARITY_SOURCE_ROOT: sourceRoot, OMP_PARITY_WORKSPACE: workspace,
      OMP_PARITY_ALLOWED_ORIGINS: origin, OMP_PARITY_NETWORK_TRACE: trace,
      ...(options.oauth || options.codexAuth ? { OMP_PARITY_MCP_OAUTH_CALLBACKS: "1" } : { OMP_PARITY_SMITHERY_REGISTRY_URL: origin }),
      ...(options.codexAuth ? { OMP_PARITY_CODEX_OAUTH_TOKEN_URL: `${origin}/oauth/token`, OMP_PARITY_CODEX_AUTH_ORIGIN: origin } : {}),
      BOUNDARY_SERVICE_ORIGIN: origin }), stdout: "pipe", stderr: "pipe" });
  try {
    const [status, stdout, stderr] = await within(Promise.all([child.exited, new Response(child.stdout).text(), new Response(child.stderr).text()]), "actual boundary child", 12_000);
    if (status !== 0) throw new Error(`Actual boundary child failed (${status}): ${stderr}\n${stdout}`);
    const result = stdout.split("\n").findLast(line => line.startsWith("OMP_BOUNDARY_RESULT="));
    if (!result) throw new Error(`Missing actual boundary result: ${stdout}\n${stderr}`);
    return { value: objectValue(JSON.parse(result.slice("OMP_BOUNDARY_RESULT=".length)), "boundary result"),
      rows: (await readFile(trace, "utf8")).split("\n").filter(Boolean).map(line => objectValue(JSON.parse(line), "boundary trace")), pid: child.pid };
  } finally {
    if (child.exitCode === null) { child.kill("SIGKILL"); await child.exited; }
    await server.stop(true);
    await rm(workspace, { recursive: true, force: true });
  }
}

describe("fixture physical transport boundaries", () => {
  it("runs the unchanged Smithery client over real HTTP and preserves scoped request semantics", async () => {
    const received: Array<{ path: string; authorization: string | null; marker: string | null }> = [];
    const result = await runBoundary(`
      const { searchSmitheryRegistry } = await import(sourceRoot + "/packages/coding-agent/src/mcp/smithery-registry.ts");
      const found = await searchSmitheryRegistry("parity", { apiKey: "owned-nonsecret-registry-key", limit: 1 });
      const request = new Request("https://registry.smithery.ai/servers?q=parity&pageSize=20&page=2", { headers: { "x-owned-marker": "preserved" } });
      const forwarded = await fetch(request, { proxy: "http://127.0.0.1:9", redirect: "manual" });
      if (!forwarded.ok) throw new Error("Actual mapped HTTP request failed");
      await forwarded.text();
      console.log("OMP_BOUNDARY_RESULT=" + JSON.stringify({ names: found.map(server => server.name), urls: found.map(server => server.config.url), forwardedStatus: forwarded.status }));
    `, request => {
      const url = new URL(request.url);
      received.push({ path: `${url.pathname}${url.search}`, authorization: request.headers.get("authorization"), marker: request.headers.get("x-owned-marker") });
      if (url.pathname === "/servers") return Response.json({ servers: [{ id: "parity/fixture", namespace: "parity", slug: "fixture", qualifiedName: "@parity/fixture", displayName: "Parity fixture" }] });
      if (url.pathname === "/servers/parity/fixture") return Response.json({ qualifiedName: "@parity/fixture", remote: true, connections: [{ type: "http", deploymentUrl: `${url.origin}/mcp` }] });
      return new Response("Unexpected route", { status: 409 });
    });
    expect(result.value.names).toEqual(["parity/fixture"]);
    expect(received.map(request => request.path)).toEqual(["/servers?q=parity&pageSize=20", "/servers/parity/fixture", "/servers?q=parity&pageSize=20&page=2"]);
    expect(received.slice(0, 2).map(request => request.authorization)).toEqual(["Bearer owned-nonsecret-registry-key", "Bearer owned-nonsecret-registry-key"]);
    expect(received[2]?.marker).toBe("preserved");
    const mapped = result.rows.filter(row => row.type === "fixture-transport" && row.phase === "request");
    expect(mapped).toHaveLength(3);
    expect(mapped.map(row => row.original)).toEqual(received.map(request => `https://registry.smithery.ai${request.path}`));
    expect(mapped.every(row => row.pid === result.pid && row.generation === 1)).toBe(true);
    expect(JSON.stringify(mapped)).not.toContain("owned-nonsecret-registry-key");
    expect(objectValue(mapped[2]?.init, "actual request init").proxy).toEqual({ origin: "http://127.0.0.1:9", credentials: false });
  }, 15_000);

  it("denies noncanonical registry destinations, methods and routes before any HTTP request", async () => {
    let received = 0;
    const result = await runBoundary(`
      const rejected = [];
      for (const [url, method] of [
        ["https://registry.smithery.ai/servers", "POST"],
        ["https://registry.smithery.ai/servers/slug-only", "GET"],
        ["https://registry.smithery.ai/servers/a/b/extra", "GET"],
        ["https://registry.smithery.ai/servers?q=parity&redirect=https://example.invalid", "GET"],
        ["https://registry.smithery.ai.evil.invalid/servers", "GET"],
      ]) {
        try { await fetch(url, { method }); throw new Error("Unexpected remote request success"); }
        catch (error) { if (!String(error).includes("Unexpected OMP parity network destination")) throw error; rejected.push(url); }
      }
      console.log("OMP_BOUNDARY_RESULT=" + JSON.stringify({ rejected }));
    `, () => { received++; return new Response("Unexpected network request", { status: 409 }); });
    expect(received).toBe(0);
    expect(result.value.rejected).toHaveLength(5);
    expect(result.rows.filter(row => typeof row.error === "string")).toHaveLength(5);
    expect(result.rows.filter(row => row.type === "fixture-transport" && row.phase === "request")).toHaveLength(0);
  }, 15_000);

  it("observes canonical MCP OAuth listeners and revokes their real callback origins on Stop", async () => {
    const tokenRequests: URLSearchParams[] = [];
    let actualRedirect = "";
    const result = await runBoundary(`
      const { MCPOAuthFlow } = await import(sourceRoot + "/packages/coding-agent/src/mcp/oauth-flow.ts");
      const authorization = Promise.withResolvers();
      let unrelated;
      const flow = new MCPOAuthFlow({ authorizationUrl: process.env.BOUNDARY_SERVICE_ORIGIN + "/authorize", tokenUrl: process.env.BOUNDARY_SERVICE_ORIGIN + "/token", clientId: "owned-fixture-client", callbackPort: 0 }, { signal: AbortSignal.timeout(5000), onAuth(info) {
        unrelated = Bun.serve({ hostname: "127.0.0.1", port: 0, reusePort: false, fetch: () => new Response("Unrelated listener") });
        authorization.resolve(info);
      } });
      const loggingIn = flow.login();
      const info = await authorization.promise;
      const response = await fetch(info.url, { redirect: "manual" });
      if (response.status !== 302) throw new Error("Owned authorization service did not redirect");
      const callback = response.headers.get("location");
      if (!callback) throw new Error("Missing actual authorization callback");
      const returned = await fetch(callback);
      if (!returned.ok) throw new Error("Actual source callback listener rejected authorization");
      await returned.text();
      const credentials = await loggingIn;
      let retired = false;
      try { await fetch(callback); } catch (error) { retired = String(error).includes("Unexpected OMP parity network destination"); }
      let unrelatedDenied = false;
      if (!unrelated) throw new Error("Actual auth callback did not create its unrelated listener");
      try { await fetch(unrelated.url); } catch (error) { unrelatedDenied = String(error).includes("Unexpected OMP parity network destination"); }
      await unrelated.stop(true);
      console.log("OMP_BOUNDARY_RESULT=" + JSON.stringify({ origin: new URL(callback).origin, retired, unrelatedDenied, receivedToken: credentials.access === "owned-fixture-access" }));
    `, async request => {
      const url = new URL(request.url);
      if (url.pathname === "/authorize") {
        actualRedirect = url.searchParams.get("redirect_uri") ?? "";
        const callback = new URL(actualRedirect);
        callback.searchParams.set("state", url.searchParams.get("state") ?? "");
        callback.searchParams.set("code", "owned-fixture-code");
        return Response.redirect(callback.href, 302);
      }
      if (url.pathname === "/token") {
        const body = new URLSearchParams(await request.text());
        tokenRequests.push(body);
        if (body.get("code") !== "owned-fixture-code" || !body.get("code_verifier") || body.get("redirect_uri") !== actualRedirect) return new Response("Invalid actual OAuth token request", { status: 400 });
        return Response.json({ access_token: "owned-fixture-access", refresh_token: "owned-fixture-refresh", token_type: "Bearer", expires_in: 3600 });
      }
      return new Response("Unexpected OAuth route", { status: 409 });
    }, { oauth: true });
    expect(result.value.retired).toBe(true);
    expect(result.value.unrelatedDenied).toBe(true);
    expect(result.value.receivedToken).toBe(true);
    expect(tokenRequests).toHaveLength(1);
    expect(tokenRequests[0]?.get("grant_type")).toBe("authorization_code");
    const registered = result.rows.filter(row => row.type === "owned-listener-registered" && row.origin === result.value.origin);
    expect(registered).toHaveLength(1);
    expect(registered[0]?.pid).toBe(result.pid);
    expect(registered[0]?.generation).toBe(1);
    expect(registered[0]?.source).toBe("packages/ai/src/registry/oauth/callback-server.ts");
    expect(result.rows.some(row => row.type === "owned-listener-closed" && row.listenerId === registered[0]?.listenerId)).toBe(true);
  }, 15_000);

  it("runs canonical Codex OAuth state and PKCE over the exact token boundary without tracing credentials", async () => {
    let challenge = "";
    let state = "";
    let tokenRequests = 0;
    const encode = (value: unknown): string => Buffer.from(JSON.stringify(value)).toString("base64url");
    const access = `${encode({ alg: "none" })}.${encode({ exp: Math.floor(Date.now() / 1000) + 3600, "https://api.openai.com/auth": { chatgpt_account_id: "owned-parity-account" }, "https://api.openai.com/profile": { email: "parity@example.invalid" } })}.owned-nonsecret-signature`;
    const result = await runBoundary(`
      const { AuthStorage } = await import(sourceRoot + "/packages/ai/src/auth-storage.ts");
      const auth = await AuthStorage.create(join(home, ".parity/auth.db"));
      let actualAuthorization = "";
      try {
        const identity = await auth.oauth.login("openai-codex", {
          signal: AbortSignal.timeout(5000),
          // The canonical flow races manual code entry against the callback; the real callback must win that race.
          onPrompt: () => new Promise<string>(() => {}),
          onAuth: async info => {
            const original = new URL(info.url);
            actualAuthorization = original.origin + original.pathname;
            if (actualAuthorization !== "https://auth.openai.com/oauth/authorize") throw new Error("Codex changed its canonical authorization URL");
            const authorization = await fetch(process.env.BOUNDARY_SERVICE_ORIGIN + "/authorize" + original.search, { redirect: "manual" });
            const callback = authorization.headers.get("location");
            if (authorization.status !== 302 || !callback) throw new Error("Owned OAuth service did not redirect");
            const completed = await fetch(callback);
            if (!completed.ok) throw new Error("Actual source callback rejected state/code");
          },
        });
        console.log("OMP_BOUNDARY_RESULT=" + JSON.stringify({ identity, actualAuthorization }));
      } finally { auth.close(); }
    `, async request => {
      const url = new URL(request.url);
      if (url.pathname === "/authorize") {
        expect(url.searchParams.get("client_id")).toBe("app_EMoamEEZ73f0CkXaXp7hrann");
        expect(url.searchParams.get("redirect_uri")).toBe("http://localhost:1455/auth/callback");
        expect(url.searchParams.get("code_challenge_method")).toBe("S256");
        challenge = url.searchParams.get("code_challenge") ?? "";
        state = url.searchParams.get("state") ?? "";
        expect(challenge.length).toBeGreaterThanOrEqual(43);
        expect(state.length).toBeGreaterThan(10);
        const callback = new URL(url.searchParams.get("redirect_uri") ?? "");
        callback.searchParams.set("code", "owned-authorization-code");
        callback.searchParams.set("state", state);
        return new Response(null, { status: 302, headers: { location: callback.href } });
      }
      expect(url.pathname).toBe("/oauth/token");
      expect(request.method).toBe("POST");
      const form = new URLSearchParams(await request.text());
      expect(form.get("grant_type")).toBe("authorization_code");
      expect(form.get("code")).toBe("owned-authorization-code");
      expect(form.get("client_id")).toBe("app_EMoamEEZ73f0CkXaXp7hrann");
      expect(form.get("redirect_uri")).toBe("http://localhost:1455/auth/callback");
      expect(createHash("sha256").update(form.get("code_verifier") ?? "").digest("base64url")).toBe(challenge);
      tokenRequests++;
      return Response.json({ access_token: access, refresh_token: "owned-nonsecret-refresh-token", expires_in: 3600, token_type: "Bearer" });
    }, { codexAuth: true });
    expect(tokenRequests).toBe(1);
    expect(result.value.identity).toEqual({ type: "oauth", accountId: "owned-parity-account", orgId: "owned-parity-account", email: "parity@example.invalid" });
    expect(result.value.actualAuthorization).toBe("https://auth.openai.com/oauth/authorize");
    expect(result.rows.filter(row => row.type === "codex-callback-preflight").map(row => row.occupied)).toEqual([false]);
    const requestRows = result.rows.filter(row => row.type === "fixture-transport" && row.service === "codex-oauth" && row.phase === "request");
    expect(requestRows).toHaveLength(1);
    expect(requestRows[0]?.codeVerifierPresent).toBe(true);
    expect(requestRows[0]?.authorizationCodePresent).toBe(true);
    expect(result.rows.filter(row => row.type === "owned-listener-observed" && row.port === 1455).length).toBeGreaterThan(0);
    const trace = JSON.stringify(result.rows);
    expect(trace).not.toContain(access);
    expect(trace).not.toContain("owned-authorization-code");
    expect(trace).not.toContain("owned-nonsecret-refresh-token");
    expect(requestRows[0]?.raw).toBeUndefined();
  }, 15_000);

  it("refuses an occupied fixed Codex callback port without contacting or stopping its listener", async () => {
    let contacted = 0;
    const occupied = Bun.serve({ hostname: "127.0.0.1", port: 1455, reusePort: false, fetch: () => { contacted++; return new Response("Owned occupied-port sentinel"); } });
    try {
      const result = await runBoundary(`
        const { AuthStorage } = await import(sourceRoot + "/packages/ai/src/auth-storage.ts");
        const auth = await AuthStorage.create(join(home, ".parity/auth.db"));
        let opened = false;
        let failed = false;
        try {
          await auth.oauth.login("openai-codex", { signal: AbortSignal.timeout(5000), onAuth: () => { opened = true; }, onPrompt: async () => { throw new Error("Unexpected prompt"); } });
        } catch { failed = true; }
        finally { auth.close(); }
        console.log("OMP_BOUNDARY_RESULT=" + JSON.stringify({ opened, failed }));
      `, () => { throw new Error("Occupied-port OAuth must not request an owned service"); }, { codexAuth: true });
      expect(result.value).toEqual({ opened: false, failed: true });
      expect(contacted).toBe(0);
      expect(occupied.port).toBe(1455);
      expect(result.rows.filter(row => row.type === "codex-callback-preflight").map(row => row.occupied)).toEqual([true]);
      expect(result.rows.filter(row => row.type === "owned-listener-observed" && row.port === 1455)).toHaveLength(0);
      expect(result.rows.filter(row => row.type === "fixture-transport")).toHaveLength(0);
    } finally { await occupied.stop(true); }
  }, 15_000);

  it("maps only the four canonical Codex quota routes and preserves real headers and request bytes", async () => {
    const received: Array<{ path: string; method: string; authorization: string | null; account: string | null; body: string }> = [];
    const result = await runBoundary(`
      const paths = [
        ["/backend-api/wham/usage?owned=unchanged", "GET"],
        ["/backend-api/wham/rate-limit-reset-credits", "GET"],
        ["/backend-api/accounts/verified_access", "GET"],
        ["/backend-api/wham/rate-limit-reset-credits/consume", "POST"],
      ];
      for (const [path, method] of paths) {
        const response = await fetch("https://chatgpt.com" + path, { method, headers: { authorization: "Bearer owned-sensitive-access", "chatgpt-account-id": "owned-account", "content-type": "application/json" },
          ...(method === "POST" ? { body: JSON.stringify({ credit_id: "owned-credit", redeem_request_id: "owned-request", account_id: "owned-account" }) } : {}) });
        if (!response.ok) throw new Error("Mapped quota failed");
        await response.json();
      }
      console.log("OMP_BOUNDARY_RESULT=" + JSON.stringify({ completed: paths.length }));
    `, async request => {
      const url = new URL(request.url);
      received.push({ path: url.pathname + url.search, method: request.method, authorization: request.headers.get("authorization"), account: request.headers.get("chatgpt-account-id"), body: await request.text() });
      return Response.json({ owned: true, access_token: "owned-sensitive-response-token" });
    }, { codexAuth: true });
    expect(result.value.completed).toBe(4);
    expect(received.map(request => request.path)).toEqual(["/backend-api/wham/usage?owned=unchanged", "/backend-api/wham/rate-limit-reset-credits", "/backend-api/accounts/verified_access", "/backend-api/wham/rate-limit-reset-credits/consume"]);
    expect(received.map(request => request.method)).toEqual(["GET", "GET", "GET", "POST"]);
    expect(received.every(request => request.authorization === "Bearer owned-sensitive-access" && request.account === "owned-account")).toBe(true);
    expect(received[3]?.body).toBe('{"credit_id":"owned-credit","redeem_request_id":"owned-request","account_id":"owned-account"}');
    const trace = JSON.stringify(result.rows);
    expect(trace).not.toContain("owned-sensitive-access");
    expect(trace).not.toContain("owned-sensitive-response-token");
    expect(result.rows.filter(row => row.type === "fixture-transport" && row.service === "codex-auth" && row.phase === "request")).toHaveLength(4);
  }, 15_000);

  it("denies every other auth host, method and route before any owned service request", async () => {
    let received = 0;
    const result = await runBoundary(`
      const rejected = [];
      for (const [url, method] of [
        ["https://auth.openai.com/oauth/token", "GET"],
        ["https://auth.openai.com/oauth/token?extra=denied", "POST"],
        ["https://auth.openai.com/oauth/authorize", "POST"],
        ["https://chatgpt.com/backend-api/wham/usage", "POST"],
        ["https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume", "GET"],
        ["https://chatgpt.com/backend-api/accounts/verified_access/extra", "GET"],
        ["https://chat.openai.com/backend-api/wham/usage", "GET"],
        ["https://chatgpt.com.evil.invalid/backend-api/wham/usage", "GET"],
      ]) {
        try { await fetch(url, { method }); throw new Error("Unexpected auth request success"); }
        catch (error) { if (!String(error).includes("Unexpected OMP parity network destination")) throw error; rejected.push(url); }
      }
      console.log("OMP_BOUNDARY_RESULT=" + JSON.stringify({ rejected }));
    `, () => { received++; return new Response("Unexpected auth network", { status: 409 }); }, { codexAuth: true });
    expect(received).toBe(0);
    expect(result.value.rejected).toHaveLength(8);
    expect(result.rows.filter(row => row.type === "fixture-transport")).toHaveLength(0);
  }, 15_000);

  it("runs canonical empty Codex model discovery only at its source-pinned version and query shape", async () => {
    const received: Array<{ path: string; authorization: string | null; account: string | null; version: string | null; beta: string | null; originator: string | null; marker: string | null }> = [];
    const result = await runBoundary(`
      const { CODEX_CLIENT_VERSION, OPENAI_HEADER_VALUES } = await import(sourceRoot + "/packages/catalog/src/wire/codex.ts");
      const { fetchCodexModels } = await import(sourceRoot + "/packages/catalog/src/discovery/codex.ts");
      const discovered = await fetchCodexModels({ accessToken: "owned-sensitive-discovery-token", accountId: "owned-discovery-account", headers: { "x-owned-marker": "preserved" } });
      const canonical = "https://chatgpt.com/backend-api/codex/models";
      const query = "?client_version=" + encodeURIComponent(CODEX_CLIENT_VERSION);
      const rejected = [];
      for (const [url, method] of [
        [canonical, "GET"],
        [canonical + "?client_version=wrong", "GET"],
        [canonical + query + "&extra=denied", "GET"],
        [canonical + query + "&client_version=" + encodeURIComponent(CODEX_CLIENT_VERSION), "GET"],
        [canonical + query, "POST"],
        ["https://chatgpt.com/backend-api/models" + query, "GET"],
        ["https://chat.openai.com/backend-api/codex/models" + query, "GET"],
      ]) {
        try { await fetch(url, { method }); throw new Error("Unexpected discovery request success"); }
        catch (error) { if (!String(error).includes("Unexpected OMP parity network destination")) throw error; rejected.push(url); }
      }
      console.log("OMP_BOUNDARY_RESULT=" + JSON.stringify({ discovered, clientVersion: CODEX_CLIENT_VERSION,
        beta: OPENAI_HEADER_VALUES.BETA_RESPONSES, originator: OPENAI_HEADER_VALUES.ORIGINATOR_CODEX, rejected }));
    `, request => {
      const url = new URL(request.url);
      received.push({ path: url.pathname + url.search, authorization: request.headers.get("authorization"), account: request.headers.get("chatgpt-account-id"),
        version: request.headers.get("version"), beta: request.headers.get("openai-beta"), originator: request.headers.get("originator"), marker: request.headers.get("x-owned-marker") });
      return Response.json({ models: [] });
    }, { codexAuth: true });
    expect(result.value.discovered).toEqual({ models: [] });
    expect(result.value.rejected).toHaveLength(7);
    const { clientVersion, beta, originator } = result.value;
    if (typeof clientVersion !== "string" || typeof beta !== "string" || typeof originator !== "string") throw new Error("Boundary result lacks the Codex discovery identity");
    expect(received).toEqual([{ path: `/backend-api/codex/models?client_version=${clientVersion}`,
      authorization: "Bearer owned-sensitive-discovery-token", account: "owned-discovery-account", version: clientVersion,
      beta, originator, marker: "preserved" }]);
    expect(result.rows.filter(row => row.type === "fixture-transport" && row.service === "codex-auth" && row.phase === "request")).toHaveLength(1);
    expect(JSON.stringify(result.rows)).not.toContain("owned-sensitive-discovery-token");
  }, 15_000);
});
