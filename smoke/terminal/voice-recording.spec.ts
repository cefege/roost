import { test, expect } from "./fixtures.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  readStoredComposerDraft,
} from "./terminal-helpers.ts";

// iOS can leave the activation-primer resume pending/rejected while
// getUserMedia switches the OS audio session. That first resume is not the
// liveness verdict: once the stream exists, the wired graph gets one
// authoritative resume and must send real PCM before Deepgram can transcribe.
test("mobile mic retries AudioContext resume after the input session opens", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    let inputSessionOpen = false;
    let liveContext: { state: string } | null = null;
    let liveNode: { port: { onmessage: ((e: { data: Float32Array }) => void) | null } } | null = null;

    class RetryContext {
      state = "suspended";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      resumeCalls = 0;
      constructor() { liveContext = this; }
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() {
        this.resumeCalls++;
        if (this.resumeCalls === 1) {
          return Promise.reject(new Error("the audio session did not respond in time — tap the mic again"));
        }
        if (!inputSessionOpen) return Promise.reject(new Error("the input session is not open"));
        this.state = "running";
        return Promise.resolve();
      }
      close() {
        this.state = "closed";
        return Promise.resolve();
      }
    }
    class LiveWorkletNode {
      port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
      constructor() { liveNode = this; }
      connect() {}
      disconnect() {}
    }

    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = RetryContext;
    win.webkitAudioContext = RetryContext;
    win.AudioWorkletNode = LiveWorkletNode;
    const liveFrame = new Float32Array(2048).fill(0.25);
    setInterval(() => {
      if (inputSessionOpen && liveContext?.state === "running") {
        liveNode?.port.onmessage?.({ data: liveFrame });
      }
    }, 20);
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          inputSessionOpen = true;
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });

    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      let sentPcm = false;
      let announcedPcm = false;
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null,
        onmessage: null as ((e: { data: string }) => void) | null,
        onerror: null, onclose: null,
        send(payload: unknown) {
          if (payload instanceof ArrayBuffer) {
            sentPcm ||= payload.byteLength > 0;
            if (sentPcm && !announcedPcm) {
              announcedPcm = true;
              setTimeout(() => {
                sock.onmessage?.({
                  data: JSON.stringify({
                    type: "Results", is_final: false,
                    channel: { alternatives: [{ transcript: "PCM ready" }] },
                  }),
                });
              }, 0);
            }
            return;
          }
          if (payload !== '{"type":"Finalize"}') return;
          setTimeout(() => {
            sock.onmessage?.({
              data: JSON.stringify({
                type: "Results", is_final: true, from_finalize: true,
                channel: {
                  alternatives: [{
                    transcript: sentPcm ? "post-stream resume delivered audio" : "",
                  }],
                },
              }),
            });
          }, 0);
        },
        close() { sock.readyState = 3; },
      };
      setTimeout(() => { sock.readyState = 1; sock.onopen?.(); }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });

  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("PCM ready");

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("post-stream resume delivered audio");
  await expect(mobileSmokePage.getByTestId("voice-caption")).toHaveCount(0);
});

// The report this whole class comes from: on a phone the FIRST recording works
// and every one after it is dead — the mic never leaves `listening`, or the stop
// wedges in `finalizing`, or the caption reads "Deepgram connection dropped".
// The graph here is genuinely LIVE (frames really flow) and the faked Deepgram
// socket really answers a Finalize, so nothing but the engine's own state can
// end a recording. The 4.5 s gap is longer than the mobile idle release
// (micIdle.releaseMs = 4 s on touch), so tap #2 pays a COLD device open exactly
// like the phone does instead of reusing a warm pipeline.
test("a second recording works exactly like the first", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    let liveNode: { port: { onmessage: ((e: { data: Float32Array }) => void) | null } } | null = null;
    class LiveWorkletNode {
      port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
      constructor() { liveNode = this; }
      connect() {}
      disconnect() {}
    }
    class LiveContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); }
      close() { return Promise.resolve(); }
    }
    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = LiveContext;
    win.webkitAudioContext = LiveContext;
    win.AudioWorkletNode = LiveWorkletNode;
    let emitInterim = false;
    win.__emitVoiceInterim = () => { emitInterim = true; };
    // 48 kHz × 40 ms = 1920 samples; 2048 clears the resampler's emit threshold,
    // so every tick delivers a real PCM chunk to the attached sink.
    setInterval(() => liveNode?.port.onmessage?.({ data: new Float32Array(2048).fill(0.3) }), 40);
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: () => Promise.resolve({ getTracks: () => [{ stop() {} }] }) },
    });
    // The hermetic stack has no internet, so the real Deepgram socket would drop
    // and claim the caption. Only that ONE host is faked; the SPA's own Sync WS
    // must keep using the real implementation. This one ANSWERS a Finalize, so a
    // stop delivers a transcript instead of timing out into an empty send.
    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null,
        onmessage: null as ((e: { data: string }) => void) | null,
        onerror: null, onclose: null,
        send(payload: unknown) {
          if (payload !== '{"type":"Finalize"}') return;
          setTimeout(() => {
            sock.onmessage?.({
              data: JSON.stringify({
                type: "Results", is_final: true, from_finalize: true,
                channel: { alternatives: [{ transcript: "hello from the mic" }] },
              }),
            });
          }, 0);
        },
        close() { sock.readyState = 3; },
      };
      setTimeout(() => {
        sock.readyState = 1;
        sock.onopen?.();
        if (emitInterim) {
          sock.onmessage?.({
            data: JSON.stringify({
              type: "Results", is_final: false,
              channel: { alternatives: [{ transcript: "still recording" }] },
            }),
          });
        }
      }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");

  const send = mobileSmokePage.getByTestId("chat-send");
  await expect(input).not.toBeFocused();
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic");
  await expect(input).not.toBeFocused();

  await send.click();
  await expect(input).toHaveValue("");
  await expect(input).not.toBeFocused();
  await expect.poll(() => readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });

  // Longer than the 4 s mobile idle release: recording two opens the device
  // cold, exactly like the reporter's second attempt.
  await mobileSmokePage.waitForTimeout(4_500);
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("");
  expect(await readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic");
  await expect(input).not.toBeFocused();

  // Preserve the unsent-recordings contract: another finalized recording
  // appends instead of replacing the still-unsent second transcript.
  await mobileSmokePage.waitForTimeout(4_500);
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic hello from the mic");
  await expect(input).not.toBeFocused();
  await expect(mobileSmokePage.getByTestId("voice-caption")).toHaveCount(0);

  // Hiding the only reachable composer must cancel the recording and restore
  // the pre-dictation draft, not retain an interim hypothesis as ordinary text.
  const settledDraft = "hello from the mic hello from the mic";
  await mobileSmokePage.evaluate(() => {
    const emitInterim = (window as unknown as { __emitVoiceInterim: () => void }).__emitVoiceInterim;
    emitInterim();
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue(`${settledDraft} still recording`);
  const drawer = mobileSmokePage.getByTestId("sidebar-drawer");
  await mobileSmokePage.getByTestId("mobile-deck-bar-menu").tap();
  await expect(drawer).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveCount(0);
  await mobileSmokePage.getByTestId("brand-row-collapse").tap();
  await expect(drawer).toHaveAttribute("data-open", "false");
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue(settledDraft);
  await expect(mobileSmokePage.getByTestId("mobile-voice-input")).toHaveAttribute("data-state", "idle");
});

test("Web Speech second recording starts from an empty sent draft", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    interface FakeResult extends Array<{ transcript: string }> {
      isFinal: boolean;
    }
    interface FakeResultEvent {
      resultIndex: number;
      results: FakeResult[];
    }
    class FakeSpeechRecognition {
      continuous = false;
      interimResults = false;
      lang = "";
      onstart: (() => void) | null = null;
      onresult: ((event: FakeResultEvent) => void) | null = null;
      onend: (() => void) | null = null;
      onerror: ((event: { error: string }) => void) | null = null;
      start() {
        // Real Web Speech fires onstart once the mic is live; the composer
        // promotes out of its "starting" state on exactly this event.
        queueMicrotask(() => this.onstart?.());
      }
      stop() {
        const result = [{ transcript: "hello from web speech" }] as FakeResult;
        result.isFinal = true;
        this.onresult?.({ resultIndex: 0, results: [result] });
        queueMicrotask(() => this.onend?.());
      }
      abort() {
        queueMicrotask(() => this.onend?.());
      }
    }
    const speechWindow = window as unknown as Window & {
      SpeechRecognition: typeof FakeSpeechRecognition;
      webkitSpeechRecognition: typeof FakeSpeechRecognition;
    };
    speechWindow.SpeechRecognition = FakeSpeechRecognition;
    speechWindow.webkitSpeechRecognition = FakeSpeechRecognition;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");
  await expect(voice).toHaveAttribute("data-engine", "web-speech");

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(input).toHaveValue("hello from web speech");

  await mobileSmokePage.getByTestId("chat-send").click();
  await expect(input).toHaveValue("");
  await expect.poll(() => readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("");
  expect(await readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(input).toHaveValue("hello from web speech");
});
