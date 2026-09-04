// These tests pin fragment parsing and synchronous credential capture.
// They model browser history and storage so secrets are scrubbed before SPA startup.
// Entry capture and URL cleanup must remain synchronous across every credential kind.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import {
  captureAndScrubFragmentCredential,
  clearCapturedFragmentCredential,
  credentialFreeUrl,
  parseFragmentCredential,
  peekCapturedFragmentCredential,
} from "../src/auth/fragment-credential.ts";
import type {
  CapturedFragmentCredential,
  CapturedFragmentCredentialKind,
} from "../src/auth/fragment-credential.ts";
import { coordBase } from "../src/connect.ts";
import { dispatchCapturedFragmentCredential } from "../src/store/sync-bootstrap.pair.ts";
const ROUTE_A = "a".repeat(64);
const ROUTE_B = "b".repeat(64);
const ACTIVATION_TOKEN = "A".repeat(43);
const RESET_TOKEN = "R".repeat(43);


class MemoryStorage implements Storage {
  readonly values = new Map<string, string>();

  get length(): number { return this.values.size; }
  clear(): void { this.values.clear(); }
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  key(index: number): string | null { return [...this.values.keys()][index] ?? null; }
  removeItem(key: string): void { this.values.delete(key); }
  setItem(key: string, value: string): void { this.values.set(key, value); }
}

interface FakeBrowser {
  location: {
    origin: string;
    pathname: string;
    search: string;
    hash: string;
  };
  session: MemoryStorage;
  local: MemoryStorage;
  restore(): void;
}

function installBrowser(
  input: {
    pathname: string;
    search?: string;
    hash?: string;
    session?: MemoryStorage;
    local?: MemoryStorage;
  },
  events: string[] = [],
): FakeBrowser {
  const locationValue = {
    origin: "https://c-test.dashboard.roosttt.com",
    pathname: input.pathname,
    search: input.search ?? "",
    hash: input.hash ?? "",
  };
  const session = input.session ?? new MemoryStorage();
  const local = input.local ?? new MemoryStorage();
  const historyValue: {
    state: unknown;
    replaceState(data: unknown, unused: string, url?: string | URL | null): void;
  } = {
    state: { retained: true },
    replaceState(data: unknown, _unused: string, url?: string | URL | null): void {
      this.state = data;
      if (url === undefined || url === null) return;
      events.push(`replace:${String(url)}`);
      const next = new URL(String(url), locationValue.origin);
      locationValue.pathname = next.pathname;
      locationValue.search = next.search;
      locationValue.hash = next.hash;
    },
  };
  const names = ["location", "history", "sessionStorage", "localStorage"] as const;
  const previous = new Map(
    names.map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)]),
  );
  Object.defineProperty(globalThis, "location", { configurable: true, value: locationValue });
  Object.defineProperty(globalThis, "history", { configurable: true, value: historyValue });
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: session });
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: local });

  return {
    location: locationValue,
    session,
    local,
    restore() {
      for (const name of names) {
        const descriptor = previous.get(name);
        if (descriptor) Object.defineProperty(globalThis, name, descriptor);
        else Reflect.deleteProperty(globalThis, name);
      }
    },
  };
}

function resetBaseCredentialState(): void {
  for (const kind of ["pair", "relocation", "activation", "reset"] as const) {
    clearCapturedFragmentCredential(kind);
  }
}

beforeEach(resetBaseCredentialState);

describe("fragment credential classifier", () => {
  test("accepts exactly one complete pair, relocation, activation, or reset shape", () => {
    expect(parseFragmentCredential("/", "#pair=one-shot")).toEqual({
      kind: "pair",
      token: "one-shot",
    });
    expect(parseFragmentCredential(`/pair/${ROUTE_A}`, "#pair=managed-shot")).toEqual({
      kind: "pair",
      token: "managed-shot",
      routeKey: ROUTE_A,
    });
    expect(parseFragmentCredential("/", "#move=relocation&handoff=id-1")).toEqual({
      kind: "relocation",
      token: "relocation",
      handoffId: "id-1",
    });
    expect(parseFragmentCredential(`/activate/${ROUTE_A}`, `#${ACTIVATION_TOKEN}`)).toEqual({
      kind: "activation",
      token: ACTIVATION_TOKEN,
      routeKey: ROUTE_A,
    });
    expect(parseFragmentCredential(`/reset-password/${ROUTE_B}`, `#${RESET_TOKEN}`)).toEqual({
      kind: "reset",
      token: RESET_TOKEN,
      routeKey: ROUTE_B,
    });
    expect(parseFragmentCredential("/reset-password", `#${RESET_TOKEN}`)).toEqual({
      kind: "reset",
      token: RESET_TOKEN,
    });
    expect(parseFragmentCredential("/", "#unrelated=value")).toEqual({ kind: "none" });
  });

  test("activation and reset token capture requires an exact route key and raw fragment", () => {
    for (const pathname of ["/", "/login", "/forgot-password"]) {
      expect(parseFragmentCredential(pathname, `#${ACTIVATION_TOKEN}`), pathname)
        .toEqual({ kind: "none" });
    }
    for (const pathname of ["/activate", "/activate/extra", `/activate/${ROUTE_A}`]) {
      expect(parseFragmentCredential(pathname, "#token=legacy"), pathname)
        .toEqual({ kind: "invalid" });
      expect(credentialFreeUrl({
        pathname,
        search: "?token=query-secret",
        hash: "#fragment-secret",
      })).toBe("/activate");
    }
  });

  test("rejects combined, partial, empty, and duplicate credential fields", () => {
    const cases: Array<[string, string]> = [
      ["/", "#pair=p&move=m&handoff=h"],
      ["/", "#move=m"],
      ["/", "#handoff=h"],
      ["/", "#move=&handoff=h"],
      ["/", "#pair="],
      ["/", "#pair=a&pair=b"],
      ["/", "#move=a&move=b&handoff=h"],
      [`/activate/${ROUTE_A}`, "#"],
      [`/activate/${ROUTE_A}`, "#token=legacy"],
      [`/activate/${ROUTE_A}`, "#short"],
      [`/reset-password/${ROUTE_B}`, `#${"x".repeat(44)}`],
    ];
    for (const [pathname, hash] of cases) {
      expect(parseFragmentCredential(pathname, hash), `${pathname}${hash}`)
        .toEqual({ kind: "invalid" });
    }
  });

  test("scrubs credential query and fragment fields without normalizing unrelated data", () => {
    expect(credentialFreeUrl({
      pathname: "/workspace",
      search: "?view=terminal&pair=query-secret&raw=a%2Fb",
      hash: "#keep=one&pair=fragment-secret&anchor&other=two",
    })).toBe("/workspace?view=terminal&raw=a%2Fb#keep=one&anchor&other=two");
    expect(credentialFreeUrl({
      pathname: `/activate/${ROUTE_A}`,
      search: "?next=%2Fapp&token=query-secret",
      hash: `#${ACTIVATION_TOKEN}`,
    })).toBe("/activate");
    expect(credentialFreeUrl({
      pathname: `/pair/${ROUTE_A}`,
      search: "",
      hash: "#pair=managed-shot",
    })).toBe("/");
    expect(credentialFreeUrl({
      pathname: "/",
      search: "?x=1",
      hash: "#move=m&handoff=h",
    })).toBe("/?x=1");
    expect(credentialFreeUrl({
      pathname: "/file/fp/path",
      search: "",
      hash: "#L42",
    })).toBe("/file/fp/path#L42");
  });
});

describe("synchronous entry capture", () => {
  test("index boots entry.ts, which scrubs before the SPA module evaluates", async () => {
    const html = await Bun.file(new URL("../index.html", import.meta.url)).text();
    expect(html).toContain('src="/src/entry.ts"');
    expect(html).not.toContain('src="/src/main.tsx"');

    const events: string[] = [];
    const browser = installBrowser({
      pathname: `/activate/${ROUTE_A}`,
      search: "?token=query-secret",
      hash: `#${ACTIVATION_TOKEN}`,
    }, events);
    try {
      mock.module("../src/main.tsx", () => {
        events.push(`main:${browser.location.pathname}${browser.location.search}${browser.location.hash}`);
        return {};
      });
      const entry = await import("../src/entry.ts");
      await entry.mainModulePromise;

      expect(events).toEqual([
        "replace:/activate",
        "main:/activate",
      ]);
      expect(peekCapturedFragmentCredential()).toEqual({
        kind: "activation",
        token: ACTIVATION_TOKEN,
        routeKey: ROUTE_A,
      });
      expect(browser.local.getItem("roost.tenantRouteKey")).toBe(ROUTE_A);
      expect(coordBase()).toBe(
        `https://c-test.dashboard.roosttt.com/_roost/t/${ROUTE_A}`,
      );
      expect(events.join("|")).not.toContain("query-secret");
      expect(events.join("|")).not.toContain("fragment-secret");
    } finally {
      clearCapturedFragmentCredential("activation");
      browser.restore();
    }
  });

  test("pair bootstrap consumes captured state only after its hash was scrubbed", async () => {
    const events: string[] = [];
    const browser = installBrowser({
      pathname: "/workspace",
      search: "?view=terminal",
      hash: "#keep=1&pair=pair-secret",
    }, events);
    try {
      expect(captureAndScrubFragmentCredential()).toEqual({
        kind: "pair",
        token: "pair-secret",
      });
      const redeemed = await dispatchCapturedFragmentCredential({
        peek: peekCapturedFragmentCredential,
        clear: (expectedKind) => {
          events.push(`clear:${expectedKind}`);
          return clearCapturedFragmentCredential(expectedKind);
        },
        reload: () => events.push("reload"),
        redeemPair: async (token) => {
          events.push(`pair:${token}`);
          return { ok: true };
        },
        redeemRelocation: async () => "success",
        warn: (message) => events.push(`warn:${message}`),
      });
      expect(redeemed).toBe(true);
      expect(events).toEqual([
        "replace:/workspace?view=terminal#keep=1",
        "pair:pair-secret",
        "clear:pair",
        "reload",
      ]);
      expect(peekCapturedFragmentCredential()).toBeNull();
    } finally {
      clearCapturedFragmentCredential("pair");
      browser.restore();
    }
  });
  test("notification navigation selects and scrubs the tenant route before app import", () => {
    const events: string[] = [];
    const browser = installBrowser({
      pathname: `/_roost/t/${ROUTE_A}/s/11111111-1111-4111-8111-111111111111`,
    }, events);
    try {
      expect(captureAndScrubFragmentCredential()).toEqual({ kind: "none" });
      expect(events).toEqual(["replace:/s/11111111-1111-4111-8111-111111111111"]);
      expect(browser.local.getItem("roost.tenantRouteKey")).toBe(ROUTE_A);
    } finally {
      browser.restore();
    }
  });


  test("captures once, persists through a module reload, and clears only the expected kind", async () => {
    const events: string[] = [];
    const session = new MemoryStorage();
    const browser = installBrowser({
      pathname: `/reset-password/${ROUTE_B}`,
      hash: `#${RESET_TOKEN}`,
      session,
    }, events);
    try {
      expect(captureAndScrubFragmentCredential()).toEqual({
        kind: "reset",
        token: RESET_TOKEN,
        routeKey: ROUTE_B,
      });
      expect(events).toEqual(["replace:/reset-password"]);
      expect(clearCapturedFragmentCredential("activation")).toBe(false);
      expect(peekCapturedFragmentCredential()).toEqual({
        kind: "reset",
        token: RESET_TOKEN,
        routeKey: ROUTE_B,
      });

      // A cache-distinct import models a document reload: module memory starts
      // empty, while the tab-scoped sessionStorage survives.
      const reloadUrl = new URL("../src/auth/fragment-credential.ts", import.meta.url);
      reloadUrl.search = "session-reload";
      const reloaded = await import(reloadUrl.href) as {
        peekCapturedFragmentCredential(): CapturedFragmentCredential | null;
        clearCapturedFragmentCredential(kind: CapturedFragmentCredentialKind): boolean;
      };
      expect(reloaded.peekCapturedFragmentCredential()).toEqual({
        kind: "reset",
        token: RESET_TOKEN,
        routeKey: ROUTE_B,
      });
      expect(reloaded.clearCapturedFragmentCredential("reset")).toBe(true);
      expect(reloaded.peekCapturedFragmentCredential()).toBeNull();
    } finally {
      clearCapturedFragmentCredential("reset");
      browser.restore();
    }
  });

  test("a captured relocation clears a stale coordinator override after hash scrubbing", () => {
    const local = new MemoryStorage();
    local.setItem("roost.deploymentMode", "self-hosted");
    local.setItem("roost.coordinatorUrl", "https://retired.example.test");
    const browser = installBrowser({
      pathname: "/app",
      hash: "#move=move-secret&handoff=handoff-id",
      local,
    });
    try {
      captureAndScrubFragmentCredential();
      expect(browser.location.hash).toBe("");
      expect(coordBase()).toBe("");
      expect(local.getItem("roost.coordinatorUrl")).toBeNull();
    } finally {
      clearCapturedFragmentCredential("relocation");
      browser.restore();
    }
  });

  test("diagnostic URL serialization removes query and fragment credentials", () => {
    const serialized = credentialFreeUrl({
      origin: "https://dashboard.roosttt.com",
      pathname: `/reset-password/${ROUTE_B}`,
      search: "?token=query-secret&keep=1",
      hash: `#${RESET_TOKEN}`,
    });
    expect(serialized).toBe(
      "https://dashboard.roosttt.com/reset-password",
    );
    expect(serialized).not.toContain("query-secret");
    expect(serialized).not.toContain("fragment-secret");
  });
});
