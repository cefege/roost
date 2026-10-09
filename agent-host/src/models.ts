import { createModels, type MutableModels } from "@earendil-works/pi-ai";
import { builtinProviders } from "@earendil-works/pi-ai/providers/all";
import type { DatabaseSync } from "node:sqlite";
import type { SqliteCredentialStore } from "./host-db.ts";

export const thinkingLevels = ["off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;
export interface ModelsService { models: MutableModels; catalog(): Promise<Record<string, unknown>>; setApiKey(provider: string, key: string): Promise<void>; }

export function createModelService(credentials: SqliteCredentialStore, db: DatabaseSync): ModelsService {
  const models = createModels({ credentials });
  for (const provider of builtinProviders()) models.setProvider(provider);
  async function catalog(): Promise<Record<string, unknown>> {
    const available = await models.getAvailable();
    const credentialRows = await credentials.list();
    const credentialsByProvider = new Map(credentialRows.map(row => [row.providerId, row.type]));
    const providers = await Promise.all(models.getProviders().map(async provider => {
      const stored = credentialsByProvider.get(provider.id);
      const auth = stored ? undefined : await models.getAuth(provider.id).catch(() => undefined);
      return {
        id: provider.id,
        name: provider.name ?? provider.id,
        configured: stored !== undefined || auth !== undefined,
        credential: stored === "oauth" ? "oauth" : stored ? "api_key" : auth ? "env" : null,
        supports_oauth: provider.auth.oauth !== undefined,
      };
    }));
    const defaultModel = db.prepare("SELECT value FROM settings WHERE key='default_model'").get() as { value: string } | undefined;
    return {
      models: available.map(model => ({ provider: model.provider, model_id: model.id, name: model.name, reasoning: model.reasoning, available: true })),
      providers,
      thinking_levels: [...thinkingLevels],
      default_model: defaultModel ? JSON.parse(defaultModel.value) : null,
    };
  }
  return { models, catalog, async setApiKey(provider, key) { await credentials.modify(provider, async () => ({ type: "api_key", key })); } };
}
