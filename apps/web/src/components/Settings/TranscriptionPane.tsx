// TranscriptionPane — voice dictation settings (Settings → Voice).
// Deepgram only: paste an API key (stored on the coordinator), Test it, or
// remove it. Settings readback is masked. For direct Deepgram dictation the
// configured key is returned only to an authenticated browser device,
// which connects to Deepgram directly.
// Callers: SettingsRoot.tsx. Depends on: coordClient transcription* RPCs +
// lib/deepgramKey (cache dropped on save).

import { createEffect, createResource, createSignal, Show } from "solid-js";
import { coordClient } from "../../connect.ts";
import { invalidateDeepgramKey } from "../../lib/deepgramKey.ts";
import { Card, Button, Icon, StatusDot, SwitchRow, TextField, Select } from "./md/primitives.tsx";

// Deepgram nova-3 languages, verbatim from the official support matrix:
// https://developers.deepgram.com/docs/models-languages-overview — every code
// incl. regional variants. Free text was a footgun: a typo / unsupported code
// made Deepgram reject the socket. `multi` does live code-switching across
// en/es/fr/de/hi/ru/pt/ja/it/nl; `__auto__` auto-detects. English first:
// default + the ONLY mode where keyterm biasing applies (deepgramDictation
// gates keyterms on English).
const LANGUAGES: { value: string; label: string }[] = [
  { value: "en", label: "English" },
  { value: "en-US", label: "English (US)" },
  { value: "en-GB", label: "English (UK)" },
  { value: "en-AU", label: "English (Australia)" },
  { value: "en-IN", label: "English (India)" },
  { value: "en-NZ", label: "English (New Zealand)" },
  { value: "ar", label: "Arabic" },
  { value: "ar-EG", label: "Arabic (Egypt)" },
  { value: "ar-SA", label: "Arabic (Saudi Arabia)" },
  { value: "ar-AE", label: "Arabic (UAE)" },
  { value: "ar-QA", label: "Arabic (Qatar)" },
  { value: "ar-KW", label: "Arabic (Kuwait)" },
  { value: "ar-SY", label: "Arabic (Syria)" },
  { value: "ar-LB", label: "Arabic (Lebanon)" },
  { value: "ar-PS", label: "Arabic (Palestine)" },
  { value: "ar-JO", label: "Arabic (Jordan)" },
  { value: "ar-SD", label: "Arabic (Sudan)" },
  { value: "ar-TD", label: "Arabic (Chad)" },
  { value: "ar-MA", label: "Arabic (Morocco)" },
  { value: "ar-DZ", label: "Arabic (Algeria)" },
  { value: "ar-TN", label: "Arabic (Tunisia)" },
  { value: "ar-IQ", label: "Arabic (Iraq)" },
  { value: "ar-IR", label: "Arabic (Iran)" },
  { value: "be", label: "Belarusian" },
  { value: "bn", label: "Bengali" },
  { value: "bs", label: "Bosnian" },
  { value: "bg", label: "Bulgarian" },
  { value: "ca", label: "Catalan" },
  { value: "zh", label: "Chinese (Mandarin, Simplified)" },
  { value: "zh-CN", label: "Chinese (Mandarin, Simplified) [zh-CN]" },
  { value: "zh-Hans", label: "Chinese (Mandarin, Simplified) [zh-Hans]" },
  { value: "zh-TW", label: "Chinese (Mandarin, Traditional)" },
  { value: "zh-Hant", label: "Chinese (Mandarin, Traditional) [zh-Hant]" },
  { value: "zh-HK", label: "Chinese (Cantonese, Traditional)" },
  { value: "hr", label: "Croatian" },
  { value: "cs", label: "Czech" },
  { value: "da", label: "Danish" },
  { value: "da-DK", label: "Danish (Denmark)" },
  { value: "nl", label: "Dutch" },
  { value: "nl-BE", label: "Flemish" },
  { value: "et", label: "Estonian" },
  { value: "fi", label: "Finnish" },
  { value: "fr", label: "French" },
  { value: "fr-CA", label: "French (Canada)" },
  { value: "de", label: "German" },
  { value: "de-CH", label: "German (Switzerland)" },
  { value: "el", label: "Greek" },
  { value: "gu", label: "Gujarati" },
  { value: "gu-IN", label: "Gujarati (India)" },
  { value: "he", label: "Hebrew" },
  { value: "hi", label: "Hindi" },
  { value: "hu", label: "Hungarian" },
  { value: "id", label: "Indonesian" },
  { value: "it", label: "Italian" },
  { value: "ja", label: "Japanese" },
  { value: "kn", label: "Kannada" },
  { value: "ko", label: "Korean" },
  { value: "ko-KR", label: "Korean (Korea)" },
  { value: "lv", label: "Latvian" },
  { value: "lt", label: "Lithuanian" },
  { value: "mk", label: "Macedonian" },
  { value: "ms", label: "Malay" },
  { value: "mr", label: "Marathi" },
  { value: "no", label: "Norwegian" },
  { value: "fa", label: "Persian" },
  { value: "pl", label: "Polish" },
  { value: "pt", label: "Portuguese" },
  { value: "pt-BR", label: "Portuguese (Brazil)" },
  { value: "pt-PT", label: "Portuguese (Portugal)" },
  { value: "ro", label: "Romanian" },
  { value: "ru", label: "Russian" },
  { value: "sr", label: "Serbian" },
  { value: "sk", label: "Slovak" },
  { value: "sl", label: "Slovenian" },
  { value: "es", label: "Spanish" },
  { value: "es-419", label: "Spanish (Latin America)" },
  { value: "sv", label: "Swedish" },
  { value: "sv-SE", label: "Swedish (Sweden)" },
  { value: "tl", label: "Tagalog" },
  { value: "ta", label: "Tamil" },
  { value: "te", label: "Telugu" },
  { value: "th", label: "Thai" },
  { value: "th-TH", label: "Thai (Thailand)" },
  { value: "tr", label: "Turkish" },
  { value: "uk", label: "Ukrainian" },
  { value: "ur", label: "Urdu" },
  { value: "vi", label: "Vietnamese" },
  { value: "multi", label: "Multilingual (code-switching)" },
  { value: "__auto__", label: "Auto-detect" },
];
import { keytermBiasing, setKeytermBiasing } from "../../lib/keytermBiasingPref.ts";




export function TranscriptionPane() {
  const [config, { refetch }] = createResource(() => coordClient.transcriptionGetConfig({}));

  const [dgKey, setDgKey] = createSignal("");
  const [lang, setLang] = createSignal("en");
  const [saving, setSaving] = createSignal(false);
  const [saveErr, setSaveErr] = createSignal("");
  const [saved, setSaved] = createSignal(false);
  const [testing, setTesting] = createSignal(false);
  const [testResult, setTestResult] = createSignal<{ ok: boolean; msg: string } | null>(null);

  // Seed language from coord once config loads.
  createEffect(() => {
    const c = config();
    if (!c) return;
    setLang(c.deepgramLanguage || "en");
  });

  async function save() {
    setSaving(true); setSaveErr(""); setSaved(false); setTestResult(null);
    try {
      await coordClient.transcriptionSetConfig({
        deepgramKey: dgKey().trim() ? dgKey().trim() : undefined,
        deepgramLanguage: lang().trim(),
      });
      invalidateDeepgramKey(); // the mic's page-session cache now holds a stale key
      setDgKey("");
      setSaved(true);
      await refetch();
    } catch (e) {
      setSaveErr(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  async function testKey() {
    setTesting(true); setTestResult(null);
    try {
      const r = await coordClient.transcriptionTest({});
      setTestResult(r.ok ? { ok: true, msg: "Deepgram key works ✓" } : { ok: false, msg: r.error || "Test failed" });
    } catch (e) {
      setTestResult({ ok: false, msg: e instanceof Error ? e.message : String(e) });
    } finally {
      setTesting(false);
    }
  }

  async function clearKey() {
    setSaving(true); setSaveErr(""); setSaved(false); setTestResult(null);
    try {
      await coordClient.transcriptionSetConfig({ deepgramKey: "", deepgramLanguage: lang().trim() });
      invalidateDeepgramKey();
      await refetch();
    } catch (e) {
      setSaveErr(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  const deepgramOn = () => !!config()?.deepgramConfigured;

  return (
    <div data-testid="settings-transcription-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      {/* ── Status hero ── */}
      <Card
        data-testid="transcription-status"
        style={{ display: "flex", "align-items": "center", gap: "var(--md-space-4)" }}
      >
        <div
          style={{
            width: "56px", height: "56px", "border-radius": "16px", flex: "0 0 auto",
            display: "grid", "place-items": "center",
            background: "color-mix(in srgb, var(--md-sys-color-primary) 16%, transparent)",
            color: "var(--md-sys-color-primary)",
          }}
        >
          <Icon name="mic" filled size="lg" />
        </div>
        <div style={{ flex: 1, "min-width": 0 }}>
          <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
            <StatusDot status={deepgramOn() ? "ok" : "warn"} />
            <span class="md-title-m" style={{ color: "var(--md-sys-color-on-surface)" }}>
              {deepgramOn() ? "Deepgram" : "Browser speech"}
            </span>
          </div>
          <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)", "margin-top": "2px" }}>
            {deepgramOn()
              ? "Live Deepgram transcription for your authenticated owner devices."
              : "Browser's built-in speech — add a Deepgram key below to upgrade."}
          </div>
        </div>
      </Card>

      {/* ── Dictation behavior (client pref) ── */}
      <Card title="Dictation">
        <SwitchRow
          headline="Bias dictation to on-screen terms"
          support="Feeds the terminal's visible text, your recent commands, and learned project jargon to Deepgram as keyterms — so names like Kysely, tailnet, or coordFactory transcribe correctly. Turn off to A/B against plain transcription. This device only; next recording."
          checked={keytermBiasing()}
          onChange={setKeytermBiasing}
          testId="keyterm-biasing-toggle"
        />
      </Card>

      {/* ── Deepgram ── */}
      <Card
        title="API key"
        supporting="Stored on the coordinator. For direct Deepgram dictation, Roost returns the configured key to this authenticated admin browser, which connects to Deepgram directly. Leave it empty to use the browser's built-in speech."
        trailing={
          <span style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
            <StatusDot status={deepgramOn() ? "ok" : "offline"} hollow={!deepgramOn()} />
            <span class="md-label-m" style={{ color: deepgramOn() ? "var(--status-ok)" : "var(--md-sys-color-on-surface-variant)" }}>
              {deepgramOn() ? "Configured" : "Not set"}
            </span>
          </span>
        }
      >
        <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-4)" }}>
          <TextField
            label="Deepgram API key"
            description={deepgramOn() ? "A key is saved. Type a new one to replace it." : "Paste a key, or leave blank to use the browser's built-in speech."}
            type="password"
            testId="transcription-deepgram-key"
            placeholder={config()?.deepgramKeyMasked || "paste API key"}
            value={dgKey()}
            onInput={setDgKey}
          />

          <Select
            label="Language"
            description="Keyterm biasing (project jargon → accurate transcription) applies in English only."
            testId="transcription-language"
            value={lang()}
            onChange={setLang}
            options={LANGUAGES}
          />

          <div style={{ display: "flex", "align-items": "center", "flex-wrap": "wrap", gap: "var(--md-space-3)" }}>
            <Button variant="default" data-testid="transcription-save" onClick={() => void save()} disabled={saving()}>{saving() ? "Saving…" : "Save"}</Button>
            <Button variant="secondary" icon="check_circle" data-testid="transcription-test" onClick={() => void testKey()} disabled={testing() || !deepgramOn()}>{testing() ? "Testing…" : "Test"}</Button>
            <Show when={deepgramOn()}>
              <Button variant="ghost" data-testid="transcription-clear" onClick={() => void clearKey()} disabled={saving()}>
                Remove key
              </Button>
            </Show>
            <Show when={saved()}>
              <span class="md-body-s" data-testid="transcription-saved" style={{ color: "var(--md-sys-color-primary)" }}>Saved</span>
            </Show>
          </div>

          <Show when={testResult()}>
            <span
              class="md-body-s"
              data-testid="transcription-test-result"
              style={{ color: testResult()!.ok ? "var(--status-ok)" : "var(--md-sys-color-error)" }}
            >
              {testResult()!.msg}
            </span>
          </Show>
          <Show when={saveErr()}>
            <span class="md-body-s" style={{ color: "var(--md-sys-color-error)" }}>{saveErr()}</span>
          </Show>
        </div>
      </Card>
    </div>
  );
}
