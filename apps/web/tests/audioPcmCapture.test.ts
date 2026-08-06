// audioPcmCapture is a WARM singleton: the mic pipeline outlives a recording so
// the next tap doesn't pay a 1–2 s device open (which used to swallow the first
// spoken words). Contracts defended here include reuse (getUserMedia runs once
// across back-to-back recordings and the sink swap is clean), AudioContext
// activation and graph rendering, and the stop/release-during-start race that
// produced the toast "Mic: null is not an object (evaluating
// 'e.createScriptProcessor')".
//
// Globals (navigator/window/AudioWorkletNode/URL) are stubbed before the import,
// but the module reads them lazily inside warmMic(), so a static import is safe
// and each test can swap window.AudioContext per case.

import { expect, test, describe, beforeEach } from "bun:test";

import {
  isMicWarm,
  micIdle,
  releaseMic,
  captureStats,
  captureQuirks,
  repairCapture,
  releaseMicIfIdle,
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
let resumeCalls = 0;
let initialContextState: AudioContextState = "running";
// How many resume() calls leave the context SUSPENDED before one succeeds —
// iOS's "resume() does not always bring it back" behaviour, per-context.
let resumeRefusals = 0;
let resumeImpl: () => Promise<void> = () => Promise.resolve();
let lastContext: WorkletCtx | NoWorkletCtx | null = null;
// gate + "entered" signal for addModule, letting a case park a warm-up on the
// worklet await deterministically — no wall-clock timers.
let addModuleGate = Promise.withResolvers<void>();
let addModuleEntered = Promise.withResolvers<void>();

class FakeSource { connect() {} disconnect() {} }

// AudioContext exposing a worklet (preferred path).
class WorkletCtx {
  sampleRate = 48000;
  destination = {};
  state: AudioContextState = initialContextState;
  closed = false;
  audioWorklet = {
    addModule: (_url: string) => {
      addModuleEntered.resolve();
      return addModuleGate.promise;
    },
  };
  constructor() { lastContext = this; }
  createMediaStreamSource() { return new FakeSource(); }
  createScriptProcessor() {
    scriptProcessorCalls++;
    lastProc = { onaudioprocess: null, connect() {}, disconnect() {} };
    return lastProc;
  }
  resume() {
    resumeCalls++;
    return resumeImpl().then(() => {
      if (resumeRefusals > 0) { resumeRefusals--; return; }
      this.state = "running";
    });
  }
  close() { this.closed = true; return Promise.resolve(); }
}

// AudioContext without a worklet — forces the ScriptProcessor fallback.
class NoWorkletCtx {
  sampleRate = 48000;
  destination = {};
  state: AudioContextState = initialContextState;
  audioWorklet: undefined = undefined;
  constructor() { lastContext = this; }
  createMediaStreamSource() { return new FakeSource(); }
  createScriptProcessor() {
    scriptProcessorCalls++;
    lastProc = { onaudioprocess: null, connect() {}, disconnect() {} };
    return lastProc;
  }
  resume() {
    resumeCalls++;
    return resumeImpl().then(() => { this.state = "running"; });
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
  connectedDestination: unknown = null;
  constructor() { lastNode = this; }
  connect(destination: unknown) { this.connectedDestination = destination; }
  disconnect() {}
}
g.AudioWorkletNode = FakeAudioWorkletNode;
g.URL.createObjectURL = () => "blob:fake";
g.URL.revokeObjectURL = () => {};

// One worklet message big enough to cross the 40 ms threshold at 48 kHz (1920
// samples), so the resampler actually emits a chunk to the attached sink.
// `amplitude` defaults to digital silence — a real mic that has been handed a
// dead input delivers exactly that, and the peak accounting must tell the two
// apart.
function pushAudio(amplitude = 0): void {
  lastNode?.port.onmessage?.({ data: new Float32Array(2048).fill(amplitude) });
}

// The ScriptProcessor equivalent: the node's callback shape, not the port's.
function pushProcAudio(amplitude = 0): void {
  // The fake proc types onaudioprocess as unknown (it stands in for a DOM
  // handler); the harness owns both sides, so the shape is asserted here.
  const handler = lastProc?.onaudioprocess as ((e: unknown) => void) | null | undefined;
  handler?.({ inputBuffer: { getChannelData: () => new Float32Array(2048).fill(amplitude) } });
}

describe("audioPcmCapture warm pipeline", () => {
  beforeEach(() => {
    releaseMic(); // the module is a singleton — every case starts cold
    micIdle.releaseMs = 60_000;
    captureQuirks.workletBroken = false; // a latched repair must not leak across cases
    scriptProcessorCalls = 0;
    getUserMediaCalls = 0;
    resumeCalls = 0;
    initialContextState = "running";
    resumeRefusals = 0;
    resumeImpl = () => Promise.resolve();
    lastContext = null;
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

    await expect(started).resolves.toBe(false);
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

    await expect(started).resolves.toBe(false);
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

  test("cold startup resumes its context before getUserMedia resolves", async () => {
    const gum = Promise.withResolvers<unknown>();
    initialContextState = "suspended";
    getUserMediaImpl = () => gum.promise;

    const started = startCapture(() => {});

    expect(lastContext).toBeInstanceOf(WorkletCtx);
    expect(resumeCalls).toBe(1);
    expect(getUserMediaCalls).toBe(1);

    gum.resolve(makeStream());
    addModuleGate.resolve();
    await expect(started).resolves.toBe(true);
    expect(lastNode?.connectedDestination).toBe(lastContext?.destination);
  });

  test("a suspended warm context resumes before routing to the new sink", async () => {
    addModuleGate.resolve();
    let first = 0;
    let second = 0;

    await startCapture(() => { first++; });
    stopCapture();
    if (!lastContext) throw new Error("expected a retained AudioContext");
    lastContext.state = "suspended";

    await startCapture(() => { second++; });
    expect(resumeCalls).toBe(1);
    expect(getUserMediaCalls).toBe(1);
    pushAudio();
    expect(first).toBe(0);
    expect(second).toBeGreaterThan(0);
  });

  test("resume failure disposes the fulfilled stream and context", async () => {
    const stream = makeStream();
    const resumeError = new Error("resume denied");
    initialContextState = "suspended";
    resumeImpl = () => Promise.reject(resumeError);
    getUserMediaImpl = () => Promise.resolve(stream);

    const started = startCapture(() => {});
    if (!(lastContext instanceof WorkletCtx)) {
      throw new Error("expected a worklet AudioContext");
    }
    const openedContext = lastContext;

    await expect(started).rejects.toBe(resumeError);
    expect(stream.track.stopped).toBe(true);
    expect(openedContext.closed).toBe(true);
    expect(isMicWarm()).toBe(false);
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

  test("H: an automatic idle/tab-hide release cannot kill a recording that is still starting", async () => {
    const gum = Promise.withResolvers<unknown>();
    const stream = makeStream();
    getUserMediaImpl = () => gum.promise;
    addModuleGate.resolve();
    let chunks = 0;

    const started = startCapture(() => { chunks++; });
    releaseMicIfIdle();          // the tab-hide / idle-timer path, mid warm-up
    gum.resolve(stream);

    await expect(started).resolves.toBe(true);
    expect(isMicWarm()).toBe(true);
    pushAudio();
    expect(chunks).toBeGreaterThan(0);
  });

  test("I: captureStats separates a live recording from one receiving digital silence", async () => {
    addModuleGate.resolve();
    await startCapture(() => {});

    expect(captureStats()).toMatchObject({ frames: 0, peak: 0, path: "worklet", ctxState: "running", sampleRate: 48000 });

    pushAudio(); // the graph runs, the device hands us zeros
    expect(captureStats()).toMatchObject({ frames: 1, peak: 0 });

    pushAudio(0.5); // …and now real audio
    const live = captureStats();
    expect(live.frames).toBe(2);
    expect(live.peak).toBeCloseTo(0.5, 5);
    stopCapture();
  });

  test("J: repairCapture rebuilds a silent pipeline onto the ScriptProcessor and keeps the same sink", async () => {
    addModuleGate.resolve();
    let chunks = 0;
    const sink = () => { chunks++; };

    expect(await startCapture(sink)).toBe(true);
    expect(captureStats().path).toBe("worklet");
    // The iPhone-only dead-worklet case: attached, warm, zero frames forever.
    expect(captureStats().frames).toBe(0);

    expect(await repairCapture(sink)).toBe(true);

    expect(captureStats().path).toBe("scriptprocessor");
    expect(getUserMediaCalls).toBe(2); // a real rebuild, not a warm reuse
    expect(scriptProcessorCalls).toBe(1);
    pushProcAudio(0.25);
    expect(chunks).toBeGreaterThan(0); // audio reaches the ORIGINAL sink
    expect(captureStats().peak).toBeCloseTo(0.25, 5);
    stopCapture();
  });

  test("K: a worklet that needed repairing is not tried again on the next recording", async () => {
    addModuleGate.resolve();
    await startCapture(() => {});
    await repairCapture(() => {});
    stopCapture();
    releaseMic(); // the device closes; the next tap opens a cold pipeline

    let chunks = 0;
    expect(await startCapture(() => { chunks++; })).toBe(true);

    expect(captureStats().path).toBe("scriptprocessor");
    pushAudio(0.5); // the worklet is not wired at all now
    expect(chunks).toBe(0);
    pushProcAudio(0.5);
    expect(chunks).toBeGreaterThan(0);
  });

  test("L: a warm pipeline whose context cannot be resumed is rebuilt, not attached to", async () => {
    addModuleGate.resolve();
    await startCapture(() => {});
    stopCapture();
    if (!lastContext) throw new Error("expected a retained AudioContext");
    const stale = lastContext;

    // iOS after a lock/call/Siri interruption: the context is suspended and the
    // FIRST resume() leaves it that way (webkit.org/b/237878). Attaching here is
    // what produced a red mic streaming zero bytes with no error at all.
    stale.state = "suspended";
    resumeRefusals = 1;

    let chunks = 0;
    // The tap must still succeed — a rebuild, not a cancelled attach.
    expect(await startCapture(() => { chunks++; })).toBe(true);

    expect(getUserMediaCalls).toBe(2); // fresh device, not the dead context
    expect(lastContext).not.toBe(stale);
    expect(lastContext?.state).toBe("running");
    expect(captureStats().ctxState).toBe("running");
    pushAudio(0.5);
    expect(chunks).toBeGreaterThan(0);
  });
});
