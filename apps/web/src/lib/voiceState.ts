// Voice capture needs one shared owner so only the active session keeps the microphone.
// Terminal composers call this module when recording starts, stops, or changes sessions.
// Coordinator configuration is cached here to keep capture policy reactive across the UI.

import { createSignal } from "solid-js";
import type { SessionId } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";

export type VoiceOwner = { sessionId: SessionId; token: number };
const [activeVoiceOwner, setActiveVoiceOwner] = createSignal<VoiceOwner | null>(null);
export { activeVoiceOwner, setActiveVoiceOwner };

export type TranscriptionConfig = { deepgramConfigured: boolean; deepgramLanguage: string };
const [transcriptionConfig, setTranscriptionConfig] = createSignal<TranscriptionConfig | null>(null);
export { transcriptionConfig };

let configFetch: Promise<unknown> | null = null;

/** Cache coordinator voice configuration across composer remounts. */
export function ensureTranscriptionConfig(): void {
  if (configFetch) return;
  let request: Promise<void>;
  request = coordClient
    .transcriptionGetConfig({})
    .then((config) => {
      setTranscriptionConfig({
        deepgramConfigured: config.deepgramConfigured,
        deepgramLanguage: config.deepgramLanguage,
      });
    })
    .catch(() => {
      if (configFetch === request) configFetch = null;
    });
  configFetch = request;
}
