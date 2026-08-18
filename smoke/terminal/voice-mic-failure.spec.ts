import { test, expect } from "./fixtures.ts";
import { spawnSmokeShell, navigateToSmokeSession } from "./terminal-helpers.ts";

// A mic that cannot start used to leave the button red and data-state="listening"
// forever, with the reason rendered as an unreadable ~124x390 ribbon anchored to
// the 44px mic wrapper. Both halves are asserted here: the recording ENDS, and
// the caption spans the composer bar.
test("a mic that cannot start ends the recording and says why", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          const e = new Error("denied");
          e.name = "NotAllowedError";
          return Promise.reject(e);
        },
      },
    });
  });
  // Re-runs the module-scope transcription-config fetch with the key now set.
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  const toggle = mobileSmokePage.getByTestId("terminal-nav-toggle");
  const panel = mobileSmokePage.getByTestId("terminal-nav-buttons");
  await expect(toggle).toHaveAttribute("data-open", "false");
  await toggle.tap();
  await expect(panel).toBeVisible();

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();
  await expect(panel).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "true");

  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(mic).toHaveAttribute("data-recording", "false");
  const caption = mobileSmokePage.getByTestId("voice-caption");
  await expect(caption).toContainText("Mic blocked");
  const box = (await caption.boundingBox())!;
  expect(box.width).toBeGreaterThan(300);
  expect(box.x).toBeGreaterThanOrEqual(0);
});

// The reported bug that no error path covers: getUserMedia RESOLVES, the graph
// builds, and not one audio frame ever arrives (iOS suspends the AudioContext
// out from under the recording; AudioWorklet ships dead on some iPhone builds).
// That used to be a red mic forever with nothing recorded and nothing logged.
// The stub below is a complete Web Audio graph that never fires a callback, so
// BOTH the worklet and the ScriptProcessor repair path stay silent.
test("a mic that opens but delivers no audio ends the recording and says why", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    class DeadPort { onmessage: unknown = null; close() {} }
    class DeadWorkletNode { port = new DeadPort(); connect() {} disconnect() {} }
    class DeadContext {
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
    win.AudioContext = DeadContext;
    win.webkitAudioContext = DeadContext;
    win.AudioWorkletNode = DeadWorkletNode;
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: () => Promise.resolve({ getTracks: () => [{ stop() {} }] }) },
    });
    // The hermetic stack has no internet, so the real Deepgram socket would
    // drop and claim the caption before the silence deadline. Only that ONE
    // host is faked (a permanently-open, send-swallowing socket); the SPA's own
    // Sync WS must keep using the real implementation.
    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null, onmessage: null, onerror: null, onclose: null,
        send() {}, close() { sock.readyState = 3; },
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
  await mic.tap();
  // The recording is genuinely live first — this is not a start failure.
  await expect(voice).toHaveAttribute("data-state", "listening");

  // Two silence windows (2.5s each): one rebuild attempt, then give up.
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 20_000 });
  await expect(mic).toHaveAttribute("data-recording", "false");
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("sent no audio");
});

// Measured on the reporter's iPhone (iOS 18.7, installed PWA): the recording ran
// with ctx_state "suspended", frames 0, chunks 0, while the Deepgram socket was
// open — starting the mic switches the OS audio session and re-suspends a
// context created before the stream, and resume() does not bring it back. The
// stub reproduces exactly that: healthy until getUserMedia resolves, suspended
// afterwards, resume() a no-op. A pipeline that cannot render must fail the tap,
// not record silence.
test("an audio session suspended by the mic start fails the tap instead of recording silence", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    const win = window as unknown as Record<string, unknown>;
    let live: { state: string } | null = null;
    class IosContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      constructor() { live = this; }
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); } // iOS: outside a gesture this does nothing
      close() { return Promise.resolve(); }
    }
    class DeadPort { onmessage: unknown = null; close() {} }
    win.AudioContext = IosContext;
    win.webkitAudioContext = IosContext;
    win.AudioWorkletNode = class { port = new DeadPort(); connect() {} disconnect() {} };
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          if (live) live.state = "suspended"; // the OS audio-session switch
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();

  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(mic).toHaveAttribute("data-recording", "false");
  // Names the real cause, and fails immediately instead of after two silence
  // windows — which is how you tell this apart from generic dead capture.
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("audio session stayed suspended");
});

// Chromium's ordinary media-device behavior cannot reproduce WebKit's
// never-settling promise (and passes on the unfixed code), so
// the stall is injected: getUserMedia parks forever on the FIRST call only.
// Unfixed, that tap never resolves — `warming` keeps the dead promise for the
// page's lifetime, so the mic breathes in `listening` and EVERY later tap awaits
// the same corpse. Fixed, the tap ends with a reason and the next one opens a
// fresh device.
test("a stalled mic open ends the recording and the next tap still works", async ({ mobileSmokePage, stack }) => {
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
    setInterval(() => liveNode?.port.onmessage?.({ data: new Float32Array(2048).fill(0.3) }), 40);
    let opens = 0;
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          opens++;
          // iOS 18.7 as an installed PWA: the OS audio session is mid-transition
          // and the promise simply never settles. Nothing cancels it.
          if (opens === 1) return Promise.withResolvers<MediaStream>().promise;
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });
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

  // The stalled tap must END, with the deadline's own caption.
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 20_000 });
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("did not respond in time");

  // …and the singleton must be usable again on the very next tap.
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue(/hello from the mic/);
});
