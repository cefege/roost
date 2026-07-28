// audioPcmCapture is a WARM singleton: the mic pipeline outlives a recording so
// the next tap doesn't pay a 1–2 s device open (which used to swallow the first
// spoken words). Two contracts are defended here — reuse (getUserMedia runs once
// across back-to-back recordings, and the sink swap is clean) and the
// stop/release-during-start race that produced the toast "Mic: null is not an
// object (evaluating 'e.createScriptProcessor')".
//
// Globals (navigator/window/AudioWorkletNode/URL) are stubbed before the import,
// but the module reads them lazily inside warmMic(), so a static import is safe
// and each test can swap window.AudioContext per case.

import { expect, test, describe, beforeEach } from "bun:test";

import {
  isMicWarm,
  micIdle,
  releaseMic,
  startCapture,
  stopCapture,
  warmMic,
} from "../src/lib/audioPcmCapture.ts";

interface FakeTrack { stopped: boolean; stop: () => void }
interface FakeStream { getTracks: () => FakeTrack[]; track: FakeTrack }
function makeStream(): FakeStream {
  const track: FakeTrack = { stopped: false, stop() { this.stopped = true; } };
  return { getTracks: () => [track], track };
}

// per-test controllable state (test doubles for APIs bun doesn't provide)
let scriptProcessorCalls = 0;
let getUserMediaCalls = 0;
let lastProc: { onaudioprocess: unknown; connect: () => void; disconnect: () => void } | null = null;
let getUserMediaImpl: () => Promise<unknown> = () => Promise.resolve(makeStream());
// gate + "entered" signal for addModule, letting a case park a warm-up on the
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
  value: { mediaDevices: { getUserMedia: () => { getUserMediaCalls++; return getUserMediaImpl(); } } },
  configurable: true, writable: true,
});
Object.defineProperty(globalThis, "window", { value: {}, configurable: true, writable: true });
let lastNode: FakeAudioWorkletNode | null = null;
class FakeAudioWorkletNode {
  port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
  constructor() { lastNode = this; }
  connect() {}
  disconnect() {}
}
g.AudioWorkletNode = FakeAudioWorkletNode;
g.URL.createObjectURL = () => "blob:fake";
g.URL.revokeObjectURL = () => {};

// One worklet message big enough to cross the 40 ms threshold at 48 kHz (1920
// samples), so the resampler actually emits a chunk to the attached sink.
function pushAudio(): void {
  lastNode?.port.onmessage?.({ data: new Float32Array(2048) });
}

describe("audioPcmCapture warm pipeline", () => {
  beforeEach(() => {
    releaseMic(); // the module is a singleton — every case starts cold
    micIdle.releaseMs = 60_000;
    scriptProcessorCalls = 0;
    getUserMediaCalls = 0;
    lastProc = null;
    lastNode = null;
    getUserMediaImpl = () => Promise.resolve(makeStream());
    addModuleGate = Promise.withResolvers<void>();
    addModuleEntered = Promise.withResolvers<void>();
    g.window.AudioContext = WorkletCtx;
  });

  test("A: stopCapture() during getUserMedia leaves the mic warm and unattached", async () => {
    const gum = Promise.withResolvers<unknown>();
    const stream = makeStream();
    getUserMediaImpl = () => gum.promise;
    let chunks = 0;

    const started = startCapture(() => { chunks++; });
    stopCapture(); // fires while the warm-up is suspended on getUserMedia
    gum.resolve(stream);
    addModuleGate.resolve();

    await expect(started).resolves.toBeUndefined();
    expect(scriptProcessorCalls).toBe(0);
    expect(isMicWarm()).toBe(true); // a stop keeps the device — that's the point
    pushAudio();
    expect(chunks).toBe(0); // …but nothing is routed to the ended recording
    releaseMic();
    expect(stream.track.stopped).toBe(true);
  });

  test("B: releaseMic() during addModule never dereferences a nulled ctx", async () => {
    const stream = makeStream();
    getUserMediaImpl = () => Promise.resolve(stream);

    const started = startCapture(() => {});
    await addModuleEntered.promise; // the warm-up is parked on the worklet await
    releaseMic(); // closes + nulls ctx
    addModuleGate.resolve();

    await expect(started).resolves.toBeUndefined();
    expect(scriptProcessorCalls).toBe(0); // guarded off the nulled ctx
    expect(isMicWarm()).toBe(false);
    expect(stream.track.stopped).toBe(true);
  });

  test("C: normal ScriptProcessor fallback still fires when no worklet", async () => {
    g.window.AudioContext = NoWorkletCtx;

    await startCapture(() => {});

    expect(scriptProcessorCalls).toBe(1);
    expect(typeof lastProc?.onaudioprocess).toBe("function");
    stopCapture();
  });

  test("D: a second recording reuses the open device and swaps the sink", async () => {
    addModuleGate.resolve();
    let first = 0;
    let second = 0;

    await startCapture(() => { first++; });
    stopCapture();
    await startCapture(() => { second++; });

    expect(getUserMediaCalls).toBe(1); // the whole point: no cold open on tap #2
    expect(isMicWarm()).toBe(true);
    pushAudio();
    expect(second).toBeGreaterThan(0);
    expect(first).toBe(0);
  });

  test("E: the idle timer releases the device after a recording ends", async () => {
    addModuleGate.resolve();
    // 0 ms keeps this deterministic rather than a tuned wall-clock guess: the
    // idle timer is armed by stopCapture BEFORE the probe below, and equal-delay
    // timers fire in insertion order.
    micIdle.releaseMs = 0;
    const stream = makeStream();
    getUserMediaImpl = () => Promise.resolve(stream);

    await startCapture(() => {});
    stopCapture();
    const idleFired = Promise.withResolvers<void>();
    setTimeout(() => idleFired.resolve(), 0);
    await idleFired.promise;

    expect(stream.track.stopped).toBe(true);
    expect(isMicWarm()).toBe(false);
  });

  test("F: a release that cancels an in-flight warm-up doesn't leave a dead mic", async () => {
    const gum = Promise.withResolvers<unknown>();
    const cancelled = makeStream();
    const live = makeStream();
    getUserMediaImpl = () => (getUserMediaCalls === 1 ? gum.promise : Promise.resolve(live));
    addModuleGate.resolve();
    let chunks = 0;

    void warmMic().catch(() => {}); // pointerdown warm-up, parked on getUserMedia
    releaseMic(); // tab hidden before the tap — cancels that warm-up
    const started = startCapture(() => { chunks++; });
    gum.resolve(cancelled);

    await started;
    expect(getUserMediaCalls).toBe(2); // re-opened instead of silently dying
    expect(cancelled.track.stopped).toBe(true);
    expect(isMicWarm()).toBe(true);
    pushAudio();
    expect(chunks).toBeGreaterThan(0);
  });

  test("G: a warm-up that never becomes a recording releases itself", async () => {
    addModuleGate.resolve();
    micIdle.releaseMs = 0; // see E: insertion order, not a wall-clock guess
    const stream = makeStream();
    getUserMediaImpl = () => Promise.resolve(stream);

    await warmMic(); // pointerdown, then the press turns into a FAB drag
    expect(isMicWarm()).toBe(true);
    const idleFired = Promise.withResolvers<void>();
    setTimeout(() => idleFired.resolve(), 0);
    await idleFired.promise;

    expect(stream.track.stopped).toBe(true);
    expect(isMicWarm()).toBe(false);
  });
});
