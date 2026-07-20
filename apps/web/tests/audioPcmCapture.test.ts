// audioPcmCapture stop-during-start race — regression for the toast
// "Mic: null is not an object (evaluating 'e.createScriptProcessor')".
// start() awaits getUserMedia then audioWorklet.addModule(); a stop() during
// either await must not fall through to ctx.createScriptProcessor() against a
// nulled ctx, and must release the mic. Contract verified with hand-rolled
// fakes (bun has no DOM): a controllable getUserMedia + a fake AudioContext.
//
// Globals (navigator/window/AudioWorkletNode/URL) are stubbed before the
// import, but createPcmCapture reads them lazily inside start(), so a static
// import is safe and each test can swap window.AudioContext per case.

import { expect, test, describe, beforeEach } from "bun:test";

import { createPcmCapture } from "../src/lib/audioPcmCapture.ts";

interface FakeTrack { stopped: boolean; stop: () => void }
function makeStream(): { getTracks: () => FakeTrack[]; track: FakeTrack } {
  const track: FakeTrack = { stopped: false, stop() { this.stopped = true; } };
  return { getTracks: () => [track], track };
}

// per-test controllable state (test doubles for APIs bun doesn't provide)
let scriptProcessorCalls = 0;
let lastProc: { onaudioprocess: unknown; connect: () => void; disconnect: () => void } | null = null;
let getUserMediaImpl: () => Promise<unknown> = () => Promise.resolve(makeStream());
// gate + "entered" signal for addModule, letting a case park start() on the
// worklet await deterministically — no wall-clock timers.
let addModuleGate = Promise.withResolvers<void>();
let addModuleEntered = Promise.withResolvers<void>();

class FakeSource { connect() {} disconnect() {} }

// AudioContext exposing a worklet (preferred path).
class WorkletCtx {
  sampleRate = 48000;
  destination = {};
  closed = false;
  audioWorklet = {
    addModule: (_url: string) => {
      addModuleEntered.resolve();
      return addModuleGate.promise;
    },
  };
  createMediaStreamSource() { return new FakeSource(); }
  createScriptProcessor() {
    scriptProcessorCalls++;
    lastProc = { onaudioprocess: null, connect() {}, disconnect() {} };
    return lastProc;
  }
  close() { this.closed = true; return Promise.resolve(); }
}

// AudioContext without a worklet — forces the ScriptProcessor fallback.
class NoWorkletCtx {
  sampleRate = 48000;
  destination = {};
  audioWorklet: undefined = undefined;
  createMediaStreamSource() { return new FakeSource(); }
  createScriptProcessor() {
    scriptProcessorCalls++;
    lastProc = { onaudioprocess: null, connect() {}, disconnect() {} };
    return lastProc;
  }
  close() { return Promise.resolve(); }
}

// ── global stubs ──────────────────────────────────────────────────────────
const g = globalThis as unknown as {
  window: { AudioContext?: unknown };
  URL: { createObjectURL: (b: unknown) => string; revokeObjectURL: (u: string) => void };
  AudioWorkletNode: unknown;
};
Object.defineProperty(globalThis, "navigator", {
  value: { mediaDevices: { getUserMedia: () => getUserMediaImpl() } },
  configurable: true, writable: true,
});
Object.defineProperty(globalThis, "window", { value: {}, configurable: true, writable: true });
class FakeAudioWorkletNode {
  port = { onmessage: null as unknown, close() {} };
  connect() {}
  disconnect() {}
}
g.AudioWorkletNode = FakeAudioWorkletNode;
g.URL.createObjectURL = () => "blob:fake";
g.URL.revokeObjectURL = () => {};

describe("createPcmCapture stop-during-start race", () => {
  beforeEach(() => {
    scriptProcessorCalls = 0;
    lastProc = null;
    getUserMediaImpl = () => Promise.resolve(makeStream());
    addModuleGate = Promise.withResolvers<void>();
    addModuleEntered = Promise.withResolvers<void>();
    g.window.AudioContext = WorkletCtx;
  });

  test("A: stop() during getUserMedia releases mic, never touches createScriptProcessor", async () => {
    const gum = Promise.withResolvers<unknown>();
    const stream = makeStream();
    getUserMediaImpl = () => gum.promise;

    const cap = createPcmCapture();
    const started = cap.start(() => {});
    cap.stop(); // fires while start is suspended on getUserMedia
    gum.resolve(stream);

    await expect(started).resolves.toBeUndefined();
    expect(scriptProcessorCalls).toBe(0);
    expect(stream.track.stopped).toBe(true); // mic released despite the race
  });

  test("B: stop() during addModule never dereferences a nulled ctx", async () => {
    const cap = createPcmCapture();
    const started = cap.start(() => {});
    await addModuleEntered.promise; // start is now parked on the worklet await
    cap.stop(); // closes + nulls ctx
    addModuleGate.resolve();

    await expect(started).resolves.toBeUndefined();
    expect(scriptProcessorCalls).toBe(0); // guarded off the nulled ctx
  });

  test("C: normal ScriptProcessor fallback still fires when no worklet", async () => {
    g.window.AudioContext = NoWorkletCtx;

    const cap = createPcmCapture();
    await cap.start(() => {});

    expect(scriptProcessorCalls).toBe(1);
    expect(typeof lastProc?.onaudioprocess).toBe("function");
    cap.stop();
  });
});
