// transcription — coord-side Deepgram dictation config. Stores the Deepgram API
// key pasted in Settings → Voice. Direct Deepgram mode returns that configured
// key only to an authenticated dashboard-admin browser (the sole owner in a
// managed coordinator), which streams to Deepgram directly. No key configured
// means the browser uses its built-in Web Speech recognizer instead.

import type { Kysely } from "kysely";
import type { DB } from "./db/schema.ts";
import { log } from "@roost/shared/log";

const K = {
  dgKey: "transcription.deepgram_key",
  dgLang: "transcription.deepgram_language",
} as const;

const DEFAULT_LANG = "en";

export interface TranscriptionConfigShape {
  deepgramConfigured: boolean;
  deepgramKeyMasked: string;
  deepgramLanguage: string;
}

export interface SetTranscriptionInput {
  deepgramKey?: string; // undefined = leave unchanged, "" = clear
  deepgramLanguage: string;
}

async function readAll(db: Kysely<DB>, dashboardId: string): Promise<Record<string, string>> {
  const rows = await db
    .selectFrom("app_settings")
    .select(["key", "value"])
    .where("dashboard_id", "=", dashboardId)
    .where("key", "like", "transcription.%")
    .execute();
  return Object.fromEntries(rows.map((r) => [r.key, r.value]));
}

async function put(db: Kysely<DB>, dashboardId: string, key: string, value: string): Promise<void> {
  const now = Date.now();
  await db
    .insertInto("app_settings")
    .values({ dashboard_id: dashboardId, key, value, updated_at_ms: now })
    .onConflict((oc) => oc.columns(["dashboard_id", "key"]).doUpdateSet({ value, updated_at_ms: now }))
    .execute();
}

function mask(key: string | undefined): string {
  if (!key) return "";
  return "····" + key.slice(-4);
}

export async function getTranscriptionConfig(
  db: Kysely<DB>,
  dashboardId: string,
): Promise<TranscriptionConfigShape> {
  const s = await readAll(db, dashboardId);
  return {
    deepgramConfigured: !!s[K.dgKey],
    deepgramKeyMasked: mask(s[K.dgKey]),
    deepgramLanguage: s[K.dgLang] || DEFAULT_LANG,
  };
}

export async function setTranscriptionConfig(
  db: Kysely<DB>,
  dashboardId: string,
  input: SetTranscriptionInput,
): Promise<TranscriptionConfigShape> {
  if (input.deepgramKey !== undefined) await put(db, dashboardId, K.dgKey, input.deepgramKey.trim());
  await put(db, dashboardId, K.dgLang, input.deepgramLanguage.trim() || DEFAULT_LANG);
  log.info("transcription", "config_set", { deepgram: input.deepgramKey !== undefined });
  return getTranscriptionConfig(db, dashboardId);
}

// Hands the configured Deepgram key to the already-authenticated dashboard-admin
// browser, which opens the listen WS directly with Sec-WebSocket-Protocol
// ["token", key]. This is not a temporary grant: restricted keys cannot mint
// /v1/auth/grant tokens (403), so expiresIn is explicitly zero.
export async function grantDeepgramToken(
  db: Kysely<DB>,
  dashboardId: string,
): Promise<{ accessToken: string; expiresIn: number }> {
  const s = await readAll(db, dashboardId);
  const key = s[K.dgKey];
  if (!key) throw new Error("deepgram_not_configured");
  return { accessToken: key, expiresIn: 0 };
}

// Test button: validate the stored key against Deepgram's projects endpoint
// (a plain authed GET — works for any key that can transcribe).
export async function testDeepgram(
  db: Kysely<DB>,
  dashboardId: string,
): Promise<{ ok: boolean; error: string }> {
  const s = await readAll(db, dashboardId);
  const key = s[K.dgKey];
  if (!key) return { ok: false, error: "No Deepgram key saved" };
  try {
    const res = await fetch("https://api.deepgram.com/v1/projects", {
      headers: { Authorization: `Token ${key}` },
    });
    if (res.ok) return { ok: true, error: "" };
    if (res.status === 401 || res.status === 403) return { ok: false, error: `Key rejected by Deepgram (${res.status})` };
    return { ok: false, error: `Deepgram returned ${res.status}` };
  } catch (err) {
    return { ok: false, error: String(err instanceof Error ? err.message : err) };
  }
}
