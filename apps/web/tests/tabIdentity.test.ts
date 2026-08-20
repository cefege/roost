import { afterAll, afterEach, describe, expect, test } from "bun:test";

const values: Record<string, string> = {};
const storage = {
  getItem: (key: string) => values[key] ?? null,
  setItem: (key: string, value: string) => { values[key] = value; },
  removeItem: (key: string) => { delete values[key]; },
  clear: () => { for (const key of Object.keys(values)) delete values[key]; },
  key: () => null,
  get length() { return Object.keys(values).length; },
} satisfies Storage;

interface TestLockManager {
  request(
    name: string,
    options: LockOptions,
    callback: (lock: Lock | null) => unknown,
  ): Promise<unknown>;
}

class FakeLockManager {
  readonly held = new Set<string>();
  readonly externallyHeld = new Set<string>();

  request(
    name: string,
    _options: LockOptions,
    callback: (lock: Lock | null) => unknown,
  ): Promise<unknown> {
    if (this.held.has(name) || this.externallyHeld.has(name)) {
      return Promise.resolve(callback(null));
    }
    this.held.add(name);
    return Promise.resolve(callback({ name, mode: "exclusive" } as Lock))
      .finally(() => this.held.delete(name));
  }
}

type ProbeMessage = { type: string; id: string; nonce: string };

class FakeBroadcastChannel {
  static occupiedIds = new Set<string>();
  static probingIds = new Set<string>();
  onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
  readonly name: string;

  constructor(name: string) {
    this.name = name;
  }

  postMessage(value: unknown): void {
    const message = value as ProbeMessage;
    if (message.type !== "probe") return;
    if (FakeBroadcastChannel.occupiedIds.has(message.id)) {
      queueMicrotask(() => this.onmessage?.({
        data: { type: "occupied", id: message.id, nonce: message.nonce },
      } as MessageEvent<unknown>));
      return;
    }
    if (FakeBroadcastChannel.probingIds.delete(message.id)) {
      queueMicrotask(() => this.onmessage?.({
        data: { type: "probe", id: message.id, nonce: "peer-probe" },
      } as MessageEvent<unknown>));
    }
  }

  close(): void {}
}

const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
const originalSessionStorage = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");
const originalBroadcastChannel = Object.getOwnPropertyDescriptor(globalThis, "BroadcastChannel");
Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: storage });

// This module must initialize after the fake sessionStorage boundary exists.
const tabIdentity = await import("../src/auth/tab-id.ts");

function setBrowserPrimitives(
  locks?: TestLockManager,
  broadcast = false,
): void {
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: locks ? { locks } : {},
  });
  Object.defineProperty(globalThis, "BroadcastChannel", {
    configurable: true,
    value: broadcast ? FakeBroadcastChannel : undefined,
  });
}

afterAll(async () => {
  await tabIdentity._releaseTabIdentityForTest();
  if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
  else Reflect.deleteProperty(globalThis, "navigator");
  if (originalSessionStorage) Object.defineProperty(globalThis, "sessionStorage", originalSessionStorage);
  else Reflect.deleteProperty(globalThis, "sessionStorage");
  if (originalBroadcastChannel) Object.defineProperty(globalThis, "BroadcastChannel", originalBroadcastChannel);
  else Reflect.deleteProperty(globalThis, "BroadcastChannel");
});

afterEach(async () => {
  await tabIdentity._releaseTabIdentityForTest();
  storage.clear();
  FakeBroadcastChannel.occupiedIds.clear();
  FakeBroadcastChannel.probingIds.clear();
});

describe("tab identity arbitration", () => {
  test("holds one Web Lock and preserves the ID across a reload", async () => {
    const locks = new FakeLockManager();
    storage.setItem("roost.tabId", "stable-tab");
    setBrowserPrimitives(locks);

    await tabIdentity.claimTabIdentity();
    expect(tabIdentity.getTabId()).toBe("stable-tab");
    expect(locks.held.has("roost.tab-id:stable-tab")).toBe(true);

    await tabIdentity._releaseTabIdentityForTest();
    expect(locks.held.has("roost.tab-id:stable-tab")).toBe(false);
    await tabIdentity.claimTabIdentity();
    expect(tabIdentity.getTabId()).toBe("stable-tab");
  });

  test("rotates only a duplicated Web Lock identity", async () => {
    const locks = new FakeLockManager();
    locks.externallyHeld.add("roost.tab-id:copied-tab");
    storage.setItem("roost.tabId", "copied-tab");
    setBrowserPrimitives(locks);

    await tabIdentity.claimTabIdentity();
    const claimed = tabIdentity.getTabId();
    expect(claimed).not.toBe("copied-tab");
    expect(storage.getItem("roost.tabId")).toBe(claimed);
    expect(locks.externallyHeld.has("roost.tab-id:copied-tab")).toBe(true);
    expect(locks.held.has(`roost.tab-id:${claimed}`)).toBe(true);
  });

  test("rotates when the BroadcastChannel fallback reports an owner", async () => {
    storage.setItem("roost.tabId", "copied-broadcast-tab");
    FakeBroadcastChannel.occupiedIds.add("copied-broadcast-tab");
    setBrowserPrimitives(undefined, true);

    await tabIdentity.claimTabIdentity();
    expect(tabIdentity.getTabId()).not.toBe("copied-broadcast-tab");
    expect(storage.getItem("roost.tabId")).toBe(tabIdentity.getTabId());
  });

  test("rotates when another BroadcastChannel document probes concurrently", async () => {
    storage.setItem("roost.tabId", "concurrently-copied-tab");
    FakeBroadcastChannel.probingIds.add("concurrently-copied-tab");
    setBrowserPrimitives(undefined, true);

    await tabIdentity.claimTabIdentity();
    expect(tabIdentity.getTabId()).not.toBe("concurrently-copied-tab");
    expect(storage.getItem("roost.tabId")).toBe(tabIdentity.getTabId());
  });

  test("falls back after Web Locks throws without blocking startup", async () => {
    storage.setItem("roost.tabId", "locks-denied-tab");
    const deniedLocks = {
      request(
        _name: string,
        _options: LockOptions,
        _callback: (lock: Lock | null) => unknown,
      ): Promise<unknown> {
        throw new DOMException("denied", "SecurityError");
      },
    } satisfies TestLockManager;
    setBrowserPrimitives(deniedLocks);

    await tabIdentity.claimTabIdentity();
    expect(tabIdentity.getTabId()).toBe("locks-denied-tab");
  });

  test("degrades without rotating when no arbitration primitive exists", async () => {
    storage.setItem("roost.tabId", "unsupported-browser-tab");
    setBrowserPrimitives();

    await tabIdentity.claimTabIdentity();
    expect(tabIdentity.getTabId()).toBe("unsupported-browser-tab");
  });
});

