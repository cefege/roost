// Owns canonical models.yml configuration and one-time owned OAuth seeding.
// seed-cli invokes these before source sessions and after the real provider starts.
// No ModelRegistry, auth implementation or provider driver is substituted here.
import { chmod, mkdir, realpath, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import type { OAuthCredential } from "@omp-source/packages/ai/src/auth/types";
import type { ModelsConfig } from "@omp-source/packages/coding-agent/src/config/models-config-schema";
import type * as SourceAuthStorage from "@omp-source/packages/coding-agent/src/session/auth-storage";
import type * as SourceDirectories from "@omp-source/packages/utils/src/dirs";
import type * as SourceYamlConfig from "@omp-source/packages/utils/src/yaml-config";
import { authWorkflowHooks } from "./builtin-auth-workflow-requirements";
import { securityWorkflowHooks } from "./builtin-security-workflow-requirements";
import { assertOwned, type ProfileManifest } from "./profile-manifest";
import type { SourceLoader } from "./source-load";

export interface ProfileModelSelection {
  provider: "openai" | "openai-codex";
  defaultId: string;
}

export function profileModelSelection(profile: ProfileManifest): ProfileModelSelection {
  const requirement = profile.scenario.requirements;
  return { provider: requirement?.models.provider ?? "openai", defaultId: requirement?.models.defaultId ?? "parity-coder" };
}

export async function seedProfileAuth(profile: ProfileManifest, load: SourceLoader): Promise<void> {
  const authHook = authWorkflowHooks[profile.scenario.id];
  const securityHook = securityWorkflowHooks[profile.scenario.id];
  if (!authHook && !securityHook) return;
  if (authHook && securityHook) throw new Error("A profile cannot have two OAuth seed owners");
  const marker = join(profile.home, ".parity/auth-seeded.json");
  if (await Bun.file(marker).exists()) throw new Error("Owned auth seeding is one-time only; restart must reuse the actual database");
  const { getAgentDbPath, getAgentDir } = await load<typeof SourceDirectories>("packages/utils/src/dirs.ts");
  const agentDir = getAgentDir();
  assertOwned(profile.home, agentDir);
  await mkdir(agentDir, { recursive: true });
  const dbPath = getAgentDbPath(agentDir);
  assertOwned(profile.home, dbPath);
  const { AuthStorage } = await load<typeof SourceAuthStorage>("packages/coding-agent/src/session/auth-storage.ts");
  const storage = await AuthStorage.create(dbPath);
  try {
    if (storage.credentials.list().length !== 0) throw new Error("Fresh owned auth seed requires an empty real credential store");
    const credentials: OAuthCredential[] = [];
    if (authHook?.oauthSeed) {
      for (const seed of authHook.oauthSeed.credentials) {
        credentials.push({ type: "oauth", access: seed.access, refresh: seed.refresh, accountId: seed.accountId, email: seed.email, expires: Date.now() + seed.expiresAfterMilliseconds });
      }
    }
    if (securityHook) {
      const seed = securityHook.oauthSeed;
      credentials.push({ type: "oauth", access: seed.access, refresh: seed.refresh, accountId: seed.accountId, expires: Date.now() + seed.expiresAfterMilliseconds });
    }
    if (credentials.length > 0) await storage.credentials.set("openai-codex", credentials);
    const rows = storage.credentials.list();
    if (rows.length !== credentials.length) throw new Error("Canonical AuthStorage did not persist the declared owned OAuth seed");
    const identities = rows.map(row => {
      if (row.credential.type !== "oauth" || !Number.isInteger(row.id) || row.id <= 0) throw new Error("Owned OAuth seed returned an invalid durable credential row");
      return { credentialId: row.id, provider: row.provider, type: row.credential.type, accountId: row.credential.accountId ?? null };
    });
    await writeFile(marker, `${JSON.stringify({ version: 1, scenarioId: profile.scenario.id, dbPath, rows: identities })}\n`, { mode: 0o600, flag: "wx" });
  } finally { storage.close(); }
}

export async function writeProfileModels(profile: ProfileManifest, load: SourceLoader): Promise<void> {
  const baseUrl = profile.environment.OMP_PARITY_PROVIDER_URL;
  if (!baseUrl || process.env.OMP_PARITY_PROVIDER_URL !== baseUrl) throw new Error("Models phase requires the final owned provider URL in the manifest and subprocess environment");
  const url = new URL(baseUrl);
  const allowed = new Set((profile.environment.OMP_PARITY_ALLOWED_ORIGINS ?? "").split(",").filter(Boolean));
  if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" || !url.port || url.username || url.password || url.search || url.hash || !allowed.has(url.origin)) throw new Error("Canonical parity model configuration accepts only the started owned loopback provider");
  const { getAgentDir } = await load<typeof SourceDirectories>("packages/utils/src/dirs.ts");
  const agentDir = getAgentDir();
  assertOwned(profile.home, agentDir);
  if (resolve(process.env.PI_CODING_AGENT_DIR ?? "") !== resolve(agentDir)) throw new Error("Models configuration escaped the owned source agent directory");
  await mkdir(agentDir, { recursive: true });
  const path = join(agentDir, "models.yml");
  if (await Bun.file(path).exists()) throw new Error("Models phase is one-time profile creation, never restart-time configuration replacement");
  const requirement = profile.scenario.requirements;
  const selection = profileModelSelection(profile);
  const ids = requirement?.models.ids ?? ["parity-coder", "parity-small"];
  if (!ids.includes(selection.defaultId)) throw new Error("Declared default model is absent from canonical model configuration");
  const providers: NonNullable<ModelsConfig["providers"]> = {
    [selection.provider]: {
      baseUrl, api: requirement?.models.api ?? "openai-completions",
      ...(selection.provider === "openai-codex" ? { auth: "oauth" as const } : { apiKey: "parity-local-unused", authHeader: true }),
      models: ids.map(id => ({ id, name: id, reasoning: requirement?.models.reasoning ?? true,
        input: requirement?.models.imageInput === false ? ["text"] : ["text", "image"], supportsTools: true,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32768, maxTokens: 4096,
      })),
    },
  };
  if (profile.scenario.id === "builtin-auth-api-key-login-logout-restart") {
    providers.ollama = { baseUrl, api: "openai-completions", auth: "oauth", models: [] };
  }
  const config: ModelsConfig = { providers };
  const { stringifyYamlConfig } = await load<typeof SourceYamlConfig>("packages/utils/src/yaml-config.ts");
  await writeFile(path, stringifyYamlConfig(config), { mode: 0o600, flag: "wx" });
  await chmod(path, 0o600);
  assertOwned(await realpath(profile.home), await realpath(path));
  await writeFile(join(profile.home, ".parity/model-configuration.json"), `${JSON.stringify({ version: 1, scenarioId: profile.scenario.id, path, provider: selection.provider, api: requirement?.models.api ?? "openai-completions", modelIds: ids, defaultId: selection.defaultId, baseUrl, configApiKeyPresent: selection.provider !== "openai-codex" })}\n`, { mode: 0o600, flag: "wx" });
}
