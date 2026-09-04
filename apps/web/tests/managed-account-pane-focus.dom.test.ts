// Account credential retries replace loading, error, and loaded subtrees inside one stable region.
// Solid's universal renderer keeps current-tree connectivity accurate across each resource settlement.
// Auth and device modules are mocked so no protected credential material leaves the component.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import {
  click,
  createSolidTestHarness,
  findByTestId,
  type TestElement,
} from "./solid-universal-test-harness.ts";

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
}

interface Credentials {
  email: string;
  hasPassword: boolean;
  googleLinked: boolean;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
const harness = await createSolidTestHarness(Solid);
const { document: fakeDocument, element } = harness;
Object.defineProperty(globalThis, "document", { configurable: true, value: fakeDocument });
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { location: { hostname: "account.example.test", assign: () => undefined } },
});

let credentialRequests: Array<Deferred<Credentials>> = [];

class FakeBusyGate {
  current: string | null = null;

  begin(action: string): boolean {
    if (this.current) return false;
    this.current = action;
    return true;
  }

  finish(action: string): boolean {
    if (this.current !== action) return false;
    this.current = null;
    return true;
  }
}

class FakeGoogleUnavailableError extends Error {}
class FakeNewPasswordError extends Error {
  readonly issue = "invalid";
}

mock.module("../src/auth/managed-credentials.ts", () => ({
  MANAGED_CREDENTIALS_UNAVAILABLE_MESSAGE: "Sign-in methods are unavailable right now. Try again.",
  MANAGED_GOOGLE_IDENTITY_UNAVAILABLE_MESSAGE: "Google is unavailable.",
  MANAGED_PASSWORD_ADD_FAILED_MESSAGE: "Password could not be added.",
  ManagedAccountBusyGate: FakeBusyGate,
  ManagedGoogleIdentityUnavailableError: FakeGoogleUnavailableError,
  getManagedCredentials: () => {
    const request = credentialRequests.shift();
    if (!request) throw new Error("unexpected credential request");
    return request.promise;
  },
  addManagedPassword: async () => ({
    email: "owner@example.test",
    hasPassword: true,
    googleLinked: true,
  }),
  beginManagedGoogleLink: async () => ({ authorizationUrl: "https://accounts.example.test" }),
}));
mock.module("../src/auth/managed-account.ts", () => ({
  ManagedNewPasswordError: FakeNewPasswordError,
}));
mock.module("../src/auth/managed-logout.ts", () => ({
  logoutManagedBrowser: async () => undefined,
}));
mock.module("../src/components/Settings/DevicesPane.tsx", () => ({
  AuthorizedDevicesCard: () => element("section", { "data-testid": "devices-card" }),
}));
mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  Button: (props: Record<string, unknown>) => element("button", props),
  Card: (props: Record<string, unknown>) => element("section", props),
  List: (props: Record<string, unknown>) => element("div", props),
  ListRow: (props: Record<string, unknown>) => element("div", {
    "data-testid": props.testId,
    children: [props.headline, props.support],
  }),
  TextField: (props: Record<string, unknown>) => element("input", {
    ...props,
    "data-testid": props.testId,
  }),
}));

// Static imports would bind production auth modules before this test installs its seams.
const { AccountPane } = await import("../src/components/Settings/AccountPane.tsx");

beforeEach(() => {
  credentialRequests = [];
  fakeDocument.activeElement = null;
});

afterEach(() => harness.cleanup());

describe("managed account credential focus", () => {
  test("retry retains one live focus region through loading, failure, and success", async () => {
    const initial = deferred<Credentials>();
    const repeatedFailure = deferred<Credentials>();
    const recovered = deferred<Credentials>();
    credentialRequests.push(initial, repeatedFailure, recovered);
    const root = harness.mount(AccountPane);
    initial.reject(new TypeError("offline"));
    await harness.settle();

    let retry = findByTestId(root, "account-credentials-retry");
    const stableRegion = findByTestId(root, "account-credentials-state");
    if (!retry || !stableRegion) throw new Error("credential error surface did not mount");
    retry.focus();
    click(retry);
    expect(retry.isConnected).toBe(false);
    expect(fakeDocument.activeElement).toBe(stableRegion);

    repeatedFailure.reject(new TypeError("still offline"));
    await harness.settle();
    retry = findByTestId(root, "account-credentials-retry");
    if (!retry) throw new Error("credential retry did not return");
    expect(fakeDocument.activeElement).toBe(stableRegion);

    retry.focus();
    click(retry);
    expect(retry.isConnected).toBe(false);
    recovered.resolve({
      email: "owner@example.test",
      hasPassword: true,
      googleLinked: true,
    });
    await harness.settle();
    expect(findByTestId(root, "account-credential-email")).toBeDefined();
    expect(fakeDocument.activeElement).toBe(stableRegion);
  });
});
