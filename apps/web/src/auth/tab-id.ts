// Per-document UUID. Sent as `x-roost-tab-id` on every Connect request so
// coord can distinguish multiple tabs from the SAME browser (same EdDSA
// fingerprint, different windows). sessionStorage normally provides exactly
// the desired lifetime: it survives reloads and is distinct between tabs.
//
// Browsers copy sessionStorage when a tab is duplicated, though. Bootstrap
// therefore claims the inherited ID before any authenticated transport uses it.
// Web Locks provide atomic arbitration; BroadcastChannel is the bounded,
// best-effort fallback for older engines.

import { signal } from "@roost/shared/diag";

const KEY = "roost.tabId";
const LOCK_PREFIX = "roost.tab-id:";
const CLAIM_CHANNEL = "roost.tab-id-claim-v1";
const BROADCAST_PROBE_WAIT_MS = 80;

type Arbitration = "web-locks" | "broadcast-channel";
type LockAttempt = "acquired" | "occupied" | "unavailable";
type BroadcastAttempt = "acquired" | "occupied" | "unavailable";

interface PendingBroadcastProbe {
  id: string;
  nonce: string;
  settle: (result: BroadcastAttempt) => void;
}

let _cached: string | null = null;
let _claimPromise: Promise<void> | null = null;

// A granted Web Lock is held by keeping its request callback pending. Browsers
// release it automatically with the document; the release handle exists only
// so focused unit tests can model a reload in one JS realm.
let _releaseWebLock: (() => void) | null = null;
let _webLockRequest: Promise<unknown> | null = null;

// The fallback channel must remain open after the probe. Its listener is what
// tells later duplicated documents that this document still owns the ID.
let _claimChannel: BroadcastChannel | null = null;
let _broadcastClaimedId: string | null = null;
let _pendingBroadcastProbe: PendingBroadcastProbe | null = null;

function safeSessionStorage(): Storage | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    return null;
  }
}

export function getTabId(): string {
  if (_cached) return _cached;
  const storage = safeSessionStorage();
  if (storage) {
    try {
      const stored = storage.getItem(KEY);
      if (stored) {
        _cached = stored;
        return stored;
      }
    } catch { /* private mode / sandboxed storage */ }
  }
  const fresh = _randomId();
  // Cache first so even a re-entrant storage shim observes one document ID.
  _cached = fresh;
  try { storage?.setItem(KEY, fresh); } catch { /* private mode / quota */ }
  return fresh;
}

function rotateTabId(previous: string, arbitration: Arbitration): string {
  let fresh = _randomId();
  // A broken UUID shim should not leave this document on the known-conflicting
  // value. The suffix is only a last-ditch compatibility path.
  if (fresh === previous) fresh = `${fresh}-${Date.now().toString(36)}`;

  // No await or callback may observe half of this update: current-document
  // readers switch immediately, and its sessionStorage copy follows in the
  // same turn. A storage failure still must not put this document back on the
  // conflicting cached ID.
  _cached = fresh;
  try { safeSessionStorage()?.setItem(KEY, fresh); } catch { /* private mode / quota */ }

  signal("tab.duplicate_identity_rotated", {
    arbitration,
    previous8: previous.slice(0, 8),
    next8: fresh.slice(0, 8),
    cooldownKey: previous,
  });
  return fresh;
}

function tryAcquireWebLock(locks: LockManager, id: string): Promise<LockAttempt> {
  return new Promise<LockAttempt>((resolve) => {
    let callbackStarted = false;
    let release!: () => void;
    const documentLifetime = new Promise<void>((done) => { release = done; });

    let request: Promise<unknown>;
    try {
      request = locks.request(
        `${LOCK_PREFIX}${id}`,
        { mode: "exclusive", ifAvailable: true },
        async (lock) => {
          callbackStarted = true;
          if (!lock) {
            resolve("occupied");
            return;
          }
          _releaseWebLock = release;
          resolve("acquired");
          await documentLifetime;
        },
      );
    } catch {
      resolve("unavailable");
      return;
    }

    const tracked = Promise.resolve(request);
    _webLockRequest = tracked;
    void tracked
      .catch(() => {
        // SecurityError / unsupported options behave like an unavailable API;
        // the BroadcastChannel path can still arbitrate this document.
        if (!callbackStarted) resolve("unavailable");
      })
      .finally(() => {
        if (_webLockRequest === tracked) _webLockRequest = null;
      });
  });
}

async function claimWithWebLocks(locks: LockManager): Promise<boolean> {
  let id = getTabId();
  for (;;) {
    const result = await tryAcquireWebLock(locks, id);
    if (result === "acquired") return true;
    if (result === "unavailable") return false;
    id = rotateTabId(id, "web-locks");
  }
}

function postOccupied(id: string, nonce: string): void {
  try {
    _claimChannel?.postMessage({ type: "occupied", id, nonce });
  } catch { /* channel closed with the document */ }
}

function handleClaimMessage(event: MessageEvent<unknown>): void {
  const value = event.data;
  if (!value || typeof value !== "object") return;
  const message = value as Record<string, unknown>;
  if (
    message.type === "probe"
    && typeof message.id === "string"
    && typeof message.nonce === "string"
  ) {
    if (_broadcastClaimedId === message.id) {
      postOccupied(message.id, message.nonce);
      return;
    }
    // BroadcastChannel does not deliver a sender its own messages. Seeing a
    // probe for our pending ID therefore proves another pending document
    // exists. Yield locally; the sender can retain the inherited value. If
    // probes cross, both yield and independently probe fresh random IDs.
    if (_pendingBroadcastProbe?.id === message.id) {
      _pendingBroadcastProbe.settle("occupied");
    }
    return;
  }
  if (
    message.type === "occupied"
    && typeof message.id === "string"
    && typeof message.nonce === "string"
    && _pendingBroadcastProbe?.id === message.id
    && _pendingBroadcastProbe.nonce === message.nonce
  ) {
    _pendingBroadcastProbe.settle("occupied");
  }
}

function openClaimChannel(): BroadcastChannel | null {
  if (_claimChannel) return _claimChannel;
  if (typeof BroadcastChannel === "undefined") return null;
  try {
    const channel = new BroadcastChannel(CLAIM_CHANNEL);
    channel.onmessage = handleClaimMessage;
    _claimChannel = channel;
    return channel;
  } catch {
    return null;
  }
}

function discardClaimChannel(): void {
  const channel = _claimChannel;
  _claimChannel = null;
  _broadcastClaimedId = null;
  if (!channel) return;
  channel.onmessage = null;
  try { channel.close(); } catch { /* already closed */ }
}

function probeBroadcastId(channel: BroadcastChannel, id: string): Promise<BroadcastAttempt> {
  return new Promise<BroadcastAttempt>((resolve) => {
    const nonce = _randomId();
    let timer: ReturnType<typeof setTimeout> | null = null;
    const pending: PendingBroadcastProbe = {
      id,
      nonce,
      settle: (result) => {
        if (_pendingBroadcastProbe !== pending) return;
        _pendingBroadcastProbe = null;
        if (timer) clearTimeout(timer);
        if (result === "acquired") _broadcastClaimedId = id;
        resolve(result);
      },
    };
    _pendingBroadcastProbe = pending;
    timer = setTimeout(() => pending.settle("acquired"), BROADCAST_PROBE_WAIT_MS);
    try {
      channel.postMessage({ type: "probe", id, nonce });
    } catch {
      pending.settle("unavailable");
    }
  });
}

async function claimWithBroadcastChannel(): Promise<void> {
  const channel = openClaimChannel();
  if (!channel) return;
  let id = getTabId();
  for (;;) {
    const result = await probeBroadcastId(channel, id);
    if (result === "acquired") return;
    if (result === "unavailable") {
      discardClaimChannel();
      return;
    }
    id = rotateTabId(id, "broadcast-channel");
  }
}

async function claimCurrentTabIdentity(): Promise<void> {
  let locks: LockManager | null = null;
  try {
    locks = typeof navigator !== "undefined" && navigator.locks
      ? navigator.locks
      : null;
  } catch { /* navigator or locks denied by the embedding context */ }
  if (locks && await claimWithWebLocks(locks)) return;
  await claimWithBroadcastChannel();
  // If neither primitive exists, intentionally keep the sessionStorage ID.
}

/** Gate every bootstrap transport on one document-lifetime identity claim. */
export function claimTabIdentity(): Promise<void> {
  if (!_claimPromise) _claimPromise = claimCurrentTabIdentity();
  return _claimPromise;
}

/** Deterministic document teardown for focused fake-browser unit tests. */
export async function _releaseTabIdentityForTest(): Promise<void> {
  _pendingBroadcastProbe?.settle("unavailable");
  discardClaimChannel();

  const request = _webLockRequest;
  const release = _releaseWebLock;
  _releaseWebLock = null;
  release?.();
  if (request) {
    try { await request; } catch { /* failed lock requests are already degraded */ }
  }
  _webLockRequest = null;
  _claimPromise = null;
  _cached = null;
}

function _randomId(): string {
  // crypto.randomUUID is universal in modern browsers; fall back to
  // a 16-byte hex if unavailable (older WebViews, Bun test envs).
  const c = globalThis.crypto;
  if (c?.randomUUID) return c.randomUUID();
  const buf = new Uint8Array(16);
  if (c?.getRandomValues) c.getRandomValues(buf);
  else for (let i = 0; i < 16; i++) buf[i] = Math.floor(Math.random() * 256);
  return Array.from(buf).map((b) => b.toString(16).padStart(2, "0")).join("");
}
