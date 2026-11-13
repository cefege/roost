// Owns isolated auth seeds and the external HTTP boundary contract for builtin workflows.
// The parent integrates these hooks with real AuthStorage, native hosts and process restart.
// Observers project presence and identity only; source auth controllers are never replaced.
import type { BuiltinRequirements } from "./builtin-requirements";

export interface AuthWorkflowOAuthSeed {
  provider: "openai-codex";
  method: "AuthStorage.credentials.set";
  credentials: Array<{
    type: "oauth";
    access: string;
    refresh: string;
    accountId: string;
    email: string;
    expiresAfterMilliseconds: 3600000;
  }>;
}

export interface AuthWorkflowServiceAccount {
  accountId: string;
  email: string;
  access: string;
  credits: Array<{ id: string; status: "available"; title: string }>;
}

export interface AuthWorkflowServiceInput {
  evidenceClass: "external-service-fixture";
  accounts: AuthWorkflowServiceAccount[];
  consumeOutcome: "reset" | "nothing_to_reset";
  expectedConsumeCounts: Record<string, number>;
  oauth: null | {
    tokenPath: "/oauth/token";
    expectedCode: "parity-owned-auth-code";
    status: 200 | 400;
    error: "PARITY_AUTH_EXCHANGE_REFUSED";
    accountId: "parity-auth-login";
    email: "login@parity.invalid";
    refresh: "parity-owned-auth-refresh";
    expiresInSeconds: 3600;
    accessJwtClaims: {
      "https://api.openai.com/auth": { chatgpt_account_id: "parity-auth-login" };
      "https://api.openai.com/profile": { email: "login@parity.invalid" };
    };
  };
}

export interface AuthWorkflowHook {
  oauthSeed?: AuthWorkflowOAuthSeed;
  service?: AuthWorkflowServiceInput;
  modelRegistry: {
    provider: "openai" | "openai-codex";
    api: "openai-completions";
    baseUrl: "owned-loopback-completions";
    discovery: "controlled-real-discovery";
    runtimeApiKeyOverride: "absent-for-openai-codex";
  };
  quotaTransport?: {
    environment: "OMP_PARITY_CODEX_AUTH_ORIGIN";
    origin: "https://chatgpt.com";
    routes: ReadonlyArray<{ method: "GET" | "POST"; path: string }>;
  };
  oauthTransport?: {
    environment: "OMP_PARITY_CODEX_OAUTH_TOKEN_URL";
    original: "https://auth.openai.com/oauth/token";
    destinationPath: "/oauth/token";
    method: "POST";
    callbackObserverEnvironment: "OMP_PARITY_MCP_OAUTH_CALLBACKS";
    callbackSource: "packages/ai/src/registry/oauth/callback-server.ts";
    callbackPort: 1455;
    callbackPath: "/auth/callback";
    delivery: "owned-http-callback" | "actual-dialog-manual-paste" | "cancel-without-callback";
    deliveryBeforeCheckpoint?: number;
    redirectBinding: "PARITY_AUTH_REDIRECT_URL";
    manualInputGate: "canonical-codex-paste-code-provider";
  };
  capture: string;
}

export interface AuthWorkflowProviderCapture {
  credentialCount: number;
  persistedCredentialCount: number;
  apiKeyPresent: boolean;
  persistedApiKeyPresent: boolean;
  activeAccountId: string | null;
  stickyPresent: boolean;
  stickyExplicit: boolean;
  stickyMatchesSavedRow: boolean;
  accounts: Record<string, { present: boolean; persisted: boolean; active: boolean; accessPresent: boolean; refreshPresent: boolean }>;
}

export interface AuthWorkflowCapture {
  providers: Record<string, AuthWorkflowProviderCapture>;
  usageReportCount: number;
  oauth: { callbackListenerOpen: boolean; authorizeStatePresent: boolean; authorizePkcePresent: boolean };
}

export interface AuthWorkflowServiceCapture {
  tokenExchangeCount: number;
  tokenStatus: number | null;
  pkceVerified: boolean;
  remoteRequests: number;
  rejectedRequests: number;
  accounts: Record<string, { availableCount: number; consumeCount: number; consumedCreditId: string | null; lastConsumeStatus: number | null; lastConsumeCode: string | null }>;
}

export const authWorkflowCaptureSource = [
  "builtin.auth projects AuthWorkflowCapture from the actual session.modelRegistry.authStorage. credentials.list(provider) yields current rows; read independently reopened owned agent SQLite auth rows for persisted presence/count, never credentials.set/remove/login in an observer. No secret/token bytes or token digests in captures, screenshots, logs or service traces.",
  "providers.ollama and providers.openai-codex always exist. apiKeyPresent/persistedApiKeyPresent mean a stored nonempty api_key row, not env/runtime/config auth. accounts includes parity-auth-a, parity-auth-b and parity-auth-login with present/persisted/accessPresent/refreshPresent from actual respective OAuth rows (false when absent). active uses authStorage.oauth.accounts(provider,actual sessionId), activeAccountId is that list's active accountId or null; do not resolve a key to manufacture affinity.",
  "stickyPresent/stickyExplicit read the actual unexpired session:sticky:<provider>:<sessionId> store cache JSON; stickyMatchesSavedRow compares its credentialId to the actual persisted row of activeAccountId, false if no sticky/row. Read canonical owned SQLite schema/cache via real store API or readonly connection; never write or clear a pin from a capture. usageReportCount is session.cachedUsageReports?.length ?? 0.",
  "oauth.callbackListenerOpen is a live actual current-pid/generation registered canonical callback listener on1455; authorizeStatePresent/authorizePkcePresent parse the actual captured host-open authorize URL (not expected inputs), retaining only presence scalars. Callback registration/retirement is observed through the approved transport boundary, not a fake server.",
].join(" ");

export const authWorkflowServiceContract = [
  "Start a fresh strict owned loopback HTTP service per adapter; source AuthStorage/SQLite/OAuth code engine/reset clients, Session/SelectorController and native dialog stay real. Preserve service state across actual /restart. Configure actual custom provider model baseUrl to its owned completion root; never a global paid-provider key. All traffic outside explicitly allowed owned origins fails and increments remoteRequests.",
  "Source normalizeCodexBaseUrl deliberately ignores loopback overrides. Use ONLY approved fixed original https://chatgpt.com routes via OMP_PARITY_CODEX_AUTH_ORIGIN: GET /backend-api/wham/rate-limit-reset-credits, POST /backend-api/wham/rate-limit-reset-credits/consume, GET /backend-api/wham/usage and GET /backend-api/accounts/verified_access. Preserve original exact method/path, empty query, headers and body at transport boundary; no source normalization change or broad auth URL map. Match Authorization to the declared deliberately nonsecret owned access value and ChatGPT-Account-Id to its account; reject missing/mismatched identity, unexpected method/path/query/body. No generic fallback. verified_access returns200 {}. Usage returns200 {plan_type:'plus',rate_limit:{allowed:true,limit_reached:false,primary_window:{used_percent:25,limit_window_seconds:18000,reset_after_seconds:3600},secondary_window:{used_percent:40,limit_window_seconds:604800,reset_after_seconds:86400}},rate_limit_reset_credits:{available_count:<actual remaining count>}}.",
  "Actual builtin Codex discovery is not disabled by omitting custom discovery. Also allow ONLY GET https://chatgpt.com/backend-api/codex/models?client_version=<actual pinned catalog/src/wire/codex.ts CODEX_CLIENT_VERSION>, via the same fixed owned transport map, and return200 {models:[]}. Validate the one exact source client_version query, preserve actual auth headers. Do not map fallback /models, fake a discovery cache or alter policy. A successful token response admits only its newly issued owned JWT/account to these routes; input.accounts are not a synthetic login result.",
  "Credit list returns200 {available_count:<actual available count>,credits:[{id,status,title,reset_type:'codex_rate_limits'}]} from service state. Consume validates exact credit_id/account_id and a nonempty UUID redeem_request_id; HTTP200 {code:input.consumeOutcome}. reset marks only that credit redeemed, decrements only its account, and records id/status/count; nothing_to_reset changes no credits. Repeated invalid/redeemed credit never spends. Expected successful consume counts are asserted, so an unused service cannot pass. HTTP/service state is external-service-fixture evidence, not a claim about OpenAI's real credit system.",
  "OAuth endpoint is only POST /oauth/token mapped from exact approved https://auth.openai.com/oauth/token using OMP_PARITY_CODEX_OAUTH_TOKEN_URL. Keep source form grant_type/client_id/code/code_verifier/redirect_uri unchanged. Require authorization_code, expectedCode, canonical client ID and actual captured redirect_uri; verify base64url(SHA256(actual code_verifier)) equals actual host-open authorize code_challenge (S256). Return200 {access_token:<unsigned deliberately nonsecret local JWT using declared claims>,refresh_token:input.oauth.refresh,expires_in:3600}, or400 {error:'invalid_grant',error_description:'PARITY_AUTH_EXCHANGE_REFUSED'}. Never print/persist raw token/form bodies to parity traces.",
  "Export builtin.auth-http as AuthWorkflowServiceCapture from actual requests/state, not requirements. Token count/status/PKCE scalars and account available/consume/status/code fields come from actual boundary traffic. No token exchange on cancel. Stop/assertConsumed must reject unexpected requests, unfulfilled declared exchanges, wrong account consumption or leaked remote traffic; repeated legitimate usage GETs from source status polling may be served statefully without exact counts.",
].join(" ");

const credentials: AuthWorkflowOAuthSeed["credentials"] = [
  { type: "oauth", access: "parity-owned-auth-access-a", refresh: "parity-owned-auth-refresh-a", accountId: "parity-auth-a", email: "a@parity.invalid", expiresAfterMilliseconds: 3600000 },
  { type: "oauth", access: "parity-owned-auth-access-b", refresh: "parity-owned-auth-refresh-b", accountId: "parity-auth-b", email: "b@parity.invalid", expiresAfterMilliseconds: 3600000 },
];

const quotaTransport: NonNullable<AuthWorkflowHook["quotaTransport"]> = {
  environment: "OMP_PARITY_CODEX_AUTH_ORIGIN", origin: "https://chatgpt.com",
  routes: [
    { method: "GET", path: "/backend-api/wham/usage" },
    { method: "GET", path: "/backend-api/wham/rate-limit-reset-credits" },
    { method: "POST", path: "/backend-api/wham/rate-limit-reset-credits/consume" },
    { method: "GET", path: "/backend-api/accounts/verified_access" },
    { method: "GET", path: "/backend-api/codex/models" },
  ],
};

function seededHook(outcome: AuthWorkflowServiceInput["consumeOutcome"] = "reset", expectedConsumeCounts: Record<string, number> = { "parity-auth-a": 0, "parity-auth-b": 0 }): AuthWorkflowHook {
  return {
    oauthSeed: { provider: "openai-codex", method: "AuthStorage.credentials.set", credentials: structuredClone(credentials) },
    modelRegistry: { provider: "openai-codex", api: "openai-completions", baseUrl: "owned-loopback-completions", discovery: "controlled-real-discovery", runtimeApiKeyOverride: "absent-for-openai-codex" },
    quotaTransport,
    service: {
      evidenceClass: "external-service-fixture", consumeOutcome: outcome, expectedConsumeCounts, oauth: null,
      accounts: credentials.map((credential, index) => ({ accountId: credential.accountId, email: credential.email, access: credential.access, credits: Array.from({ length: index === 0 ? 2 : 1 }, (_, creditIndex) => ({ id: `parity-auth-${index === 0 ? "a" : "b"}-credit-${creditIndex + 1}`, status: "available", title: "Parity reset" })) })),
    },
    capture: authWorkflowCaptureSource,
  };
}

function oauthHook(delivery: NonNullable<AuthWorkflowHook["oauthTransport"]>["delivery"], status: 200 | 400 = 200): AuthWorkflowHook {
  return {
    modelRegistry: { provider: "openai", api: "openai-completions", baseUrl: "owned-loopback-completions", discovery: "controlled-real-discovery", runtimeApiKeyOverride: "absent-for-openai-codex" },
    quotaTransport,
    service: { evidenceClass: "external-service-fixture", accounts: [], consumeOutcome: "reset", expectedConsumeCounts: {}, oauth: {
      tokenPath: "/oauth/token", expectedCode: "parity-owned-auth-code", status, error: "PARITY_AUTH_EXCHANGE_REFUSED", accountId: "parity-auth-login", email: "login@parity.invalid", refresh: "parity-owned-auth-refresh", expiresInSeconds: 3600,
      accessJwtClaims: { "https://api.openai.com/auth": { chatgpt_account_id: "parity-auth-login" }, "https://api.openai.com/profile": { email: "login@parity.invalid" } },
    } },
    oauthTransport: { environment: "OMP_PARITY_CODEX_OAUTH_TOKEN_URL", original: "https://auth.openai.com/oauth/token", destinationPath: "/oauth/token", method: "POST", callbackObserverEnvironment: "OMP_PARITY_MCP_OAUTH_CALLBACKS", callbackSource: "packages/ai/src/registry/oauth/callback-server.ts", callbackPort: 1455, callbackPath: "/auth/callback", delivery, ...(delivery === "cancel-without-callback" ? {} : { deliveryBeforeCheckpoint: 1 }), redirectBinding: "PARITY_AUTH_REDIRECT_URL", manualInputGate: "canonical-codex-paste-code-provider" },
    capture: authWorkflowCaptureSource,
  };
}

export const authWorkflowHooks: Readonly<Record<string, AuthWorkflowHook>> = {
  "builtin-auth-api-key-login-logout-restart": { modelRegistry: { provider: "openai", api: "openai-completions", baseUrl: "owned-loopback-completions", discovery: "controlled-real-discovery", runtimeApiKeyOverride: "absent-for-openai-codex" }, capture: authWorkflowCaptureSource },
  "builtin-auth-oauth-callback-restart": oauthHook("owned-http-callback"),
  "builtin-auth-oauth-manual-restart": oauthHook("actual-dialog-manual-paste"),
  "builtin-auth-oauth-cancel": oauthHook("cancel-without-callback"),
  "builtin-auth-oauth-exchange-error": oauthHook("owned-http-callback", 400),
  "builtin-auth-account-pin-logout-restart": seededHook(),
  "builtin-auth-usage-reset-selection": seededHook("reset", { "parity-auth-a": 2, "parity-auth-b": 1 }),
  "builtin-auth-usage-reset-refused": seededHook("nothing_to_reset", { "parity-auth-a": 1, "parity-auth-b": 0 }),
};

export function applyAuthWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  for (const [id, hook] of Object.entries(authWorkflowHooks)) {
    const requirement = requirements[id];
    if (!requirement) throw new Error(`Missing explicit auth workflow ${id}`);
    requirement.session.seed = "transcript";
    requirement.session.userText = "PARITY_AUTH_SEED_QUESTION";
    requirement.session.assistantText = "PARITY_AUTH_SEED_ANSWER";
    requirement.models.provider = hook.modelRegistry.provider;
    requirement.models.api = hook.modelRegistry.api;
    requirement.models.ids = ["parity-coder", "parity-small"];
    requirement.models.roles = { default: `${hook.modelRegistry.provider}/parity-coder`, smol: `${hook.modelRegistry.provider}/parity-small` };
    requirement.models.turns = [];
    requirement.host.oauth = "empty-owned-store";
    requirement.lifecycle.superviseRealRestart = id.endsWith("restart") || id === "builtin-auth-usage-reset-selection";
    requirement.lifecycle.resumeSameSession = true;
    requirement.prerequisites.push(
      "REQUIRED authWorkflowHooks integration before initial command: fresh owned HOME/workspace/agent DB, scrub inherited provider keys/config and broker URLs; never open operator profiles. Real source TUI/nativeHTML editor and builtin dispatcher only. Missing seed, service, capture, manual prompt or checkpoint hook is a hard prerequisite failure, never skip/exclude/fake success.",
      "Create actual custom registry models using hook.modelRegistry provider/api and owned baseUrl, with controlled real builtin discovery through the approved exact transport boundary. No openai-codex runtime/config API-key override. If oauthSeed exists, set its real OAuth array once using AuthStorage.credentials.set before registry creation, setting expires=Date.now()+expiresAfterMilliseconds, and retain actual returned/listed durable row IDs. Never register an OAuth driver or replace authStorage/store/controller methods. Reopen existing DB on restart; do not reseed.",
      authWorkflowCaptureSource,
    );
    if (hook.service) requirement.prerequisites.push(authWorkflowServiceContract);
    if (hook.oauthTransport) requirement.prerequisites.push(
      "OAuth checkpoint hooks: capture actual host-open authorize URL but do not navigate it remotely or auto-deliver. Source canonical listener must itself bind1455 with reusePort:false; refuse occupied operator port, no kill/reuse/fallback. Require current-pid/generation listener registration before callback. Before zero-based checkpoint1 derive PARITY_AUTH_REDIRECT_URL from the actual authorize redirect_uri and state, append code=parity-owned-auth-code. Never fabricate state/PKCE or patch the source.",
      "owned-http-callback delivery issues one owned loopback GET of that derived URL before checkpoint1, observes real200 and awaits actual token exchange/controller settlement. actual-dialog-manual-paste binds the URL into checkpoint1 actual dialog input (terminal bracketed paste/browser fill+Continue), sends no HTTP callback. The actual pinned catalog policy has openai-codex pasteCode:true (oauth-code default); registry/build.ts/derived.ts put Codex in PASTE_CODE_LOGIN_PROVIDERS, so SelectorController2126-2132 naturally mounts manual input on ordinary TUI and nativeHTML with no hosted-flag injection. No OAuth driver DI, manualInput.submit injection or synthetic callback. cancel-without-callback never sends a callback/exchange. Await actual listener retirement before post-login/cancel/error observations; absence is observed, not manufactured.",
    );
    for (const capture of requirement.captures) {
      if (capture.channel === "builtin.auth") capture.source = hook.capture;
      if (capture.channel === "builtin.auth-http") capture.source = authWorkflowServiceContract;
    }
  }
}
