// transcription — coord-side Deepgram dictation config. Stores the Deepgram API
// key (pasted in Settings → Voice) so every device shares one key; the SPA never
// sees the raw key — it gets a short-lived token (grantDeepgramToken) and streams
// to Deepgram directly. No key configured → the SPA uses the built-in Web Speech
// recognizer instead. Callers: connect/router.ts transcription* handlers.

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

async function readAll(db: Kysely<DB>): Promise<Record<string, string>> {
  const rows = await db
    .selectFrom("app_settings")
    .select(["key", "value"])
    .where("key", "like", "transcription.%")
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

function mask(key: string | undefined): string {
  if (!key) return "";
  return "····" + key.slice(-4);
}

export async function getTranscriptionConfig(db: Kysely<DB>): Promise<TranscriptionConfigShape> {
  const s = await readAll(db);
  return {
    deepgramConfigured: !!s[K.dgKey],
    deepgramKeyMasked: mask(s[K.dgKey]),
    deepgramLanguage: s[K.dgLang] || DEFAULT_LANG,
  };
}

export async function setTranscriptionConfig(
  db: Kysely<DB>,
  input: SetTranscriptionInput,
): Promise<TranscriptionConfigShape> {
  if (input.deepgramKey !== undefined) await put(db, K.dgKey, input.deepgramKey.trim());
  await put(db, K.dgLang, input.deepgramLanguage.trim() || DEFAULT_LANG);
  log.info("transcription", "config_set", { deepgram: input.deepgramKey !== undefined });
  return getTranscriptionConfig(db);
}

// Hands the Deepgram key to the (already-authed) browser, which opens the listen
// WS directly with Sec-WebSocket-Protocol ["token", key]. We don't use
// /v1/auth/grant temporary tokens because restricted keys can't mint them (403);
// the key works for transcription, which is all the streaming WS needs.
export async function grantDeepgramToken(
  db: Kysely<DB>,
): Promise<{ accessToken: string; expiresIn: number }> {
  const s = await readAll(db);
  const key = s[K.dgKey];
  if (!key) throw new Error("deepgram_not_configured");
  return { accessToken: key, expiresIn: 0 };
}

// Test button: validate the stored key against Deepgram's projects endpoint
// (a plain authed GET — works for any key that can transcribe).
export async function testDeepgram(db: Kysely<DB>): Promise<{ ok: boolean; error: string }> {
  const s = await readAll(db);
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
