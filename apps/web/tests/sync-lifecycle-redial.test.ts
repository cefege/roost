// Regression: browser lifecycle states that only a reload could clear.
//
// The retired policy parked the ONE owning Sync loop after eight consecutive
// failures. A visible page could then sit with no live socket and no scheduled
// re-dial — terminal frozen, banner red, nothing left to recover it but F5. The
// current policy caps the DELAY (30 s) and never the attempt count, lets only a
// HIDDEN document sleep, and treats every page-lifecycle resume edge
// (visibilitychange→visible, pageshow/bfcache restore, Page Lifecycle resume,
// window focus) as one coalesced wake that re-dials in place.
//
// Drives the real `_runConnectSync` loop against a fake WebSocket so the
// generation/hydration/park state machine under test is the production one. Bun
// fake timers advance the capped backoff (and performance.now with it), so no
// wall-clock seconds are spent proving a 30 s cap.

import { afterAll, beforeAll, describe, expect, mock, test, vi } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomain,
  SyncDomainGenerationSchema,
  SyncSubscribedFrameSchema,
} from "@roost/shared/proto/sync_pb";

// ─── fake browser host ────────────────────────────────────────────────────────
// No jsdom/happy-dom (repo convention): a small fake covers exactly what the
// firehose touches — a socket, a visibility-bearing document, and a window for
// the page-lifecycle edges.

class FakeSyncSocket extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readyState = FakeSyncSocket.CONNECTING;
  binaryType = "blob";
  readonly sent: Uint8Array[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: ArrayBuffer }) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  onclose: ((ev: { code: number; reason: string }) => void) | null = null;

  constructor(readonly url: string, readonly protocols?: string[]) {
    super();
    dialed.push(this);
  }

  send(data: Uint8Array): void { this.sent.push(data); }

  close(): void {
    if (this.readyState === FakeSyncSocket.CLOSED) return;
    this.readyState = FakeSyncSocket.CLOSED;
    this.onclose?.({ code: 1000, reason: "" });
  }

  /** Server accepted the dial. */
  accept(): void {
    this.readyState = FakeSyncSocket.OPEN;
    this.onopen?.();
  }

  /** Deliver one encoded FirehoseFrame, exactly as the browser would. */
  deliver(bytes: Uint8Array): void {
    const copy = new Uint8Array(bytes);
    this.onmessage?.({ data: copy.buffer });
    this.dispatchEvent(new Event("message"));
  }
}

const dialed: FakeSyncSocket[] = [];
const storage = new Map<string, string>();

const fakeDocument = Object.assign(new EventTarget(), { visibilityState: "visible" });
const fakeWindow = new EventTarget();

Object.assign(globalThis, {
  WebSocket: FakeSyncSocket,
  document: fakeDocument,
  window: fakeWindow,
  // connect.ts reads location.hash for relocation/pair fragments at module scope.
  location: {
    origin: "http://127.0.0.1:65000",
    protocol: "http:",
    host: "127.0.0.1:65000",
    hostname: "127.0.0.1",
    href: "http://127.0.0.1:65000/",
    pathname: "/",
    search: "",
    hash: "",
  },
  localStorage: {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => { storage.set(key, value); },
    removeItem: (key: string) => { storage.delete(key); },
  },
});

mock.module("../src/auth/web-key.ts", () => ({
  signCoordinatorJwt: async () => "test-jwt",
  getPublicKeyB64: async () => "test-key",
  persistedWebKeyAtStartup: true,
}));

// Intentional module-loading boundary: the host fakes and the JWT mock must be
// installed before the singleton sync module evaluates.
const sync = await import("../src/store/sync.ts");
const { setForceHidden, setForceVisible } = await import("../src/lib/pageVisible.ts");
const { SYNC_REDIAL_MAX_MS, SYNC_REFOCUS_STALE_MS } = await import("../src/store/sync-watchdog.ts");

// ─── drivers ─────────────────────────────────────────────────────────────────

/** Let the loop's awaits settle. Every hop between a wake and the next dial is
 *  a microtask (the mocked JWT sign is the only async step). */
async function flush(turns = 24): Promise<void> {
  for (let turn = 0; turn < turns; turn++) await Promise.resolve();
}

async function advance(ms: number): Promise<void> {
  vi.advanceTimersByTime(ms);
  await flush();
}

/** Fail the newest dial the way a refused/dropped connection does: no
 *  abortReason, so the loop counts it as a failure rather than a lifecycle
 *  close. */
async function dropNewestDial(): Promise<void> {
  const socket = dialed.at(-1);
  if (!socket) throw new Error("no dial to drop");
  socket.close();
  await flush();
}

function subscribedFrame(socketId: string, processEpoch: string): Uint8Array {
  const domains = [
    SyncDomain.TERMINAL, SyncDomain.WORKERS, SyncDomain.WORKSPACES,
    SyncDomain.TASKS, SyncDomain.PERMISSIONS, SyncDomain.MCP,
    SyncDomain.PAIR, SyncDomain.WEBHOOK, SyncDomain.AUDIT,
  ];
  return toBinary(FirehoseFrameSchema, create(FirehoseFrameSchema, {
    frame: {
      case: "subscribed",
      value: create(SyncSubscribedFrameSchema, {
        socketId,
        processEpoch,
        generations: domains.map((domain) => create(SyncDomainGenerationSchema, {
          domain,
          generation: 1n,
          subscribed: domain === SyncDomain.TERMINAL,
        })),
      }),
    },
  }));
}

/** Bring the newest dial to terminal readiness: accept, subscribe, hydrate. */
async function completeNewestDial(epoch: string): Promise<void> {
  const socket = dialed.at(-1);
  if (!socket) throw new Error("no dial to complete");
  socket.accept();
  await flush();
  socket.deliver(subscribedFrame(`socket-${epoch}`, epoch));
  await flush();
}

/** Every page-lifecycle edge a bfcache restore fires, in one burst. */
function resumeBurst(): void {
  setForceVisible(true);                                   // visibilitychange
  fakeWindow.dispatchEvent(new Event("pageshow"));
  fakeDocument.dispatchEvent(new Event("resume"));
  fakeWindow.dispatchEvent(new Event("focus"));
}

// Owners reconcile on the generation handler — the single wake edge. Hydration
// is what a resume must replay before those owners can claim.
const generations: Array<{ socketGeneration: number; ready: boolean } | null> = [];
const hydrated: number[] = [];

beforeAll(() => {
  vi.useFakeTimers();
  setForceVisible(true);
  sync.registerSyncV2GenerationHandler((state) => {
    generations.push(state && {
      socketGeneration: state.socketGeneration,
      ready: state.ready,
    });
  });
  sync.registerSyncDomainHydrator(SyncDomain.TERMINAL, async (token) => ({
    snapshotToken: `snapshot-${token.socketGeneration}`,
    apply: () => { hydrated.push(token.socketGeneration); },
  }));
  sync.installSyncLifecycleWake(() => { resumes.push(sync.syncWsGeneration()); });
  void sync._runConnectSync();
});

afterAll(async () => {
  // Quiesce the singleton loop: no live socket, no pending timer, waiting on a
  // resume that never arrives. Its stale-link interval would otherwise outlive
  // the suite and keep the test process alive once real timers are restored.
  setForceHidden(true);
  storage.set("roostSmoke", "1");
  sync.pauseSyncTransport();
  await flush();
  vi.useRealTimers();
});

const resumes: number[] = [];

describe("Sync redial never leaves a visible page parked", () => {
  test("capped redial continues past the retired eight-failure park", async () => {
    await flush();
    expect(dialed.length).toBe(1);

    await dropNewestDial();
    expect(sync.syncRedialStatus()).toEqual({
      failures: 1,
      nextDelayMs: 1_000,
      hiddenParked: false,
      liveness: "none",
    });

    // Walk well past the retired cap. Each dial must still be scheduled.
    for (let failure = 2; failure <= 12; failure++) {
      await advance(sync.syncRedialStatus().nextDelayMs);
      expect(dialed.length).toBe(failure);
      await dropNewestDial();
      expect(sync.syncRedialStatus().failures).toBe(failure);
    }
    expect(sync.syncRedialStatus()).toEqual({
      failures: 12,
      nextDelayMs: SYNC_REDIAL_MAX_MS,
      hiddenParked: false,
      liveness: "none",
    });

    // The delay is capped, not the loop: the 13th dial lands one cap later.
    await advance(SYNC_REDIAL_MAX_MS - 1);
    expect(dialed.length).toBe(12);
    await advance(1);
    expect(dialed.length).toBe(13);
  });

  test("a hidden document sleeps instead of dialing", async () => {
    setForceHidden(true);
    await dropNewestDial();
    expect(sync.syncRedialStatus().hiddenParked).toBe(true);

    const parkedAt = dialed.length;
    await advance(10 * SYNC_REDIAL_MAX_MS);
    expect(dialed.length).toBe(parkedAt);
  });

  test("one resume burst re-dials exactly once and replays owners", async () => {
    const parkedAt = dialed.length;
    const generationsAt = generations.length;
    resumes.length = 0;

    resumeBurst();
    await flush();

    // Four lifecycle edges, one wake: a second dial would race two generations
    // onto the same tab and pay for another handshake plus backfill.
    expect(resumes).toEqual([parkedAt]);
    expect(dialed.length).toBe(parkedAt + 1);
    expect(sync.syncRedialStatus()).toEqual({
      failures: 0,
      nextDelayMs: 1_000,
      hiddenParked: false,
      liveness: "dialing",
    });

    // A dial in flight IS the redial. A later resume edge — past the coalesce
    // window, so it is handled — must not close a still-connecting socket:
    // that would discard this generation and immediately start another.
    const dialing = dialed.at(-1)!;
    await advance(1_000);
    resumeBurst();
    await flush();
    expect(resumes).toEqual([parkedAt, parkedAt + 1]);
    expect(dialing.readyState).toBe(FakeSyncSocket.CONNECTING);
    expect(dialed.length).toBe(parkedAt + 1);

    await completeNewestDial("epoch-resume");

    // The resume replayed the store: this socket's generation re-hydrated, and
    // mounted owners saw exactly one ready generation to reconcile against.
    const generation = sync.syncWsGeneration();
    expect(hydrated.at(-1)).toBe(generation);
    const woken = generations.slice(generationsAt);
    expect(woken.filter((state) => state?.ready)).toEqual([
      { socketGeneration: generation, ready: true },
    ]);
    expect(new Set(woken.filter((state) => state !== null).map((state) => state!.socketGeneration)))
      .toEqual(new Set([generation]));
    expect(sync.currentSyncV2TerminalState()).toMatchObject({
      socketGeneration: generation,
      processEpoch: "epoch-resume",
      ready: true,
    });
  });

  test("a resume keeps a live socket and replaces only a silent one", async () => {
    const liveGeneration = sync.syncWsGeneration();
    const dialsBefore = dialed.length;

    // Fresh link: a tab switch must not spend a JWT sign, a handshake and the
    // since= backfill ahead of the terminal's reveal snapshot.
    await advance(1_000);
    setForceHidden(true);
    resumeBurst();
    await flush();
    expect(dialed.length).toBe(dialsBefore);
    expect(sync.syncWsGeneration()).toBe(liveGeneration);
    expect(sync.syncRedialStatus().liveness).toBe("open");

    // Silent past the refocus budget: that socket is suspended or half-open, so
    // the resume closes it and the loop dials exactly one replacement.
    await advance(SYNC_REFOCUS_STALE_MS + 1_000);
    setForceHidden(true);
    resumeBurst();
    await flush();
    expect(dialed.length).toBe(dialsBefore + 1);
    expect(sync.syncWsGeneration()).toBe(liveGeneration + 1);

    await completeNewestDial("epoch-stale");
    expect(hydrated.at(-1)).toBe(sync.syncWsGeneration());
  });
});
