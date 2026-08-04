import { afterAll, beforeEach, describe, expect, mock, test } from "bun:test";

const pushGetConfigCalls: unknown[] = [];
const pushSubscribeCalls: unknown[] = [];
const pushUnsubscribeCalls: unknown[] = [];
mock.module("../src/connect.ts", () => ({
  coordClient: {
    pushGetConfig: async (request: unknown) => {
      pushGetConfigCalls.push(request);
      return { available: true, vapidPublicKeyB64: "AQID" };
    },
    pushSubscribe: async (request: unknown) => {
      pushSubscribeCalls.push(request);
      return { ok: true };
    },
    pushUnsubscribe: async (request: unknown) => {
      pushUnsubscribeCalls.push(request);
      return { ok: true };
    },
  },
}));

const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
const originalNotification = Object.getOwnPropertyDescriptor(globalThis, "Notification");

let permission: NotificationPermission = "default";
let requestedPermission: NotificationPermission = "granted";
let requestCount = 0;
let registerCount = 0;
let subscribeCount = 0;
let unsubscribeCount = 0;
let subscription: PushSubscription | null = null;

const notification = {
  get permission() { return permission; },
  requestPermission: async () => {
    requestCount++;
    permission = requestedPermission;
    return permission;
  },
};

function makeSubscription(): PushSubscription {
  return {
    endpoint: "https://push.example/device",
    expirationTime: null,
    options: {} as PushSubscriptionOptions,
    getKey: () => null,
    toJSON: () => ({
      endpoint: "https://push.example/device",
      keys: { p256dh: "device-p256dh", auth: "device-auth" },
    }),
    unsubscribe: async () => {
      unsubscribeCount++;
      subscription = null;
      return true;
    },
  };
}

const registration = {
  pushManager: {
    getSubscription: async () => subscription,
    subscribe: async (options: PushSubscriptionOptionsInit) => {
      subscribeCount++;
      expect(options.userVisibleOnly).toBe(true);
      expect(options.applicationServerKey).toBeInstanceOf(Uint8Array);
      subscription = makeSubscription();
      return subscription;
    },
  },
};

const serviceWorker = {
  register: async () => { registerCount++; return registration; },
  ready: Promise.resolve(registration),
  getRegistration: async () => registration,
};

Object.defineProperty(globalThis, "Notification", { configurable: true, value: notification });
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { PushManager: class {}, Notification: notification },
});
Object.defineProperty(globalThis, "navigator", {
  configurable: true,
  value: { serviceWorker },
});

const {
  ensurePushSubscription,
  subscribeToPush,
  unsubscribeFromPush,
} = await import("../src/lib/push-client.ts");

beforeEach(() => {
  permission = "default";
  requestedPermission = "granted";
  requestCount = 0;
  registerCount = 0;
  subscribeCount = 0;
  unsubscribeCount = 0;
  subscription = null;
  pushGetConfigCalls.length = 0;
  pushSubscribeCalls.length = 0;
  pushUnsubscribeCalls.length = 0;
});

afterAll(() => {
  if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
  else Reflect.deleteProperty(globalThis, "window");
  if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
  else Reflect.deleteProperty(globalThis, "navigator");
  if (originalNotification) Object.defineProperty(globalThis, "Notification", originalNotification);
  else Reflect.deleteProperty(globalThis, "Notification");
});

describe("Web Push browser client", () => {
  test("does not contact the coordinator when permission is denied", async () => {
    requestedPermission = "denied";
    await expect(subscribeToPush()).rejects.toThrow("Allow notifications");
    expect(requestCount).toBe(1);
    expect(pushGetConfigCalls).toHaveLength(0);
    expect(pushSubscribeCalls).toHaveLength(0);
  });

  test("requests permission, creates a subscription, and upserts coordinator state", async () => {
    await subscribeToPush();
    expect(requestCount).toBe(1);
    expect(registerCount).toBe(1);
    expect(subscribeCount).toBe(1);
    expect(pushSubscribeCalls).toEqual([{
      endpoint: "https://push.example/device",
      p256dh: "device-p256dh",
      auth: "device-auth",
    }]);
  });

  test("repairs an enabled subscription without prompting and unsubscribes both sides", async () => {
    permission = "granted";
    subscription = makeSubscription();
    await ensurePushSubscription();
    expect(requestCount).toBe(0);
    expect(pushSubscribeCalls).toHaveLength(1);

    await unsubscribeFromPush();
    expect(unsubscribeCount).toBe(1);
    expect(pushUnsubscribeCalls).toEqual([{ endpoint: "https://push.example/device" }]);
  });
});
