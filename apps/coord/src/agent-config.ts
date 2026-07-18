// agent-config — coord-side default-agent (launch-button) config. Stores the
// selected agent id and an optional custom command in app_settings so every
// device shares one choice (same KV contract as transcription). The server keeps
// raw strings and does NOT validate `selected` against the catalog — the SPA owns
// the catalog and resolves unknown ids to claude client-side.
// Callers: connect/router.ts agentConfig* handlers.

import type { Kysely } from "kysely";
import type { DB } from "./db/schema.ts";

const K = { selected: "agent.selected", custom: "agent.custom_command", autoLaunch: "agent.auto_launch" } as const;
const DEFAULT_SELECTED = "claude";

export interface AgentConfigShape {
  selected: string;
  customCommand: string;
  autoLaunch: boolean;
}

async function readAll(db: Kysely<DB>): Promise<Record<string, string>> {
  const rows = await db
    .selectFrom("app_settings")
    .select(["key", "value"])
    .where("key", "like", "agent.%")
    .execute();
  return Object.fromEntries(rows.map((r) => [r.key, r.value]));
}

async function put(db: Kysely<DB>, key: string, value: string): Promise<void> {
  const now = Date.now();
  await db
    .insertInto("app_settings")
    .values({ key, value, updated_at_ms: now })
    .onConflict((oc) => oc.column("key").doUpdateSet({ value, updated_at_ms: now }))
    .execute();
}
export async function getAgentConfig(db: Kysely<DB>): Promise<AgentConfigShape> {
  const s = await readAll(db);
  return { selected: s[K.selected] || DEFAULT_SELECTED, customCommand: s[K.custom] ?? "", autoLaunch: s[K.autoLaunch] === "true" };
}

export async function setAgentConfig(
  db: Kysely<DB>,
  input: { selected: string; customCommand: string; autoLaunch: boolean },
): Promise<AgentConfigShape> {
  await put(db, K.selected, input.selected.trim() || DEFAULT_SELECTED);
  await put(db, K.custom, input.customCommand); // may be ""
  await put(db, K.autoLaunch, String(input.autoLaunch));
  return getAgentConfig(db);
}
