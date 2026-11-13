import { createHash, randomUUID } from "node:crypto";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { CodexResetConsumeCode } from "@omp-source/packages/ai/src/usage/openai-codex-reset";
import type { AuthWorkflowHook, AuthWorkflowServiceAccount, AuthWorkflowServiceCapture, AuthWorkflowServiceInput } from "./builtin-auth-workflow-requirements";
import type { BuiltinSeedContext } from "./builtin-seed";
import type { ServiceTrace } from "./fixture-service-http";
import type { SecurityWorkflowHook } from "./builtin-security-workflow-requirements";
import { deniedDestinationPrefix, isEscapingRequest } from "./network-classification";

type ServiceAccountInput = Omit<AuthWorkflowServiceAccount, "email">;
type ServiceInput = Omit<AuthWorkflowServiceInput, "accounts"> & { accounts: ServiceAccountInput[] };

interface AccountState {
  accountId: string;
  access: string;
  credits: Array<{ id: string; title: string; status: "available" | "redeemed" }>;
  consumeCount: number;
  consumedCreditId: string | null;
  lastConsumeStatus: number | null;
  lastConsumeCode: string | null;
}

export interface RunningBuiltinAuthService {
  url: string;
  environment: Record<string, string>;
  snapshot(): AuthWorkflowServiceCapture;
  assertConsumed(): void;
  dispose(): Promise<void>;
}

export async function startBuiltinAuthService(hook: AuthWorkflowHook | SecurityWorkflowHook, context: BuiltinSeedContext, trace: ServiceTrace): Promise<RunningBuiltinAuthService> {
  const security = "bindingsFileEnvironment" in hook ? hook : undefined;
  const auth = "bindingsFileEnvironment" in hook ? undefined : hook;
  const input: ServiceInput | undefined = security ? {
    evidenceClass: "external-service-fixture", consumeOutcome: "nothing_to_reset", oauth: null,
    accounts: [{ accountId: security.oauthSeed.accountId, access: security.oauthSeed.access, credits: [] }],
    expectedConsumeCounts: { [security.oauthSeed.accountId]: 0 },
  } : auth?.service;
  if (!input || input.evidenceClass !== "external-service-fixture") throw new Error("Auth transport requires an explicit external-service fixture hook");
  if (input.oauth && !auth?.oauthTransport) throw new Error("Auth exchange requires an explicit real callback workflow");
  const production = await context.load<typeof import("@omp-source/packages/catalog/src/wire/codex")>("packages/catalog/src/wire/codex.ts");
  if (!production.CODEX_CLIENT_VERSION) throw new Error("Canonical Codex discovery version is unavailable");
  const accounts = new Map<string, AccountState>();
  for (const account of input.accounts) {
    if (accounts.has(account.accountId) || [...accounts.values()].some(existing => existing.access === account.access) || !account.access) throw new Error("Auth fixture account identities must be distinct and nonempty");
    if (new Set(account.credits.map(credit => credit.id)).size !== account.credits.length) throw new Error("Auth fixture credit identities must be distinct");
    accounts.set(account.accountId, accountState(account));
  }
  for (const account of input.accounts) {
    const expected = input.expectedConsumeCounts[account.accountId];
    if (!Number.isSafeInteger(expected) || expected < 0) throw new Error("Auth fixture requires an explicit consume count for each declared account");
  }
  if (Object.keys(input.expectedConsumeCounts).some(accountId => !accounts.has(accountId))) throw new Error("Auth fixture consume expectation references an undeclared account");
  const tracePath = join(context.home, ".parity", "service-trace.jsonl");
  const networkPath = join(context.home, ".parity", "network.jsonl");
  const hostPath = join(context.home, ".parity", "host.jsonl");
  let tokenExchangeCount = 0;
  let tokenStatus: number | null = null;
  let pkceVerified = false;
  let rejectedRequests = 0;
  let disposed = false;
  const redemptions = new Map<string, { accountId: string; creditId: string; code: CodexResetConsumeCode }>();
  const snapshot = (): AuthWorkflowServiceCapture => ({
    tokenExchangeCount, tokenStatus, pkceVerified,
    remoteRequests: readRows(networkPath).filter(row => typeof row === "object" && row !== null && "error" in row && typeof row.error === "string" && row.error.startsWith(deniedDestinationPrefix) && isEscapingRequest(row)).length,
    rejectedRequests,
    accounts: Object.fromEntries([...accounts.values()].map(account => [account.accountId, {
      availableCount: account.credits.filter(credit => credit.status === "available").length,
      consumeCount: account.consumeCount, consumedCreditId: account.consumedCreditId,
      lastConsumeStatus: account.lastConsumeStatus, lastConsumeCode: account.lastConsumeCode,
    }])),
  });
  const publish = () => appendFileSync(tracePath, `${JSON.stringify({ channel: "builtin.auth-http", value: snapshot() })}\n`);
  const reject = (status: number, reason: string) => {
    rejectedRequests++;
    trace("auth-http", { direction: "rejected", status, reason });
    return Response.json({ error: reason }, { status });
  };
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const url = new URL(request.url);
    trace("auth-http", { transport: "http", direction: "request", method: request.method, path: url.pathname, hasAuthorization: request.headers.has("authorization") });
    try {
      const discovery = request.method === "GET" && url.pathname === "/backend-api/codex/models";
      if (discovery ? [...url.searchParams].length !== 1 || url.searchParams.get("client_version") !== production.CODEX_CLIENT_VERSION : Boolean(url.search)) return reject(400, "unexpected_query");
      if (url.pathname === "/oauth/token" && request.method === "POST" && input.oauth) {
        tokenExchangeCount++;
        const form = new URLSearchParams(await request.text());
        const permitted: Record<string, true> = { grant_type: true, client_id: true, code: true, code_verifier: true, redirect_uri: true };
        const actual = actualAuthorization(hostPath);
        const verifier = form.get("code_verifier") ?? "";
        pkceVerified = Boolean(actual && verifier && createHash("sha256").update(verifier).digest("base64url") === actual.searchParams.get("code_challenge"));
        if ([...form.keys()].some(key => !Object.hasOwn(permitted, key) || form.getAll(key).length !== 1) || form.get("grant_type") !== "authorization_code" || form.get("client_id") !== "app_EMoamEEZ73f0CkXaXp7hrann" || form.get("code") !== input.oauth.expectedCode || !actual || form.get("redirect_uri") !== actual.searchParams.get("redirect_uri") || actual.searchParams.get("code_challenge_method") !== "S256" || !actual.searchParams.get("state") || !pkceVerified) {
          tokenStatus = 400;
          return reject(400, "invalid_exchange_binding");
        }
        tokenStatus = input.oauth.status;
        if (input.oauth.status === 400) return Response.json({ error: "invalid_grant", error_description: input.oauth.error }, { status: 400 });
        const now = Math.floor(Date.now() / 1000);
        const access = `${Buffer.from(JSON.stringify({ alg: "none", typ: "JWT" })).toString("base64url")}.${Buffer.from(JSON.stringify({ ...input.oauth.accessJwtClaims, iat: now, exp: now + input.oauth.expiresInSeconds, jti: randomUUID() })).toString("base64url")}.`;
        accounts.set(input.oauth.accountId, accountState({ accountId: input.oauth.accountId, access, credits: [] }));
        return Response.json({ access_token: access, refresh_token: input.oauth.refresh, expires_in: input.oauth.expiresInSeconds, token_type: "Bearer" }, { headers: { "Cache-Control": "no-store" } });
      }
      const accountId = request.headers.get("ChatGPT-Account-Id") ?? "";
      const account = accounts.get(accountId);
      if (!account || request.headers.get("Authorization") !== `Bearer ${account.access}`) return reject(401, "invalid_account_identity");
      if (request.method === "GET") {
        if (request.headers.has("Content-Length") && request.headers.get("Content-Length") !== "0") return reject(400, "unexpected_read_body");
        if (discovery) return Response.json({ models: [] });
        if (url.pathname === "/backend-api/accounts/verified_access") return Response.json({});
        const availableCount = account.credits.filter(credit => credit.status === "available").length;
        if (url.pathname === "/backend-api/wham/rate-limit-reset-credits") return Response.json({ available_count: availableCount, credits: account.credits.map(credit => ({ ...credit, reset_type: "codex_rate_limits" })) });
        if (url.pathname === "/backend-api/wham/usage") return Response.json({ plan_type: "plus", rate_limit: { allowed: true, limit_reached: false, primary_window: { used_percent: 25, limit_window_seconds: 18000, reset_after_seconds: 3600 }, secondary_window: { used_percent: 40, limit_window_seconds: 604800, reset_after_seconds: 86400 } }, rate_limit_reset_credits: { available_count: availableCount } });
      }
      if (request.method === "POST" && url.pathname === "/backend-api/wham/rate-limit-reset-credits/consume") {
        const body: unknown = await request.json();
        if (typeof body !== "object" || body === null || Array.isArray(body) || Object.keys(body).some(key => !["credit_id", "redeem_request_id", "account_id"].includes(key)) || !("account_id" in body) || body.account_id !== accountId || !("credit_id" in body) || typeof body.credit_id !== "string" || !("redeem_request_id" in body) || typeof body.redeem_request_id !== "string" || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(body.redeem_request_id)) return reject(400, "invalid_consume_body");
        const credit = account.credits.find(candidate => candidate.id === body.credit_id);
        if (!credit) return reject(400, "unknown_credit");
        const prior = redemptions.get(body.redeem_request_id);
        if (prior && (prior.accountId !== accountId || prior.creditId !== credit.id)) return reject(409, "reused_redemption_identity");
        const code: CodexResetConsumeCode = prior?.code ?? (credit.status === "redeemed" ? "already_redeemed" : input.consumeOutcome);
        account.consumeCount++;
        account.lastConsumeStatus = 200;
        account.lastConsumeCode = code;
        if (!prior && code === "reset") {
          credit.status = "redeemed";
          account.consumedCreditId = credit.id;
        }
        redemptions.set(body.redeem_request_id, { accountId, creditId: credit.id, code });
        return Response.json({ code });
      }
      return reject(403, "unexpected_route_or_method");
    } catch {
      return reject(400, "invalid_request_payload");
    } finally { publish(); }
  } });
  const url = `http://127.0.0.1:${server.port}`;
  try {
    trace("auth-http", { direction: "listening", origin: url });
    publish();
  } catch (error) {
    await server.stop(true);
    throw error;
  }
  const assertConsumed = () => {
    const state = snapshot();
    const expectedExchanges = input.oauth && auth?.oauthTransport?.delivery !== "cancel-without-callback" ? 1 : 0;
    if (state.remoteRequests || state.rejectedRequests) throw new Error("Auth fixture observed rejected or remote requests");
    if (state.tokenExchangeCount !== expectedExchanges || (expectedExchanges && (!state.pkceVerified || state.tokenStatus !== input.oauth?.status))) throw new Error("Auth fixture did not observe the declared real token exchange");
    for (const [accountId, expected] of Object.entries(input.expectedConsumeCounts)) {
      if (accounts.get(accountId)?.consumeCount !== expected) throw new Error(`Auth fixture did not observe the declared reset request count for ${accountId}`);
    }
  };
  return {
    url,
    environment: { OMP_PARITY_CODEX_AUTH_ORIGIN: url, OMP_PARITY_CODEX_OAUTH_TOKEN_URL: `${url}/oauth/token`, ...(auth?.oauthTransport ? { OMP_PARITY_MCP_OAUTH_CALLBACKS: "1" } : {}) },
    snapshot, assertConsumed,
    async dispose() {
      if (disposed) return;
      disposed = true;
      await server.stop(true);
      publish();
      assertConsumed();
    },
  };
}

function accountState(input: ServiceAccountInput): AccountState {
  return { accountId: input.accountId, access: input.access, credits: input.credits.map(credit => ({ ...credit })), consumeCount: 0, consumedCreditId: null, lastConsumeStatus: null, lastConsumeCode: null };
}

function actualAuthorization(path: string): URL | undefined {
  for (const row of readRows(path).toReversed()) {
    if (typeof row !== "object" || row === null || !("channel" in row) || row.channel !== "builtin.host-open-url" || !("value" in row) || typeof row.value !== "string" || !URL.canParse(row.value)) continue;
    const url = new URL(row.value);
    if (url.origin === "https://auth.openai.com" && url.pathname === "/oauth/authorize" && !url.username && !url.password && !url.hash) return url;
  }
}

function readRows(path: string): unknown[] {
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8").split("\n").filter(Boolean).map(line => JSON.parse(line) as unknown);
}

