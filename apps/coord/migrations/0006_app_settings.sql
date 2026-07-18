-- app_settings: a small key→value store for coord-held configuration that
-- isn't event-sourced. First use: voice-transcription secrets + options
-- (Deepgram + OpenRouter API keys, OpenRouter model, cleanup toggle). Keys
-- stay server-side; the SPA only ever sees masked values + short-lived tokens.

CREATE TABLE IF NOT EXISTS app_settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
