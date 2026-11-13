import { Database } from "bun:sqlite";
import { join } from "node:path";
import type { SessionCredential } from "@omp-source/packages/ai/src/auth/affinity";
import type { AuthWorkflowCapture, AuthWorkflowProviderCapture } from "./builtin-auth-workflow-requirements";
import type { CaptureState } from "./builtin-capture-state";
import { objectValue, ownedPath, readOwnedText } from "./builtin-capture-state";
import { probe } from "./probe";

async function rawTrace(state: CaptureState, environment: string): Promise<Record<string, unknown>[]> {
  const path = process.env[environment];
  if (!path) throw new Error(`Auth observation requires actual ${environment}`);
  return (await readOwnedText(state, path)).split("\n").filter(Boolean).map(line => objectValue(JSON.parse(line) as unknown, `Actual ${environment} row`));
}

async function captureOAuthState(state: CaptureState): Promise<AuthWorkflowCapture["oauth"]> {
  const generation = objectValue(JSON.parse(await readOwnedText(state, join(state.home, ".parity", "process-generation.json"))) as unknown, "Actual auth process generation");
  if (generation.pid !== process.pid || typeof generation.generation !== "number" || !Number.isSafeInteger(generation.generation) || generation.generation < 1) throw new Error("Auth observation lacks current owned process generation");
  const network = (await rawTrace(state, "OMP_PARITY_NETWORK_TRACE")).filter(row => row.pid === process.pid && row.generation === generation.generation);
  const callbackListenerOpen = network.some((row, index) => row.type === "owned-listener-registered" && row.source === "packages/ai/src/registry/oauth/callback-server.ts" && typeof row.origin === "string" && new URL(row.origin).port === "1455" && typeof row.listenerId === "string" && !network.slice(index + 1).some(closed => closed.type === "owned-listener-closed" && closed.listenerId === row.listenerId));
  let authorize: URL | undefined;
  for (const row of await rawTrace(state, "OMP_PARITY_HOST_TRACE")) {
    if (row.channel !== "builtin.host-open-url" || typeof row.value !== "string") continue;
    const url = new URL(row.value);
    if (url.origin !== "https://auth.openai.com" || url.pathname !== "/oauth/authorize") continue;
    if (typeof row.pid !== "number" || typeof row.generation !== "number") throw new Error("Actual OAuth authorize URL lacks process-generation provenance");
    if (row.pid === process.pid && row.generation === generation.generation) authorize = url;
  }
  return { callbackListenerOpen, authorizeStatePresent: Boolean(authorize?.searchParams.get("state")), authorizePkcePresent: Boolean(authorize?.searchParams.get("code_challenge")) && authorize?.searchParams.get("code_challenge_method") === "S256" };
}

export async function captureAuth(state: CaptureState): Promise<AuthWorkflowCapture> {
  const session = probe.requireSession();
  const storage = session.modelRegistry.authStorage;
  const utilities = await state.context.load<typeof import("@omp-source/packages/utils/src/index")>("packages/utils/src/index.ts");
  const affinity = await state.context.load<typeof import("@omp-source/packages/ai/src/auth/affinity")>("packages/ai/src/auth/affinity.ts");
  const database = new Database(await ownedPath(state, utilities.getAgentDbPath()), { readonly: true, strict: true });
  const providers: Record<string, AuthWorkflowProviderCapture> = {};
  try {
    const savedRows = database.query<{ id: number; provider: string; credential_type: string; account_id: string | null; key_present: number }, []>("SELECT id, provider, credential_type, json_extract(data, '$.accountId') AS account_id, CASE WHEN length(json_extract(data, '$.key')) > 0 THEN 1 ELSE 0 END AS key_present FROM auth_credentials WHERE disabled_cause IS NULL ORDER BY id ASC").all();
    const names = new Set(["ollama", "openai-codex", ...storage.credentials.list().map(row => row.provider), ...savedRows.map(row => row.provider)]);
    for (const provider of names) {
      const current = storage.credentials.list(provider);
      const persisted = savedRows.filter(row => row.provider === provider);
      const summaries = storage.oauth.accounts(provider, session.sessionId);
      const activeAccountId = summaries.find(account => account.active)?.accountId ?? null;
      const cached = database.query<{ value: string }, [string]>("SELECT value FROM cache WHERE key = ? AND expires_at > unixepoch()").get(`${affinity.SESSION_STICKY_CACHE_PREFIX}${provider}:${session.sessionId}`);
      const sticky = cached ? JSON.parse(cached.value) as SessionCredential : undefined;
      const activeSaved = persisted.find(row => row.credential_type === "oauth" && row.account_id === activeAccountId);
      const accounts: AuthWorkflowProviderCapture["accounts"] = {};
      const accountIds = new Set(["parity-auth-a", "parity-auth-b", "parity-auth-login", ...current.flatMap(row => row.credential.type === "oauth" && row.credential.accountId ? [row.credential.accountId] : []), ...persisted.flatMap(row => row.credential_type === "oauth" && row.account_id ? [row.account_id] : [])]);
      for (const accountId of accountIds) {
        const row = current.find(row => row.credential.type === "oauth" && row.credential.accountId === accountId);
        const oauth = row?.credential.type === "oauth" ? row.credential : undefined;
        accounts[accountId] = { present: row !== undefined, persisted: persisted.some(row => row.credential_type === "oauth" && row.account_id === accountId), active: summaries.some(account => account.accountId === accountId && account.active), accessPresent: Boolean(oauth?.access), refreshPresent: Boolean(oauth?.refresh) };
      }
      providers[provider] = {
        credentialCount: current.length, persistedCredentialCount: persisted.length,
        apiKeyPresent: current.some(row => row.credential.type === "api_key" && row.credential.key.length > 0),
        persistedApiKeyPresent: persisted.some(row => row.credential_type === "api_key" && row.key_present === 1),
        activeAccountId, stickyPresent: sticky !== undefined, stickyExplicit: sticky?.explicit === true,
        stickyMatchesSavedRow: sticky !== undefined && activeAccountId !== null && activeSaved !== undefined && sticky.credentialId === activeSaved.id, accounts,
      };
    }
  } finally { database.close(); }
  return { providers, usageReportCount: session.cachedUsageReports?.length ?? 0, oauth: await captureOAuthState(state) };
}
